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

//! The CELT forward MDCT (celt/mdct.c, `clt_mdct_forward_c`, float build) for the standard 48 kHz
//! / 960 sample mode. The FFT part is a copy of the one in celt_mdct.rs (which keeps it private);
//! the twiddle tables are the same libopus tables.

use alloc::vec;
use alloc::vec::Vec;

use super::celt_tables::{FFT_TWIDDLES48000_960, MDCT_TWIDDLES960};

#[derive(Clone, Copy, Default)]
struct Cpx {
    r: f32,
    i: f32,
}

#[inline]
fn c_mul(a: Cpx, b: Cpx) -> Cpx {
    Cpx { r: a.r * b.r - a.i * b.i, i: a.r * b.i + a.i * b.r }
}
#[inline]
fn c_add(a: Cpx, b: Cpx) -> Cpx {
    Cpx { r: a.r + b.r, i: a.i + b.i }
}
#[inline]
fn c_sub(a: Cpx, b: Cpx) -> Cpx {
    Cpx { r: a.r - b.r, i: a.i - b.i }
}

const MAXFACTORS: usize = 8;

struct KissFft {
    nfft: usize,
    /// log2 of (base nfft / this nfft): the twiddle stride shift (0 for the base FFT).
    shift: usize,
    /// (radix, remaining length) pairs.
    factors: [i32; 2 * MAXFACTORS],
    bitrev: Vec<u16>,
}

/// `kf_factor`: the radix-4 first, then 2, 3, 5, ordered so that a radix-4 comes last.
fn kf_factor(n: usize, facbuf: &mut [i32; 2 * MAXFACTORS]) -> bool {
    let mut p: i32 = 4;
    let mut stages = 0usize;
    let mut n_i = n as i32;
    let nbak = n_i;
    loop {
        while n_i % p != 0 {
            p = match p {
                4 => 2,
                2 => 3,
                _ => p + 2,
            };
            if p > 32000 || (p as i64) * (p as i64) > n_i as i64 {
                p = n_i;
            }
        }
        n_i /= p;
        if p > 5 {
            return false;
        }
        if stages >= MAXFACTORS {
            return false;
        }
        facbuf[2 * stages] = p;
        if p == 2 && stages > 1 {
            facbuf[2 * stages] = 4;
            facbuf[2] = 2;
        }
        stages += 1;
        if n_i <= 1 {
            break;
        }
    }
    let mut n_i = nbak;
    for i in 0..stages / 2 {
        facbuf.swap(2 * i, 2 * (stages - i - 1));
    }
    for i in 0..stages {
        n_i /= facbuf[2 * i];
        facbuf[2 * i + 1] = n_i;
    }
    true
}

/// `compute_bitrev_table`.
fn compute_bitrev_table(
    fout: usize,
    f: &mut [u16],
    f_off: usize,
    fstride: usize,
    in_stride: usize,
    factors: &[i32],
) {
    let p = factors[0] as usize;
    let m = factors[1] as usize;
    let mut f_off = f_off;
    let mut fout = fout;
    if m == 1 {
        for j in 0..p {
            f[f_off] = (fout + j) as u16;
            f_off += fstride * in_stride;
        }
    } else {
        for _ in 0..p {
            compute_bitrev_table(fout, f, f_off, fstride * p, in_stride, &factors[2..]);
            f_off += fstride * in_stride;
            fout += m;
        }
    }
}

fn kf_bfly2(fout: &mut [Cpx], n: usize) {
    // m == 4 here: the radix-2 is always just after a radix-4
    let tw = 0.7071067812f32;
    let mut o = 0;
    for _ in 0..n {
        let mut t = fout[o + 4];
        fout[o + 4] = c_sub(fout[o], t);
        fout[o] = c_add(fout[o], t);

        t.r = (fout[o + 5].r + fout[o + 5].i) * tw;
        t.i = (fout[o + 5].i - fout[o + 5].r) * tw;
        fout[o + 5] = c_sub(fout[o + 1], t);
        fout[o + 1] = c_add(fout[o + 1], t);

        t.r = fout[o + 6].i;
        t.i = -fout[o + 6].r;
        fout[o + 6] = c_sub(fout[o + 2], t);
        fout[o + 2] = c_add(fout[o + 2], t);

        t.r = (fout[o + 7].i - fout[o + 7].r) * tw;
        t.i = -(fout[o + 7].i + fout[o + 7].r) * tw;
        fout[o + 7] = c_sub(fout[o + 3], t);
        fout[o + 3] = c_add(fout[o + 3], t);
        o += 8;
    }
}

fn kf_bfly4(fout: &mut [Cpx], fstride: usize, tws: &[Cpx], m: usize, n: usize, mm: usize) {
    if m == 1 {
        let mut o = 0;
        for _ in 0..n {
            let scratch0 = c_sub(fout[o], fout[o + 2]);
            fout[o] = c_add(fout[o], fout[o + 2]);
            let mut scratch1 = c_add(fout[o + 1], fout[o + 3]);
            fout[o + 2] = c_sub(fout[o], scratch1);
            fout[o] = c_add(fout[o], scratch1);
            scratch1 = c_sub(fout[o + 1], fout[o + 3]);

            fout[o + 1].r = scratch0.r + scratch1.i;
            fout[o + 1].i = scratch0.i - scratch1.r;
            fout[o + 3].r = scratch0.r - scratch1.i;
            fout[o + 3].i = scratch0.i + scratch1.r;
            o += 4;
        }
    } else {
        let m2 = 2 * m;
        let m3 = 3 * m;
        for i in 0..n {
            let base = i * mm;
            let mut t1 = 0usize;
            let mut t2 = 0usize;
            let mut t3 = 0usize;
            for j in 0..m {
                let f = base + j;
                let s0 = c_mul(fout[f + m], tws[t1]);
                let s1 = c_mul(fout[f + m2], tws[t2]);
                let s2 = c_mul(fout[f + m3], tws[t3]);

                let s5 = c_sub(fout[f], s1);
                fout[f] = c_add(fout[f], s1);
                let s3 = c_add(s0, s2);
                let s4 = c_sub(s0, s2);
                fout[f + m2] = c_sub(fout[f], s3);
                t1 += fstride;
                t2 += fstride * 2;
                t3 += fstride * 3;
                fout[f] = c_add(fout[f], s3);

                fout[f + m].r = s5.r + s4.i;
                fout[f + m].i = s5.i - s4.r;
                fout[f + m3].r = s5.r - s4.i;
                fout[f + m3].i = s5.i + s4.r;
            }
        }
    }
}

fn kf_bfly3(fout: &mut [Cpx], fstride: usize, tws: &[Cpx], m: usize, n: usize, mm: usize) {
    let m2 = 2 * m;
    let epi3 = tws[fstride * m];
    for i in 0..n {
        let base = i * mm;
        let mut t1 = 0usize;
        let mut t2 = 0usize;
        for j in 0..m {
            let f = base + j;
            let s1 = c_mul(fout[f + m], tws[t1]);
            let s2 = c_mul(fout[f + m2], tws[t2]);

            let s3 = c_add(s1, s2);
            let mut s0 = c_sub(s1, s2);
            t1 += fstride;
            t2 += fstride * 2;

            fout[f + m].r = fout[f].r - s3.r * 0.5;
            fout[f + m].i = fout[f].i - s3.i * 0.5;

            s0.r *= epi3.i;
            s0.i *= epi3.i;

            fout[f] = c_add(fout[f], s3);

            fout[f + m2].r = fout[f + m].r + s0.i;
            fout[f + m2].i = fout[f + m].i - s0.r;

            fout[f + m].r -= s0.i;
            fout[f + m].i += s0.r;
        }
    }
}

fn kf_bfly5(fout: &mut [Cpx], fstride: usize, tws: &[Cpx], m: usize, n: usize, mm: usize) {
    let ya = tws[fstride * m];
    let yb = tws[fstride * 2 * m];
    for i in 0..n {
        let base = i * mm;
        for u in 0..m {
            let f0 = base + u;
            let f1 = f0 + m;
            let f2 = f0 + 2 * m;
            let f3 = f0 + 3 * m;
            let f4 = f0 + 4 * m;
            let s0 = fout[f0];
            let s1 = c_mul(fout[f1], tws[u * fstride]);
            let s2 = c_mul(fout[f2], tws[2 * u * fstride]);
            let s3 = c_mul(fout[f3], tws[3 * u * fstride]);
            let s4 = c_mul(fout[f4], tws[4 * u * fstride]);

            let s7 = c_add(s1, s4);
            let s10 = c_sub(s1, s4);
            let s8 = c_add(s2, s3);
            let s9 = c_sub(s2, s3);

            fout[f0].r += s7.r + s8.r;
            fout[f0].i += s7.i + s8.i;

            let s5 = Cpx { r: s0.r + (s7.r * ya.r + s8.r * yb.r), i: s0.i + (s7.i * ya.r + s8.i * yb.r) };
            let s6 = Cpx { r: s10.i * ya.i + s9.i * yb.i, i: -(s10.r * ya.i + s9.r * yb.i) };

            fout[f1] = c_sub(s5, s6);
            fout[f4] = c_add(s5, s6);

            let s11 = Cpx { r: s0.r + (s7.r * yb.r + s8.r * ya.r), i: s0.i + (s7.i * yb.r + s8.i * ya.r) };
            let s12 = Cpx { r: s9.i * ya.i - s10.i * yb.i, i: s10.r * yb.i - s9.r * ya.i };

            fout[f2] = c_add(s11, s12);
            fout[f3] = c_sub(s11, s12);
        }
    }
}

impl KissFft {
    /// `opus_fft_impl`: the in-place FFT on bit-reversed input.
    fn fft_impl(&self, tws: &[Cpx], fout: &mut [Cpx]) {
        let shift = self.shift;
        let mut fstride = [0usize; MAXFACTORS + 1];
        fstride[0] = 1;
        let mut l = 0usize;
        loop {
            let p = self.factors[2 * l] as usize;
            let m = self.factors[2 * l + 1];
            fstride[l + 1] = fstride[l] * p;
            l += 1;
            if m == 1 || l >= MAXFACTORS {
                break;
            }
        }
        let mut m = self.factors[2 * l - 1] as usize;
        for i in (0..l).rev() {
            let m2 = if i != 0 { self.factors[2 * i - 1] as usize } else { 1 };
            match self.factors[2 * i] {
                2 => kf_bfly2(fout, fstride[i]),
                4 => kf_bfly4(fout, fstride[i] << shift, tws, m, fstride[i], m2),
                3 => kf_bfly3(fout, fstride[i] << shift, tws, m, fstride[i], m2),
                5 => kf_bfly5(fout, fstride[i] << shift, tws, m, fstride[i], m2),
                _ => {}
            }
            m = m2;
        }
    }
}

/// The forward MDCT lookup of the 48 kHz / 960 mode (1920-point MDCT, shifts 0..=3).
pub struct MdctFwd {
    n: usize,
    maxshift: usize,
    ffts: Vec<KissFft>,
    twiddles: Vec<Cpx>,
    trig: Vec<f32>,
}

impl MdctFwd {
    pub fn new() -> MdctFwd {
        let n = 1920usize;
        let maxshift = 3usize;
        let base_nfft = n >> 2;
        let mut twiddles = Vec::with_capacity(base_nfft);
        for i in 0..base_nfft {
            twiddles.push(Cpx { r: FFT_TWIDDLES48000_960[2 * i], i: FFT_TWIDDLES48000_960[2 * i + 1] });
        }
        let mut ffts = Vec::new();
        for s in 0..=maxshift {
            let nfft = base_nfft >> s;
            let mut factors = [0i32; 2 * MAXFACTORS];
            kf_factor(nfft, &mut factors);
            let mut bitrev = vec![0u16; nfft];
            compute_bitrev_table(0, &mut bitrev, 0, 1, 1, &factors);
            ffts.push(KissFft { nfft, shift: s, factors, bitrev });
        }
        MdctFwd { n, maxshift, ffts, twiddles, trig: MDCT_TWIDDLES960.to_vec() }
    }

    /// `clt_mdct_forward_c`: the MDCT of `input[..N + overlap]` (N = 1920 >> shift) windowed with
    /// `window` (the `overlap`-sample power-complementary window), into
    /// `out[out_off + k * stride]` (k < N / 2). Returns without output on a size mismatch.
    pub fn forward(&self, input: &[f32], out: &mut [f32], out_off: usize, window: &[f32], overlap: usize, shift: usize, stride: usize) {
        if shift > self.maxshift {
            return;
        }
        let mut n = self.n;
        let mut trig_off = 0usize;
        for _ in 0..shift {
            n >>= 1;
            trig_off += n;
        }
        let n2 = n >> 1;
        let n4 = n >> 2;
        if input.len() < n2 + overlap || window.len() < overlap || out.len() < out_off + stride * (n2 - 1) + 1 {
            return;
        }
        let trig = &self.trig[trig_off..];
        let st = &self.ffts[shift];
        let scale = 1.0f32 / (st.nfft as f32);

        // Window, shuffle, fold
        let mut f = vec![0.0f32; n2];
        {
            let mut xp1 = (overlap >> 1) as isize;
            let mut xp2 = (n2 - 1 + (overlap >> 1)) as isize;
            let n2i = n2 as isize;
            let mut yp = 0usize;
            let mut wp1 = (overlap >> 1) as isize;
            let mut wp2 = (overlap >> 1) as isize - 1;
            let mut i = 0usize;
            let a = (overlap + 3) >> 2;
            while i < a {
                f[yp] = input[(xp1 + n2i) as usize] * window[wp2 as usize] + input[xp2 as usize] * window[wp1 as usize];
                f[yp + 1] = input[xp1 as usize] * window[wp1 as usize] - input[(xp2 - n2i) as usize] * window[wp2 as usize];
                yp += 2;
                xp1 += 2;
                xp2 -= 2;
                wp1 += 2;
                wp2 -= 2;
                i += 1;
            }
            wp1 = 0;
            wp2 = overlap as isize - 1;
            while i < n4 - a {
                f[yp] = input[xp2 as usize];
                f[yp + 1] = input[xp1 as usize];
                yp += 2;
                xp1 += 2;
                xp2 -= 2;
                i += 1;
            }
            while i < n4 {
                f[yp] = -input[(xp1 - n2i) as usize] * window[wp1 as usize] + input[xp2 as usize] * window[wp2 as usize];
                f[yp + 1] = input[xp1 as usize] * window[wp2 as usize] + input[(xp2 + n2i) as usize] * window[wp1 as usize];
                yp += 2;
                xp1 += 2;
                xp2 -= 2;
                wp1 += 2;
                wp2 -= 2;
                i += 1;
            }
        }
        // Pre-rotation
        let mut f2 = vec![Cpx::default(); n4];
        for i in 0..n4 {
            let t0 = trig[2 * i];
            let t1 = trig[2 * i + 1];
            let re = f[2 * i];
            let im = f[2 * i + 1];
            let yr = re * t1 - im * t0;
            let yi = im * t1 + re * t0;
            f2[st.bitrev[i] as usize] = Cpx { r: yr * scale, i: yi * scale };
        }
        st.fft_impl(&self.twiddles, &mut f2);
        // Post-rotate
        let mut yp1 = out_off;
        let mut yp2 = out_off + stride * (n2 - 1);
        for i in 0..n4 {
            let t0 = trig[2 * i];
            let t1 = trig[2 * i + 1];
            let fp = f2[i];
            out[yp1] = fp.i * t0 - fp.r * t1;
            out[yp2] = fp.r * t0 + fp.i * t1;
            yp1 += 2 * stride;
            yp2 = yp2.wrapping_sub(2 * stride);
        }
    }
}
