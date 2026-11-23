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

//! CELT band shape encoding (celt/bands.c, `quant_all_bands` with encode = 1, mono, long blocks
//! only: no time-frequency changes, no short-block interleaving, no folding resynthesis, which the
//! encoder does not need). The bit budget arithmetic is that of the decoder (celt_bands.rs) so
//! that both sides allocate identically.

use super::celt_enc_math::atan2p_norm;
use super::celt_enc_vq::alg_quant;
use super::celt_modes::{CeltMode, NB_EBANDS};
use super::celt_rate::{bits2pulses, get_pulses, pulses2bits, BITRES, QTHETA_OFFSET};
use super::mathops::{floor_f64, sqrt_f64};
use super::rangeenc::RangeEncoder;

/// `FRAC_MUL16`: (16384 + a*b) >> 15 on 16-bit values.
#[inline]
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15
}

/// `bitexact_cos`.
fn bitexact_cos(x: i32) -> i32 {
    let x = x as i16 as i32;
    let tmp = (4096 + x * x) >> 13;
    let x2 = tmp;
    let x2 = (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)));
    1 + x2
}

fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = 32 - (icos as u32).leading_zeros() as i32;
    let ls = 32 - (isin as u32).leading_zeros() as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let n2 = 2 * n - 1;
    let mut qb = (b + n2 * offset) / n2;
    qb = core::cmp::min(b - pulse_cap - (4 << BITRES), qb);
    qb = core::cmp::min(8 << BITRES, qb);
    if qb < ((1 << BITRES) >> 1) {
        1
    } else {
        let qn = EXP2_TABLE8[(qb & 7) as usize] >> (14 - (qb >> BITRES));
        (qn + 1) >> 1 << 1
    }
}

struct BandCtx<'a> {
    enc: &'a mut RangeEncoder,
    i: usize,
    spread: i32,
    remaining_bits: i32,
}

/// `stereo_itheta` for the mono split: the angle (Q14, 16384 = pi/2) between the energies of the
/// two halves `x[..n]` and `x[n..2n]`.
fn split_itheta(x: &[f32], n: usize) -> i32 {
    let mut emid = 0.0f32;
    let mut eside = 0.0f32;
    for j in 0..n {
        emid += x[j] * x[j];
        eside += x[n + j] * x[n + j];
    }
    let mid = sqrt_f64(emid as f64);
    let side = sqrt_f64(eside as f64);
    let q30 = floor_f64(0.5 + 65536.0 * 16384.0 * atan2p_norm(side, mid)) as i64;
    (q30 >> 16) as i32
}

/// `quant_partition` (encoding, mono, one block): splits the band in two while there are more
/// bits than a single pulse vector can use, else writes the pulse vector.
fn quant_partition(ctx: &mut BandCtx, x: &mut [f32], n: usize, b: i32, lm: i32) {
    let i = ctx.i;
    let cache = CeltMode::cache_bits((lm + 1) as usize, i);
    // If we need 1.5 more bit than we can produce, split the band in two.
    let split = if lm != -1 && n > 2 && !cache.is_empty() {
        let top = cache.get(cache[0] as usize).copied().unwrap_or(0) as i32;
        b > top + 12
    } else {
        false
    };
    if split {
        let n = n >> 1;
        let lm = lm - 1;
        let mut b = b;
        // compute_theta
        let pulse_cap = CeltMode::log_n(i) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1) - QTHETA_OFFSET;
        let qn = compute_qn(n as i32, b, offset, pulse_cap);
        let mut itheta = split_itheta(x, n);
        let tell = ctx.enc.tell_frac() as i32;
        if qn != 1 {
            itheta = (itheta * qn + 8192) >> 14;
            // Triangular pdf
            let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
            let fs = if itheta <= (qn >> 1) { itheta + 1 } else { qn + 1 - itheta };
            let fl = if itheta <= (qn >> 1) { (itheta * (itheta + 1)) >> 1 } else { ft - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1) };
            ctx.enc.encode(fl as u32, (fl + fs) as u32, ft as u32);
            itheta = (itheta * 16384) / qn;
        } else {
            // theta is not coded: the decoder assumes 0
            itheta = 0;
        }
        let qalloc = ctx.enc.tell_frac() as i32 - tell;
        b -= qalloc;
        let delta = if itheta == 0 {
            -16384
        } else if itheta == 16384 {
            16384
        } else {
            let imid = bitexact_cos(itheta);
            let iside = bitexact_cos(16384 - itheta);
            frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid))
        };
        let mut mbits = core::cmp::max(0, core::cmp::min(b, (b - delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= qalloc;
        let (xa, xb) = x.split_at_mut(n);
        let mut rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            quant_partition(ctx, xa, n, mbits, lm);
            rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            quant_partition(ctx, xb, n, sbits, lm);
        } else {
            quant_partition(ctx, xb, n, sbits, lm);
            rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            quant_partition(ctx, xa, n, mbits, lm);
        }
    } else {
        // This is the basic no-split case
        let mut q = bits2pulses(i, lm, b);
        let mut curr_bits = pulses2bits(i, lm, q);
        ctx.remaining_bits -= curr_bits;
        // Ensures we can never bust the budget
        while ctx.remaining_bits < 0 && q > 0 {
            ctx.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses2bits(i, lm, q);
            ctx.remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = get_pulses(q);
            alg_quant(x, n, k, ctx.spread, 1, ctx.enc);
        }
    }
}

/// `quant_band` (encoding, mono, one block).
fn quant_band(ctx: &mut BandCtx, x: &mut [f32], n: usize, b: i32, lm: i32) {
    if n == 1 {
        // quant_band_n1
        if ctx.remaining_bits >= 1 << BITRES {
            let sign = (x[0] < 0.0) as u32;
            ctx.enc.bits(sign, 1);
            ctx.remaining_bits -= 1 << BITRES;
        }
        return;
    }
    quant_partition(ctx, x, n, b, lm);
}

/// `quant_all_bands` for one channel, encoding. `x` holds the unit-norm bands of the whole frame
/// (`M * 120` samples); it is modified (rotated) in place.
#[allow(clippy::too_many_arguments)]
pub fn quant_all_bands(
    start: usize,
    end: usize,
    x: &mut [f32],
    pulses: &[i32; NB_EBANDS],
    spread: i32,
    total_bits: i32,
    balance: i32,
    enc: &mut RangeEncoder,
    lm: usize,
    coded_bands: usize,
) {
    let eb = |i: usize| CeltMode::ebands(i);
    let m = 1usize << lm;
    let mut balance = balance;
    let mut ctx = BandCtx { enc, i: 0, spread, remaining_bits: 0 };
    for i in start..end {
        ctx.i = i;
        let xo = m * eb(i) as usize;
        let n = m * eb(i + 1) as usize - xo;
        let tell = ctx.enc.tell_frac() as i32;
        // Compute how many bits we want to allocate to this band
        if i != start {
            balance -= tell;
        }
        let remaining_bits = total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        let b: i32 = if i + 1 <= coded_bands {
            let curr_balance = balance / core::cmp::min(3, (coded_bands - i) as i32);
            core::cmp::max(0, core::cmp::min(16383, core::cmp::min(remaining_bits + 1, pulses[i] + curr_balance)))
        } else {
            0
        };
        quant_band(&mut ctx, &mut x[xo..xo + n], n, b, lm as i32);
        balance += pulses[i] + tell;
    }
}
