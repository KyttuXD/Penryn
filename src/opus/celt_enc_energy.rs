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

//! CELT band energy encoding (celt/quant_bands.c encoder side, celt/laplace.c `ec_laplace_encode`,
//! `amp2Log2`, `compute_band_energies`, `normalise_bands`; float build, mono).

use super::celt_modes::{CeltMode, NB_EBANDS};
use super::celt_rate::MAX_FINE_BITS;
use super::celt_tables::{E_MEANS, E_PROB_MODEL};
use super::celt_enc_math::{celt_log2, round_half_up, sqrt32};
use super::mathops::{floor_f64, maxf, minf};
use super::rangeenc::RangeEncoder;

/// prediction coefficients: 0.9, 0.8, 0.65, 0.5
const PRED_COEF: [f32; 4] = [29440.0 / 32768.0, 26112.0 / 32768.0, 21248.0 / 32768.0, 16384.0 / 32768.0];
const BETA_COEF: [f32; 4] = [30147.0 / 32768.0, 22282.0 / 32768.0, 12124.0 / 32768.0, 6554.0 / 32768.0];
const BETA_INTRA: f32 = 4915.0 / 32768.0;
const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

fn ec_laplace_get_freq1(fs0: u32, decay: i32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    ((ft as i64 * (16384 - decay) as i64) >> 15) as u32
}

/// `ec_laplace_encode`: codes `*value` (which is clamped to what the model can represent).
pub fn ec_laplace_encode(enc: &mut RangeEncoder, value: &mut i32, fs: u32, decay: i32) {
    let mut fs = fs;
    let mut fl: u32 = 0;
    let mut val = *value;
    if val != 0 {
        let s: i32 = -((val < 0) as i32);
        val = (val + s) ^ s;
        fl = fs;
        fs = ec_laplace_get_freq1(fs, decay);
        // Search the decaying part of the PDF.
        let mut i = 1;
        while fs > 0 && i < val {
            fs *= 2;
            fl += fs + 2 * LAPLACE_MINP;
            fs = ((fs as i64 * decay as i64) >> 15) as u32;
            i += 1;
        }
        // Everything beyond that has probability LAPLACE_MINP.
        if fs == 0 {
            let mut ndi_max = ((32768 - fl + LAPLACE_MINP - 1) >> 0) as i32; // LAPLACE_LOG_MINP = 0
            ndi_max = (ndi_max - s) >> 1;
            let di = core::cmp::min(val - i, ndi_max - 1);
            fl += ((2 * di + 1 + s) as u32) * LAPLACE_MINP;
            fs = core::cmp::min(LAPLACE_MINP, 32768 - fl);
            *value = (i + di + s) ^ s;
        } else {
            fs += LAPLACE_MINP;
            fl += fs & !(s as u32);
        }
    }
    enc.encode_bin(fl, fl + fs, 15);
}

/// `compute_band_energies` (mono): the amplitude (sqrt energy) of each band.
pub fn compute_band_energies(freq: &[f32], band_e: &mut [f32; NB_EBANDS], end: usize, lm: usize) {
    for i in 0..end {
        let a = (CeltMode::ebands(i) as usize) << lm;
        let b = (CeltMode::ebands(i + 1) as usize) << lm;
        let mut sum = 1e-27f32;
        for j in a..b {
            sum += freq[j] * freq[j];
        }
        band_e[i] = sqrt32(sum);
    }
}

/// `amp2Log2` (mono).
pub fn amp2log2(eff_end: usize, end: usize, band_e: &[f32; NB_EBANDS], band_log_e: &mut [f32; NB_EBANDS]) {
    for i in 0..eff_end {
        band_log_e[i] = celt_log2(band_e[i]) - E_MEANS[i];
    }
    for i in eff_end..end {
        band_log_e[i] = -14.0;
    }
}

/// `normalise_bands` (mono): scales each band to unit norm.
pub fn normalise_bands(freq: &[f32], x: &mut [f32], band_e: &[f32; NB_EBANDS], end: usize, m: usize) {
    for i in 0..end {
        let g = 1.0f32 / (1e-27f32 + band_e[i]);
        let a = m * CeltMode::ebands(i) as usize;
        let b = m * CeltMode::ebands(i + 1) as usize;
        for j in a..b {
            x[j] = freq[j] * g;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn quant_coarse_energy_impl(
    start: usize,
    end: usize,
    e_bands: &[f32; NB_EBANDS],
    old_e_bands: &mut [f32; NB_EBANDS],
    budget: i32,
    prob_model: &[u8; 42],
    error: &mut [f32; NB_EBANDS],
    enc: &mut RangeEncoder,
    lm: usize,
    intra: bool,
    max_decay: f32,
) -> i32 {
    let mut badness = 0i32;
    let mut prev = 0.0f32;
    let tell0 = enc.tell();
    if tell0 + 3 <= budget {
        enc.bit_logp(intra, 3);
    }
    let (coef, beta) = if intra { (0.0f32, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
    // Encode at a fixed coarse resolution
    for i in start..end {
        let x = e_bands[i];
        let old_e = maxf(-9.0, old_e_bands[i]);
        let f = x - coef * old_e - prev;
        // Rounding to nearest integer here is really important!
        let mut qi = round_half_up(f);
        let decay_bound = maxf(-28.0, old_e_bands[i]) - max_decay;
        // Prevent the energy from going down too quickly (e.g. for bands that have just one bin)
        if qi < 0 && x < decay_bound {
            qi += (decay_bound - x) as i32;
            if qi > 0 {
                qi = 0;
            }
        }
        let qi0 = qi;
        // If we don't have enough bits to encode all the energy, just assume something safe.
        let tell = enc.tell();
        let bits_left = budget - tell - 3 * (end - i) as i32;
        if i != start && bits_left < 30 {
            if bits_left < 24 {
                qi = core::cmp::min(1, qi);
            }
            if bits_left < 16 {
                qi = core::cmp::max(-1, qi);
            }
        }
        if budget - tell >= 15 {
            let pi = 2 * core::cmp::min(i, 20);
            ec_laplace_encode(enc, &mut qi, (prob_model[pi] as u32) << 7, (prob_model[pi + 1] as i32) << 6);
        } else if budget - tell >= 2 {
            qi = core::cmp::max(-1, core::cmp::min(qi, 1));
            enc.icdf(((2 * qi) ^ -((qi < 0) as i32)) as usize, &SMALL_ENERGY_ICDF, 2);
        } else if budget - tell >= 1 {
            qi = core::cmp::min(0, qi);
            enc.bit_logp(qi != 0, 1);
        } else {
            qi = -1;
        }
        error[i] = f - qi as f32;
        badness += (qi0 - qi).abs();
        let q = qi as f32;
        let tmp = coef * old_e + prev + q;
        old_e_bands[i] = tmp;
        prev = prev + q - beta * q;
    }
    badness
}

/// `quant_coarse_energy` with the two-pass intra/inter decision (no loss-rate bias, mono).
#[allow(clippy::too_many_arguments)]
pub fn quant_coarse_energy(
    start: usize,
    end: usize,
    e_bands: &[f32; NB_EBANDS],
    old_e_bands: &mut [f32; NB_EBANDS],
    budget: i32,
    error: &mut [f32; NB_EBANDS],
    enc: &mut RangeEncoder,
    lm: usize,
    nb_available_bytes: i32,
    force_intra: bool,
) -> bool {
    let mut two_pass = true;
    let tell = enc.tell();
    if tell + 3 > budget {
        two_pass = false;
    }
    let mut max_decay = 16.0f32;
    if end - start > 10 {
        max_decay = minf(max_decay, 0.125 * nb_available_bytes as f32);
    }
    if force_intra && two_pass {
        // the frame must not depend on the previous energies (first frame, or after a reset)
        let mut err = [0.0f32; NB_EBANDS];
        let mut old = *old_e_bands;
        let _ = quant_coarse_energy_impl(start, end, e_bands, &mut old, budget, &E_PROB_MODEL[lm][1], &mut err, enc, lm, true, max_decay);
        *old_e_bands = old;
        *error = err;
        return true;
    }
    if !two_pass {
        // inter-frame prediction only (intra is only chosen by the comparison)
        let _ = quant_coarse_energy_impl(start, end, e_bands, old_e_bands, budget, &E_PROB_MODEL[lm][0], error, enc, lm, false, max_decay);
        return false;
    }
    let start_state = enc.clone();
    let mut old_intra = *old_e_bands;
    let mut error_intra = [0.0f32; NB_EBANDS];
    let badness1 = quant_coarse_energy_impl(start, end, e_bands, &mut old_intra, budget, &E_PROB_MODEL[lm][1], &mut error_intra, enc, lm, true, max_decay);
    let tell_intra = enc.tell_frac() as i32;
    let intra_state = core::mem::replace(enc, start_state);
    let badness2 = quant_coarse_energy_impl(start, end, e_bands, old_e_bands, budget, &E_PROB_MODEL[lm][0], error, enc, lm, false, max_decay);
    if badness1 < badness2 || (badness1 == badness2 && (enc.tell_frac() as i32) > tell_intra) {
        *enc = intra_state;
        *old_e_bands = old_intra;
        *error = error_intra;
        return true;
    }
    false
}

/// `quant_fine_energy` (mono, no previous pass).
pub fn quant_fine_energy(
    start: usize,
    end: usize,
    old_e_bands: &mut [f32; NB_EBANDS],
    error: &mut [f32; NB_EBANDS],
    extra_quant: &[i32; NB_EBANDS],
    enc: &mut RangeEncoder,
) {
    for i in start..end {
        let eq = extra_quant[i];
        if eq <= 0 {
            continue;
        }
        let extra = (1i32 << eq) as f32;
        if enc.tell() + eq > enc.storage() as i32 * 8 {
            continue;
        }
        let mut q2 = floor_f64(((error[i] + 0.5) * extra) as f64) as i32;
        if q2 > (1 << eq) - 1 {
            q2 = (1 << eq) - 1;
        }
        if q2 < 0 {
            q2 = 0;
        }
        enc.bits(q2 as u32, eq as u32);
        let offset = ((q2 as f32) + 0.5) * ((1i32 << (14 - eq)) as f32) * (1.0 / 16384.0) - 0.5;
        old_e_bands[i] += offset;
        error[i] -= offset;
    }
}

/// `quant_energy_finalise` (mono): spends the leftover bits on one more fine bit.
pub fn quant_energy_finalise(
    start: usize,
    end: usize,
    old_e_bands: &mut [f32; NB_EBANDS],
    error: &mut [f32; NB_EBANDS],
    fine_quant: &[i32; NB_EBANDS],
    fine_priority: &[i32; NB_EBANDS],
    bits_left: i32,
    enc: &mut RangeEncoder,
) {
    let mut bits_left = bits_left;
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= 1 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            let q2: u32 = if error[i] < 0.0 { 0 } else { 1 };
            enc.bits(q2, 1);
            let offset = ((q2 as f32) - 0.5) * ((1i32 << (14 - fine_quant[i] - 1)) as f32) * (1.0 / 16384.0);
            old_e_bands[i] += offset;
            error[i] -= offset;
            bits_left -= 1;
            i += 1;
        }
    }
}
