// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2007-2008 CSIRO
// Copyright (c) 2007-2009 Xiph.Org Foundation
// Copyright (c) 2007-2009 Timothy B. Terriberry
// Written by Timothy B. Terriberry and Jean-Marc Valin
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

//! Pulse vector decoding (celt/cwrs.c, the SMALL_FOOTPRINT variant: the U(N,K) rows are computed
//! on the fly instead of read from a large table; the codebook is identical).

use alloc::vec;

use super::rangedec::RangeDecoder;

/// `unext`: the next row/column of u[i][j] = u[i-1][j] + u[i][j-1] + u[i-1][j-1].
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

/// `uprev`: the previous row/column of the same recurrence.
fn uprev(ui: &mut [u32], n: usize, ui0: u32) {
    let mut ui0 = ui0;
    let mut j = 1;
    while j < n {
        let ui1 = ui[j].wrapping_sub(ui[j - 1]).wrapping_sub(ui0);
        ui[j - 1] = ui0;
        ui0 = ui1;
        j += 1;
    }
    ui[j - 1] = ui0;
}

/// `ncwrs_urow`: V(n,k), and U(n, 0..=k+1) in `u`. Needs n >= 2, k > 0 and `u.len() >= k + 2`.
fn ncwrs_urow(n: usize, k: usize, u: &mut [u32]) -> u32 {
    let len = k + 2;
    u[0] = 0;
    u[1] = 1;
    let mut kk = 2;
    while kk < len {
        u[kk] = ((kk as u32) << 1).wrapping_sub(1);
        kk += 1;
    }
    for _ in 2..n {
        unext(&mut u[1..], k + 1, 1);
    }
    u[k].wrapping_add(u[k + 1])
}

/// `cwrsi`: the i'th combination of k pulses in n dimensions, with signs, into `y`. Returns the
/// sum of squares of the pulses.
fn cwrsi(n: usize, k: usize, i: u32, y: &mut [i32], u: &mut [u32]) -> f32 {
    let mut i = i;
    let mut k = k;
    let mut yy = 0.0f32;
    for j in 0..n {
        let mut p = u[k + 1];
        let s: i32 = if i >= p { -1 } else { 0 };
        i = i.wrapping_sub(p & (s as u32));
        let mut yj = k as i32;
        p = u[k];
        while p > i && k > 0 {
            k -= 1;
            p = u[k];
        }
        i = i.wrapping_sub(p);
        yj -= k as i32;
        let val = (yj.wrapping_add(s)) ^ s;
        y[j] = val;
        yy += (val as f32) * (val as f32);
        uprev(u, k + 2, 0);
    }
    yy
}

/// `decode_pulses`: reads the codeword of n-dimensional vectors with k pulses; returns sum(y^2).
/// `y.len() >= n`, n >= 2, k >= 1.
pub fn decode_pulses(y: &mut [i32], n: usize, k: usize, dec: &mut RangeDecoder) -> f32 {
    let mut u = vec![0u32; k + 2];
    let v = ncwrs_urow(n, k, &mut u);
    if v < 2 {
        // V(N,K) overflowed or is degenerate: only corrupt streams get here
        for e in y.iter_mut().take(n) {
            *e = 0;
        }
        dec.error = true;
        return 0.0;
    }
    let idx = dec.uint(v);
    cwrsi(n, k, idx, y, &mut u)
}
