//! Math helpers of the CELT encoder that core lacks (no libm): log2 and atan2 in double precision.
//! These only steer encoder decisions and the energy quantiser input, so a few ulps do not matter.

use super::mathops::{floor_f64, sqrt_f64, PI};

const INV_LN2: f64 = 1.442_695_040_888_963_387;

/// log2(x) for x > 0 (finite, normal or subnormal); -inf-like (-1000) for x <= 0.
pub fn log2_f64(x: f64) -> f64 {
    if !(x > 0.0) {
        return -1000.0;
    }
    if x == f64::INFINITY {
        return 1000.0;
    }
    let mut x = x;
    let mut e: i32 = 0;
    // scale subnormals into the normal range
    if x < 1.0e-300 {
        x *= 1.0e100;
        e -= 332; // log2(1e100) = 332.19; fixed up below
    }
    let bits = x.to_bits();
    let ex = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | (1023u64 << 52)); // [1,2)
    let mut ee = ex;
    if m > 1.414_213_562_373_095 {
        m *= 0.5;
        ee += 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0f64;
    let mut k = 1.0f64;
    let mut i = 0;
    while i < 12 {
        sum += term / k;
        term *= s2;
        k += 2.0;
        i += 1;
    }
    let ln_m = 2.0 * sum;
    let r = (ee as f64) + ln_m * INV_LN2;
    if e != 0 {
        // x was multiplied by 1e100 = 2^332.1928094887362
        r - 332.192_809_488_736_2
    } else {
        r
    }
}

/// `celt_log2` of the float build.
#[inline]
pub fn celt_log2(x: f32) -> f32 {
    log2_f64(x as f64) as f32
}

/// atan(t) for t >= 0.
fn atan_pos(t: f64) -> f64 {
    if t > 1.0 {
        return PI / 2.0 - atan_pos(1.0 / t);
    }
    // t in [0, 1]
    let (u, off) = if t > 0.414_213_562_373_095 { ((t - 1.0) / (t + 1.0), PI / 4.0) } else { (t, 0.0) };
    let u2 = u * u;
    let mut term = u;
    let mut sum = 0.0f64;
    let mut k = 1.0f64;
    let mut sign = 1.0f64;
    let mut i = 0;
    while i < 22 {
        sum += sign * term / k;
        term *= u2;
        k += 2.0;
        sign = -sign;
        i += 1;
    }
    off + sum
}

/// `celt_atan2p_norm`: atan2(y, x) / (pi/2) for y, x >= 0, in [0, 1].
pub fn atan2p_norm(y: f64, x: f64) -> f64 {
    if !(y > 0.0) && !(x > 0.0) {
        return 0.0;
    }
    if !(x > 0.0) {
        return 1.0;
    }
    if !(y > 0.0) {
        return 0.0;
    }
    atan_pos(y / x) * (2.0 / PI)
}

/// round-half-up to an integer, as `(int)floor(.5f + x)`.
#[inline]
pub fn round_half_up(x: f32) -> i32 {
    let f = floor_f64(0.5 + x as f64);
    if f > 2.0e9 {
        2_000_000_000
    } else if f < -2.0e9 {
        -2_000_000_000
    } else {
        f as i32
    }
}

/// sqrt of an f32 via the double-precision Newton routine.
#[inline]
pub fn sqrt32(x: f32) -> f32 {
    sqrt_f64(x as f64) as f32
}
