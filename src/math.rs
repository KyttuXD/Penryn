//! The float functions `core` lacks (no libm is linked).

pub const PI: f64 = core::f64::consts::PI;

pub fn floor(x: f64) -> f64 {
    if !(x.abs() < 4.5e15) {
        return x; // huge, infinite or NaN: already whole
    }
    let t = x as i64 as f64;
    if t > x {
        t - 1.0
    } else {
        t
    }
}

pub fn ifloor(x: f64) -> i32 {
    floor(x) as i32
}

pub fn sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    // A good first guess from the exponent, then Newton.
    let mut g = f64::from_bits((x.to_bits() >> 1) + (1023u64 << 51));
    for _ in 0..6 {
        g = 0.5 * (g + x / g);
    }
    g
}

/// sin for any angle (reduced to [-pi, pi], then a Taylor series to x^15).
pub fn sin(x: f64) -> f64 {
    let tau = 2.0 * PI;
    let mut a = x - tau * floor(x / tau + 0.5);
    // fold to [-pi/2, pi/2]
    if a > PI / 2.0 {
        a = PI - a;
    } else if a < -PI / 2.0 {
        a = -PI - a;
    }
    // The same series (to a^17), by Horner's rule: no division in it.
    let a2 = a * a;
    a * (1.0
        + a2 * (-1.0 / 6.0
            + a2 * (1.0 / 120.0
                + a2 * (-1.0 / 5040.0
                    + a2 * (1.0 / 362880.0
                        + a2 * (-1.0 / 39916800.0 + a2 * (1.0 / 6227020800.0 + a2 * (-1.0 / 1307674368000.0 + a2 * (1.0 / 355687428096000.0)))))))))
}

pub fn cos(x: f64) -> f64 {
    sin(x + PI / 2.0)
}

pub fn atan(x: f64) -> f64 {
    // Reduce to |x| <= 0.4 with atan(x) = 2 atan(x / (1 + sqrt(1 + x^2))), then a series.
    let mut y = x;
    let mut k = 1.0;
    while y.abs() > 0.4 {
        y = y / (1.0 + sqrt(1.0 + y * y));
        k *= 2.0;
    }
    let y2 = y * y;
    let mut term = y;
    let mut sum = y;
    for i in 1..12 {
        term *= -y2;
        sum += term / (2 * i + 1) as f64;
    }
    k * sum
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x > 0.0 {
        atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            atan(y / x) + PI
        } else {
            atan(y / x) - PI
        }
    } else if y > 0.0 {
        PI / 2.0
    } else if y < 0.0 {
        -PI / 2.0
    } else {
        0.0
    }
}

pub fn to_rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

/// x mod m, always in [0, m).
pub fn wrap(x: f64, m: f64) -> f64 {
    let r = x - m * floor(x / m);
    if r >= m { 0.0 } else { r }
}

/// `x.floor()` for f32, as std would have it.
pub trait FloorExt {
    fn floor(self) -> Self;
}

impl FloorExt for f32 {
    fn floor(self) -> f32 {
        floor(self as f64) as f32
    }
}

pub fn sqrtf(x: f32) -> f32 {
    sqrt(x as f64) as f32
}

pub fn atan2f(y: f32, x: f32) -> f32 {
    atan2(y as f64, x as f64) as f32
}
