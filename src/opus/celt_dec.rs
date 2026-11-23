// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2010 Xiph.Org Foundation
// Copyright (c) 2008 Gregory Maxwell
// Written by Jean-Marc Valin and Gregory Maxwell
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

//! The CELT decoder (celt/celt_decoder.c, float build): one mono CELT frame of the 48 kHz mode,
//! with the postfilter, de-emphasis, and packet loss concealment (noise substitution and the
//! pitch-based periodic extrapolation).
//!
//! Output samples are `libopus int16 / 32768` before the final rounding, i.e. in about [-1, 1].

use alloc::vec;
use alloc::vec::Vec;

use super::celt_bands::{anti_collapse, celt_lcg_rand, denormalise_bands, quant_all_bands};
use super::celt_modes::{CeltMode, DECODE_BUFFER_SIZE, EFF_EBANDS, MAX_PERIOD, NB_EBANDS, OVERLAP, SHORT_MDCT_SIZE};
use super::celt_pitch::{
    celt_autocorr, celt_fir, celt_iir, celt_lpc, comb_filter_fir, comb_filter_inplace, pitch_downsample,
    pitch_search, CELT_LPC_ORDER, COMBFILTER_MINPERIOD,
};
use super::celt_quant::{unquant_coarse_energy, unquant_energy_finalise, unquant_fine_energy};
use super::celt_rate::{clt_compute_allocation, init_caps, BITRES};
use super::celt_vq::{renormalise_vector, SPREAD_NORMAL};
use super::mathops::{celt_sqrt, maxf, minf};
use super::rangedec::RangeDecoder;

const PLC_PITCH_LAG_MAX: usize = 720;
const PLC_PITCH_LAG_MIN: usize = 100;

const FRAME_NONE: i32 = 0;
const FRAME_NORMAL: i32 = 1;
const FRAME_PLC_NOISE: i32 = 2;
const FRAME_PLC_PERIODIC: i32 = 3;

const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
const TAPSET_ICDF: [u8; 3] = [2, 1, 0];

/// TF change table. Positive values mean better frequency resolution (longer effective window),
/// whereas negative values mean better time resolution (shorter effective window). The second
/// index is computed as: 4*isTransient + 2*tf_select + per_band_flag
const TF_SELECT_TABLE: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1], // 2.5 ms
    [0, -1, 0, -2, 1, 0, 1, -1],  // 5 ms
    [0, -2, 0, -3, 2, 0, 1, -1],  // 10 ms
    [0, -2, 0, -3, 3, 0, 1, -1],  // 20 ms
];

/// The pre-emphasis coefficient of the 48 kHz mode.
const PREEMPH0: f32 = 0.85000610f32;

pub struct CeltDecoder {
    mode: CeltMode,
    start: usize,
    end: usize,
    rng: u32,
    error: bool,
    last_pitch_index: i32,
    plc_decay: [f32; 2],
    loss_duration: i32,
    plc_duration: i32,
    last_frame_type: i32,
    skip_plc: bool,
    postfilter_period: i32,
    postfilter_period_old: i32,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    prefilter_and_fold: bool,
    preemph_mem_d: f32,
    /// The synthesis history (DECODE_BUFFER_SIZE) plus the MDCT overlap tail.
    decode_mem: Vec<f32>,
    old_band_e: [f32; 2 * NB_EBANDS],
    old_log_e: [f32; 2 * NB_EBANDS],
    old_log_e2: [f32; 2 * NB_EBANDS],
    background_log_e: [f32; 2 * NB_EBANDS],
    lpc: [f32; CELT_LPC_ORDER],
}

impl CeltDecoder {
    pub fn new() -> CeltDecoder {
        let mut d = CeltDecoder {
            mode: CeltMode::new(),
            start: 0,
            end: EFF_EBANDS,
            rng: 0,
            error: false,
            last_pitch_index: 0,
            plc_decay: [0.0; 2],
            loss_duration: 0,
            plc_duration: 0,
            last_frame_type: FRAME_NONE,
            skip_plc: true,
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            prefilter_and_fold: false,
            preemph_mem_d: 0.0,
            decode_mem: vec![0.0; DECODE_BUFFER_SIZE + OVERLAP],
            old_band_e: [0.0; 2 * NB_EBANDS],
            old_log_e: [-28.0; 2 * NB_EBANDS],
            old_log_e2: [-28.0; 2 * NB_EBANDS],
            background_log_e: [0.0; 2 * NB_EBANDS],
            lpc: [0.0; CELT_LPC_ORDER],
        };
        d.reset();
        d
    }

    /// `OPUS_RESET_STATE`.
    pub fn reset(&mut self) {
        self.rng = 0;
        self.error = false;
        self.last_pitch_index = 0;
        self.plc_decay = [0.0; 2];
        self.loss_duration = 0;
        self.plc_duration = 0;
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.prefilter_and_fold = false;
        self.preemph_mem_d = 0.0;
        for v in self.decode_mem.iter_mut() {
            *v = 0.0;
        }
        self.old_band_e = [0.0; 2 * NB_EBANDS];
        self.old_log_e = [-28.0; 2 * NB_EBANDS];
        self.old_log_e2 = [-28.0; 2 * NB_EBANDS];
        self.background_log_e = [0.0; 2 * NB_EBANDS];
        self.lpc = [0.0; CELT_LPC_ORDER];
        self.skip_plc = true;
        self.last_frame_type = FRAME_NONE;
    }

    /// The range coder's final state of the last frame (OPUS_GET_FINAL_RANGE), for the Opus-level
    /// conformance check.
    pub fn final_range(&self) -> u32 {
        self.rng
    }

    /// `deemphasis` (mono): the first-order IIR de-emphasis of the synthesis in
    /// `decode_mem[DB - n ..]`, scaled to [-1, 1].
    fn deemphasis(&mut self, n: usize, accum: bool, out: &mut [f32]) {
        let x = &self.decode_mem[DECODE_BUFFER_SIZE - n..DECODE_BUFFER_SIZE];
        let mut m = self.preemph_mem_d;
        let scale = 1.0f32 / 32768.0;
        let very_small = 1e-30f32;
        if accum {
            for j in 0..n {
                let tmp = x[j] + m + very_small;
                m = PREEMPH0 * tmp;
                out[j] = out[j] + scale * tmp;
            }
        } else {
            for j in 0..n {
                let tmp = x[j] + very_small + m;
                m = PREEMPH0 * tmp;
                out[j] = scale * tmp;
            }
        }
        self.preemph_mem_d = m;
    }

    /// `celt_synthesis` (mono): de-normalises `x`, runs the inverse MDCT(s) into the history
    /// buffer at `DB - n`.
    fn synthesis(&mut self, x: &[f32], start: usize, eff_end: usize, is_transient: bool, lm: usize, silence: bool) {
        let n = SHORT_MDCT_SIZE << lm;
        let m = 1usize << lm;
        let mut freq = vec![0.0f32; n];
        let (b, nb, shift) = if is_transient {
            (m, SHORT_MDCT_SIZE, 3usize)
        } else {
            (1usize, SHORT_MDCT_SIZE << lm, 3 - lm)
        };
        denormalise_bands(x, &mut freq, &self.old_band_e, start, eff_end, m, silence);
        let base = DECODE_BUFFER_SIZE - n;
        for blk in 0..b {
            self.mode.mdct.backward(
                &freq,
                blk,
                &mut self.decode_mem,
                base + nb * blk,
                &self.mode.window,
                OVERLAP,
                shift,
                b,
            );
        }
    }

    /// `prefilter_and_fold`: applies the pre-filter to the MDCT overlap for the next frame
    /// (because the post-filter is re-applied after the overlap) and simulates the TDAC on it.
    fn prefilter_and_fold(&mut self, n: usize) {
        let mut etmp = vec![0.0f32; OVERLAP];
        comb_filter_fir(
            &mut etmp,
            &self.decode_mem,
            DECODE_BUFFER_SIZE - n,
            self.postfilter_period_old,
            self.postfilter_period,
            OVERLAP,
            -self.postfilter_gain_old,
            -self.postfilter_gain,
            self.postfilter_tapset_old,
            self.postfilter_tapset,
        );
        // Simulate TDAC on the concealed audio so that it blends with the MDCT of the next frame.
        let w = &self.mode.window;
        for i in 0..OVERLAP / 2 {
            self.decode_mem[DECODE_BUFFER_SIZE - n + i] = w[i] * etmp[OVERLAP - 1 - i] + w[OVERLAP - i - 1] * etmp[i];
        }
    }

    /// The postfilter on the freshly synthesised frame at `DB - n` (both halves).
    fn postfilter_frame(&mut self, n: usize, lm: usize, new_period: i32, new_gain: f32, new_tapset: usize) {
        let base = DECODE_BUFFER_SIZE - n;
        self.postfilter_period = core::cmp::max(self.postfilter_period, COMBFILTER_MINPERIOD);
        self.postfilter_period_old = core::cmp::max(self.postfilter_period_old, COMBFILTER_MINPERIOD);
        comb_filter_inplace(
            &mut self.decode_mem,
            base,
            self.postfilter_period_old,
            self.postfilter_period,
            SHORT_MDCT_SIZE,
            self.postfilter_gain_old,
            self.postfilter_gain,
            self.postfilter_tapset_old,
            self.postfilter_tapset,
            &self.mode.window,
            OVERLAP,
        );
        if lm != 0 {
            comb_filter_inplace(
                &mut self.decode_mem,
                base + SHORT_MDCT_SIZE,
                self.postfilter_period,
                new_period,
                n - SHORT_MDCT_SIZE,
                self.postfilter_gain,
                new_gain,
                self.postfilter_tapset,
                new_tapset,
                &self.mode.window,
                OVERLAP,
            );
        }
    }

    /// Packet loss concealment for one frame (`frame_size` = 120, 240, 480 or 960 samples).
    pub fn decode_lost(&mut self, frame_size: usize, out: &mut [f32]) {
        self.decode_lost_impl(frame_size, false, out);
    }

    fn decode_lost_impl(&mut self, frame_size: usize, accumulate: bool, out: &mut [f32]) {
        let lm = match frame_size {
            120 => 0usize,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => return,
        };
        if out.len() < frame_size {
            return;
        }
        let n = frame_size;
        self.celt_decode_lost(n, lm);
        self.deemphasis(n, accumulate, out);
    }

    /// `celt_decode_lost`.
    fn celt_decode_lost(&mut self, n: usize, lm: usize) {
        let db = DECODE_BUFFER_SIZE;
        let overlap = OVERLAP;
        let start = self.start;
        let loss_duration = self.loss_duration;
        let mut curr_frame_type = FRAME_PLC_PERIODIC;
        if self.plc_duration >= 40 || start != 0 || self.skip_plc {
            curr_frame_type = FRAME_PLC_NOISE;
        }

        if curr_frame_type == FRAME_PLC_NOISE {
            // Noise-based PLC/CNG
            let end = self.end;
            let eff_end = core::cmp::max(start, core::cmp::min(end, EFF_EBANDS));
            let mut x = vec![0.0f32; n];
            self.decode_mem.copy_within(n..db + overlap, 0);

            if self.prefilter_and_fold {
                self.prefilter_and_fold(n);
            }

            // Energy decay
            let mut decay = ((1u32 << lm) as f32) * 0.0625f32;
            if self.last_frame_type != FRAME_PLC_NOISE {
                decay += maxf(1.0, (core::cmp::min(160, loss_duration) as f32) * 0.0625f32);
            }
            for i in start..end {
                self.old_band_e[i] = maxf(self.background_log_e[i], self.old_band_e[i] - decay);
            }
            let mut seed = self.rng;
            for i in start..eff_end {
                let boffs = (CeltMode::ebands(i) as usize) << lm;
                let blen = ((CeltMode::ebands(i + 1) - CeltMode::ebands(i)) as usize) << lm;
                for j in 0..blen {
                    seed = celt_lcg_rand(seed);
                    x[boffs + j] = ((seed as i32) >> 20) as f32;
                }
                renormalise_vector(&mut x[boffs..], blen, 1.0);
            }
            self.rng = seed;

            self.synthesis(&x, start, eff_end, false, lm, false);

            // Run the postfilter with the last parameters.
            let (pp, pg, pt) = (self.postfilter_period, self.postfilter_gain, self.postfilter_tapset);
            self.postfilter_frame(n, lm, pp, pg, pt);
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;

            self.prefilter_and_fold = false;
            // Skip regular PLC until we get two consecutive packets.
            self.skip_plc = true;
        } else {
            // Pitch-based PLC
            let mut fade = 1.0f32;
            let mut pitch_index: i32;
            if self.last_frame_type != FRAME_PLC_PERIODIC {
                // celt_plc_pitch_search
                let mut lp = vec![0.0f32; db >> 1];
                pitch_downsample(&self.decode_mem, &mut lp, db >> 1, 2);
                let p = pitch_search(
                    &lp[PLC_PITCH_LAG_MAX >> 1..],
                    &lp,
                    db - PLC_PITCH_LAG_MAX,
                    PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
                );
                pitch_index = PLC_PITCH_LAG_MAX as i32 - p;
                self.last_pitch_index = pitch_index;
            } else {
                pitch_index = self.last_pitch_index;
                if self.plc_duration * (SHORT_MDCT_SIZE as i32) >= pitch_index {
                    fade = 0.8f32;
                }
            }
            if pitch_index < 1 {
                pitch_index = 1;
            }
            if pitch_index as usize > PLC_PITCH_LAG_MAX {
                pitch_index = PLC_PITCH_LAG_MAX as i32;
            }
            let pitch0 = pitch_index as usize;

            // We want the excitation for 2 pitch periods in order to look for a decaying signal,
            // and at least max_period to search for pitch multiples.
            let exc_length = core::cmp::max(MAX_PERIOD, 2 * pitch0);
            let mut sig = vec![0.0f32; exc_length + CELT_LPC_ORDER];
            let mut exc = vec![0.0f32; exc_length];
            let mut old_overlap = vec![0.0f32; overlap >> 1];
            let mut etmp = vec![0.0f32; overlap];
            let window = &self.mode.window;

            let mut pitch_index = pitch0;
            for i in 0..exc_length + CELT_LPC_ORDER {
                sig[i] = self.decode_mem[db - exc_length - CELT_LPC_ORDER + i];
            }

            if self.last_frame_type != FRAME_PLC_PERIODIC {
                let mut ac = [0.0f32; CELT_LPC_ORDER + 1];
                // Compute LPC coefficients for the last max_period samples before the first loss
                // so we can work in the excitation-filter domain.
                celt_autocorr(
                    &sig[CELT_LPC_ORDER + exc_length - MAX_PERIOD..],
                    &mut ac,
                    Some(window),
                    overlap,
                    CELT_LPC_ORDER,
                    MAX_PERIOD,
                );
                // Add a noise floor of -40 dB.
                ac[0] *= 1.0001f32;
                // Use lag windowing to stabilize the Levinson-Durbin recursion.
                for i in 1..=CELT_LPC_ORDER {
                    let fi = i as f32;
                    ac[i] -= ac[i] * (0.008f32 * 0.008f32) * fi * fi;
                }
                celt_lpc(&mut self.lpc, &ac, CELT_LPC_ORDER);
            }
            // Initialize the LPC history with the samples just before the start of the region for
            // which we're computing the excitation.
            celt_fir(&sig, CELT_LPC_ORDER, &self.lpc, &mut exc, exc_length, CELT_LPC_ORDER);

            // Check if the waveform is decaying, and if so how fast. We do this to avoid adding
            // energy when concealing in a segment with decaying energy.
            let decay: f32;
            if self.last_frame_type != FRAME_PLC_PERIODIC {
                let mut e1 = 1.0f32;
                let mut e2 = 1.0f32;
                for i in 0..pitch_index {
                    let e = exc[exc_length - pitch_index + i];
                    e1 += e * e;
                    let e = exc[exc_length - 2 * pitch_index + i];
                    e2 += e * e;
                }
                e1 = minf(e1, e2);
                decay = celt_sqrt(e1 / e2);
                self.plc_decay[0] = decay;
            } else {
                decay = self.plc_decay[0];
            }
            // Use a large integer multiple of the pitch period to avoid repeating a short
            // sequence too many times, stopping before any >6 dB change.
            {
                let kk = exc_length / pitch_index;
                let mut e0 = 1.0f32;
                for i in 0..pitch_index {
                    let e = exc[exc_length - pitch_index + i];
                    e0 += e * e;
                }
                let mut k = 2;
                while k <= kk {
                    let mut ek = 1.0f32;
                    for i in 0..pitch_index {
                        let e = exc[exc_length - k * pitch_index + i];
                        ek += e * e;
                    }
                    if ek < 0.25f32 * e0 || e0 < 0.25f32 * ek {
                        break;
                    }
                    k += 1;
                }
                pitch_index *= k - 1;
            }

            // Save the folded right-overlap tail from the previous MDCT frame before moving the
            // decoder memory one frame to the left.
            if self.last_frame_type == FRAME_NORMAL {
                old_overlap.copy_from_slice(&self.decode_mem[db..db + (overlap >> 1)]);
            }
            self.decode_mem.copy_within(n..db, 0);

            // Extrapolate from the end of the excitation with a period of "pitch_index", scaling
            // down each period by an additional factor of "decay".
            let extrapolation_offset = exc_length - pitch_index;
            // We need to extrapolate enough samples to cover a complete MDCT window (including
            // overlap/2 samples on both sides).
            let extrapolation_len = n + overlap;
            // We also apply fading if this is not the first loss.
            let mut attenuation = fade * decay;
            let mut s1 = 0.0f32;
            {
                let mut j = 0usize;
                for i in 0..extrapolation_len {
                    if j >= pitch_index {
                        j -= pitch_index;
                        attenuation = attenuation * decay;
                    }
                    self.decode_mem[db - n + i] = attenuation * exc[extrapolation_offset + j];
                    // Compute the expected energy of the attenuated signal whose excitation we're
                    // copying.
                    let tmp = attenuation * sig[CELT_LPC_ORDER + extrapolation_offset + j];
                    s1 += tmp * tmp;
                    j += 1;
                }
            }
            {
                let mut lpc_mem = [0.0f32; CELT_LPC_ORDER];
                // Copy the last decoded samples (prior to the overlap region) to synthesis filter
                // memory so we can have a continuous signal.
                for i in 0..CELT_LPC_ORDER {
                    lpc_mem[i] = self.decode_mem[db - n - 1 - i];
                }
                // Apply the synthesis filter to convert the excitation back into the signal domain.
                celt_iir(&mut self.decode_mem, db - n, &self.lpc, extrapolation_len, CELT_LPC_ORDER, &mut lpc_mem);
            }

            // Check if the synthesis energy is higher than expected, which can happen with the
            // signal changes during our window. If so, attenuate.
            {
                let mut s2 = 0.0f32;
                for i in 0..extrapolation_len {
                    let tmp = self.decode_mem[db - n + i];
                    s2 += tmp * tmp;
                }
                // This checks for an "explosion" in the synthesis. The test is written this way
                // to catch NaNs in the output of the IIR filter at the same time.
                if !(s1 > 0.2f32 * s2) {
                    for i in 0..extrapolation_len {
                        self.decode_mem[db - n + i] = 0.0;
                    }
                } else if s1 < s2 {
                    let ratio = celt_sqrt((s1 + 1.0) / (s2 + 1.0));
                    for i in 0..overlap {
                        let tmp_g = 1.0f32 - window[i] * (1.0f32 - ratio);
                        self.decode_mem[db - n + i] = tmp_g * self.decode_mem[db - n + i];
                    }
                    for i in overlap..extrapolation_len {
                        self.decode_mem[db - n + i] = ratio * self.decode_mem[db - n + i];
                    }
                }
            }

            if self.last_frame_type == FRAME_NORMAL {
                comb_filter_fir(
                    &mut etmp,
                    &self.decode_mem,
                    db - n,
                    self.postfilter_period_old,
                    self.postfilter_period_old,
                    overlap,
                    -self.postfilter_gain_old,
                    -self.postfilter_gain_old,
                    self.postfilter_tapset_old,
                    self.postfilter_tapset_old,
                );
                for i in 0..overlap / 2 {
                    let w1 = window[i];
                    let w2 = window[overlap - 1 - i];
                    let x2 = old_overlap[i];
                    let x1 = w2 * etmp[overlap - 1 - i] - w1 * etmp[i];
                    self.decode_mem[db - n + i] = w2 * x2 - w1 * x1;
                    self.decode_mem[db - n + overlap - 1 - i] = w1 * x2 + w2 * x1;
                }
                comb_filter_inplace(
                    &mut self.decode_mem,
                    db - n,
                    self.postfilter_period_old,
                    self.postfilter_period_old,
                    overlap,
                    self.postfilter_gain_old,
                    self.postfilter_gain_old,
                    self.postfilter_tapset_old,
                    self.postfilter_tapset_old,
                    &[],
                    0,
                );
            }
            self.prefilter_and_fold = true;
        }

        // Saturate to something large to avoid wrap-around.
        self.loss_duration = core::cmp::min(10000, loss_duration + (1 << lm));
        self.plc_duration = core::cmp::min(10000, self.plc_duration + (1 << lm));
        self.last_frame_type = curr_frame_type;
    }

    /// `tf_decode`.
    fn tf_decode(start: usize, end: usize, is_transient: bool, tf_res: &mut [i32; NB_EBANDS], lm: usize, dec: &mut RangeDecoder) {
        let mut budget = dec.storage() as i32 * 8;
        let mut tell = dec.tell();
        let mut logp: u32 = if is_transient { 2 } else { 4 };
        let tf_select_rsv = lm > 0 && tell + logp as i32 + 1 <= budget;
        budget -= tf_select_rsv as i32;
        let mut tf_changed = 0i32;
        let mut curr = 0i32;
        let it = is_transient as usize;
        for i in start..end {
            if tell + logp as i32 <= budget {
                curr ^= dec.bit_logp(logp);
                tell = dec.tell();
                tf_changed |= curr;
            }
            tf_res[i] = curr;
            logp = if is_transient { 4 } else { 5 };
        }
        let mut tf_select = 0usize;
        if tf_select_rsv
            && TF_SELECT_TABLE[lm][4 * it + tf_changed as usize] != TF_SELECT_TABLE[lm][4 * it + 2 + tf_changed as usize]
        {
            tf_select = dec.bit_logp(1) as usize;
        }
        for i in start..end {
            tf_res[i] = TF_SELECT_TABLE[lm][4 * it + 2 * tf_select + tf_res[i] as usize] as i32;
        }
    }

    /// One CELT frame from `rc`. `frame_size` is samples per channel at 48 kHz: 120, 240, 480 or
    /// 960. `start_band` is 0 for CELT-only packets and 17 for the CELT part of a hybrid packet;
    /// `end_band` is 13 (narrowband), 17 (wideband), 19 (super-wideband) or 21 (fullband). Output
    /// f32 in about [-1, 1], `out.len() >= frame_size`. `accumulate` adds into `out` instead of
    /// overwriting (hybrid adds CELT over the SILK output). Returns false if the frame is corrupt
    /// (invalid arguments, or the frame used more bits than it has); the output is still filled
    /// with whatever was decoded, as libopus does.
    pub fn decode(
        &mut self,
        rc: &mut RangeDecoder,
        frame_size: usize,
        start_band: usize,
        end_band: usize,
        accumulate: bool,
        out: &mut [f32],
    ) -> bool {
        let lm = match frame_size {
            120 => 0usize,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => return false,
        };
        if start_band >= end_band || end_band > NB_EBANDS || out.len() < frame_size {
            return false;
        }
        let n = frame_size;
        let m = 1usize << lm;
        self.start = start_band;
        self.end = end_band;
        let start = start_band;
        let end = end_band;
        let eff_end = core::cmp::min(end, EFF_EBANDS);
        let len = rc.storage() as i32;
        if len > 1275 {
            return false;
        }

        if len <= 1 {
            self.decode_lost_impl(n, accumulate, out);
            return true;
        }

        // Check if there are at least two packets received consecutively before turning on the
        // pitch-based PLC
        if self.loss_duration == 0 {
            self.skip_plc = false;
        }

        // (the stream is mono: C == 1)
        for i in 0..NB_EBANDS {
            self.old_band_e[i] = maxf(self.old_band_e[i], self.old_band_e[NB_EBANDS + i]);
        }

        let mut total_bits = len * 8;
        let mut tell = rc.tell();

        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            rc.bit_logp(15) != 0
        } else {
            false
        };
        if silence {
            // Pretend we've read all the remaining bits
            tell = len * 8;
            rc.mark_all_read(tell);
        }

        let mut postfilter_gain = 0.0f32;
        let mut postfilter_pitch = 0i32;
        let mut postfilter_tapset = 0usize;
        if start == 0 && tell + 16 <= total_bits {
            if rc.bit_logp(1) != 0 {
                let octave = rc.uint(6);
                postfilter_pitch = (16i32 << octave) + rc.bits(4 + octave) as i32 - 1;
                let qg = rc.bits(3) as i32;
                if rc.tell() + 2 <= total_bits {
                    postfilter_tapset = rc.icdf(&TAPSET_ICDF, 2) as usize;
                }
                postfilter_gain = 0.09375f32 * ((qg + 1) as f32);
            }
            tell = rc.tell();
        }

        let is_transient;
        if lm > 0 && tell + 3 <= total_bits {
            is_transient = rc.bit_logp(3) != 0;
            tell = rc.tell();
        } else {
            is_transient = false;
        }
        let short_blocks = is_transient;

        // Decode the global flags (first symbols in the stream)
        let intra_ener = if tell + 3 <= total_bits { rc.bit_logp(3) != 0 } else { false };
        // If recovering from packet loss, make sure we make the energy prediction safe to reduce
        // the risk of getting loud artifacts.
        if !intra_ener && self.loss_duration != 0 {
            for c in 0..2 {
                let mut safety = 0.0f32;
                let missing = core::cmp::min(10, self.loss_duration >> lm);
                if lm == 0 {
                    safety = 1.5;
                } else if lm == 1 {
                    safety = 0.5;
                }
                for i in start..end {
                    let o = c * NB_EBANDS + i;
                    if self.old_band_e[o] < maxf(self.old_log_e[o], self.old_log_e2[o]) {
                        // If energy is going down already, continue the trend.
                        let mut e0 = self.old_band_e[o];
                        let e1 = self.old_log_e[o];
                        let e2 = self.old_log_e2[o];
                        let mut slope = maxf(e1 - e0, 0.5f32 * (e2 - e0));
                        slope = minf(slope, 2.0);
                        e0 -= maxf(0.0, ((1 + missing) as f32) * slope);
                        self.old_band_e[o] = maxf(-20.0, e0);
                    } else {
                        // Otherwise take the min of the last frames.
                        self.old_band_e[o] = minf(minf(self.old_band_e[o], self.old_log_e[o]), self.old_log_e2[o]);
                    }
                    // Shorter frames have more natural fluctuations -- play it safe.
                    self.old_band_e[o] -= safety;
                }
            }
        }
        // Get band energies
        unquant_coarse_energy(start, end, &mut self.old_band_e, intra_ener, rc, lm);

        let mut tf_res = [0i32; NB_EBANDS];
        Self::tf_decode(start, end, is_transient, &mut tf_res, lm, rc);

        tell = rc.tell();
        let mut spread_decision = SPREAD_NORMAL;
        if tell + 4 <= total_bits {
            spread_decision = rc.icdf(&SPREAD_ICDF, 5);
        }

        let mut cap = [0i32; NB_EBANDS];
        init_caps(lm, &mut cap);

        let mut offsets = [0i32; NB_EBANDS];

        let mut dynalloc_logp: u32 = 6;
        total_bits <<= BITRES;
        let mut tell_f = rc.tell_frac() as i32;
        for i in start..end {
            let width = (CeltMode::ebands(i + 1) - CeltMode::ebands(i)) << lm;
            // quanta is 6 bits, but no more than 1 bit/sample and no less than 1/8 bit/sample
            let quanta = core::cmp::min(width << BITRES, core::cmp::max(6 << BITRES, width));
            let mut dynalloc_loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            while tell_f + ((dynalloc_loop_logp as i32) << BITRES) < total_bits && boost < cap[i] {
                let flag = rc.bit_logp(dynalloc_loop_logp);
                tell_f = rc.tell_frac() as i32;
                if flag == 0 {
                    break;
                }
                boost += quanta;
                total_bits -= quanta;
                dynalloc_loop_logp = 1;
            }
            offsets[i] = boost;
            // Making dynalloc more likely
            if boost > 0 {
                dynalloc_logp = core::cmp::max(2, dynalloc_logp - 1);
            }
        }

        let alloc_trim = if tell_f + (6 << BITRES) <= total_bits { rc.icdf(&TRIM_ICDF, 7) } else { 5 };

        let mut bits = ((len * 8) << BITRES) - rc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) { 1 << BITRES } else { 0 };
        bits -= anti_collapse_rsv;

        let mut pulses = [0i32; NB_EBANDS];
        let mut fine_quant = [0i32; NB_EBANDS];
        let mut fine_priority = [0i32; NB_EBANDS];
        let mut balance = 0i32;
        let coded_bands = clt_compute_allocation(
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
            rc,
        );

        unquant_fine_energy(start, end, &mut self.old_band_e, None, &fine_quant, rc);

        // Shift the synthesis history
        let db = DECODE_BUFFER_SIZE;
        self.decode_mem.copy_within(n..db + OVERLAP, 0);

        // Decode fixed codebook
        let mut x = vec![0.0f32; n];
        let mut collapse_masks = [0u8; NB_EBANDS];
        quant_all_bands(
            start,
            end,
            &mut x,
            &mut collapse_masks,
            &pulses,
            short_blocks,
            spread_decision,
            &tf_res,
            len * (8 << BITRES) - anti_collapse_rsv,
            balance,
            rc,
            lm,
            coded_bands,
            &mut self.rng,
        );

        let mut anti_collapse_on = false;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = rc.bits(1) != 0;
        }
        unquant_energy_finalise(
            start,
            end,
            Some(&mut self.old_band_e),
            &fine_quant,
            &fine_priority,
            len * 8 - rc.tell(),
            rc,
        );
        if anti_collapse_on {
            anti_collapse(
                &mut x,
                &collapse_masks,
                lm,
                start,
                end,
                &self.old_band_e,
                &self.old_log_e,
                &self.old_log_e2,
                &pulses,
                self.rng,
            );
        }

        if silence {
            for i in 0..NB_EBANDS {
                self.old_band_e[i] = -28.0;
            }
        }
        if self.prefilter_and_fold {
            self.prefilter_and_fold(n);
        }
        self.synthesis(&x, start, eff_end, is_transient, lm, silence);

        self.postfilter_frame(n, lm, postfilter_pitch, postfilter_gain, postfilter_tapset);
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        // (stream is mono)
        for i in 0..NB_EBANDS {
            self.old_band_e[NB_EBANDS + i] = self.old_band_e[i];
        }

        if !is_transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
        } else {
            for i in 0..2 * NB_EBANDS {
                self.old_log_e[i] = minf(self.old_log_e[i], self.old_band_e[i]);
            }
        }
        // In normal circumstances, we only allow the noise floor to increase by up to 2.4 dB/second,
        // but when we're in DTX we give the weight of all missing packets to the update packet.
        let max_background_increase = (core::cmp::min(160, self.loss_duration + m as i32) as f32) * 0.001f32;
        for i in 0..2 * NB_EBANDS {
            self.background_log_e[i] = minf(self.background_log_e[i] + max_background_increase, self.old_band_e[i]);
        }
        // In case start or end were to change
        for c in 0..2 {
            for i in 0..start {
                self.old_band_e[c * NB_EBANDS + i] = 0.0;
                self.old_log_e[c * NB_EBANDS + i] = -28.0;
                self.old_log_e2[c * NB_EBANDS + i] = -28.0;
            }
            for i in end..NB_EBANDS {
                self.old_band_e[c * NB_EBANDS + i] = 0.0;
                self.old_log_e[c * NB_EBANDS + i] = -28.0;
                self.old_log_e2[c * NB_EBANDS + i] = -28.0;
            }
        }
        self.rng = rc.range();

        self.deemphasis(n, accumulate, out);
        self.loss_duration = 0;
        self.plc_duration = 0;
        self.last_frame_type = FRAME_NORMAL;
        self.prefilter_and_fold = false;
        if rc.tell() > 8 * len {
            return false;
        }
        if rc.error {
            self.error = true;
        }
        true
    }
}
