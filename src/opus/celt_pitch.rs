// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Copyright (c) 2009-2010 Xiph.Org Foundation (celt_lpc.c)
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

//! The CELT pitch pre/post-filter pieces the decoder needs: the postfilter comb filter
//! (celt/celt.c `comb_filter`), and for packet loss concealment the pitch search (celt/pitch.c)
//! and the LPC helpers (celt/celt_lpc.c), float build.

use alloc::vec;
use alloc::vec::Vec;

pub const COMBFILTER_MAXPERIOD: usize = 1024;
pub const COMBFILTER_MINPERIOD: i32 = 15;
pub const CELT_LPC_ORDER: usize = 24;

const GAINS: [[f32; 3]; 3] = [
    [0.3066406250, 0.2170410156, 0.1296386719],
    [0.4638671875, 0.2680664062, 0.0],
    [0.7998046875, 0.1000976562, 0.0],
];

/// `comb_filter_const_c`, in place: y = x on the same buffer (an IIR comb filter), over
/// `buf[off .. off + n]`, with the constant filter (T, g10, g11, g12).
fn comb_filter_const_inplace(buf: &mut [f32], off: usize, t: usize, n: usize, g10: f32, g11: f32, g12: f32) {
    let mut x4 = buf[off - t - 2];
    let mut x3 = buf[off - t - 1];
    let mut x2 = buf[off - t];
    let mut x1 = buf[off - t + 1];
    for i in 0..n {
        let x0 = buf[off + i - t + 2];
        buf[off + i] = buf[off + i] + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
}

/// `comb_filter` with y == x == `buf[x0..]` (the postfilter). `t0`/`t1` are the old and new
/// periods, `window` the overlap window.
#[allow(clippy::too_many_arguments)]
pub fn comb_filter_inplace(
    buf: &mut [f32],
    x0: usize,
    t0: i32,
    t1: i32,
    n: usize,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    window: &[f32],
    overlap: usize,
) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    // When the gain is zero, T0 and/or T1 is set to zero. We need to have then be at least 2 to
    // avoid processing garbage data.
    let t0 = core::cmp::max(t0, COMBFILTER_MINPERIOD) as usize;
    let t1 = core::cmp::max(t1, COMBFILTER_MINPERIOD) as usize;
    let tapset0 = core::cmp::min(tapset0, 2);
    let tapset1 = core::cmp::min(tapset1, 2);
    if x0 < t0.max(t1) + 2 || x0 + n > buf.len() {
        return;
    }
    let g00 = g0 * GAINS[tapset0][0];
    let g01 = g0 * GAINS[tapset0][1];
    let g02 = g0 * GAINS[tapset0][2];
    let g10 = g1 * GAINS[tapset1][0];
    let g11 = g1 * GAINS[tapset1][1];
    let g12 = g1 * GAINS[tapset1][2];
    let mut x1 = buf[x0 - t1 + 1];
    let mut x2 = buf[x0 - t1];
    let mut x3 = buf[x0 - t1 - 1];
    let mut x4 = buf[x0 - t1 - 2];
    // If the filter didn't change, we don't need the overlap
    let overlap = if g0 == g1 && t0 == t1 && tapset0 == tapset1 { 0 } else { core::cmp::min(overlap, n) };
    for i in 0..overlap {
        let xn0 = buf[x0 + i + 2 - t1];
        let f = window[i] * window[i];
        let a = 1.0f32 - f;
        let y = buf[x0 + i]
            + (a * g00) * buf[x0 + i - t0]
            + (a * g01) * (buf[x0 + i + 1 - t0] + buf[x0 + i - t0 - 1])
            + (a * g02) * (buf[x0 + i + 2 - t0] + buf[x0 + i - t0 - 2])
            + (f * g10) * x2
            + (f * g11) * (x1 + x3)
            + (f * g12) * (xn0 + x4);
        buf[x0 + i] = y;
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = xn0;
    }
    if g1 == 0.0 {
        return;
    }
    // Compute the part with the constant filter.
    comb_filter_const_inplace(buf, x0 + overlap, t1, n - overlap, g10, g11, g12);
}

/// `comb_filter` with separate input and output and no window (overlap 0): the FIR form used to
/// pre-filter the overlap region (y[i] = x[i] + g * x[i - T ...]). `x[xi - T - 2 ..]` must exist.
#[allow(clippy::too_many_arguments)]
pub fn comb_filter_fir(
    y: &mut [f32],
    x: &[f32],
    xi: usize,
    t0: i32,
    t1: i32,
    n: usize,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
) {
    if g0 == 0.0 && g1 == 0.0 {
        y[..n].copy_from_slice(&x[xi..xi + n]);
        return;
    }
    let t1 = core::cmp::max(t1, COMBFILTER_MINPERIOD) as usize;
    let _ = (t0, tapset0, g0);
    let tapset1 = core::cmp::min(tapset1, 2);
    if xi < t1 + 2 || xi + n > x.len() || y.len() < n {
        return;
    }
    if g1 == 0.0 {
        y[..n].copy_from_slice(&x[xi..xi + n]);
        return;
    }
    let g10 = g1 * GAINS[tapset1][0];
    let g11 = g1 * GAINS[tapset1][1];
    let g12 = g1 * GAINS[tapset1][2];
    let mut x4 = x[xi - t1 - 2];
    let mut x3 = x[xi - t1 - 1];
    let mut x2 = x[xi - t1];
    let mut x1 = x[xi - t1 + 1];
    for i in 0..n {
        let x0 = x[xi + i + 2 - t1];
        y[i] = x[xi + i] + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
}

/// `celt_inner_prod_c`.
fn inner_prod(x: &[f32], y: &[f32], n: usize) -> f32 {
    let mut xy = 0.0f32;
    for i in 0..n {
        xy += x[i] * y[i];
    }
    xy
}

/// `celt_pitch_xcorr_c`: xcorr[i] = sum_j x[j] * y[i + j].
fn pitch_xcorr(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    for i in 0..max_pitch {
        xcorr[i] = inner_prod(x, &y[i..], len);
    }
}

/// `_celt_autocorr` (float): the autocorrelation of x[0..n] for lags 0..=lag, optionally windowed
/// at both ends with `window` (overlap > 0).
pub fn celt_autocorr(x: &[f32], ac: &mut [f32], window: Option<&[f32]>, overlap: usize, lag: usize, n: usize) {
    let fast_n = n - lag;
    let xx: Vec<f32>;
    let xptr: &[f32] = match window {
        Some(w) if overlap > 0 => {
            let mut v: Vec<f32> = x[..n].to_vec();
            for i in 0..overlap {
                let wi = w[i];
                v[i] = x[i] * wi;
                v[n - i - 1] = x[n - i - 1] * wi;
            }
            xx = v;
            &xx
        }
        _ => &x[..n],
    };
    pitch_xcorr(xptr, xptr, ac, fast_n, lag + 1);
    for k in 0..=lag {
        let mut d = 0.0f32;
        let mut i = k + fast_n;
        while i < n {
            d += xptr[i] * xptr[i - k];
            i += 1;
        }
        ac[k] += d;
    }
}

/// `_celt_lpc` (float): Levinson-Durbin on the autocorrelation `ac[0..=p]`.
pub fn celt_lpc(lpc: &mut [f32], ac: &[f32], p: usize) {
    for v in lpc[..p].iter_mut() {
        *v = 0.0;
    }
    let mut error = ac[0];
    if ac[0] > 1e-10f32 {
        for i in 0..p {
            // Sum up this iteration's reflection coefficient
            let mut rr = 0.0f32;
            for j in 0..i {
                rr += lpc[j] * ac[i - j];
            }
            rr += ac[i + 1];
            let r = -(rr / error);
            // Update LPC coefficients and total error
            lpc[i] = r;
            for j in 0..((i + 1) >> 1) {
                let tmp1 = lpc[j];
                let tmp2 = lpc[i - 1 - j];
                lpc[j] = tmp1 + r * tmp2;
                lpc[i - 1 - j] = tmp2 + r * tmp1;
            }
            error = error - (r * r) * error;
            // Bail out once we get 30 dB gain
            if error <= 0.001f32 * ac[0] {
                break;
            }
        }
    }
}

/// `celt_fir_c` (float): y[i] = x[i] + sum_j num[j] * x[i - 1 - j], with `x[xo - ord ..]`
/// holding the filter history.
pub fn celt_fir(x: &[f32], xo: usize, num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    let mut rnum = [0.0f32; CELT_LPC_ORDER];
    for i in 0..ord {
        rnum[i] = num[ord - i - 1];
    }
    for i in 0..n {
        let mut sum = x[xo + i];
        for j in 0..ord {
            sum += rnum[j] * x[xo + i + j - ord];
        }
        y[i] = sum;
    }
}

/// `celt_iir` (float): y[i] = x[i] - sum_j den[j] * y[i - 1 - j], with `mem[j]` the output j+1
/// samples ago; updates `mem`. In place on `buf[off .. off + n]`. This follows the unrolled
/// libopus implementation (four outputs at a time through the `xcorr_kernel` order of summation)
/// so that the rounding matches.
pub fn celt_iir(buf: &mut [f32], off: usize, den: &[f32], n: usize, ord: usize, mem: &mut [f32; CELT_LPC_ORDER]) {
    let mut rden = [0.0f32; CELT_LPC_ORDER];
    for i in 0..ord {
        rden[i] = den[ord - i - 1];
    }
    let mut y = vec![0.0f32; n + ord];
    for i in 0..ord {
        y[i] = -mem[ord - i - 1];
    }
    let mut i = 0usize;
    while i + 3 < n {
        let mut sum = [buf[off + i], buf[off + i + 1], buf[off + i + 2], buf[off + i + 3]];
        // xcorr_kernel(rden, y + i, sum, ord): each sum[k] accumulates in increasing j order
        for j in 0..ord {
            let r = rden[j];
            sum[0] += r * y[i + j];
            sum[1] += r * y[i + j + 1];
            sum[2] += r * y[i + j + 2];
            sum[3] += r * y[i + j + 3];
        }
        // Patch up the result to compensate for the fact that this is an IIR
        y[i + ord] = -sum[0];
        buf[off + i] = sum[0];
        sum[1] += y[i + ord] * den[0];
        y[i + ord + 1] = -sum[1];
        buf[off + i + 1] = sum[1];
        sum[2] += y[i + ord + 1] * den[0];
        sum[2] += y[i + ord] * den[1];
        y[i + ord + 2] = -sum[2];
        buf[off + i + 2] = sum[2];

        sum[3] += y[i + ord + 2] * den[0];
        sum[3] += y[i + ord + 1] * den[1];
        sum[3] += y[i + ord] * den[2];
        y[i + ord + 3] = -sum[3];
        buf[off + i + 3] = sum[3];
        i += 4;
    }
    while i < n {
        let mut sum = buf[off + i];
        for j in 0..ord {
            sum += rden[j] * y[i + j];
        }
        y[i + ord] = -sum;
        buf[off + i] = sum;
        i += 1;
    }
    for i in 0..ord {
        mem[i] = buf[off + n - i - 1];
    }
}

fn celt_fir5(x: &mut [f32], num: &[f32; 5], n: usize) {
    let (num0, num1, num2, num3, num4) = (num[0], num[1], num[2], num[3], num[4]);
    let (mut mem0, mut mem1, mut mem2, mut mem3, mut mem4) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        let mut sum = x[i];
        sum += num0 * mem0;
        sum += num1 * mem1;
        sum += num2 * mem2;
        sum += num3 * mem3;
        sum += num4 * mem4;
        mem4 = mem3;
        mem3 = mem2;
        mem2 = mem1;
        mem1 = mem0;
        mem0 = x[i];
        x[i] = sum;
    }
}

/// `pitch_downsample` (one channel, factor 2): lowpass and decimate `x[0 .. 2 * len]` into
/// `x_lp[0..len]`.
pub fn pitch_downsample(x: &[f32], x_lp: &mut [f32], len: usize, factor: usize) {
    let offset = factor / 2;
    for i in 1..len {
        x_lp[i] = 0.25f32 * x[factor * i - offset] + 0.25f32 * x[factor * i + offset] + 0.5f32 * x[factor * i];
    }
    x_lp[0] = 0.25f32 * x[offset] + 0.5f32 * x[0];
    let mut ac = [0.0f32; 5];
    celt_autocorr(x_lp, &mut ac, None, 0, 4, len);

    // Noise floor -40 dB
    ac[0] *= 1.0001f32;
    // Lag windowing
    for i in 1..=4 {
        let fi = i as f32;
        ac[i] -= ac[i] * (0.008f32 * fi) * (0.008f32 * fi);
    }

    let mut lpc = [0.0f32; 4];
    celt_lpc(&mut lpc, &ac, 4);
    let mut tmp = 1.0f32;
    for i in 0..4 {
        tmp = 0.9f32 * tmp;
        lpc[i] = lpc[i] * tmp;
    }
    // Add a zero
    let c1 = 0.8f32;
    let lpc2 = [
        lpc[0] + 0.8f32,
        lpc[1] + c1 * lpc[0],
        lpc[2] + c1 * lpc[1],
        lpc[3] + c1 * lpc[2],
        c1 * lpc[3],
    ];
    celt_fir5(x_lp, &lpc2, len);
}

fn find_best_pitch(xcorr: &[f32], y: &[f32], len: usize, max_pitch: usize, best_pitch: &mut [i32; 2]) {
    let mut syy = 1.0f32;
    let mut best_num = [-1.0f32, -1.0f32];
    let mut best_den = [0.0f32, 0.0f32];
    best_pitch[0] = 0;
    best_pitch[1] = 1;
    for j in 0..len {
        syy += y[j] * y[j];
    }
    for i in 0..max_pitch {
        if xcorr[i] > 0.0 {
            // Considering the range of xcorr16, this should avoid both underflows and overflows
            // (inf) when squaring xcorr16
            let xcorr16 = xcorr[i] * 1e-12f32;
            let num = xcorr16 * xcorr16;
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best_pitch[0] = i as i32;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best_pitch[1] = i as i32;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        if syy < 1.0 {
            syy = 1.0;
        }
    }
}

/// `pitch_search`: the best pitch lag (in samples of the 2x decimated signal x2) of `x_lp`
/// against the history `y`, `len` samples of x_lp, lags 0..max_pitch.
pub fn pitch_search(x_lp: &[f32], y: &[f32], len: usize, max_pitch: usize) -> i32 {
    let lag = len + max_pitch;
    let mut x_lp4 = vec![0.0f32; len >> 2];
    let mut y_lp4 = vec![0.0f32; lag >> 2];
    let mut xcorr = vec![0.0f32; max_pitch >> 1];

    // Downsample by 2 again
    for j in 0..len >> 2 {
        x_lp4[j] = x_lp[2 * j];
    }
    for j in 0..lag >> 2 {
        y_lp4[j] = y[2 * j];
    }

    // Coarse search with 4x decimation
    pitch_xcorr(&x_lp4, &y_lp4, &mut xcorr, len >> 2, max_pitch >> 2);
    let mut best_pitch = [0i32; 2];
    find_best_pitch(&xcorr, &y_lp4, len >> 2, max_pitch >> 2, &mut best_pitch);

    // Finer search with 2x decimation
    for i in 0..max_pitch >> 1 {
        xcorr[i] = 0.0;
        let ii = i as i32;
        if (ii - 2 * best_pitch[0]).abs() > 2 && (ii - 2 * best_pitch[1]).abs() > 2 {
            continue;
        }
        let sum = inner_prod(x_lp, &y[i..], len >> 1);
        xcorr[i] = if sum > -1.0 { sum } else { -1.0 };
    }
    find_best_pitch(&xcorr, y, len >> 1, max_pitch >> 1, &mut best_pitch);

    // Refine by pseudo-interpolation
    let offset: i32;
    if best_pitch[0] > 0 && (best_pitch[0] as usize) < (max_pitch >> 1) - 1 {
        let bp = best_pitch[0] as usize;
        let a = xcorr[bp - 1];
        let b = xcorr[bp];
        let c = xcorr[bp + 1];
        if (c - a) > 0.7f32 * (b - a) {
            offset = 1;
        } else if (a - c) > 0.7f32 * (b - c) {
            offset = -1;
        } else {
            offset = 0;
        }
    } else {
        offset = 0;
    }
    2 * best_pitch[0] - offset
}
