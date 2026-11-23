// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Copyright (c) 2008-2009 Gregory Maxwell
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

//! CELT band shape decoding and synthesis helpers (celt/bands.c, mono decoder path, float build):
//! the allocation-driven band loop (`quant_all_bands`) with its splitting, folding, time-frequency
//! changes and spreading, de-normalisation, and anti-collapse.

use alloc::vec;
use alloc::vec::Vec;

use super::celt_modes::{CeltMode, NB_EBANDS};
use super::celt_quant::e_means;
use super::celt_rate::{bits2pulses, get_pulses, pulses2bits, BITRES, QTHETA_OFFSET};
use super::celt_vq::{alg_unquant, renormalise_vector, SPREAD_AGGRESSIVE};
use super::mathops::{celt_exp2, celt_rsqrt, celt_sqrt, isqrt32, maxf, minf};
use super::rangedec::RangeDecoder;

/// `celt_lcg_rand`.
#[inline]
pub fn celt_lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(1664525).wrapping_add(1013904223)
}

/// `FRAC_MUL16`: (16384 + a*b) >> 15 on 16-bit values.
#[inline]
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15
}

/// `bitexact_cos`: a cos() approximation designed to be bit-exact on any platform.
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

/// `denormalise_bands` (mono, no downsampling): scales the unit-norm bands by their energies.
pub fn denormalise_bands(
    x: &[f32],
    freq: &mut [f32],
    band_log_e: &[f32],
    start: usize,
    end: usize,
    m: usize,
    silence: bool,
) {
    let n = m * 120;
    let mut bound = m * CeltMode::ebands(end) as usize;
    let (mut start, mut end) = (start, end);
    if silence {
        bound = 0;
        start = 0;
        end = 0;
    }
    let mut f = 0usize;
    let mut xi = m * CeltMode::ebands(start) as usize;
    if start != 0 {
        for _ in 0..m * CeltMode::ebands(start) as usize {
            freq[f] = 0.0;
            f += 1;
        }
    } else {
        f += m * CeltMode::ebands(start) as usize;
    }
    for i in start..end {
        let band_end = m * CeltMode::ebands(i + 1) as usize;
        let mut j = m * CeltMode::ebands(i) as usize;
        let lg = band_log_e[i] + e_means(i);
        let g = celt_exp2(minf(32.0, lg));
        loop {
            freq[f] = x[xi] * g;
            f += 1;
            xi += 1;
            j += 1;
            if j >= band_end {
                break;
            }
        }
    }
    for v in freq[bound..n].iter_mut() {
        *v = 0.0;
    }
}

/// `anti_collapse` (mono, decoding): fills the short blocks that collapsed to zero with noise.
#[allow(clippy::too_many_arguments)]
pub fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8],
    lm: usize,
    start: usize,
    end: usize,
    log_e: &[f32],
    prev1_log_e: &[f32],
    prev2_log_e: &[f32],
    pulses: &[i32; NB_EBANDS],
    seed: u32,
) {
    let mut seed = seed;
    for i in start..end {
        let n0 = (CeltMode::ebands(i + 1) - CeltMode::ebands(i)) as usize;
        // depth in 1/8 bits
        let depth = ((1 + pulses[i]) / (n0 as i32)) >> lm;

        let thresh = 0.5f32 * celt_exp2(-0.125f32 * (depth as f32));
        let sqrt_1 = celt_rsqrt((n0 << lm) as f32);

        let prev1 = maxf(prev1_log_e[i], prev1_log_e[NB_EBANDS + i]);
        let prev2 = maxf(prev2_log_e[i], prev2_log_e[NB_EBANDS + i]);
        let mut ediff = log_e[i] - minf(prev1, prev2);
        ediff = maxf(0.0, ediff);

        // r needs to be multiplied by 2 or 2*sqrt(2) depending on LM because short blocks don't
        // have the same energy as long
        let mut r = 2.0f32 * celt_exp2(-ediff);
        if lm == 3 {
            r *= 1.41421356f32;
        }
        r = minf(thresh, r);
        r = r * sqrt_1;
        let xo = (CeltMode::ebands(i) as usize) << lm;
        let mut renormalize = false;
        for k in 0..(1usize << lm) {
            // Detect collapse
            if collapse_masks[i] & (1u8 << k) == 0 {
                // Fill with noise
                for j in 0..n0 {
                    seed = celt_lcg_rand(seed);
                    x[xo + (j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                }
                renormalize = true;
            }
        }
        // We just added some energy, so we need to renormalise
        if renormalize {
            renormalise_vector(&mut x[xo..], n0 << lm, 1.0);
        }
    }
}

/// Indexing table for converting from natural Hadamard to ordery Hadamard.
const ORDERY_TABLE: [usize; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    if stride < 1 || n > x.len() {
        return;
    }
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        if stride < 2 || stride - 2 + stride > ORDERY_TABLE.len() {
            return;
        }
        let ordery = &ORDERY_TABLE[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[ordery[i] * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    if stride < 1 || n > x.len() {
        return;
    }
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        if stride < 2 || stride - 2 + stride > ORDERY_TABLE.len() {
            return;
        }
        let ordery = &ORDERY_TABLE[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[ordery[i] * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let n0 = n0 >> 1;
    if stride * 2 * n0 > x.len() {
        return;
    }
    for i in 0..stride {
        for j in 0..n0 {
            let tmp1 = 0.70710678f32 * x[stride * 2 * j + i];
            let tmp2 = 0.70710678f32 * x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = tmp1 + tmp2;
            x[stride * (2 * j + 1) + i] = tmp1 - tmp2;
        }
    }
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let n2 = 2 * n - 1;
    // The upper limit ensures that in a stereo split with itheta==16384, we'll always have
    // enough bits left over to code at least one pulse in the side; otherwise it would
    // collapse, since it doesn't get folded.
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

struct BandCtx<'a, 'b> {
    rc: &'a mut RangeDecoder<'b>,
    /// The band being decoded.
    i: usize,
    spread: i32,
    tf_change: i32,
    remaining_bits: i32,
    seed: u32,
}

struct SplitCtx {
    imid: i32,
    iside: i32,
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

/// `compute_theta` for a mono split (the angle between the two halves).
fn compute_theta(
    ctx: &mut BandCtx,
    n: i32,
    b: &mut i32,
    bb: i32,
    b0: i32,
    lm: i32,
    fill: &mut i32,
) -> SplitCtx {
    // Decide on the resolution to give to the split parameter theta
    let pulse_cap = CeltMode::log_n(ctx.i) + lm * (1 << BITRES);
    let offset = (pulse_cap >> 1) - QTHETA_OFFSET;
    let qn = compute_qn(n, *b, offset, pulse_cap);
    let mut itheta: i32 = 0;
    let tell = ctx.rc.tell_frac() as i32;
    if qn != 1 {
        // Entropy coding of the angle. We use a uniform pdf for the time split, a step for
        // stereo, and a triangular one for the rest.
        if b0 > 1 {
            // Uniform pdf
            itheta = ctx.rc.uint((qn + 1) as u32) as i32;
        } else {
            let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
            // Triangular pdf
            let fm = ctx.rc.decode(ft as u32) as i32;
            let (fl, fs);
            if fm < (((qn >> 1) * ((qn >> 1) + 1)) >> 1) {
                itheta = ((isqrt32((8 * fm + 1) as u32) as i32) - 1) >> 1;
                fs = itheta + 1;
                fl = (itheta * (itheta + 1)) >> 1;
            } else {
                itheta = (2 * (qn + 1) - (isqrt32((8 * (ft - fm - 1) + 1) as u32) as i32)) >> 1;
                fs = qn + 1 - itheta;
                fl = ft - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1);
            }
            ctx.rc.update(fl as u32, (fl + fs) as u32, ft as u32);
        }
        itheta = (itheta * 16384) / qn;
    }
    let qalloc = ctx.rc.tell_frac() as i32 - tell;
    *b -= qalloc;

    let (imid, iside, delta);
    if itheta == 0 {
        imid = 32767;
        iside = 0;
        *fill &= (1 << bb) - 1;
        delta = -16384;
    } else if itheta == 16384 {
        imid = 0;
        iside = 32767;
        *fill &= ((1 << bb) - 1) << bb;
        delta = 16384;
    } else {
        imid = bitexact_cos(itheta);
        iside = bitexact_cos(16384 - itheta);
        // This is the mid vs side allocation that minimizes squared error in that band.
        delta = frac_mul16((n - 1) << 7, bitexact_log2tan(iside, imid));
    }
    SplitCtx { imid, iside, delta, itheta, qalloc }
}

fn quant_band_n1(ctx: &mut BandCtx, x: &mut [f32], lowband_out: Option<&mut [f32]>) -> u32 {
    let mut sign = 0;
    if ctx.remaining_bits >= 1 << BITRES {
        sign = ctx.rc.bits(1);
        ctx.remaining_bits -= 1 << BITRES;
    }
    x[0] = if sign != 0 { -1.0 } else { 1.0 };
    if let Some(lo) = lowband_out {
        lo[0] = x[0];
    }
    1
}

/// `quant_partition`: decodes (and splits, recursively) a mono partition of `n` samples.
#[allow(clippy::too_many_arguments)]
fn quant_partition(
    ctx: &mut BandCtx,
    x: &mut [f32],
    n: usize,
    b: i32,
    bb: i32,
    lowband: Option<&[f32]>,
    lm: i32,
    gain: f32,
    fill: i32,
) -> u32 {
    let mut n = n;
    let mut b = b;
    let mut bb = bb;
    let mut fill = fill;
    let mut lm = lm;
    let b0 = bb;
    let i = ctx.i;
    let mut cm: u32;

    // If we need 1.5 more bit than we can produce, split the band in two.
    let split = if lm != -1 && n > 2 {
        let cache = CeltMode::cache_bits((lm + 1) as usize, i);
        let top = cache.first().map(|&c| cache.get(c as usize).copied().unwrap_or(0) as i32).unwrap_or(0);
        !cache.is_empty() && b > top + 12
    } else {
        false
    };
    if split {
        n >>= 1;
        lm -= 1;
        if bb == 1 {
            fill = (fill & 1) | (fill << 1);
        }
        bb = (bb + 1) >> 1;

        let sctx = compute_theta(ctx, n as i32, &mut b, bb, b0, lm, &mut fill);
        let mut delta = sctx.delta;
        let itheta = sctx.itheta;
        let mid = (1.0f32 / 32768.0) * (sctx.imid as f32);
        let side = (1.0f32 / 32768.0) * (sctx.iside as f32);

        // Give more bits to low-energy MDCTs than they would otherwise deserve
        if b0 > 1 && (itheta & 0x3fff) != 0 {
            if itheta > 8192 {
                // Rough approximation for pre-echo masking
                delta -= delta >> (4 - lm);
            } else {
                // Corresponds to a forward-masking slope of 1.5 dB per 10 ms
                delta = core::cmp::min(0, delta + (((n as i32) << BITRES) >> (5 - lm)));
            }
        }
        let mut mbits = core::cmp::max(0, core::cmp::min(b, (b - delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= sctx.qalloc;

        let next_lowband2 = match lowband {
            Some(l) => l.get(n..),
            None => None,
        };

        let (xa, xb) = x.split_at_mut(n);
        let mut rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            cm = quant_partition(ctx, xa, n, mbits, bb, lowband, lm, gain * mid, fill);
            rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(ctx, xb, n, sbits, bb, next_lowband2, lm, gain * side, fill >> bb) << (b0 >> 1);
        } else {
            cm = quant_partition(ctx, xb, n, sbits, bb, next_lowband2, lm, gain * side, fill >> bb) << (b0 >> 1);
            rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(ctx, xa, n, mbits, bb, lowband, lm, gain * mid, fill);
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
            // Finally do the actual quantization
            cm = alg_unquant(x, n, k, ctx.spread, bb as usize, ctx.rc, gain);
        } else {
            // If there's no pulse, fill the band anyway
            cm = 0;
            let cm_mask: u32 = ((1u64 << bb) - 1) as u32;
            fill &= cm_mask as i32;
            if fill == 0 {
                for v in x[..n].iter_mut() {
                    *v = 0.0;
                }
            } else {
                match lowband {
                    None => {
                        // Noise
                        for j in 0..n {
                            ctx.seed = celt_lcg_rand(ctx.seed);
                            x[j] = ((ctx.seed as i32) >> 20) as f32;
                        }
                        cm = cm_mask;
                    }
                    Some(lb) => {
                        // Folded spectrum
                        for j in 0..n {
                            ctx.seed = celt_lcg_rand(ctx.seed);
                            // About 48 dB below the "normal" folding level
                            let tmp = if ctx.seed & 0x8000 != 0 { 1.0f32 / 256.0 } else { -1.0f32 / 256.0 };
                            x[j] = lb.get(j).copied().unwrap_or(0.0) + tmp;
                        }
                        cm = fill as u32;
                    }
                }
                renormalise_vector(x, n, gain);
            }
        }
    }
    cm
}

const BIT_INTERLEAVE_TABLE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
const BIT_DEINTERLEAVE_TABLE: [u8; 16] =
    [0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF];

/// `quant_band` (mono): decodes one band, including the time-frequency changes.
#[allow(clippy::too_many_arguments)]
fn quant_band(
    ctx: &mut BandCtx,
    x: &mut [f32],
    n: usize,
    b: i32,
    bb: i32,
    lowband: Option<&[f32]>,
    lm: i32,
    lowband_out: Option<&mut [f32]>,
    gain: f32,
    fill: i32,
) -> u32 {
    let n0 = n;
    let mut b_cur = bb;
    let mut fill = fill;
    let mut time_divide = 0;
    let mut recombine = 0usize;
    let mut tf_change = ctx.tf_change;
    let long_blocks = bb == 1;

    let mut n_b = (n as i32) / bb;

    // Special case for one sample
    if n == 1 {
        return quant_band_n1(ctx, x, lowband_out);
    }

    if tf_change > 0 {
        recombine = tf_change as usize;
    }
    // (the C code works on the folded spectrum in place or in a scratch copy: always copy here)
    let mut lb: Option<Vec<f32>> = match lowband {
        Some(l) => {
            let mut v = vec![0.0f32; n];
            let m = core::cmp::min(n, l.len());
            v[..m].copy_from_slice(&l[..m]);
            Some(v)
        }
        None => None,
    };

    // Band recombining to increase frequency resolution
    for k in 0..recombine {
        if let Some(l) = lb.as_mut() {
            haar1(l, n >> k, 1 << k);
        }
        fill = (BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize] as i32)
            | ((BIT_INTERLEAVE_TABLE[((fill >> 4) & 0xF) as usize] as i32) << 2);
    }
    b_cur >>= recombine;
    n_b <<= recombine;

    // Increasing the time resolution
    while (n_b & 1) == 0 && tf_change < 0 {
        if let Some(l) = lb.as_mut() {
            haar1(l, n_b as usize, b_cur as usize);
        }
        fill |= fill << b_cur;
        b_cur <<= 1;
        n_b >>= 1;
        time_divide += 1;
        tf_change += 1;
    }
    let b0 = b_cur;
    let n_b0 = n_b;

    // Reorganize the samples in time order instead of frequency order
    if b0 > 1 {
        if let Some(l) = lb.as_mut() {
            deinterleave_hadamard(l, (n_b >> recombine) as usize, (b0 << recombine) as usize, long_blocks);
        }
    }

    let mut cm = quant_partition(ctx, x, n, b, b_cur, lb.as_deref(), lm, gain, fill);

    // Undo the sample reorganization going from time order to frequency order
    if b0 > 1 {
        interleave_hadamard(x, (n_b >> recombine) as usize, (b0 << recombine) as usize, long_blocks);
    }

    // Undo time-freq changes that we did earlier
    n_b = n_b0;
    b_cur = b0;
    for _ in 0..time_divide {
        b_cur >>= 1;
        n_b <<= 1;
        cm |= cm >> b_cur;
        haar1(x, n_b as usize, b_cur as usize);
    }

    for k in 0..recombine {
        cm = BIT_DEINTERLEAVE_TABLE[(cm & 0xF) as usize] as u32;
        haar1(x, n0 >> k, 1 << k);
    }
    b_cur <<= recombine;

    // Scale output for later folding
    if let Some(lo) = lowband_out {
        let nn = celt_sqrt(n0 as f32);
        for j in 0..n0 {
            lo[j] = nn * x[j];
        }
    }
    cm &= ((1u64 << b_cur) - 1) as u32;
    cm
}

/// `quant_all_bands` for one channel, decoding. `x` holds the band shapes of the whole frame
/// (`M * 120` samples); `collapse_masks[band]` receives each band's collapse mask.
#[allow(clippy::too_many_arguments)]
pub fn quant_all_bands(
    start: usize,
    end: usize,
    x: &mut [f32],
    collapse_masks: &mut [u8; NB_EBANDS],
    pulses: &[i32; NB_EBANDS],
    short_blocks: bool,
    spread: i32,
    tf_res: &[i32; NB_EBANDS],
    total_bits: i32,
    balance: i32,
    rc: &mut RangeDecoder,
    lm: usize,
    coded_bands: usize,
    seed: &mut u32,
) {
    let eb = |i: usize| CeltMode::ebands(i);
    let m = 1usize << lm;
    let bb: i32 = if short_blocks { m as i32 } else { 1 };
    let norm_offset = m * eb(start) as usize;
    // No need to allocate norm for the last band because we don't need an output in that band.
    let mut norm = vec![0.0f32; m * eb(NB_EBANDS - 1) as usize - norm_offset];
    let mut balance = balance;
    let mut lowband_offset = 0usize;
    let mut update_lowband = true;

    let mut ctx = BandCtx { rc, i: 0, spread, tf_change: 0, remaining_bits: 0, seed: *seed };
    // (the encoder's "avoid split noise" is not needed by the decoder)

    for i in start..end {
        ctx.i = i;
        let last = i == end - 1;
        let xo = m * eb(i) as usize;
        let n = m * eb(i + 1) as usize - m * eb(i) as usize;
        let tell = ctx.rc.tell_frac() as i32;

        // Compute how many bits we want to allocate to this band
        if i != start {
            balance -= tell;
        }
        let remaining_bits = total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        let b: i32;
        if i + 1 <= coded_bands {
            let curr_balance = balance / core::cmp::min(3, (coded_bands - i) as i32);
            b = core::cmp::max(0, core::cmp::min(16383, core::cmp::min(remaining_bits + 1, pulses[i] + curr_balance)));
        } else {
            b = 0;
        }

        if (m * eb(i) as usize >= m * eb(start) as usize + n || i == start + 1) && (update_lowband || lowband_offset == 0) {
            lowband_offset = i;
        }
        if i == start + 1 {
            // special_hybrid_folding: duplicate enough of the first band's folding data to be able
            // to fold the second band (copies nothing for CELT-only frames)
            let n1 = m * (eb(start + 1) - eb(start)) as usize;
            let n2 = m * (eb(start + 2) - eb(start + 1)) as usize;
            if n2 > n1 && 2 * n1 >= n2 && n1 + (n2 - n1) <= norm.len() {
                let (src, cnt) = (2 * n1 - n2, n2 - n1);
                for k in 0..cnt {
                    norm[n1 + k] = norm[src + k];
                }
            }
        }

        ctx.tf_change = tf_res[i];

        // Get a conservative estimate of the collapse_mask's for the bands we're going to be
        // folding from.
        let x_cm_in: u32;
        let mut effective_lowband: i32 = -1;
        if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || bb > 1 || ctx.tf_change < 0) {
            // This ensures we never repeat spectral content within one band
            let eff = core::cmp::max(0, (m * eb(lowband_offset) as usize) as i32 - norm_offset as i32 - n as i32);
            effective_lowband = eff;
            let mut fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if !((m * eb(fold_start) as usize) as i32 > eff + norm_offset as i32) {
                    break;
                }
            }
            let mut fold_end = lowband_offset - 1;
            loop {
                // while(++fold_end < i && M*eBands[fold_end] < effective_lowband+norm_offset+N)
                fold_end += 1;
                if !(fold_end < i && ((m * eb(fold_end) as usize) as i32) < eff + norm_offset as i32 + n as i32) {
                    break;
                }
            }
            let mut cm = 0u32;
            let mut fold_i = fold_start;
            loop {
                cm |= collapse_masks[fold_i] as u32;
                fold_i += 1;
                if fold_i >= fold_end {
                    break;
                }
            }
            x_cm_in = cm;
        } else {
            // Otherwise, we'll be using the LCG to fold, so all blocks will (almost always) be
            // non-zero.
            x_cm_in = ((1u64 << bb) - 1) as u32;
        }

        let lowband: Option<&[f32]> = if effective_lowband != -1 { Some(&norm[effective_lowband as usize..]) } else { None };
        // lowband borrows norm; the band's folded output goes to a separate buffer first
        let mut out_buf: Vec<f32> = Vec::new();
        let lowband_out: Option<&mut [f32]> = if last {
            None
        } else {
            out_buf.resize(n, 0.0);
            Some(&mut out_buf[..])
        };
        let xband = &mut x[xo..xo + n];
        let x_cm = quant_band(&mut ctx, xband, n, b, bb, lowband, lm as i32, lowband_out, 1.0, x_cm_in as i32);
        if !last {
            let o = xo - norm_offset;
            let cnt = core::cmp::min(n, norm.len().saturating_sub(o));
            norm[o..o + cnt].copy_from_slice(&out_buf[..cnt]);
        }
        collapse_masks[i] = x_cm as u8;
        balance += pulses[i] + tell;

        // Update the folding position only as long as we have 1 bit/sample depth.
        update_lowband = b > ((n as i32) << BITRES);
    }
    *seed = ctx.seed;
}

