// Copyright (c) 2006-2011, Skype Limited. All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// - Redistributions of source code must retain the above copyright notice,
// this list of conditions and the following disclaimer.
// - Redistributions in binary form must reproduce the above copyright
// notice, this list of conditions and the following disclaimer in the
// documentation and/or other materials provided with the distribution.
// - Neither the name of Internet Society, IETF or IETF Trust, nor the
// names of specific contributors, may be used to endorse or promote
// products derived from this software without specific prior written
// permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.
// Port of the integer macros and inline helpers of libopus silk/macros.h, SigProc_FIX.h, Inlines.h
// (silk_SMULWB, silk_SMLAWB, silk_RSHIFT_ROUND, silk_SAT16, silk_CLZ32, silk_lin2log, silk_log2lin,
// silk_DIV32_varQ, silk_INVERSE32_varQ, silk_SQRT_APPROX, silk_sum_sqr_shift, ...). Every helper has the
// exact 32-bit wrap-around / 64-bit intermediate semantics of the libopus build with OPUS_FAST_INT64.

#![allow(dead_code)]

#[inline(always)]
pub fn smulwb(a: i32, b: i32) -> i32 {
    ((a as i64 * (b as i16) as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(((b as i64 * (c as i16) as i64) >> 16) as i32)
}

#[inline(always)]
pub fn smulwt(a: i32, b: i32) -> i32 {
    ((a as i64 * (b >> 16) as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlawt(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(((b as i64 * (c >> 16) as i64) >> 16) as i32)
}

#[inline(always)]
pub fn smulbb(a: i32, b: i32) -> i32 {
    (a as i16 as i32).wrapping_mul(b as i16 as i32)
}

#[inline(always)]
pub fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add((b as i16 as i32).wrapping_mul(c as i16 as i32))
}

#[inline(always)]
pub fn smultt(a: i32, b: i32) -> i32 {
    (a >> 16).wrapping_mul(b >> 16)
}

#[inline(always)]
pub fn smulww(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(((b as i64 * c as i64) >> 16) as i32)
}

/// silk_SMMUL: (a * b) >> 32.
#[inline(always)]
pub fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}

#[inline(always)]
pub fn smull(a: i32, b: i32) -> i64 {
    a as i64 * b as i64
}

#[inline(always)]
pub fn add_sat32(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}

#[inline(always)]
pub fn sub_sat32(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}

#[inline(always)]
pub fn sat16(a: i32) -> i32 {
    if a > 32767 {
        32767
    } else if a < -32768 {
        -32768
    } else {
        a
    }
}

#[inline(always)]
pub fn add_sat16(a: i32, b: i32) -> i32 {
    sat16(a.wrapping_add(b))
}

/// silk_LSHIFT / silk_LSHIFT32 (wraps, shift in 0..32).
#[inline(always)]
pub fn lshift(a: i32, s: u32) -> i32 {
    ((a as u32).wrapping_shl(s)) as i32
}

/// silk_LSHIFT_SAT32.
#[inline(always)]
pub fn lshift_sat32(a: i32, s: u32) -> i32 {
    let lo = i32::MIN >> s;
    let hi = i32::MAX >> s;
    let v = if a > hi {
        hi
    } else if a < lo {
        lo
    } else {
        a
    };
    lshift(v, s)
}

/// silk_RSHIFT_ROUND for 32-bit values (shift >= 1).
#[inline(always)]
pub fn rshift_round(a: i32, s: u32) -> i32 {
    if s == 1 {
        (a >> 1).wrapping_add(a & 1)
    } else {
        ((a >> (s - 1)).wrapping_add(1)) >> 1
    }
}

/// silk_RSHIFT_ROUND64 (shift >= 1).
#[inline(always)]
pub fn rshift_round64(a: i64, s: u32) -> i64 {
    if s == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (s - 1)) + 1) >> 1
    }
}

#[inline(always)]
pub fn clz32(a: i32) -> i32 {
    (a as u32).leading_zeros() as i32
}

/// silk_RAND.
#[inline(always)]
pub fn rand(seed: i32) -> i32 {
    907633515i32.wrapping_add(seed.wrapping_mul(196314165))
}

/// silk_ROR32.
#[inline(always)]
pub fn ror32(a: i32, rot: i32) -> i32 {
    let x = a as u32;
    if rot == 0 {
        a
    } else if rot < 0 {
        let m = (-rot) as u32;
        ((x << m) | (x >> (32 - m))) as i32
    } else {
        let m = rot as u32;
        ((x << (32 - m)) | (x >> m)) as i32
    }
}

/// silk_CLZ_FRAC: (leading zeros, the 7 bits right after the leading one).
#[inline]
pub fn clz_frac(x: i32) -> (i32, i32) {
    let lz = clz32(x);
    (lz, ror32(x, 24 - lz) & 0x7f)
}

/// silk_SQRT_APPROX.
pub fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let (lz, frac_q7) = clz_frac(x);
    let mut y: i32 = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, smulbb(213, frac_q7))
}

/// silk_lin2log: approximation of 128 * log2().
pub fn lin2log(x: i32) -> i32 {
    let (lz, frac_q7) = clz_frac(x);
    smlawb(frac_q7, frac_q7.wrapping_mul(128 - frac_q7), 179).wrapping_add(lshift(31 - lz, 7))
}

/// silk_log2lin: approximation of 2^().
pub fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    } else if in_log_q7 >= 3967 {
        return i32::MAX;
    }
    let mut out = lshift(1, (in_log_q7 >> 7) as u32);
    let frac_q7 = in_log_q7 & 0x7F;
    if in_log_q7 < 2048 {
        out = out.wrapping_add(
            out.wrapping_mul(smlawb(frac_q7, smulbb(frac_q7, 128 - frac_q7), -174)) >> 7,
        );
    } else {
        out = out.wrapping_add((out >> 7).wrapping_mul(smlawb(frac_q7, smulbb(frac_q7, 128 - frac_q7), -174)));
    }
    out
}

/// silk_DIV32_varQ: a good approximation of (a32 << Qres) / b32.
pub fn div32_var_q(a32: i32, b32: i32, qres: i32) -> i32 {
    if b32 == 0 {
        return 0;
    }
    let a_headrm = clz32(a32.wrapping_abs()) - 1;
    let mut a32_nrm = lshift(a32, core::cmp::max(a_headrm, 0) as u32);
    let b_headrm = clz32(b32.wrapping_abs()) - 1;
    let b32_nrm = lshift(b32, core::cmp::max(b_headrm, 0) as u32);
    let d = b32_nrm >> 16;
    if d == 0 {
        return 0;
    }
    let b32_inv = (i32::MAX >> 2) / d;
    let mut result = smulwb(a32_nrm, b32_inv);
    a32_nrm = a32_nrm.wrapping_sub(lshift(smmul(b32_nrm, result), 3));
    result = smlawb(result, a32_nrm, b32_inv);
    let lsh = 29 + a_headrm - b_headrm - qres;
    if lsh < 0 {
        lshift_sat32(result, (-lsh) as u32)
    } else if lsh < 32 {
        result >> lsh
    } else {
        0
    }
}

/// silk_INVERSE32_varQ: a good approximation of (1 << Qres) / b32.
pub fn inverse32_var_q(b32: i32, qres: i32) -> i32 {
    if b32 == 0 {
        return 0;
    }
    let b_headrm = clz32(b32.wrapping_abs()) - 1;
    let b32_nrm = lshift(b32, core::cmp::max(b_headrm, 0) as u32);
    let d = b32_nrm >> 16;
    if d == 0 {
        return 0;
    }
    let b32_inv = (i32::MAX >> 2) / d;
    let mut result = lshift(b32_inv, 16);
    let err_q32 = lshift((1i32 << 29).wrapping_sub(smulwb(b32_nrm, b32_inv)), 3);
    result = smlaww(result, err_q32, b32_inv);
    let lsh = 61 - b_headrm - qres;
    if lsh <= 0 {
        lshift_sat32(result, (-lsh) as u32)
    } else if lsh < 32 {
        result >> lsh
    } else {
        0
    }
}

/// silk_sum_sqr_shift: (energy, shift) of the sum of squares of `x`, shifted right so it fits in an i32.
pub fn sum_sqr_shift(x: &[i16]) -> (i32, i32) {
    if x.is_empty() {
        return (0, 0);
    }
    let len = x.len() as i32;
    let mut shft = 31 - clz32(len);
    let pass = |shft: i32| -> i32 {
        // nrg starts at `len` in the first pass (conservative rounding) and at 0 in the second.
        let mut nrg: u32 = 0;
        let mut i = 0;
        while i + 1 < x.len() {
            let a = x[i] as i32;
            let b = x[i + 1] as i32;
            let tmp = (a.wrapping_mul(a) as u32).wrapping_add(b.wrapping_mul(b) as u32);
            nrg = nrg.wrapping_add(tmp >> shft);
            i += 2;
        }
        if i < x.len() {
            let a = x[i] as i32;
            nrg = nrg.wrapping_add((a.wrapping_mul(a) as u32) >> shft);
        }
        nrg as i32
    };
    let nrg = (len as u32).wrapping_add(pass(shft) as u32) as i32;
    shft = core::cmp::max(0, shft + 3 - clz32(nrg));
    (pass(shft), shft)
}
