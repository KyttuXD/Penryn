// Port of the top-level decoder of libopus (mono output at 48 kHz): src/opus_decoder.c (opus_decode_frame,
// opus_decode_native, opus_decode / opus_decode_float, the PLC and FEC dispatch), src/opus.c
// (opus_packet_parse_impl, the opus_packet_get_* helpers, opus_pcm_soft_clip) and the float to int16
// conversion of celt/float_cast.h. Reference version: libopus 1.6.x (float build, decoder complexity 0).
//
// Copyright (c) 2010 Xiph.Org Foundation, Skype Limited
// Copyright (c) 2024 Arm Limited
// Written by Jean-Marc Valin and Koen Vos
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
//
// - Redistributions of source code must retain the above copyright
// notice, this list of conditions and the following disclaimer.
//
// - Redistributions in binary form must reproduce the above copyright
// notice, this list of conditions and the following disclaimer in the
// documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// ``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
// OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
// EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
// PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
// PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
// LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
// NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
// SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// What this file does: TOC / frame-count parsing with the RFC 6716 size rules, the mode and bandwidth
// dispatch to the SILK and CELT decoders (silk_dec.rs, celt_dec.rs), hybrid decoding on one shared range
// decoder, the CELT redundancy frames and the crossfades at mode transitions, the PLC / FEC dispatch,
// the soft clip and the float to int16 conversion. Mono API output only. Stereo packets (TOC stereo bit)
// are NOT decoded (the stage-1 SILK and CELT decoders are mono): they are rejected with
// `OpusError::Unsupported` and leave the decoder state untouched.
//
// No unsafe code; no panics on any input (all slicing is bounds-checked by construction or by `get`).

#![allow(dead_code)]

use super::celt_dec::CeltDecoder;
use super::celt_tables::WINDOW120;
use super::rangedec::RangeDecoder;
use super::silk_dec::SilkDecoder;
use alloc::vec::Vec;

/// 2.5 / 5 / 10 / 20 ms in samples at 48 kHz.
const F2_5: usize = 120;
const F5: usize = 240;
const F10: usize = 480;
const F20: usize = 960;
/// The longest packet: 120 ms.
pub const MAX_PACKET_SAMPLES: usize = 5760;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    SilkOnly,
    Hybrid,
    CeltOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bandwidth {
    Narrowband,
    Mediumband,
    Wideband,
    SuperWideband,
    Fullband,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpusError {
    BadArg,
    BufferTooSmall,
    InvalidPacket,
    Internal,
    /// A stereo packet (the stage-1 decoders are mono).
    Unsupported,
}

// ---------------------------------------------------------------------------------------------
// Packet parsing (opus.c)
// ---------------------------------------------------------------------------------------------

/// `opus_packet_get_mode`. `None` for an empty packet.
pub fn packet_get_mode(data: &[u8]) -> Option<Mode> {
    let t = *data.first()?;
    Some(if t & 0x80 != 0 {
        Mode::CeltOnly
    } else if t & 0x60 == 0x60 {
        Mode::Hybrid
    } else {
        Mode::SilkOnly
    })
}

/// `opus_packet_get_bandwidth`. `None` for an empty packet.
pub fn packet_get_bandwidth(data: &[u8]) -> Option<Bandwidth> {
    let t = *data.first()?;
    Some(if t & 0x80 != 0 {
        match (t >> 5) & 0x3 {
            0 => Bandwidth::Narrowband,
            1 => Bandwidth::Wideband,
            2 => Bandwidth::SuperWideband,
            _ => Bandwidth::Fullband,
        }
    } else if t & 0x60 == 0x60 {
        if t & 0x10 != 0 {
            Bandwidth::Fullband
        } else {
            Bandwidth::SuperWideband
        }
    } else {
        match (t >> 5) & 0x3 {
            0 => Bandwidth::Narrowband,
            1 => Bandwidth::Mediumband,
            _ => Bandwidth::Wideband,
        }
    })
}

/// `opus_packet_get_nb_channels`: 1 or 2 (the TOC stereo bit). `None` for an empty packet.
pub fn packet_get_nb_channels(data: &[u8]) -> Option<usize> {
    let t = *data.first()?;
    Some(if t & 0x4 != 0 { 2 } else { 1 })
}

/// `opus_packet_get_samples_per_frame` at 48 kHz. 0 for an empty packet.
pub fn packet_get_samples_per_frame(data: &[u8]) -> usize {
    let t = match data.first() {
        Some(t) => *t as usize,
        None => return 0,
    };
    if t & 0x80 != 0 {
        (48000usize << ((t >> 3) & 0x3)) / 400
    } else if t & 0x60 == 0x60 {
        if t & 0x08 != 0 {
            F20
        } else {
            F10
        }
    } else {
        let a = (t >> 3) & 0x3;
        if a == 3 {
            48000 * 60 / 1000
        } else {
            (48000usize << a) / 100
        }
    }
}

/// `opus_packet_get_nb_frames`.
pub fn packet_get_nb_frames(data: &[u8]) -> Result<usize, OpusError> {
    let t = match data.first() {
        Some(t) => *t,
        None => return Err(OpusError::BadArg),
    };
    match t & 0x3 {
        0 => Ok(1),
        3 => match data.get(1) {
            Some(c) => Ok((*c & 0x3F) as usize),
            None => Err(OpusError::InvalidPacket),
        },
        _ => Ok(2),
    }
}

/// `opus_packet_get_nb_samples` at 48 kHz (more than 120 ms is invalid).
pub fn packet_get_nb_samples(data: &[u8]) -> Result<usize, OpusError> {
    let count = packet_get_nb_frames(data)?;
    let samples = count * packet_get_samples_per_frame(data);
    if samples * 25 > 48000 * 3 {
        Err(OpusError::InvalidPacket)
    } else {
        Ok(samples)
    }
}

/// The result of `parse_packet`: the TOC, the number of frames, the offset of the first frame in the
/// packet and the frame sizes (frames follow each other from `payload_offset`).
#[derive(Clone, Debug)]
pub struct ParsedPacket {
    pub toc: u8,
    pub count: usize,
    pub payload_offset: usize,
    pub size: [usize; 48],
}

impl ParsedPacket {
    /// The start of frame `i` in the packet.
    pub fn frame_offset(&self, i: usize) -> usize {
        let mut o = self.payload_offset;
        for k in 0..i.min(48) {
            o += self.size[k];
        }
        o
    }
}

fn parse_size(data: &[u8], pos: usize, len: i32) -> (i32, i32) {
    if len < 1 {
        return (-1, -1);
    }
    let b0 = data.get(pos).copied().unwrap_or(0) as i32;
    if b0 < 252 {
        (1, b0)
    } else if len < 2 {
        (-1, -1)
    } else {
        let b1 = data.get(pos + 1).copied().unwrap_or(0) as i32;
        (2, 4 * b1 + b0)
    }
}

/// `opus_packet_parse_impl` (not self-delimited): RFC 6716 section 3.2 framing for the frame count codes
/// 0 to 3, with libopus's exact validity rules.
pub fn parse_packet(data: &[u8]) -> Result<ParsedPacket, OpusError> {
    if data.is_empty() {
        return Err(OpusError::InvalidPacket);
    }
    let framesize = packet_get_samples_per_frame(data);
    let toc = data[0];
    let mut pos = 1usize;
    let mut len = data.len() as i32 - 1;
    let mut last_size = len;
    let mut size = [0usize; 48];
    let mut sz = [0i32; 48];
    let count: usize;
    match toc & 0x3 {
        0 => count = 1,
        1 => {
            count = 2;
            if len & 1 != 0 {
                return Err(OpusError::InvalidPacket);
            }
            last_size = len / 2;
            sz[0] = last_size;
        }
        2 => {
            count = 2;
            let (bytes, s0) = parse_size(data, pos, len);
            if s0 < 0 {
                return Err(OpusError::InvalidPacket);
            }
            len -= bytes;
            if s0 > len {
                return Err(OpusError::InvalidPacket);
            }
            sz[0] = s0;
            pos += bytes as usize;
            last_size = len - s0;
        }
        _ => {
            if len < 1 {
                return Err(OpusError::InvalidPacket);
            }
            let ch = data[pos];
            pos += 1;
            count = (ch & 0x3F) as usize;
            if count == 0 || framesize * count > MAX_PACKET_SAMPLES {
                return Err(OpusError::InvalidPacket);
            }
            len -= 1;
            if ch & 0x40 != 0 {
                loop {
                    if len <= 0 {
                        return Err(OpusError::InvalidPacket);
                    }
                    let p = data.get(pos).copied().unwrap_or(0) as i32;
                    pos += 1;
                    len -= 1;
                    let tmp = if p == 255 { 254 } else { p };
                    len -= tmp;
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return Err(OpusError::InvalidPacket);
            }
            let cbr = ch & 0x80 == 0;
            if !cbr {
                last_size = len;
                for i in 0..count - 1 {
                    let (bytes, s) = parse_size(data, pos, len);
                    if s < 0 {
                        return Err(OpusError::InvalidPacket);
                    }
                    len -= bytes;
                    if s > len {
                        return Err(OpusError::InvalidPacket);
                    }
                    sz[i] = s;
                    pos += bytes as usize;
                    last_size -= bytes + s;
                }
                if last_size < 0 {
                    return Err(OpusError::InvalidPacket);
                }
            } else {
                last_size = len / count as i32;
                if last_size * count as i32 != len {
                    return Err(OpusError::InvalidPacket);
                }
                for i in 0..count - 1 {
                    sz[i] = last_size;
                }
            }
        }
    }
    // The size of the last frame (or of all frames, for CBR) is implicit; it must fit in 1275 bytes.
    if last_size > 1275 {
        return Err(OpusError::InvalidPacket);
    }
    sz[count - 1] = last_size;
    for i in 0..count {
        size[i] = sz[i].max(0) as usize;
    }
    Ok(ParsedPacket { toc, count, payload_offset: pos, size })
}

// ---------------------------------------------------------------------------------------------
// Float output helpers (opus.c soft clip, float_cast.h)
// ---------------------------------------------------------------------------------------------

/// `FLOAT2INT16`: scale by 32768, saturate, round to nearest even (lrintf).
#[inline]
fn float2int16(x: f32) -> i16 {
    let mut v = x * 32768.0;
    // MAX32 / MIN32 as in the C macros (a NaN ends up at the bound)
    v = if v > -32768.0 { v } else { -32768.0 };
    v = if v < 32767.0 { v } else { 32767.0 };
    // round half to even: |v| < 2^22, so adding and subtracting 1.5 * 2^23 rounds in the FPU
    let r = (v + 12582912.0f32) - 12582912.0f32;
    r as i16
}

/// `opus_pcm_soft_clip_impl` for one channel: a smooth limiter to [-1, 1] that keeps the zero crossings,
/// with the state `mem` (the non-linearity of the previous frame) carried over between calls.
fn soft_clip(x: &mut [f32], mem: &mut f32) {
    let n = x.len();
    if n < 1 {
        return;
    }
    // Clamp everything to [-2, +2], the domain of the non-linearity
    for v in x.iter_mut() {
        let mut c = *v;
        c = if -2.0f32 > c { -2.0 } else { c };
        c = if 2.0f32 < c { 2.0 } else { c };
        *v = c;
    }
    let mut a = *mem;
    // Continue applying the non-linearity from the previous frame to avoid any discontinuity
    let mut i = 0;
    while i < n {
        if x[i] * a >= 0.0 {
            break;
        }
        x[i] = x[i] + a * x[i] * x[i];
        i += 1;
    }
    let mut curr = 0usize;
    let x0 = x[0];
    loop {
        let mut i = curr;
        while i < n {
            if x[i] > 1.0 || x[i] < -1.0 {
                break;
            }
            i += 1;
        }
        if i == n {
            a = 0.0;
            break;
        }
        let mut peak_pos = i;
        let mut start = i;
        let mut end = i;
        let mut maxval = abs_f32(x[i]);
        // first zero crossing before the clipping
        while start > 0 && x[i] * x[start - 1] >= 0.0 {
            start -= 1;
        }
        // first zero crossing after the clipping, looking for other peaks on the way
        while end < n && x[i] * x[end] >= 0.0 {
            if abs_f32(x[end]) > maxval {
                maxval = abs_f32(x[end]);
                peak_pos = end;
            }
            end += 1;
        }
        // the special case: clipping before the first zero crossing
        let special = start == 0 && x[i] * x[0] >= 0.0;
        // a such that maxval + a * maxval^2 = 1
        a = (maxval - 1.0) / (maxval * maxval);
        a += a * 2.4e-7f32;
        if x[i] > 0.0 {
            a = -a;
        }
        for j in start..end {
            x[j] = x[j] + a * x[j] * x[j];
        }
        if special && peak_pos >= 2 {
            // a linear ramp from the first sample to the peak avoids a discontinuity at the frame start
            let mut offset = x0 - x[0];
            let delta = offset / peak_pos as f32;
            for j in curr..peak_pos {
                offset -= delta;
                x[j] += offset;
                let lo = if 1.0f32 < x[j] { 1.0 } else { x[j] };
                x[j] = if -1.0f32 > lo { -1.0 } else { lo };
            }
        }
        curr = end;
        if curr == n {
            break;
        }
    }
    *mem = a;
}

#[inline]
fn abs_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7FFF_FFFF)
}

/// `smooth_fade` (Fs = 48 kHz, mono): out[i] = w^2 * in2[i] + (1 - w^2) * in1[i] over `overlap` samples.
/// `out` may be the same buffer as one of the inputs, so the caller passes owned copies for those.
fn smooth_fade(in1: &[f32], in2: &[f32], out: &mut [f32], overlap: usize) {
    for i in 0..overlap {
        let mut w = WINDOW120[i];
        w = w * w;
        out[i] = w * in2[i] + (1.0f32 - w) * in1[i];
    }
}

fn endband_of(bw: Bandwidth) -> usize {
    match bw {
        Bandwidth::Narrowband => 13,
        Bandwidth::Mediumband | Bandwidth::Wideband => 17,
        Bandwidth::SuperWideband => 19,
        Bandwidth::Fullband => 21,
    }
}

// ---------------------------------------------------------------------------------------------
// The decoder
// ---------------------------------------------------------------------------------------------

pub struct OpusDecoder {
    silk: SilkDecoder,
    celt: CeltDecoder,
    /// DecControl.internalSampleRate in kHz (kept across packets: the PLC reuses it).
    silk_khz: u32,
    /// What the CELT decoder's start / end band settings (CELT_SET_START_BAND / END_BAND) are in libopus.
    celt_start: usize,
    celt_end: usize,
    /// The fields of struct OpusDecoder that a reset clears.
    mode: Mode,
    bandwidth: Bandwidth,
    prev_mode: Option<Mode>,
    frame_size: usize,
    prev_redundancy: bool,
    last_packet_duration: usize,
    softclip_mem: f32,
    range_final: u32,
    redundancy_frames: u32,
    concealed_frames: u32,
    scratch: Vec<f32>,
}

impl OpusDecoder {
    pub fn new() -> OpusDecoder {
        OpusDecoder {
            silk: SilkDecoder::new(),
            celt: CeltDecoder::new(),
            silk_khz: 0,
            celt_start: 0,
            celt_end: 21,
            mode: Mode::SilkOnly,
            bandwidth: Bandwidth::Narrowband,
            prev_mode: None,
            frame_size: F2_5,
            prev_redundancy: false,
            last_packet_duration: 0,
            softclip_mem: 0.0,
            range_final: 0,
            redundancy_frames: 0,
            concealed_frames: 0,
            scratch: Vec::new(),
        }
    }

    /// OPUS_RESET_STATE.
    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.mode = Mode::SilkOnly;
        self.bandwidth = Bandwidth::Narrowband;
        self.prev_mode = None;
        self.frame_size = F2_5;
        self.prev_redundancy = false;
        self.last_packet_duration = 0;
        self.softclip_mem = 0.0;
        self.range_final = 0;
    }

    /// Libopus-build compatibility switches of the SILK decoder (see `SilkDecoder::set_compat`): both
    /// default to true, matching the system libopus 1.6.1 (OSCE and deep PLC compiled in); false, false is
    /// the plain C build of the same sources.
    pub fn set_compat(&mut self, osce_clamp: bool, wb_glue_off: bool) {
        self.silk.set_compat(osce_clamp, wb_glue_off);
    }

    /// What the last packet was: (mode, bandwidth, frame size in samples at 48 kHz). Before the first
    /// packet this is (SilkOnly, Narrowband, 120).
    pub fn last_packet_info(&self) -> (Mode, Bandwidth, usize) {
        (self.mode, self.bandwidth, self.frame_size)
    }

    /// OPUS_GET_FINAL_RANGE: the final range coder state of the last decoded frame (XORed with the
    /// redundancy frame's), 0 after a concealed frame. For conformance checks.
    pub fn final_range(&self) -> u32 {
        self.range_final
    }

    /// How many CELT redundancy frames were decoded since creation (for statistics and tests).
    pub fn redundancy_frames(&self) -> u32 {
        self.redundancy_frames
    }

    /// How many frames were concealed (lost packets, empty frames, mode-transition concealment) since
    /// creation (for statistics and tests).
    pub fn concealed_frames(&self) -> u32 {
        self.concealed_frames
    }

    /// The number of samples of the last decoded or concealed packet (OPUS_GET_LAST_PACKET_DURATION).
    pub fn last_packet_duration(&self) -> usize {
        self.last_packet_duration
    }

    /// Decodes one Opus packet (with its TOC byte) into mono 16-bit samples at 48 kHz, as libopus's
    /// `opus_decode` does (soft clip, then round to nearest). `pcm.len()` is the room available (at most
    /// 5760 samples are used). An empty packet is a lost one, concealed for `last_packet_duration` samples.
    /// Returns the number of samples written.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize, OpusError> {
        if pcm.is_empty() {
            return Err(OpusError::BadArg);
        }
        let mut frame_size = pcm.len().min(MAX_PACKET_SAMPLES);
        if packet.is_empty() {
            frame_size = self.last_packet_duration.min(frame_size);
            frame_size -= frame_size % F2_5;
            if frame_size == 0 {
                return Err(OpusError::BadArg);
            }
        } else {
            let nb = packet_get_nb_samples(packet)?;
            if nb == 0 {
                return Err(OpusError::InvalidPacket);
            }
            frame_size = frame_size.min(nb);
        }
        let mut buf = core::mem::take(&mut self.scratch);
        buf.clear();
        buf.resize(frame_size, 0.0);
        let r = self.decode_native(packet, &mut buf, frame_size, false, true);
        if let Ok(n) = r {
            for i in 0..n.min(pcm.len()) {
                pcm[i] = float2int16(buf[i]);
            }
        }
        self.scratch = buf;
        r
    }

    /// As `decode`, float output (int16 / 32768 scale, no soft clip, no clipping): for mixing.
    pub fn decode_float(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, OpusError> {
        if pcm.is_empty() {
            return Err(OpusError::BadArg);
        }
        let mut frame_size = pcm.len().min(MAX_PACKET_SAMPLES);
        if packet.is_empty() {
            frame_size = self.last_packet_duration.min(frame_size);
            frame_size -= frame_size % F2_5;
            if frame_size == 0 {
                return Err(OpusError::BadArg);
            }
        } else {
            let nb = packet_get_nb_samples(packet)?;
            if nb == 0 {
                return Err(OpusError::InvalidPacket);
            }
            frame_size = frame_size.min(nb);
        }
        self.decode_native(packet, pcm, frame_size, false, false)
    }

    /// A lost packet: conceals `frame_samples` samples (a multiple of 120; the duration of the previous
    /// packet if 0), as `opus_decode(NULL, ...)` does. Returns the number of samples written.
    pub fn decode_lost(&mut self, frame_samples: usize, pcm: &mut [i16]) -> usize {
        let mut n = if frame_samples == 0 { self.last_packet_duration } else { frame_samples };
        n = n.min(pcm.len()).min(MAX_PACKET_SAMPLES);
        n -= n % F2_5;
        if n == 0 {
            return 0;
        }
        let mut buf = core::mem::take(&mut self.scratch);
        buf.clear();
        buf.resize(n, 0.0);
        let r = self.decode_native(&[], &mut buf, n, false, true);
        let mut written = 0;
        if let Ok(k) = r {
            written = k.min(n);
            for i in 0..written {
                pcm[i] = float2int16(buf[i]);
            }
        }
        self.scratch = buf;
        written
    }

    /// Float version of `decode_lost`.
    pub fn decode_lost_float(&mut self, frame_samples: usize, pcm: &mut [f32]) -> usize {
        let mut n = if frame_samples == 0 { self.last_packet_duration } else { frame_samples };
        n = n.min(pcm.len()).min(MAX_PACKET_SAMPLES);
        n -= n % F2_5;
        if n == 0 {
            return 0;
        }
        match self.decode_native(&[], pcm, n, false, false) {
            Ok(k) => k.min(n),
            Err(_) => 0,
        }
    }

    /// In-band FEC (opus_decode with decode_fec = 1): conceals the packet that PRECEDED `next_packet`,
    /// `pcm.len()` samples long (a multiple of 120), from the LBRR data in `next_packet` (SILK / hybrid
    /// packets only; otherwise, or where no FEC data is present, the loss concealment runs). The packet
    /// itself must then be decoded with `decode`.
    pub fn decode_fec(&mut self, next_packet: &[u8], pcm: &mut [i16]) -> Result<usize, OpusError> {
        if pcm.is_empty() {
            return Err(OpusError::BadArg);
        }
        let frame_size = pcm.len().min(MAX_PACKET_SAMPLES);
        let mut buf = core::mem::take(&mut self.scratch);
        buf.clear();
        buf.resize(frame_size, 0.0);
        let r = self.decode_native(next_packet, &mut buf, frame_size, true, true);
        if let Ok(n) = r {
            for i in 0..n.min(pcm.len()) {
                pcm[i] = float2int16(buf[i]);
            }
        }
        self.scratch = buf;
        r
    }

    /// `opus_decode_native` (mono; `packet` empty means lost).
    fn decode_native(&mut self, packet: &[u8], pcm: &mut [f32], frame_size_in: usize, decode_fec: bool, soft_clip_on: bool) -> Result<usize, OpusError> {
        let frame_size = frame_size_in.min(pcm.len());
        // For FEC / PLC, frame_size has to be a multiple of 2.5 ms
        if (decode_fec || packet.is_empty()) && frame_size % F2_5 != 0 {
            return Err(OpusError::BadArg);
        }
        if packet.is_empty() {
            let mut count = 0usize;
            loop {
                let ret = self.decode_frame(None, &mut pcm[count..], frame_size - count, false)?;
                count += ret;
                if count >= frame_size || ret == 0 {
                    break;
                }
            }
            self.last_packet_duration = count;
            return Ok(count);
        }

        let packet_mode = packet_get_mode(packet).unwrap_or(Mode::SilkOnly);
        let packet_bandwidth = packet_get_bandwidth(packet).unwrap_or(Bandwidth::Narrowband);
        let packet_frame_size = packet_get_samples_per_frame(packet);
        let packet_channels = packet_get_nb_channels(packet).unwrap_or(1);
        let parsed = parse_packet(packet)?;
        let count = parsed.count;

        if decode_fec {
            // If no FEC can be present, run the PLC (recursive call in libopus)
            if frame_size < packet_frame_size || packet_mode == Mode::CeltOnly || self.mode == Mode::CeltOnly {
                return self.decode_native(&[], pcm, frame_size, false, soft_clip_on);
            }
            if packet_channels == 2 {
                return Err(OpusError::Unsupported);
            }
            // Otherwise, run the PLC on everything except the size for which we might have FEC
            let duration_copy = self.last_packet_duration;
            if frame_size - packet_frame_size != 0 {
                let ret = self.decode_native(&[], pcm, frame_size - packet_frame_size, false, soft_clip_on);
                match ret {
                    Err(e) => {
                        self.last_packet_duration = duration_copy;
                        return Err(e);
                    }
                    Ok(_) => {}
                }
            }
            // Complete with FEC
            self.mode = packet_mode;
            self.bandwidth = packet_bandwidth;
            self.frame_size = packet_frame_size;
            let off = parsed.frame_offset(0);
            let frame = packet.get(off..off + parsed.size[0]).unwrap_or(&[]);
            let start = frame_size - packet_frame_size;
            self.decode_frame(Some(frame), &mut pcm[start..], packet_frame_size, true)?;
            self.last_packet_duration = frame_size;
            return Ok(frame_size);
        }

        if count * packet_frame_size > frame_size {
            return Err(OpusError::BufferTooSmall);
        }
        if packet_channels == 2 {
            return Err(OpusError::Unsupported);
        }

        // Update the state as the last step to avoid updating it on an invalid packet
        self.mode = packet_mode;
        self.bandwidth = packet_bandwidth;
        self.frame_size = packet_frame_size;

        let mut nb_samples = 0usize;
        let mut off = parsed.payload_offset;
        for i in 0..count {
            let sz = parsed.size[i];
            let frame = packet.get(off..off + sz).unwrap_or(&[]);
            let ret = self.decode_frame(Some(frame), &mut pcm[nb_samples..], frame_size - nb_samples, false)?;
            off += sz;
            nb_samples += ret;
        }
        self.last_packet_duration = nb_samples;
        if soft_clip_on {
            soft_clip(&mut pcm[..nb_samples], &mut self.softclip_mem);
        } else {
            self.softclip_mem = 0.0;
        }
        Ok(nb_samples)
    }

    /// `opus_decode_frame`: one frame of a packet (`data`, at least 2 bytes, else the frame is concealed),
    /// or a concealed frame (`data` None). Returns the number of samples written.
    fn decode_frame(&mut self, data: Option<&[u8]>, pcm: &mut [f32], frame_size_in: usize, decode_fec: bool) -> Result<usize, OpusError> {
        let mut frame_size = frame_size_in.min(pcm.len());
        if frame_size < F2_5 {
            return Err(OpusError::BufferTooSmall);
        }
        // Limit frame_size to avoid excessive allocations
        frame_size = frame_size.min(MAX_PACKET_SAMPLES);
        let mut len: i32 = data.map_or(0, |d| d.len() as i32);
        // Payloads of 1 (2 including the TOC) or 0 bytes trigger the PLC / DTX
        let data = if len <= 1 { None } else { data };
        if len <= 1 {
            // don't conceal more than what the TOC says
            frame_size = frame_size.min(self.frame_size);
        }
        let have = data.is_some();
        if !have || decode_fec {
            self.concealed_frames = self.concealed_frames.wrapping_add(1);
        }
        let mut rc = RangeDecoder::new(data.unwrap_or(&[]));
        let mode: Mode;
        let bandwidth: Option<Bandwidth>;
        let mut audiosize: usize;
        if have {
            audiosize = self.frame_size;
            mode = self.mode;
            bandwidth = Some(self.bandwidth);
        } else {
            audiosize = frame_size;
            // Run PLC using the last used mode (CELT if we ended with CELT redundancy)
            let m = if self.prev_redundancy { Some(Mode::CeltOnly) } else { self.prev_mode };
            bandwidth = None;
            match m {
                None => {
                    // no packet yet: all we can do is return zeros
                    for v in pcm[..audiosize].iter_mut() {
                        *v = 0.0;
                    }
                    return Ok(audiosize);
                }
                Some(m) => mode = m,
            }
            // Avoids running the PLC on sizes other than 2.5 (CELT), 5 (CELT), 10 or 20 ms
            if audiosize > F20 {
                let mut remaining = audiosize;
                let mut off = 0usize;
                loop {
                    let ret = self.decode_frame(None, &mut pcm[off..], remaining.min(F20), false)?;
                    off += ret;
                    remaining = remaining.saturating_sub(ret);
                    if remaining == 0 || ret == 0 {
                        break;
                    }
                }
                return Ok(frame_size);
            } else if audiosize < F20 {
                if audiosize > F10 {
                    audiosize = F10;
                } else if mode != Mode::SilkOnly && audiosize > F5 && audiosize < F10 {
                    audiosize = F5;
                }
            }
        }

        // CELT accumulates on top of the SILK output
        let celt_accum = mode != Mode::CeltOnly;

        let mut transition = false;
        let mut pcm_transition = [0.0f32; F5];
        if have {
            if let Some(pm) = self.prev_mode {
                if (mode == Mode::CeltOnly && pm != Mode::CeltOnly && !self.prev_redundancy) || (mode != Mode::CeltOnly && pm == Mode::CeltOnly) {
                    transition = true;
                    if mode == Mode::CeltOnly {
                        let _ = self.decode_frame(None, &mut pcm_transition, F5.min(audiosize), false);
                    }
                }
            }
        }
        if audiosize > frame_size {
            return Err(OpusError::BadArg);
        }
        frame_size = audiosize;

        // SILK processing
        if mode != Mode::CeltOnly {
            let pcm_too_small = frame_size < F10;
            if self.prev_mode == Some(Mode::CeltOnly) {
                self.silk.reset();
            }
            let mut pcm_silk = [0.0f32; F10];
            // The SILK PLC cannot produce frames of less than 10 ms
            let payload_ms = core::cmp::max(10, 1000 * audiosize / 48000) as u32;
            if have {
                self.silk_khz = if mode == Mode::SilkOnly {
                    match self.bandwidth {
                        Bandwidth::Narrowband => 8,
                        Bandwidth::Mediumband => 12,
                        _ => 16,
                    }
                } else {
                    16
                };
            }
            if self.silk_khz == 0 {
                self.silk_khz = 16;
            }
            let khz = self.silk_khz;
            let n = {
                let out: &mut [f32] = if pcm_too_small { &mut pcm_silk[..] } else { &mut pcm[..] };
                if !have {
                    self.silk.decode_lost(khz, payload_ms, out)
                } else if decode_fec {
                    self.silk.decode_fec(&mut rc, khz, payload_ms, out)
                } else {
                    self.silk.decode(&mut rc, khz, payload_ms, out)
                }
            };
            if n == 0 {
                if !have || decode_fec {
                    // PLC failure is not fatal: silence
                    let out: &mut [f32] = if pcm_too_small { &mut pcm_silk[..] } else { &mut pcm[..] };
                    for v in out[..frame_size].iter_mut() {
                        *v = 0.0;
                    }
                } else {
                    return Err(OpusError::Internal);
                }
            }
            if pcm_too_small {
                pcm[..frame_size].copy_from_slice(&pcm_silk[..frame_size]);
            }
        }

        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes: i32 = 0;
        if !decode_fec && mode != Mode::CeltOnly && have && rc.tell() + 17 + if mode == Mode::Hybrid { 20 } else { 0 } <= 8 * len {
            // Check if we have a redundant 0-8 kHz band
            redundancy = if mode == Mode::Hybrid { rc.bit_logp(12) != 0 } else { true };
            if redundancy {
                celt_to_silk = rc.bit_logp(1) != 0;
                // redundancy_bytes is at least two, in the non-hybrid case due to the tell check above
                redundancy_bytes = if mode == Mode::Hybrid { rc.uint(256) as i32 + 2 } else { len - ((rc.tell() + 7) >> 3) };
                len -= redundancy_bytes;
                // A sanity check: it should never happen for a valid packet
                if len * 8 < rc.tell() {
                    len = 0;
                    redundancy_bytes = 0;
                    redundancy = false;
                }
                // Shrink the decoder because of the raw bits
                rc.shrink(redundancy_bytes as u32);
            }
        }
        let start_band = if mode != Mode::CeltOnly { 17 } else { 0 };

        if redundancy {
            transition = false;
        }
        if transition && mode != Mode::CeltOnly {
            let _ = self.decode_frame(None, &mut pcm_transition, F5.min(audiosize), false);
        }

        if let Some(bw) = bandwidth {
            self.celt_end = endband_of(bw);
        }

        // The redundant frame (5 ms), CELT-coded after the main frame's bytes
        let red_data: &[u8] = match data {
            Some(d) if redundancy => {
                let a = len.max(0) as usize;
                d.get(a..a + redundancy_bytes.max(0) as usize).unwrap_or(&[])
            }
            _ => &[],
        };
        let mut redundant_audio = [0.0f32; F5];
        let mut redundant_rng = 0u32;

        // 5 ms redundant frame for CELT -> SILK
        if redundancy {
            self.redundancy_frames = self.redundancy_frames.wrapping_add(1);
        }
        if redundancy && celt_to_silk {
            self.celt_start = 0;
            let mut rrc = RangeDecoder::new(red_data);
            self.celt.decode(&mut rrc, F5, 0, self.celt_end, false, &mut redundant_audio);
            redundant_rng = self.celt.final_range();
        }

        // MUST be after the PLC
        self.celt_start = start_band;

        let mut celt_failed = false;
        if mode != Mode::SilkOnly {
            let celt_frame_size = F20.min(frame_size);
            // Make sure to discard any previous CELT state
            if let Some(pm) = self.prev_mode {
                if mode != pm && !self.prev_redundancy {
                    self.celt.reset();
                }
            }
            // Decode CELT (an empty range decoder makes the CELT decoder conceal)
            let mut empty = RangeDecoder::new(&[]);
            let use_main = !decode_fec && have && len > 1;
            let rcsel: &mut RangeDecoder = if use_main { &mut rc } else { &mut empty };
            let ok = self.celt.decode(rcsel, celt_frame_size, start_band, self.celt_end, celt_accum, pcm);
            if !ok {
                celt_failed = true;
            }
            self.range_final = self.celt.final_range();
        } else {
            // For hybrid -> SILK transitions, we let the CELT MDCT do a fade-out by decoding a silence frame
            if self.prev_mode == Some(Mode::Hybrid) && !(redundancy && celt_to_silk && self.prev_redundancy) {
                self.celt_start = 0;
                let silence = [0xFFu8, 0xFF];
                let mut src = RangeDecoder::new(&silence);
                self.celt.decode(&mut src, F2_5, 0, self.celt_end, celt_accum, pcm);
            }
            // The range decoder is not initialised when concealing
            self.range_final = if have { rc.range() } else { 0 };
        }

        // 5 ms redundant frame for SILK -> CELT
        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt_start = 0;
            let mut rrc = RangeDecoder::new(red_data);
            self.celt.decode(&mut rrc, F5, 0, self.celt_end, false, &mut redundant_audio);
            redundant_rng = self.celt.final_range();
            let base = frame_size - F2_5;
            let mut tail = [0.0f32; F2_5];
            tail.copy_from_slice(&pcm[base..base + F2_5]);
            smooth_fade(&tail, &redundant_audio[F2_5..], &mut pcm[base..base + F2_5], F2_5);
        }
        // 5 ms redundant frame for CELT -> SILK; ignored if the previous frame did not use CELT (the first
        // redundancy frame in a transition from SILK may have been lost)
        if redundancy && celt_to_silk && (self.prev_mode != Some(Mode::SilkOnly) || self.prev_redundancy) {
            pcm[..F2_5].copy_from_slice(&redundant_audio[..F2_5]);
            let mut head = [0.0f32; F2_5];
            head.copy_from_slice(&pcm[F2_5..2 * F2_5]);
            smooth_fade(&redundant_audio[F2_5..], &head, &mut pcm[F2_5..2 * F2_5], F2_5);
        }
        if transition {
            if audiosize >= F5 {
                pcm[..F2_5].copy_from_slice(&pcm_transition[..F2_5]);
                let mut head = [0.0f32; F2_5];
                head.copy_from_slice(&pcm[F2_5..2 * F2_5]);
                smooth_fade(&pcm_transition[F2_5..], &head, &mut pcm[F2_5..2 * F2_5], F2_5);
            } else {
                // Not enough time for a clean transition, but we do it anyway
                let mut head = [0.0f32; F2_5];
                head.copy_from_slice(&pcm[..F2_5]);
                smooth_fade(&pcm_transition[..F2_5], &head, &mut pcm[..F2_5], F2_5);
            }
        }

        if len <= 1 {
            self.range_final = 0;
        } else {
            self.range_final ^= redundant_rng;
        }

        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;

        if celt_failed {
            Err(OpusError::Internal)
        } else {
            Ok(audiosize)
        }
    }
}
