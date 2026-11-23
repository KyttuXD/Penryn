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

//! CELT band energy decoding (celt/quant_bands.c, decoder side, float build): the coarse energy
//! (Laplace coded, predicted in time and frequency), the fine energy bits and the final leftover
//! bits.

use super::celt_laplace::ec_laplace_decode;
use super::celt_modes::NB_EBANDS;
use super::celt_rate::MAX_FINE_BITS;
use super::celt_tables::{E_MEANS, E_PROB_MODEL};
use super::mathops::maxf;
use super::rangedec::RangeDecoder;

/// prediction coefficients: 0.9, 0.8, 0.65, 0.5
const PRED_COEF: [f32; 4] = [29440.0 / 32768.0, 26112.0 / 32768.0, 21248.0 / 32768.0, 16384.0 / 32768.0];
const BETA_COEF: [f32; 4] = [30147.0 / 32768.0, 22282.0 / 32768.0, 12124.0 / 32768.0, 6554.0 / 32768.0];
const BETA_INTRA: f32 = 4915.0 / 32768.0;

const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

/// The mean energy of band `i` (eMeans).
#[inline]
pub fn e_means(i: usize) -> f32 {
    E_MEANS[i]
}

/// `unquant_coarse_energy` for one channel. `old_band_e` holds the previous frame's energies (log2,
/// 1 = 6 dB) at index `band`, and receives the new ones.
pub fn unquant_coarse_energy(
    start: usize,
    end: usize,
    old_band_e: &mut [f32],
    intra: bool,
    dec: &mut RangeDecoder,
    lm: usize,
) {
    let prob_model = &E_PROB_MODEL[lm][intra as usize];
    let mut prev = 0.0f32;
    let (coef, beta) = if intra { (0.0f32, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
    let budget = dec.storage() as i32 * 8;
    // Decode at a fixed coarse resolution
    for i in start..end {
        let tell = dec.tell();
        let qi: i32;
        if budget - tell >= 15 {
            let pi = 2 * core::cmp::min(i, 20);
            qi = ec_laplace_decode(dec, (prob_model[pi] as u32) << 7, (prob_model[pi + 1] as i32) << 6);
        } else if budget - tell >= 2 {
            let q = dec.icdf(&SMALL_ENERGY_ICDF, 2);
            qi = (q >> 1) ^ -(q & 1);
        } else if budget - tell >= 1 {
            qi = -dec.bit_logp(1);
        } else {
            qi = -1;
        }
        let q = qi as f32;
        old_band_e[i] = maxf(-9.0, old_band_e[i]);
        let tmp = coef * old_band_e[i] + prev + q;
        old_band_e[i] = tmp;
        prev = prev + q - beta * q;
    }
}

/// `unquant_fine_energy`. `prev_quant` is the number of fine bits already spent in an earlier
/// pass (None for 0).
pub fn unquant_fine_energy(
    start: usize,
    end: usize,
    old_band_e: &mut [f32],
    prev_quant: Option<&[i32]>,
    extra_quant: &[i32; NB_EBANDS],
    dec: &mut RangeDecoder,
) {
    for i in start..end {
        let extra = extra_quant[i];
        if extra <= 0 {
            continue;
        }
        if dec.tell() + extra > dec.storage() as i32 * 8 {
            continue;
        }
        let prev = match prev_quant {
            Some(p) => p[i],
            None => 0,
        };
        let q2 = dec.bits(extra as u32) as i32;
        let mut offset = ((q2 as f32) + 0.5) * ((1i32 << (14 - extra)) as f32) * (1.0 / 16384.0) - 0.5;
        offset *= ((1i32 << (14 - prev)) as f32) * (1.0 / 16384.0);
        old_band_e[i] += offset;
    }
}

/// `unquant_energy_finalise`: uses the leftover bits for one more fine bit in the bands that
/// asked for it.
pub fn unquant_energy_finalise(
    start: usize,
    end: usize,
    old_band_e: Option<&mut [f32]>,
    fine_quant: &[i32; NB_EBANDS],
    fine_priority: &[i32; NB_EBANDS],
    bits_left: i32,
    dec: &mut RangeDecoder,
) {
    let mut bits_left = bits_left;
    let mut old = old_band_e;
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= 1 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            let q2 = dec.bits(1) as i32;
            let offset = ((q2 as f32) - 0.5) * ((1i32 << (14 - fine_quant[i] - 1)) as f32) * (1.0 / 16384.0);
            if let Some(o) = old.as_deref_mut() {
                o[i] += offset;
            }
            bits_left -= 1;
            i += 1;
        }
    }
}

