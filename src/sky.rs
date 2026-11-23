//! The overworld sky (LevelRenderer / SkyRenderer): stars, sun and moon on the celestial
//! angle of the day clock, and the fancy clouds of clouds.png (12 x 4 x 12 boxes at y 192.33).

use crate::gui::Gui;
use crate::math;
use crate::sdl::*;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// java.util.Random, for vanilla's star field (seed 10842).
struct JRand(u64);
impl JRand {
    fn new(seed: u64) -> JRand {
        JRand((seed ^ 0x5DEECE66D) & ((1 << 48) - 1))
    }
    fn next(&mut self, bits: u32) -> u32 {
        self.0 = (self.0.wrapping_mul(0x5DEECE66D).wrapping_add(0xB)) & ((1 << 48) - 1);
        (self.0 >> (48 - bits)) as u32
    }
    fn float(&mut self) -> f32 {
        self.next(24) as f32 / (1u32 << 24) as f32
    }
}

/// The clouds' mesh, kept on the card and made again only when the cloud grid moves a cell,
/// the camera moves half a block against it (the fade at the edge of the range), or the
/// colour or range changes.
struct CloudMesh {
    vbo: u32,
    count: i32,
    key: (i64, i64, i64, [u8; 3], u64),
    at: [f64; 2],
}

pub struct Sky {
    stars: Vec<[f32; 3]>,
    /// The stars on the card (built once; turned with the sky by the matrix).
    star_vbo: Cell<u32>,
    clouds: RefCell<CloudMesh>,
    /// The rain and thunder levels (src/weather.rs): the sun, moon and stars fade behind the
    /// rain, clouds grey.
    pub rain: f32,
    pub thunder: f32,
}

/// DimensionType.timeOfDay: the sun's angle as a fraction of a turn (0: noon... as vanilla).
pub fn celestial_angle(day_time: i64) -> f64 {
    let f = math::wrap(day_time as f64 / 24000.0 - 0.25, 1.0);
    let g = 0.5 - math::cos(f * math::PI) / 2.0;
    (f * 2.0 + g) / 3.0
}

/// The sky's turn (Axis.YP(-90) * Axis.XP(angle), at half size) of a point, `s` and `c` the
/// sine and cosine of the angle.
fn turn_point(s: f64, c: f64, p: [f64; 3]) -> [f32; 3] {
    let (x, y, z) = (p[0], p[1], p[2]);
    let (y2, z2) = (y * c - z * s, y * s + z * c);
    [(-z2) as f32 * 0.5, y2 as f32 * 0.5, x as f32 * 0.5]
}

/// The same turn as a column-major matrix.
fn turn_matrix(s: f64, c: f64) -> [f64; 16] {
    [0.0, 0.0, 0.5, 0.0, -s * 0.5, c * 0.5, 0.0, 0.0, -c * 0.5, -s * 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// Checks for `penryn selftest`: the star field turned by the matrix lands where the
/// per-star turn put it.
pub fn selftest() -> Vec<(&'static str, alloc::string::String, &'static str)> {
    let a = 1.234f64;
    let (s, c) = (math::sin(a), math::cos(a));
    let m = turn_matrix(s, c);
    let mut worst = 0.0f64;
    for p in [[30.0, -70.0, 12.5], [-99.0, 3.0, 8.0], [0.5, 0.25, -100.0]] {
        let q = turn_point(s, c, p);
        for row in 0..3 {
            let v = m[row] * p[0] + m[4 + row] * p[1] + m[8 + row] * p[2];
            worst = worst.max((v - q[row] as f64).abs());
        }
    }
    alloc::vec![("sky star turn matrix", alloc::format!("{}", worst < 1e-4), "true")]
}

impl Sky {
    pub fn new() -> Sky {
        // SkyRenderer.buildStars: 1500 tries, seed 10842, quads on a sphere of radius 100.
        let mut r = JRand::new(10842);
        let mut stars = Vec::new();
        for _ in 0..1500 {
            let x = (r.float() * 2.0 - 1.0) as f64;
            let y = (r.float() * 2.0 - 1.0) as f64;
            let z = (r.float() * 2.0 - 1.0) as f64;
            let size = 0.15 + r.float() as f64 * 0.1;
            let d = x * x + y * y + z * z;
            if d <= 0.010000001 || d >= 1.0 {
                continue;
            }
            let n = math::sqrt(d);
            let (x, y, z) = (x / n * 100.0, y / n * 100.0, z / n * 100.0);
            let yaw = math::atan2(x, z);
            let (ys, yc) = (math::sin(yaw), math::cos(yaw));
            let pitch = math::atan2(math::sqrt(x * x + z * z), y);
            let (ps, pc) = (math::sin(pitch), math::cos(pitch));
            let roll = r.float() as f64 * math::PI * 2.0;
            let (rs, rc) = (math::sin(roll), math::cos(roll));
            for k in 0..4 {
                let a = (((k & 2) as i32 - 1) as f64) * size;
                let b = ((((k + 1) & 2) as i32 - 1) as f64) * size;
                let ra = a * rc - b * rs;
                let rb = b * rc + a * rs;
                let py = ra * ps;
                let pz = -ra * pc;
                stars.push([(x + pz * ys - rb * yc) as f32, (y + py) as f32, (z + rb * ys + pz * yc) as f32]);
            }
        }
        Sky {
            stars,
            star_vbo: Cell::new(0),
            clouds: RefCell::new(CloudMesh { vbo: 0, count: 0, key: (0, 0, -1, [0; 3], 0), at: [0.0; 2] }),
            rain: 0.0,
            thunder: 0.0,
        }
    }

    /// Stars, sun and moon around the eye (`view`: rotation only), behind everything.
    pub fn draw_celestial(&self, gl: &Gl, view: &[f32; 16], gui: &Gui, day_time: i64, daylight: f32) {
        let angle = celestial_angle(day_time) * 2.0 * math::PI;
        let (s, c) = (math::sin(angle), math::cos(angle));
        // Axis.YP(-90) * Axis.XP(angle): first about x (y and z mix), then y -90 (x <- -z, z <- x).
        let rot = |p: [f64; 3]| -> [f32; 3] { turn_point(s, c, p) };
        let mut v: Vec<V> = Vec::new();
        let mut quad = |v: &mut Vec<V>, size: f64, h: f64, uv: [f32; 4], col: [u8; 4]| {
            let corners = [(-size, -size, uv[0], uv[1]), (size, -size, uv[2], uv[1]), (size, size, uv[2], uv[3]), (-size, size, uv[0], uv[3])];
            for (a, b, u, w) in corners {
                v.push(V { p: rot([a, h, b]), uv: [u, w], c: col });
            }
        };
        let sun = gui.sprite_uv("sky/sun");
        let phase_names = [
            "sky/moon/full_moon",
            "sky/moon/waning_gibbous",
            "sky/moon/third_quarter",
            "sky/moon/waning_crescent",
            "sky/moon/new_moon",
            "sky/moon/waxing_crescent",
            "sky/moon/first_quarter",
            "sky/moon/waxing_gibbous",
        ];
        let moon = gui.sprite_uv(phase_names[(day_time.div_euclid(24000).rem_euclid(8)) as usize]);
        // SkyRenderState.rainBrightness fades the sun and moon; the weather blends the stars'
        // brightness to 0 (STAR_BRIGHTNESS, not times rainBrightness).
        let rain_b = (1.0 - self.rain).clamp(0.0, 1.0);
        let sm = [255, 255, 255, (rain_b * 255.0) as u8];
        if let Some(uv) = sun {
            quad(&mut v, 30.0, 100.0, uv, sm);
        }
        if let Some(uv) = moon {
            quad(&mut v, 20.0, -100.0, [uv[2], uv[3], uv[0], uv[1]], sm);
        }
        let star_b = ((1.0 - daylight) * 0.5).clamp(0.0, 1.0) * (1.0 - (self.rain - self.thunder)) * (1.0 - self.thunder);
        unsafe {
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(view.as_ptr());
            (gl.Disable)(FOG);
            (gl.Disable)(CULL_FACE);
            (gl.DepthMask)(0);
            (gl.Disable)(DEPTH_TEST);
            (gl.Enable)(BLEND);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
        }
        if star_b > 0.0 {
            // The star field sits on the card; the sky's turn (rot above) is in the matrix.
            if self.star_vbo.get() == 0 {
                let mut id = 0;
                unsafe {
                    (gl.GenBuffers)(1, &mut id);
                    (gl.BindBuffer)(ARRAY_BUFFER, id);
                    (gl.BufferData)(ARRAY_BUFFER, (self.stars.len() * 12) as isize, self.stars.as_ptr() as *const c_void, STATIC_DRAW);
                }
                self.star_vbo.set(id);
            }
            let r = turn_matrix(s, c);
            let mut m = [0f32; 16];
            for col in 0..4 {
                for row in 0..4 {
                    let mut acc = 0.0;
                    for k in 0..4 {
                        acc += view[k * 4 + row] as f64 * r[col * 4 + k];
                    }
                    m[col * 4 + row] = acc as f32;
                }
            }
            unsafe {
                (gl.LoadMatrixf)(m.as_ptr());
                (gl.BlendFunc)(SRC_ALPHA, ONE);
                (gl.Disable)(TEXTURE_2D);
                (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
                (gl.DisableClientState)(COLOR_ARRAY);
                (gl.Color4ub)(255, 255, 255, (star_b * 255.0) as u8);
                (gl.BindBuffer)(ARRAY_BUFFER, self.star_vbo.get());
                (gl.VertexPointer)(3, FLOAT, 12, core::ptr::null());
                (gl.DrawArrays)(QUADS, 0, self.stars.len() as i32);
                (gl.BindBuffer)(ARRAY_BUFFER, 0);
                (gl.Color4ub)(255, 255, 255, 255);
                (gl.EnableClientState)(COLOR_ARRAY);
                (gl.LoadMatrixf)(view.as_ptr());
            }
        }
        if !v.is_empty() {
            unsafe {
                // Additive, as vanilla blends the sun and moon into the sky.
                (gl.BlendFunc)(SRC_ALPHA, ONE);
                (gl.Enable)(TEXTURE_2D);
                (gl.BindTexture)(TEXTURE_2D, gui.texture());
                (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
                (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
                (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, v.len() as i32);
                (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
                (gl.Disable)(TEXTURE_2D);
            }
        }
        unsafe {
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.Disable)(BLEND);
            (gl.Enable)(DEPTH_TEST);
            (gl.DepthMask)(1);
            (gl.Enable)(CULL_FACE);
            (gl.Enable)(FOG);
        }
    }

    /// The fancy clouds: each clouds.png pixel a 12 x 4 x 12 box at y 192.33, drifting with
    /// the game time, shaded per face and fogged, within `radius` blocks.
    pub fn draw_clouds(&self, gl: &Gl, view: &[f32; 16], gui: &Gui, eye: [f64; 3], game_ticks: f64, daylight: f32, radius: f64) {
        let side = gui.clouds_side;
        if side == 0 {
            return;
        }
        let cloud = |x: i64, z: i64| gui.clouds[(z.rem_euclid(side as i64) as usize) * side + x.rem_euclid(side as i64) as usize];
        let drift = game_ticks * 0.03;
        let (cx0, cz0) = (eye[0] + drift, eye[2] + 3.96);
        let cell = 12.0;
        let (ox, oz) = (math::floor(cx0 / cell) as i64, math::floor(cz0 / cell) as i64);
        let n = (radius / cell) as i64 + 1;
        let y0 = 192.33 - eye[1];
        let base = [(255.0 * (0.1 + 0.9 * daylight)) as u8; 3];
        // WeatherAttributes: the clouds blend to grey under rain, more under thunder.
        let base = {
            let mut c = [base[0] as f32, base[1] as f32, base[2] as f32];
            let grey = |c: [f32; 3], b: f32, f: f32| {
                let g = (c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11) * b;
                [c[0] + (g - c[0]) * f, c[1] + (g - c[1]) * f, c[2] + (g - c[2]) * f]
            };
            let mix = |t: f32, a: [f32; 3], b: [f32; 3]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
            let only_rain = self.rain - self.thunder;
            if only_rain > 0.0 {
                c = mix(only_rain, c, grey(c, 0.24, 0.5));
            }
            if self.thunder > 0.0 {
                c = mix(self.thunder, c, grey(c, 0.095, 0.94));
            }
            [c[0] as u8, c[1] as u8, c[2] as u8]
        };
        // Clouds fade out towards the edge of their range (not with the terrain's fog).
        let shade = |k: f32, d: f32| -> [u8; 4] {
            let fade = (1.0 - (d / radius as f32 - 0.6) / 0.4).clamp(0.0, 1.0);
            [(base[0] as f32 * k) as u8, (base[1] as f32 * k) as u8, (base[2] as f32 * k) as u8, (204.0 * fade) as u8]
        };
        // The mesh is kept relative to the corner of cell (ox, oz) at the clouds' height; the
        // matrix moves it by the rest.
        let key = (ox, oz, n, base, radius.to_bits());
        let mut mesh = self.clouds.borrow_mut();
        let moved = (cx0 - mesh.at[0]).abs() > 3.0 || (cz0 - mesh.at[1]).abs() > 3.0;
        if mesh.key != key || moved || mesh.vbo == 0 {
            mesh.key = key;
            mesh.at = [cx0, cz0];
            let mut v: Vec<V> = Vec::new();
            for dz in -n..=n {
                for dx in -n..=n {
                    let (gx, gz) = (ox + dx, oz + dz);
                    if !cloud(gx, gz) {
                        continue;
                    }
                    let (x0, z0) = ((dx as f64 * cell) as f32, (dz as f64 * cell) as f32);
                    let (x1, z1) = (x0 + cell as f32, z0 + cell as f32);
                    let (y0f, y1f) = (0.0f32, 4.0f32);
                    // The distance from the camera, as it was when the mesh was made.
                    let (ex, ez) = ((gx as f64 * cell - cx0) as f32, (gz as f64 * cell - cz0) as f32);
                    let dist = math::sqrt(((ex + 6.0) * (ex + 6.0) + (ez + 6.0) * (ez + 6.0)) as f64) as f32;
                    if dist > radius as f32 {
                        continue;
                    }
                    let mut face = |pts: [[f32; 3]; 4], k: f32| {
                        let c = shade(k, dist);
                        for p in pts {
                            v.push(V { p, uv: [0.0; 2], c });
                        }
                    };
                    face([[x0, y1f, z0], [x0, y1f, z1], [x1, y1f, z1], [x1, y1f, z0]], 1.0);
                    face([[x0, y0f, z0], [x1, y0f, z0], [x1, y0f, z1], [x0, y0f, z1]], 0.7);
                    if !cloud(gx - 1, gz) {
                        face([[x0, y0f, z0], [x0, y0f, z1], [x0, y1f, z1], [x0, y1f, z0]], 0.9);
                    }
                    if !cloud(gx + 1, gz) {
                        face([[x1, y0f, z1], [x1, y0f, z0], [x1, y1f, z0], [x1, y1f, z1]], 0.9);
                    }
                    if !cloud(gx, gz - 1) {
                        face([[x1, y0f, z0], [x0, y0f, z0], [x0, y1f, z0], [x1, y1f, z0]], 0.8);
                    }
                    if !cloud(gx, gz + 1) {
                        face([[x0, y0f, z1], [x1, y0f, z1], [x1, y1f, z1], [x0, y1f, z1]], 0.8);
                    }
                }
            }
            unsafe {
                if mesh.vbo == 0 {
                    (gl.GenBuffers)(1, &mut mesh.vbo);
                }
                (gl.BindBuffer)(ARRAY_BUFFER, mesh.vbo);
                (gl.BufferData)(ARRAY_BUFFER, (v.len() * 24) as isize, v.as_ptr() as *const c_void, STATIC_DRAW);
                (gl.BindBuffer)(ARRAY_BUFFER, 0);
            }
            mesh.count = v.len() as i32;
        }
        if mesh.count == 0 {
            return;
        }
        let t = [(ox as f64 * cell - cx0) as f32, y0 as f32, (oz as f64 * cell - cz0) as f32];
        let mut m = *view;
        for row in 0..3 {
            m[12 + row] = view[row] * t[0] + view[4 + row] * t[1] + view[8 + row] * t[2] + view[12 + row];
        }
        unsafe {
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(m.as_ptr());
            // Back faces only matter from inside the layer (the boxes span 4 blocks of height).
            if y0 < 0.0 && y0 > -4.0 {
                (gl.Disable)(CULL_FACE);
            } else {
                (gl.Enable)(CULL_FACE);
                (gl.CullFace)(BACK);
            }
            (gl.Disable)(TEXTURE_2D);
            (gl.Disable)(FOG);
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.BindBuffer)(ARRAY_BUFFER, mesh.vbo);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
            (gl.VertexPointer)(3, FLOAT, 24, core::ptr::null());
            (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, 20 as *const c_void);
            (gl.DrawArrays)(QUADS, 0, mesh.count);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.Disable)(BLEND);
            (gl.Enable)(CULL_FACE);
            (gl.Enable)(FOG);
        }
    }
}
