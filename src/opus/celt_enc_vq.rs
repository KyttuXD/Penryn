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

//! The CELT pyramid vector quantiser, encoder side (celt/vq.c `op_pvq_search_c` and `alg_quant`,
//! celt/cwrs.c `icwrs` + `encode_pulses`, the SMALL_FOOTPRINT variant; float build).

use alloc::vec;
use alloc::vec::Vec;

use super::celt_vq::exp_rotation;
use super::mathops::floor_f64;
use super::rangeenc::RangeEncoder;

/// `unext` of cwrs.c.
fn unext(ui: &mut [u32], len: usize, ui0: u32) {
    let mut ui0 = ui0;
    let mut j = 1;
    while j < len {
        let ui1 = ui[j].wrapping_add(ui[j - 1]).wrapping_add(ui0);
        ui[j - 1] = ui0;
        ui0 = ui1;
        j += 1;
    }
    ui[j - 1] = ui0;
}

/// `icwrs`: the index of the pulse vector `y` (n >= 2, sum |y| = k >= 1) among the
/// V(n,k) codewords, and V(n,k).
fn icwrs(n: usize, kk: usize, y: &[i32], u: &mut [u32]) -> (u32, u32) {
    u[0] = 0;
    for k in 1..=kk + 1 {
        u[k] = ((k as u32) << 1).wrapping_sub(1);
    }
    let mut k = y[n - 1].unsigned_abs() as usize;
    let mut i = (y[n - 1] < 0) as u32;
    let mut j = n - 2;
    i = i.wrapping_add(u[k]);
    k += y[j].unsigned_abs() as usize;
    if y[j] < 0 {
        i = i.wrapping_add(u[k + 1]);
    }
    while j > 0 {
        j -= 1;
        unext(u, kk + 2, 0);
        i = i.wrapping_add(u[k]);
        k += y[j].unsigned_abs() as usize;
        if y[j] < 0 {
            i = i.wrapping_add(u[k + 1]);
        }
    }
    (i, u[k].wrapping_add(u[k + 1]))
}

/// `encode_pulses`.
pub fn encode_pulses(y: &[i32], n: usize, k: usize, enc: &mut RangeEncoder) {
    if k == 0 || n < 2 || y.len() < n {
        return;
    }
    let mut u: Vec<u32> = vec![0; k + 3];
    let (i, nc) = icwrs(n, k, y, &mut u);
    enc.uint(i, nc);
}

/// `op_pvq_search_c`: the integer vector `iy` with K pulses closest in direction to `x`.
pub fn op_pvq_search(x: &[f32], iy: &mut [i32], k: i32, n: usize) {
    let mut xa = vec![0.0f32; n];
    let mut y = vec![0.0f32; n];
    let mut signx = vec![false; n];
    for j in 0..n {
        signx[j] = x[j] < 0.0;
        xa[j] = if x[j] < 0.0 { -x[j] } else { x[j] };
        iy[j] = 0;
    }
    let mut xy = 0.0f32;
    let mut yy = 0.0f32;
    let mut pulses_left = k;
    // Do a pre-search by projecting on the pyramid
    if k > (n >> 1) as i32 {
        let mut sum = 0.0f32;
        for j in 0..n {
            sum += xa[j];
        }
        // Prevents infinities and NaNs from causing too many pulses to be allocated.
        if !(sum > 1e-15 && sum < 64.0) {
            xa[0] = 1.0;
            for j in 1..n {
                xa[j] = 0.0;
            }
            sum = 1.0;
        }
        // Using K+e with e < 1 guarantees we cannot get more than K pulses.
        let rcp = (k as f32 + 0.8) * (1.0 / sum);
        for j in 0..n {
            iy[j] = floor_f64((rcp * xa[j]) as f64) as i32;
            y[j] = iy[j] as f32;
            yy += y[j] * y[j];
            xy += xa[j] * y[j];
            y[j] *= 2.0;
            pulses_left -= iy[j];
        }
    }
    // This should never happen, but just in case it does (e.g. on silence) we fill the first bin.
    if pulses_left > n as i32 + 3 {
        let tmp = pulses_left as f32;
        yy += tmp * tmp;
        yy += tmp * y[0];
        iy[0] += pulses_left;
        pulses_left = 0;
    }
    for _ in 0..pulses_left.max(0) {
        let mut best_id = 0usize;
        // The squared magnitude term gets added anyway, so we might as well add it outside the loop
        yy += 1.0;
        let mut rxy = xy + xa[0];
        let mut ryy = yy + y[0];
        rxy = rxy * rxy;
        let mut best_den = ryy;
        let mut best_num = rxy;
        for j in 1..n {
            rxy = xy + xa[j];
            ryy = yy + y[j];
            rxy = rxy * rxy;
            if best_den * rxy > ryy * best_num {
                best_den = ryy;
                best_num = rxy;
                best_id = j;
            }
        }
        xy += xa[best_id];
        yy += y[best_id];
        y[best_id] += 2.0;
        iy[best_id] += 1;
    }
    // Put the original sign back
    for j in 0..n {
        if signx[j] {
            iy[j] = -iy[j];
        }
    }
}

/// `alg_quant` (without resynthesis): rotates `x` (n >= 2, a band of `b` interleaved blocks),
/// searches the `k` (> 0) pulse vector and writes its codeword.
pub fn alg_quant(x: &mut [f32], n: usize, k: i32, spread: i32, b: usize, enc: &mut RangeEncoder) {
    if k <= 0 || n < 2 {
        return;
    }
    exp_rotation(x, n, 1, b, k, spread);
    let mut iy = vec![0i32; n];
    op_pvq_search(x, &mut iy, k, n);
    encode_pulses(&iy, n, k as usize, enc);
}
