// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Written by Jean-Marc Valin
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

//! A mono, 48 kHz, CELT-only Opus encoder: the essential parts of celt/celt_encoder.c (float
//! build). It writes a complete Opus packet (TOC byte + CELT frame, code 0) that libopus and
//! `CeltDecoder` decode. Always long blocks (no transient detection / TF analysis), no pitch
//! pre-filter, no stereo, CBR. Included from libopus: range coding of the energies (two-pass
//! intra/inter coarse energy, fine energy, finalisation), band allocation with the skipping
//! decision, dynamic allocation boosts, the allocation trim analysis, the spreading decision and
//! the PVQ search.
//!
//! Output scaling: the decoder emits int16 / 32768, so the encoder works on int16-valued floats.

use alloc::vec;
use alloc::vec::Vec;

use super::celt_enc_alloc::clt_compute_allocation_enc;
use super::celt_enc_bands::quant_all_bands;
use super::celt_enc_energy::{
    amp2log2, compute_band_energies, normalise_bands, quant_coarse_energy, quant_energy_finalise, quant_fine_energy,
};
use super::celt_enc_math::round_half_up;
use super::celt_enc_mdct::MdctFwd;
use super::celt_modes::{CeltMode, NB_EBANDS, OVERLAP};
use super::celt_rate::{init_caps, BITRES};
use super::celt_tables::{E_MEANS, WINDOW120};
use super::celt_vq::{SPREAD_AGGRESSIVE, SPREAD_LIGHT, SPREAD_NONE, SPREAD_NORMAL};
use super::mathops::{absf, floor_f64, maxf, minf};
use super::rangeenc::RangeEncoder;

const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];

/// The pre-emphasis coefficient of the 48 kHz mode.
const PREEMPH0: f32 = 0.85000610f32;

/// The bandwidth (number of coded bands) the TOC byte can signal.
const BAND_ENDS: [usize; 4] = [13, 17, 19, 21];

pub struct CeltEncoder {
    fwd: MdctFwd,
    end: usize,
    preemph_mem: f32,
    /// The last OVERLAP pre-emphasised samples of the previous frame.
    in_mem: [f32; OVERLAP],
    /// The largest magnitude among the last OVERLAP input samples of the previous frame.
    overlap_max: f32,
    old_band_e: [f32; NB_EBANDS],
    energy_error: [f32; NB_EBANDS],
    spread_decision: i32,
    tonal_average: i32,
    last_coded_bands: usize,
    /// The next frame is coded without prediction from the previous energies.
    force_intra: bool,
    /// Tuning knobs for experiments.
    pub tune: Tuning,
    /// True if the range coder overran its buffer in the last frame (never expected; the packet
    /// is then still well-formed but truncated).
    pub last_frame_error: bool,
    /// Diagnostics of the last frame: the measured band log energies (before the quantisation bias).
    pub dbg_band_log_e: [f32; NB_EBANDS],
    pub dbg_intra: bool,
}

/// Encoder decisions that can be switched for experiments.
#[derive(Clone, Copy)]
pub struct Tuning {
    pub dynalloc: bool,
    pub trim_analysis: bool,
    pub spread_analysis: bool,
    /// The lsb_depth of dynalloc_analysis's noise floor (24 in libopus' default).
    pub lsb_depth: i32,
    /// Fixed trim used when the analysis is off.
    pub fixed_trim: i32,
}

impl Tuning {
    pub const fn default() -> Tuning {
        Tuning { dynalloc: true, trim_analysis: true, spread_analysis: true, lsb_depth: 24, fixed_trim: 5 }
    }
}

fn median_of_5(x: &[f32]) -> f32 {
    let t2 = x[2];
    let (mut t0, mut t1) = if x[0] > x[1] { (x[1], x[0]) } else { (x[0], x[1]) };
    let (mut t3, mut t4) = if x[3] > x[4] { (x[4], x[3]) } else { (x[3], x[4]) };
    if t0 > t3 {
        core::mem::swap(&mut t0, &mut t3);
        core::mem::swap(&mut t1, &mut t4);
    }
    if t2 > t1 {
        if t1 < t3 {
            minf(t2, t3)
        } else {
            minf(t4, t1)
        }
    } else if t2 < t3 {
        minf(t1, t3)
    } else {
        minf(t2, t4)
    }
}

fn median_of_3(x: &[f32]) -> f32 {
    let (t0, t1) = if x[0] > x[1] { (x[1], x[0]) } else { (x[0], x[1]) };
    let t2 = x[2];
    if t1 < t2 {
        t1
    } else if t0 < t2 {
        t2
    } else {
        t0
    }
}

/// `dynalloc_analysis` (mono, non-transient CBR, no tone / analysis information): the number of
/// boost quanta per band, and the spread weights. Returns the total boost in 1/8 bits.
fn dynalloc_analysis(
    band_log_e: &[f32; NB_EBANDS],
    end: usize,
    lm: usize,
    effective_bytes: i32,
    lsb_depth: i32,
    enable: bool,
    offsets: &mut [i32; NB_EBANDS],
    spread_weight: &mut [i32; NB_EBANDS],
) -> i32 {
    let mut tot_boost = 0i32;
    *offsets = [0; NB_EBANDS];
    let mut noise_floor = [0.0f32; NB_EBANDS];
    let mut max_depth = -31.9f32;
    for i in 0..end {
        noise_floor[i] = 0.0625 * CeltMode::log_n(i) as f32 + 0.5 + (9 - lsb_depth) as f32 - E_MEANS[i]
            + 0.0062 * ((i + 5) * (i + 5)) as f32;
        max_depth = maxf(max_depth, band_log_e[i] - noise_floor[i]);
    }
    // A really simple masking model to avoid taking into account completely masked bands when
    // computing the spreading decision.
    {
        let mut mask = [0.0f32; NB_EBANDS];
        let mut sig = [0.0f32; NB_EBANDS];
        for i in 0..end {
            mask[i] = band_log_e[i] - noise_floor[i];
            sig[i] = mask[i];
        }
        for i in 1..end {
            mask[i] = maxf(mask[i], mask[i - 1] - 2.0);
        }
        for i in (0..end.saturating_sub(1)).rev() {
            mask[i] = maxf(mask[i], mask[i + 1] - 3.0);
        }
        for i in 0..end {
            let smr = sig[i] - maxf(maxf(0.0, max_depth - 12.0), mask[i]);
            let shift = core::cmp::min(5, core::cmp::max(0, -(floor_f64(0.5 + smr as f64) as i32)));
            spread_weight[i] = 32 >> shift;
        }
    }
    // Make sure that dynamic allocation can't make us bust the budget.
    if enable && effective_bytes >= (30 + 5 * lm as i32) && end >= 5 {
        let mut f = [0.0f32; NB_EBANDS];
        let b3 = band_log_e;
        let mut last = 0usize;
        f[0] = b3[0];
        for i in 1..end {
            // The last band to be at least 3 dB higher than the previous one is the last we'll
            // consider. Otherwise, we run into problems on bandlimited signals.
            if b3[i] > b3[i - 1] + 0.5 {
                last = i;
            }
            f[i] = minf(f[i - 1] + 1.5, b3[i]);
        }
        for i in (0..last).rev() {
            f[i] = minf(f[i], minf(f[i + 1] + 2.0, b3[i]));
        }
        // Combine with a median filter to avoid dynalloc triggering unnecessarily.
        let offset = 1.0f32;
        for i in 2..end - 2 {
            f[i] = maxf(f[i], median_of_5(&b3[i - 2..]) - offset);
        }
        let tmp = median_of_3(&b3[0..]) - offset;
        f[0] = maxf(f[0], tmp);
        f[1] = maxf(f[1], tmp);
        let tmp = median_of_3(&b3[end - 3..]) - offset;
        f[end - 2] = maxf(f[end - 2], tmp);
        f[end - 1] = maxf(f[end - 1], tmp);
        for i in 0..end {
            f[i] = maxf(f[i], noise_floor[i]);
        }
        for i in 0..end {
            f[i] = maxf(0.0, band_log_e[i] - f[i]);
        }
        // For non-transient CBR frames, halve the dynalloc contribution
        for i in 0..end {
            f[i] *= 0.5;
        }
        for i in 0..end {
            if i < 8 {
                f[i] *= 2.0;
            }
            if i >= 12 {
                f[i] *= 0.5;
            }
        }
        if effective_bytes > 320 {
            f[0] += minf(1.5, 1e-3 * (effective_bytes - 320) as f32);
        }
        for i in 0..end {
            f[i] = minf(f[i], 4.0);
            let width = (CeltMode::ebands(i + 1) - CeltMode::ebands(i)) << lm;
            let boost;
            let boost_bits;
            if width < 6 {
                boost = f[i] as i32;
                boost_bits = (boost * width) << BITRES;
            } else if width > 48 {
                boost = (f[i] * 8.0) as i32;
                boost_bits = ((boost * width) << BITRES) / 8;
            } else {
                boost = (f[i] * width as f32 / 6.0) as i32;
                boost_bits = (boost * 6) << BITRES;
            }
            // For CBR frames, limit dynalloc to 2/3 of the bits
            if ((tot_boost + boost_bits) >> BITRES >> 3) > 2 * effective_bytes / 3 {
                let cap = (2 * effective_bytes / 3) << BITRES << 3;
                offsets[i] = cap - tot_boost;
                tot_boost = cap;
                break;
            } else {
                offsets[i] = boost;
                tot_boost += boost_bits;
            }
        }
    }
    tot_boost
}

/// `alloc_trim_analysis` (mono, no tonality information): the spectral tilt based trim.
fn alloc_trim_analysis(band_log_e: &[f32; NB_EBANDS], end: usize, equiv_rate: i32) -> i32 {
    let mut trim = 5.0f32;
    // At low bitrate, reducing the trim seems to help.
    if equiv_rate < 64000 {
        trim = 4.0;
    } else if equiv_rate < 80000 {
        let frac = (equiv_rate - 64000) >> 10;
        trim = 4.0 + (1.0 / 16.0) * frac as f32;
    }
    // Estimate spectral tilt
    let mut diff = 0.0f32;
    for i in 0..end - 1 {
        diff += band_log_e[i] * (2 + 2 * i as i32 - end as i32) as f32;
    }
    diff /= (end - 1) as f32;
    trim -= maxf(-2.0, minf(2.0, (diff + 1.0) / 6.0));
    let idx = round_half_up(trim);
    core::cmp::max(0, core::cmp::min(10, idx))
}

/// `spreading_decision` (mono).
fn spreading_decision(
    x: &[f32],
    average: &mut i32,
    last_decision: i32,
    end: usize,
    m: usize,
    spread_weight: &[i32; NB_EBANDS],
) -> i32 {
    let eb = |i: usize| CeltMode::ebands(i) as usize;
    if m * (eb(end) - eb(end - 1)) <= 8 {
        return SPREAD_NONE;
    }
    let mut sum = 0i32;
    let mut nb_bands = 0i32;
    for i in 0..end {
        let n = m * (eb(i + 1) - eb(i));
        if n <= 8 {
            continue;
        }
        let xs = &x[m * eb(i)..m * eb(i) + n];
        let mut tcount = [0i32; 3];
        for &v in xs {
            let x2n = v * v * n as f32;
            if x2n < 0.25 {
                tcount[0] += 1;
            }
            if x2n < 0.0625 {
                tcount[1] += 1;
            }
            if x2n < 0.015625 {
                tcount[2] += 1;
            }
        }
        let nn = n as i32;
        let tmp = (2 * tcount[2] >= nn) as i32 + (2 * tcount[1] >= nn) as i32 + (2 * tcount[0] >= nn) as i32;
        sum += tmp * spread_weight[i];
        nb_bands += spread_weight[i];
    }
    if nb_bands <= 0 {
        return SPREAD_NORMAL;
    }
    sum = (sum << 8) / nb_bands;
    // Recursive averaging
    sum = (sum + *average) >> 1;
    *average = sum;
    // Hysteresis
    sum = (3 * sum + (((3 - last_decision) << 7) + 64) + 2) >> 2;
    if sum < 80 {
        SPREAD_AGGRESSIVE
    } else if sum < 256 {
        SPREAD_NORMAL
    } else if sum < 384 {
        SPREAD_LIGHT
    } else {
        SPREAD_NONE
    }
}

impl CeltEncoder {
    pub fn new() -> CeltEncoder {
        CeltEncoder {
            fwd: MdctFwd::new(),
            end: 21,
            preemph_mem: 0.0,
            in_mem: [0.0; OVERLAP],
            overlap_max: 0.0,
            old_band_e: [0.0; NB_EBANDS],
            energy_error: [0.0; NB_EBANDS],
            spread_decision: SPREAD_NORMAL,
            tonal_average: 256,
            last_coded_bands: 0,
            force_intra: true,
            tune: Tuning::default(),
            last_frame_error: false,
            dbg_band_log_e: [0.0; NB_EBANDS],
            dbg_intra: false,
        }
    }

    /// Forgets all history (as a freshly created encoder).
    pub fn reset(&mut self) {
        self.preemph_mem = 0.0;
        self.in_mem = [0.0; OVERLAP];
        self.overlap_max = 0.0;
        self.old_band_e = [0.0; NB_EBANDS];
        self.energy_error = [0.0; NB_EBANDS];
        self.spread_decision = SPREAD_NORMAL;
        self.tonal_average = 256;
        self.last_coded_bands = 0;
        self.force_intra = true;
    }

    /// Makes the next frame independent of the previous ones' band energies (call it when a
    /// receiver may have lost frames or started to listen mid-stream; costs some bits once).
    pub fn request_intra(&mut self) {
        self.force_intra = true;
    }

    /// The reconstructed band log energies after the last frame (what the decoder holds).
    pub fn recon_band_log_e(&self) -> [f32; NB_EBANDS] {
        self.old_band_e
    }

    /// Bandwidth in coded bands: 13 (narrowband), 17 (wideband), 19 (super-wideband) or 21
    /// (fullband); other values are rounded up to the next valid one.
    pub fn set_end_band(&mut self, end: usize) {
        self.end = if end <= 13 {
            13
        } else if end <= 17 {
            17
        } else if end <= 19 {
            19
        } else {
            21
        };
    }

    /// Encodes one frame of mono audio (48 kHz, `pcm.len()` of 120, 240, 480 or 960 samples; other
    /// lengths use the largest of those that fits, zero padded below 120) into an Opus packet of at
    /// most `max_bytes` bytes (CBR: the payload is `bitrate * frame_ms / 8000` bytes, at least 2).
    /// An all-zero frame (after an all-zero previous tail) is coded as a 2-byte payload. With
    /// `max_bytes` < 3 only the TOC byte is produced (which decoders treat as a lost frame).
    pub fn encode(&mut self, pcm: &[i16], bitrate: u32, max_bytes: usize) -> Vec<u8> {
        let fs = if pcm.len() >= 960 {
            960
        } else if pcm.len() >= 480 {
            480
        } else if pcm.len() >= 240 {
            240
        } else {
            120
        };
        let lm = match fs {
            120 => 0usize,
            240 => 1,
            480 => 2,
            _ => 3,
        };
        let bw = BAND_ENDS.iter().position(|&e| e == self.end).unwrap_or(3);
        let toc = (((16 + 4 * bw + lm) as u8) << 3) as u8;
        if max_bytes == 0 {
            return Vec::new();
        }
        let mut x = vec![0.0f32; fs];
        for i in 0..fs.min(pcm.len()) {
            x[i] = pcm[i] as f32;
        }
        let bytes = ((bitrate as u64 * fs as u64 + 4 * 48000) / (8 * 48000)) as usize;
        let cap = core::cmp::min(1275, max_bytes - 1);
        let nbytes = core::cmp::max(2, core::cmp::min(bytes, cap));
        if cap < 2 {
            // not even the minimum payload fits: a TOC-only packet (decoded as a lost frame)
            let _ = self.encode_frame(&x, lm, 2);
            return vec![toc];
        }
        let payload = self.encode_frame(&x, lm, nbytes);
        let mut out = Vec::with_capacity(1 + payload.len());
        out.push(toc);
        out.extend_from_slice(&payload);
        out
    }

    /// One CELT frame of `120 << lm` samples into exactly `nbytes` bytes (2 for a silent frame).
    fn encode_frame(&mut self, pcm: &[f32], lm: usize, nbytes: usize) -> Vec<u8> {
        let n = SHORT << lm;
        let m = 1usize << lm;
        let start = 0usize;
        let end = self.end;
        let eff_end = core::cmp::min(end, NB_EBANDS);

        // Silence detection: the frame and the previous frame's tail are all zero.
        let mut sample_max = self.overlap_max;
        for i in 0..n - OVERLAP {
            sample_max = maxf(sample_max, absf(pcm[i]));
        }
        let mut om = 0.0f32;
        for i in n - OVERLAP..n {
            om = maxf(om, absf(pcm[i]));
        }
        self.overlap_max = om;
        sample_max = maxf(sample_max, om);
        let silence = !(sample_max > 0.0);

        // Pre-emphasis; the first OVERLAP samples are the previous frame's tail.
        let mut inb = vec![0.0f32; n + OVERLAP];
        inb[..OVERLAP].copy_from_slice(&self.in_mem);
        let mut mem = self.preemph_mem;
        for i in 0..n {
            let xv = pcm[i];
            inb[OVERLAP + i] = xv - mem;
            mem = PREEMPH0 * xv;
        }
        self.preemph_mem = mem;
        self.in_mem.copy_from_slice(&inb[n..n + OVERLAP]);

        if silence {
            let mut enc = RangeEncoder::new(2);
            enc.bit_logp(true, 15);
            for i in 0..NB_EBANDS {
                self.old_band_e[i] = -28.0;
            }
            self.energy_error = [0.0; NB_EBANDS];
            self.finish_state(start, end);
            self.last_frame_error = enc.error;
            return enc.done();
        }

        let mut enc = RangeEncoder::new(nbytes);
        let mut total_bits = nbytes as i32 * 8;
        let nb_available_bytes = nbytes as i32;
        let effective_bytes = nbytes as i32;
        let tell0 = 1i32; // `tell` of the C code: nothing coded yet
        enc.bit_logp(false, 15); // not silent

        // No pitch pre-filter
        if tell0 + 16 <= total_bits {
            enc.bit_logp(false, 1);
        }
        // Not a transient
        if lm > 0 && enc.tell() + 3 <= total_bits {
            enc.bit_logp(false, 3);
        }

        // MDCT
        let mut freq = vec![0.0f32; n];
        self.fwd.forward(&inb, &mut freq, 0, &WINDOW120, OVERLAP, 3 - lm, 1);
        let mut band_e = [0.0f32; NB_EBANDS];
        compute_band_energies(&freq, &mut band_e, eff_end, lm);
        let mut band_log_e = [0.0f32; NB_EBANDS];
        amp2log2(eff_end, end, &band_e, &mut band_log_e);
        let mut xn = vec![0.0f32; n];
        normalise_bands(&freq, &mut xn, &band_e, eff_end, m);

        // Dynamic allocation analysis
        let mut offsets = [0i32; NB_EBANDS];
        let mut spread_weight = [1i32; NB_EBANDS];
        let tot_boost_est = dynalloc_analysis(
            &band_log_e,
            end,
            lm,
            effective_bytes,
            self.tune.lsb_depth,
            self.tune.dynalloc,
            &mut offsets,
            &mut spread_weight,
        );
        let _ = tot_boost_est;

        self.dbg_band_log_e = band_log_e;
        // Bias the energy quantisation towards the previous error when the energy is stable
        for i in start..end {
            if absf(band_log_e[i] - self.old_band_e[i]) < 2.0 {
                band_log_e[i] -= 0.25 * self.energy_error[i];
            }
        }
        let mut error = [0.0f32; NB_EBANDS];
        self.dbg_intra = quant_coarse_energy(start, end, &band_log_e, &mut self.old_band_e, total_bits, &mut error, &mut enc, lm, nb_available_bytes, self.force_intra);
        self.force_intra = false;

        // tf_encode: all bands at the default resolution (tf_res = 0, tf_select = 0)
        {
            let mut budget = total_bits as u32;
            let mut tell = enc.tell() as u32;
            let mut logp: u32 = 4;
            let tf_select_rsv = lm > 0 && tell + logp + 1 <= budget;
            budget -= tf_select_rsv as u32;
            for _ in start..end {
                if tell + logp <= budget {
                    enc.bit_logp(false, logp);
                    tell = enc.tell() as u32;
                }
                logp = 5;
            }
            // (tf_select is only coded if the table row differs between 0 and 1: never for the
            // non-transient rows of the 48 kHz tf_select_table)
        }

        // Spreading
        if enc.tell() + 4 <= total_bits {
            if lm == 0 || nb_available_bytes < 10 || !self.tune.spread_analysis {
                self.spread_decision = SPREAD_NORMAL;
            } else {
                self.spread_decision = spreading_decision(&xn, &mut self.tonal_average, self.spread_decision, eff_end, m, &spread_weight);
            }
            enc.icdf(self.spread_decision as usize, &SPREAD_ICDF, 5);
        } else {
            self.spread_decision = SPREAD_NORMAL;
        }

        let mut cap = [0i32; NB_EBANDS];
        init_caps(lm, &mut cap);

        // Dynamic allocation
        let mut dynalloc_logp: u32 = 6;
        total_bits <<= BITRES;
        let mut total_boost = 0i32;
        let mut tell = enc.tell_frac() as i32;
        for i in start..end {
            let width = (CeltMode::ebands(i + 1) - CeltMode::ebands(i)) << lm;
            // quanta is 6 bits, but no more than 1 bit/sample and no less than 1/8 bit/sample
            let quanta = core::cmp::min(width << BITRES, core::cmp::max(6 << BITRES, width));
            let mut dynalloc_loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            let mut j = 0i32;
            while tell + ((dynalloc_loop_logp as i32) << BITRES) < total_bits - total_boost && boost < cap[i] {
                let flag = j < offsets[i];
                enc.bit_logp(flag, dynalloc_loop_logp);
                tell = enc.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_boost += quanta;
                dynalloc_loop_logp = 1;
                j += 1;
            }
            // Making dynalloc more likely
            if j > 0 {
                dynalloc_logp = core::cmp::max(2, dynalloc_logp - 1);
            }
            offsets[i] = boost;
        }

        // Allocation trim
        let mut alloc_trim = 5;
        if tell + (6 << BITRES) <= total_bits - total_boost {
            let equiv_rate = (nbytes as i32 * 8 * 50) * (1 << (3 - lm)) - 60 * ((400 >> lm) - 50);
            alloc_trim = if self.tune.trim_analysis { alloc_trim_analysis(&band_log_e, end, equiv_rate) } else { self.tune.fixed_trim };
            enc.icdf(alloc_trim as usize, &TRIM_ICDF, 7);
        }

        // Bit allocation
        let bits = ((nbytes as i32 * 8) << BITRES) - enc.tell_frac() as i32 - 1;
        let signal_bandwidth = end - 1;
        let mut pulses = [0i32; NB_EBANDS];
        let mut fine_quant = [0i32; NB_EBANDS];
        let mut fine_priority = [0i32; NB_EBANDS];
        let mut balance = 0i32;
        let coded_bands = clt_compute_allocation_enc(
            start,
            end,
            &offsets,
            &cap,
            alloc_trim,
            bits,
            &mut balance,
            &mut pulses,
            &mut fine_quant,
            &mut fine_priority,
            lm as i32,
            &mut enc,
            self.last_coded_bands,
            signal_bandwidth,
        );
        if self.last_coded_bands != 0 {
            self.last_coded_bands = core::cmp::min(self.last_coded_bands + 1, core::cmp::max(self.last_coded_bands - 1, coded_bands));
        } else {
            self.last_coded_bands = coded_bands;
        }

        quant_fine_energy(start, end, &mut self.old_band_e, &mut error, &fine_quant, &mut enc);
        self.energy_error = [0.0; NB_EBANDS];

        // Residual quantisation
        quant_all_bands(
            start,
            end,
            &mut xn,
            &pulses,
            self.spread_decision,
            nbytes as i32 * (8 << BITRES),
            balance,
            &mut enc,
            lm,
            coded_bands,
        );

        let bits_left = nbytes as i32 * 8 - enc.tell();
        quant_energy_finalise(start, end, &mut self.old_band_e, &mut error, &fine_quant, &fine_priority, bits_left, &mut enc);
        for i in start..end {
            self.energy_error[i] = maxf(-0.5, minf(0.5, error[i]));
        }
        self.finish_state(start, end);
        self.last_frame_error = enc.error;
        enc.done()
    }

    /// The per-frame history update after the bands were coded.
    fn finish_state(&mut self, start: usize, end: usize) {
        for i in 0..start {
            self.old_band_e[i] = 0.0;
        }
        for i in end..NB_EBANDS {
            self.old_band_e[i] = 0.0;
        }
    }
}

const SHORT: usize = 120;
