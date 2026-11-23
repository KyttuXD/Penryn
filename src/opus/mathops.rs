// Ported from libopus, BSD-3-Clause licence. Original notice:
// Copyright (c) 2002-2008 Jean-Marc Valin
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

//! The math helpers the CELT decoder needs: `core` has no `sqrt`, `exp`, `sin`, `cos` or `floor`
//! for floats, and the libopus float build calls them in double precision (`(float)sqrt(x)`,
//! `(float)exp(...)`, `cos(...)` for the window and twiddles). These are accurate to about 1e-16
//! relative in double precision, so the float results match libm's except in the very rare case of
//! a value within 1e-16 of a float rounding boundary.

pub const PI: f64 = 3.141_592_653_589_793;

/// floor() for the moderate magnitudes used here (|x| < 2^62).
pub fn floor_f64(x: f64) -> f64 {
    if !(x > -4.0e18 && x < 4.0e18) {
        return x;
    }
    let i = x as i64;
    let f = i as f64;
    if f > x {
        f - 1.0
    } else {
        f
    }
}

/// sqrt() in double precision by Newton's method. Negative and NaN inputs give 0 (libm gives NaN;
/// nothing in the decoder can rely on that), infinity gives infinity.
pub fn sqrt_f64(x: f64) -> f64 {
    if !(x > 0.0) {
        return 0.0;
    }
    if x == f64::INFINITY {
        return x;
    }
    // Scale to [1, 4) by an even power of two so that the initial guess is good everywhere
    // (this also handles subnormals).
    let mut m = x;
    let mut scale = 1.0f64;
    while m >= 4.0 {
        m *= 0.25;
        scale *= 2.0;
        if scale > 1.0e300 {
            break;
        }
    }
    while m < 1.0 {
        m *= 4.0;
        scale *= 0.5;
        if scale < 1.0e-300 {
            break;
        }
    }
    // m in [1, 4): a linear guess, then Newton iterations (quadratic convergence).
    let mut y = 0.5 + 0.5 * m;
    y = 0.5 * (y + m / y);
    y = 0.5 * (y + m / y);
    y = 0.5 * (y + m / y);
    y = 0.5 * (y + m / y);
    y = 0.5 * (y + m / y);
    y = 0.5 * (y + m / y);
    y * scale
}

/// `celt_sqrt` of the float build: `(float)sqrt(x)`.
pub fn celt_sqrt(x: f32) -> f32 {
    sqrt_f64(x as f64) as f32
}

/// `celt_rsqrt` of the float build: `1.f / celt_sqrt(x)`.
pub fn celt_rsqrt(x: f32) -> f32 {
    1.0 / celt_sqrt(x)
}

const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01;
const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10;
const INV_LN2: f64 = 1.442_695_040_888_963_387;

/// exp() in double precision: x = k ln2 + r, Taylor series for exp(r) on |r| <= 0.35.
pub fn exp_f64(x: f64) -> f64 {
    if !(x > -745.0) {
        return 0.0;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    let k = floor_f64(x * INV_LN2 + 0.5);
    let r = (x - k * LN2_HI) - k * LN2_LO;
    // exp(r) = sum r^n / n!, to n = 17 (|r| <= 0.3466: the remainder is below 1e-21)
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    let mut n = 1;
    while n <= 17 {
        term *= r / (n as f64);
        sum += term;
        n += 1;
    }
    // sum * 2^k, in two steps so that the exponent field stays in range
    let ki = k as i32;
    let k1 = ki / 2;
    let k2 = ki - k1;
    sum * pow2_i32(k1) * pow2_i32(k2)
}

/// 2^k as a double, for |k| <= 1022.
fn pow2_i32(k: i32) -> f64 {
    let k = if k > 1022 {
        1022
    } else if k < -1022 {
        -1022
    } else {
        k
    };
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `celt_exp2` of the float build: `(float)exp(0.6931471805599453094*(x))`.
pub fn celt_exp2(x: f32) -> f32 {
    exp_f64(0.693_147_180_559_945_309_4 * (x as f64)) as f32
}

const PIO2_1: f64 = 1.570_796_326_734_125_614_17e+00;
const PIO2_1T: f64 = 6.077_100_506_506_192_249_32e-11;

/// Reduces x to r in [-pi/4, pi/4] and the quadrant n (mod 4), x = n pi/2 + r.
fn reduce(x: f64) -> (f64, i32) {
    let n = floor_f64(x * (2.0 / PI) + 0.5);
    let r = (x - n * PIO2_1) - n * PIO2_1T;
    let q = (n as i64).rem_euclid(4) as i32;
    (r, q)
}

fn sin_poly(r: f64) -> f64 {
    let r2 = r * r;
    let mut term = r;
    let mut sum = r;
    let mut n = 1;
    while n <= 13 {
        term *= -r2 / (((2 * n) * (2 * n + 1)) as f64);
        sum += term;
        n += 1;
    }
    sum
}

fn cos_poly(r: f64) -> f64 {
    let r2 = r * r;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    let mut n = 1;
    while n <= 13 {
        term *= -r2 / (((2 * n - 1) * (2 * n)) as f64);
        sum += term;
        n += 1;
    }
    sum
}

/// sin() in double precision.
pub fn sin_f64(x: f64) -> f64 {
    let (r, q) = reduce(x);
    match q {
        0 => sin_poly(r),
        1 => cos_poly(r),
        2 => -sin_poly(r),
        _ => -cos_poly(r),
    }
}

/// cos() in double precision.
pub fn cos_f64(x: f64) -> f64 {
    let (r, q) = reduce(x);
    match q {
        0 => cos_poly(r),
        1 => -sin_poly(r),
        2 => -cos_poly(r),
        _ => sin_poly(r),
    }
}

/// `celt_cos_norm` of the float build: `(float)cos((.5f*PI)*(x))`.
pub fn celt_cos_norm(x: f32) -> f32 {
    cos_f64((0.5 * PI) * (x as f64)) as f32
}

/// `isqrt32`: floor(sqrt(val)) for val > 0, with exact integer arithmetic.
pub fn isqrt32(val: u32) -> u32 {
    if val == 0 {
        return 0;
    }
    let mut val = val;
    let mut g: u32 = 0;
    let mut bshift: i32 = (ilog32(val) - 1) >> 1;
    let mut b: u32 = 1u32 << bshift;
    loop {
        let t = ((g << 1).wrapping_add(b)) << bshift;
        if t <= val {
            g += b;
            val -= t;
        }
        b >>= 1;
        bshift -= 1;
        if bshift < 0 {
            break;
        }
    }
    g
}

/// `EC_ILOG`: the number of bits needed to represent v (0 for 0).
#[inline]
pub fn ilog32(v: u32) -> i32 {
    32 - v.leading_zeros() as i32
}

/// Absolute value of a float without `core` float methods.
#[inline]
pub fn absf(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7fff_ffff)
}

#[inline]
pub fn maxf(a: f32, b: f32) -> f32 {
    if a > b {
        a
    } else {
        b
    }
}

#[inline]
pub fn minf(a: f32, b: f32) -> f32 {
    if a < b {
        a
    } else {
        b
    }
}
