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

//! The CELT pyramid vector quantiser, decoder side (celt/vq.c, float build): the pulse vector to
//! unit-norm band, the spreading rotation, and vector renormalisation.

use alloc::vec;

use super::celt_cwrs::decode_pulses;
use super::mathops::{celt_cos_norm, celt_rsqrt};
use super::rangedec::RangeDecoder;

pub const SPREAD_NONE: i32 = 0;
pub const SPREAD_LIGHT: i32 = 1;
pub const SPREAD_NORMAL: i32 = 2;
pub const SPREAD_AGGRESSIVE: i32 = 3;

fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    let ms = -s;
    if len > stride {
        for i in 0..len - stride {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 + ms * x2;
        }
    }
    let mut i = len as isize - 2 * stride as isize - 1;
    while i >= 0 {
        let iu = i as usize;
        let x1 = x[iu];
        let x2 = x[iu + stride];
        x[iu + stride] = c * x2 + s * x1;
        x[iu] = c * x1 + ms * x2;
        i -= 1;
    }
}

/// `exp_rotation`: the (inverse, for dir < 0) spreading rotation of a band of `len` samples
/// made of `stride` interleaved short blocks.
pub fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: i32, spread: i32) {
    const SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    if 2 * k >= len as i32 || spread == SPREAD_NONE || spread < 1 || spread > 3 {
        return;
    }
    let factor = SPREAD_FACTOR[(spread - 1) as usize];

    let gain = (1.0f32 * (len as f32)) / ((len as i32 + factor * k) as f32);
    let theta = 0.5f32 * (gain * gain);

    let c = celt_cos_norm(theta);
    let s = celt_cos_norm(1.0f32 - theta); // sin(theta)

    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        // This is just a simple (equivalent) way of computing sqrt(len/stride) with rounding.
        // It's basically incrementing long as (stride2+0.5)^2 < len/stride.
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride;
    for i in 0..stride {
        let xs = &mut x[i * len..];
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(xs, len, stride2, s, c);
            }
            exp_rotation1(xs, len, 1, c, s);
        } else {
            exp_rotation1(xs, len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(xs, len, stride2, s, -c);
            }
        }
    }
}

/// `normalise_residual`: scales the integer pulse vector to norm `gain`.
fn normalise_residual(iy: &[i32], x: &mut [f32], n: usize, ryy: f32, gain: f32) {
    // (ryy is 0 only for a corrupt stream; keep the output finite)
    let g = if ryy > 0.0 { celt_rsqrt(ryy) * gain } else { 0.0 };
    for i in 0..n {
        x[i] = (iy[i] as f32) * g;
    }
}

fn extract_collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut collapse_mask = 0u32;
    for i in 0..b {
        let mut tmp = 0i32;
        for j in 0..n0 {
            tmp |= iy[i * n0 + j];
        }
        collapse_mask |= ((tmp != 0) as u32) << i;
    }
    collapse_mask
}

/// `alg_unquant`: decodes a PVQ band of `n` (>= 2) samples with `k` (> 0) pulses into `x`, with the
/// given `gain`. Returns the collapse mask.
pub fn alg_unquant(
    x: &mut [f32],
    n: usize,
    k: i32,
    spread: i32,
    b: usize,
    dec: &mut RangeDecoder,
    gain: f32,
) -> u32 {
    let mut iy = vec![0i32; n];
    let ryy = decode_pulses(&mut iy, n, k as usize, dec);
    normalise_residual(&iy, x, n, ryy, gain);
    exp_rotation(x, n, -1, b, k, spread);
    extract_collapse_mask(&iy, n, b)
}

/// `renormalise_vector`: scales `x` to norm `gain`.
pub fn renormalise_vector(x: &mut [f32], n: usize, gain: f32) {
    let mut sum = 0.0f32;
    for i in 0..n {
        sum += x[i] * x[i];
    }
    let e = 1e-15f32 + sum;
    let g = celt_rsqrt(e) * gain;
    for i in 0..n {
        x[i] = g * x[i];
    }
}
