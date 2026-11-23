//! The light map (Lightmap / LightmapRenderStateExtractor / lightmap.fsh of 26.x): a 16 x 16
//! texture, x the block light level and y the sky light level, whose texel is the colour a
//! surface with that light is multiplied by. Vanilla draws it on the GPU in a shader; Penryn
//! works out the same 256 texels on the CPU (a pure function of `Input`), uploads them to
//! texture unit 1 when they change, and keeps a copy for the entities and particles, which
//! look their light up at their position.
//!
//! The environment (sky light factor and colour, ambient colour) is vanilla's: the dimension
//! type's attributes, the day timeline's tracks for the overworld, and the weather's layers.

use crate::math;
use crate::sdl::*;

/// The inputs of the light map: what the shader's uniform block holds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Input {
    pub sky_factor: f32,
    pub block_factor: f32,
    pub night_vision_factor: f32,
    pub darkness_scale: f32,
    pub boss_darkening: f32,
    pub brightness: f32,
    pub block_tint: [f32; 3],
    pub sky_color: [f32; 3],
    pub ambient: [f32; 3],
    pub night_vision_color: [f32; 3],
}

/// The default block light tint (EnvironmentAttributes.BLOCK_LIGHT_TINT: 0xFFD88C).
pub const BLOCK_TINT: [f32; 3] = [255.0 / 255.0, 216.0 / 255.0, 140.0 / 255.0];
/// The default night vision colour (0x999999).
pub const NIGHT_VISION_COLOR: [f32; 3] = [0.6, 0.6, 0.6];
/// Timelines.NIGHT_SKY_LIGHT_COLOR (0x7a7aff).
const NIGHT_SKY: [f32; 3] = [122.0 / 255.0, 122.0 / 255.0, 1.0];

/// Which dimension's attributes apply.
pub const OVERWORLD: u8 = 0;
pub const NETHER: u8 = 1;
pub const END: u8 = 2;

fn lerp(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

fn lerp3(t: f32, a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [lerp(t, a[0], b[0]), lerp(t, a[1], b[1]), lerp(t, a[2], b[2])]
}

fn from_rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 255) as f32 / 255.0, (c >> 8 & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

/// A keyframe track over the 24000-tick day: the segment `t` is in (wrapping from the last
/// key round to the first of the next day) and how far along it: (from, to, fraction).
fn segment(t: f32, ticks: &[f32]) -> (usize, usize, f32) {
    let n = ticks.len();
    let last = ticks[n - 1];
    if t >= last {
        let span = ticks[0] + 24000.0 - last;
        return (n - 1, 0, (t - last) / span);
    }
    if t < ticks[0] {
        let span = ticks[0] + 24000.0 - last;
        return (n - 1, 0, (t + 24000.0 - last) / span);
    }
    let mut i = 0;
    while ticks[i + 1] <= t {
        i += 1;
    }
    (i, i + 1, (t - ticks[i]) / (ticks[i + 1] - ticks[i]))
}

/// The day timeline's `visual/sky_light_factor` and `visual/sky_light_color` tracks at `t`
/// ticks into the day (data/minecraft/timeline/day.json).
fn day_track(t: f32) -> (f32, [f32; 3]) {
    const TICKS: [f32; 4] = [730.0, 11270.0, 13140.0, 22860.0];
    const FACTOR: [f32; 4] = [1.0, 1.0, 0.24, 0.24];
    let colour = [[1.0; 3], [1.0; 3], NIGHT_SKY, NIGHT_SKY];
    let (a, b, f) = segment(t, &TICKS);
    (lerp(f, FACTOR[a], FACTOR[b]), lerp3(f, colour[a], colour[b]))
}

/// The sky light factor and colour and the ambient colour of a dimension at `day_time` under
/// the weather (WeatherAttributes: the rain, then the thunder, each an alpha blend towards
/// the dimmer values, weighted by its level).
pub fn environment(dimension: u8, day_time: f64, rain: f32, thunder: f32) -> (f32, [f32; 3], [f32; 3]) {
    let (mut factor, mut colour, ambient) = match dimension {
        NETHER => (0.0, NIGHT_SKY, from_rgb(0x302821)),
        END => (0.0, from_rgb(0xac60cd), from_rgb(0x3f473f)),
        _ => {
            let t = math::wrap(day_time, 24000.0) as f32;
            let (f, c) = day_track(t);
            (f, c, from_rgb(0x0a0a0a))
        }
    };
    let only_rain = rain - thunder;
    if only_rain > 0.0 {
        let a = 79.0 / 255.0; // ARGB.color(0.3125, ...) floors the alpha to 8 bits
        factor = lerp(only_rain, factor, lerp(0.3125, factor, 0.24));
        colour = lerp3(only_rain, colour, lerp3(a, colour, NIGHT_SKY));
    }
    if thunder > 0.0 {
        let a = 134.0 / 255.0; // 0.52734375
        factor = lerp(thunder, factor, lerp(0.52734375, factor, 0.24));
        colour = lerp3(thunder, colour, lerp3(a, colour, NIGHT_SKY));
    }
    (factor, colour, ambient)
}

/// The raw game values the inputs come from.
pub struct Params {
    pub dimension: u8,
    /// The dimension's day clock, in ticks (partial tick included).
    pub day_time: f64,
    pub rain: f32,
    pub thunder: f32,
    /// The block light's flicker (`Lightmap::flicker`).
    pub flicker: f32,
    /// The brightness option (0 to 1).
    pub gamma: f32,
    /// The darkness effect's blend (0 to 1, with partial tick) and the option's scale.
    pub darkness_blend: f32,
    pub darkness_option: f32,
    /// The player's tick count minus the partial tick (for the darkness' pulse).
    pub tick: f64,
    /// 0 to 1: GameRenderer.nightVisionScale when the effect is on.
    pub night_vision: f32,
    /// GameRenderer.bossOverlayWorldDarkening.
    pub boss: f32,
}

impl Params {
    pub fn input(&self) -> Input {
        let (sky_factor, sky_color, ambient) = environment(self.dimension, self.day_time, self.rain, self.thunder);
        let blend = self.darkness_blend * self.darkness_option;
        // calculateDarknessScale(blend) * option
        let pulse = math::cos(self.tick * core::f64::consts::PI * 0.025) as f32 * (0.45 * blend);
        Input {
            sky_factor,
            block_factor: self.flicker + 1.4,
            night_vision_factor: self.night_vision,
            darkness_scale: pulse.max(0.0) * self.darkness_option,
            boss_darkening: self.boss,
            brightness: (self.gamma - blend).max(0.0),
            block_tint: BLOCK_TINT,
            sky_color,
            ambient,
            night_vision_color: NIGHT_VISION_COLOR,
        }
    }
}

fn brightness(level: f32) -> f32 {
    level / (4.0 - 3.0 * level)
}

/// lightmap.fsh for the 256 texels: index `sky * 16 + block`.
pub fn compute(i: &Input, out: &mut [[f32; 3]; 256]) {
    for sky in 0..16 {
        for block in 0..16 {
            let block_level = block as f32 / 15.0;
            let sky_level = sky as f32 / 15.0;
            let block_b = brightness(block_level) * i.block_factor;
            let sky_b = brightness(sky_level) * i.sky_factor;
            let mut c = [0f32; 3];
            for k in 0..3 {
                c[k] = i.ambient[k].max(i.night_vision_color[k] * i.night_vision_factor);
                c[k] += i.sky_color[k] * sky_b;
            }
            let m = (2.0 * block_level - 1.0) * (2.0 * block_level - 1.0);
            let tint = lerp3(0.9 * m, i.block_tint, [1.0; 3]);
            for k in 0..3 {
                c[k] += tint[k] * block_b;
                // The boss bar's darkening, then the darkness effect.
                c[k] = lerp(i.boss_darkening, c[k], c[k] * [0.7, 0.6, 0.6][k]);
                c[k] = (c[k] - i.darkness_scale).clamp(0.0, 1.0);
            }
            // notGamma, mixed in by the brightness.
            let mx = c[0].max(c[1]).max(c[2]);
            let inv = 1.0 - mx;
            let scaled = 1.0 - inv * inv * inv * inv;
            let k = if mx > 0.0 { scaled / mx } else { 0.0 };
            for j in 0..3 {
                c[j] = lerp(i.brightness, c[j], c[j] * k);
            }
            out[sky * 16 + block] = c;
        }
    }
}

/// The light map on the card, its CPU copy, and the block light's flicker.
pub struct Lightmap {
    pub tex: u32,
    /// The colours, index `sky * 16 + block`.
    pub cpu: [[f32; 3]; 256],
    input: Option<Input>,
    flicker: f32,
    rng: u32,
}

impl Lightmap {
    pub fn new() -> Lightmap {
        let mut l = Lightmap { tex: 0, cpu: [[1.0; 3]; 256], input: None, flicker: 0.0, rng: 0x2545_F491 };
        // Before the first update: full brightness.
        compute(&Params { dimension: OVERWORLD, day_time: 6000.0, rain: 0.0, thunder: 0.0, flicker: 0.0, gamma: 0.5, darkness_blend: 0.0, darkness_option: 1.0, tick: 0.0, night_vision: 0.0, boss: 0.0 }.input(), &mut l.cpu);
        l
    }

    fn next_float(&mut self) -> f32 {
        // xorshift32: the flicker is random noise, not vanilla's seeded sequence.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    /// LightmapRenderStateExtractor.tick: the flicker random-walks and decays.
    pub fn tick(&mut self) {
        let (a, b, c, d) = (self.next_float(), self.next_float(), self.next_float(), self.next_float());
        self.flicker += (a - b) * c * d * 0.1;
        self.flicker *= 0.9;
    }

    pub fn flicker(&self) -> f32 {
        self.flicker
    }

    /// Works the texels out again when the inputs changed, and uploads them.
    pub fn update(&mut self, gl: &Gl, i: &Input) {
        if self.tex != 0 && self.input.as_ref() == Some(i) {
            return;
        }
        self.input = Some(*i);
        compute(i, &mut self.cpu);
        let mut px = [0u8; 256 * 4];
        for (n, c) in self.cpu.iter().enumerate() {
            for k in 0..3 {
                px[n * 4 + k] = (c[k] * 255.0 + 0.5) as u8;
            }
            px[n * 4 + 3] = 255;
        }
        unsafe {
            (gl.ActiveTexture)(TEXTURE1);
            if self.tex == 0 {
                (gl.GenTextures)(1, &mut self.tex);
                (gl.BindTexture)(TEXTURE_2D, self.tex);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, LINEAR);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
            } else {
                (gl.BindTexture)(TEXTURE_2D, self.tex);
            }
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, 16, 16, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const core::ffi::c_void);
            (gl.ActiveTexture)(TEXTURE0);
        }
    }

    /// Turns texture unit 1 on for the terrain: the light map, multiplied into unit 0's colour,
    /// with the texture matrix that maps a vertex's light (1/16 steps of a level, 0 to 240) to
    /// the texel's centre, (value + 8) / 256.
    pub fn enable(&self, gl: &Gl) {
        let m: [f32; 16] = [1.0 / 256.0, 0.0, 0.0, 0.0, 0.0, 1.0 / 256.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 8.0 / 256.0, 8.0 / 256.0, 0.0, 1.0];
        unsafe {
            (gl.ActiveTexture)(TEXTURE1);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.tex);
            (gl.TexEnvi)(TEXTURE_ENV, TEXTURE_ENV_MODE, MODULATE);
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadMatrixf)(m.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.ActiveTexture)(TEXTURE0);
            (gl.ClientActiveTexture)(TEXTURE1);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.ClientActiveTexture)(TEXTURE0);
        }
    }

    /// Turns unit 1 off again, so that everything else is drawn as before.
    pub fn disable(&self, gl: &Gl) {
        unsafe {
            (gl.ClientActiveTexture)(TEXTURE1);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
            (gl.ClientActiveTexture)(TEXTURE0);
            (gl.ActiveTexture)(TEXTURE1);
            (gl.Disable)(TEXTURE_2D);
            (gl.ActiveTexture)(TEXTURE0);
        }
    }
}

/// Checks for `penryn selftest`: the arithmetic against values worked out by hand.
/// How things (entities, items, particles, the hand) are lit: the world's light at a place,
/// through a copy of the light map's CPU table. Vanilla lights an entity as a whole with the light
/// of the block at its eye, then multiplies that colour into its vertex colours.
pub struct Lit<'a> {
    pub world: &'a crate::world::World,
    pub table: &'a [[f32; 3]; 256],
    /// The light map's texture, bound on unit 1 while `tint_begin` is on (any complete texture
    /// would do).
    pub tex: u32,
}

impl<'a> Lit<'a> {
    /// Turns unit 1 into a colour multiplier whose colour `tint_set` picks, for pieces of one
    /// buffer that each have their own light (signs).
    pub fn tint_begin(&self, gl: &Gl) {
        unsafe {
            (gl.ActiveTexture)(TEXTURE1);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.tex);
            (gl.TexEnvi)(TEXTURE_ENV, TEXTURE_ENV_MODE, COMBINE);
            (gl.TexEnvi)(TEXTURE_ENV, COMBINE_RGB, MODULATE);
            (gl.TexEnvi)(TEXTURE_ENV, SOURCE0_RGB, PREVIOUS);
            (gl.TexEnvi)(TEXTURE_ENV, OPERAND0_RGB, SRC_COLOR as i32);
            (gl.TexEnvi)(TEXTURE_ENV, SOURCE1_RGB, CONSTANT);
            (gl.TexEnvi)(TEXTURE_ENV, OPERAND1_RGB, SRC_COLOR as i32);
            (gl.TexEnvi)(TEXTURE_ENV, COMBINE_ALPHA, REPLACE);
            (gl.TexEnvi)(TEXTURE_ENV, SOURCE0_ALPHA, PREVIOUS);
            (gl.TexEnvi)(TEXTURE_ENV, OPERAND0_ALPHA, 0x0302);
            (gl.ActiveTexture)(TEXTURE0);
        }
    }

    pub fn tint_set(&self, gl: &Gl, c: [f32; 3]) {
        let c = [c[0], c[1], c[2], 1.0];
        unsafe {
            (gl.ActiveTexture)(TEXTURE1);
            (gl.TexEnvfv)(TEXTURE_ENV, TEXTURE_ENV_COLOR, c.as_ptr());
            (gl.ActiveTexture)(TEXTURE0);
        }
    }

    pub fn tint_end(&self, gl: &Gl) {
        unsafe {
            (gl.ActiveTexture)(TEXTURE1);
            (gl.Disable)(TEXTURE_2D);
            (gl.ActiveTexture)(TEXTURE0);
        }
    }
}

impl<'a> Lit<'a> {
    /// The colour of light levels, `sky` and `block` 0 to 15.
    pub fn colour(&self, sky: u8, block: u8) -> [f32; 3] {
        self.table[(sky.min(15) as usize) * 16 + block.min(15) as usize]
    }

    /// The light colour of the block cell.
    pub fn cell(&self, x: i32, y: i32, z: i32) -> [f32; 3] {
        let (s, b) = self.world.light_at(x, y, z);
        self.colour(s, b)
    }

    /// The light colour at a point of the world.
    pub fn at(&self, p: [f64; 3]) -> [f32; 3] {
        self.cell(crate::math::ifloor(p[0]), crate::math::ifloor(p[1]), crate::math::ifloor(p[2]))
    }

    /// The light colour at a point, with at least `block` block light (a burning entity: 15; a
    /// glow item frame: 5).
    pub fn at_min_block(&self, p: [f64; 3], block: u8) -> [f32; 3] {
        let (s, b) = self.world.light_at(crate::math::ifloor(p[0]), crate::math::ifloor(p[1]), crate::math::ifloor(p[2]));
        self.colour(s, b.max(block))
    }
}

/// A vertex colour times a light colour.
pub fn tint(c: [u8; 4], l: [f32; 3]) -> [u8; 4] {
    [(c[0] as f32 * l[0] + 0.5) as u8, (c[1] as f32 * l[1] + 0.5) as u8, (c[2] as f32 * l[2] + 0.5) as u8, c[3]]
}

pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    let mut out = alloc::vec::Vec::new();
    let near = |a: f32, b: f32| (a - b).abs() < 0.002;
    let p = |dim, t, rain, thunder| Params { dimension: dim, day_time: t, rain, thunder, flicker: 0.0, gamma: 0.5, darkness_blend: 0.0, darkness_option: 1.0, tick: 0.0, night_vision: 0.0, boss: 0.0 };
    let mut t = [[0f32; 3]; 256];

    // Noon: sky 15, block 0 is white; no light at all is the ambient colour (0x0a0a0a, then
    // brightness 0.5 lifts it by notGamma / 2); sky 0 and block 15 is the warm torch colour.
    compute(&p(OVERWORLD, 6000.0, 0.0, 0.0).input(), &mut t);
    out.push(("lightmap noon sky 15", format!("{}", t[15 * 16].iter().all(|&v| near(v, 1.0))), "true"));
    let a = 10.0 / 255.0;
    let lifted = a + (a * ((1.0 - (1.0 - a).powi4()) / a) - a) * 0.5;
    out.push(("lightmap no light", format!("{}", t[0].iter().all(|&v| near(v, lifted))), "true"));
    // Block 15, sky 0: ambient + tint * (1 * 1.4) = clamp(0.04 + (1, 1, 1) * 1.4) -> white
    // (the tint is mixed to white at the top level), so the colour is 1.
    out.push(("lightmap torch level 15", format!("{}", t[15].iter().all(|&v| near(v, 1.0))), "true"));
    // Block 8 and sky 0: brightness 8/15 / (4 - 3 * 8/15) = 0.1567 * 1.4 = 0.2194 of the tint,
    // the tint mixed 0.9 * (16/15 - 1)^2 = 0.0040 towards white; plus the ambient 0.0392.
    {
        let b = brightness(8.0 / 15.0) * 1.4;
        let m = 0.9 * (2.0 * 8.0 / 15.0 - 1.0) * (2.0 * 8.0 / 15.0 - 1.0);
        let r = 0.0392157 + lerp(m, 1.0, 1.0) * b;
        let g = 0.0392157 + lerp(m, BLOCK_TINT[1], 1.0) * b;
        // The brightness option's lift is a scale of the colour: the ratios are kept.
        let ratio = t[8][1] / t[8][0];
        let want = g / r;
        out.push(("lightmap torch hue", format!("{}", (ratio - want).abs() < 0.003), "true"));
    }
    // Texels worked out by hand from lightmap.fsh (gamma 0.5, ambient 0x0a0a0a, tint 0xFFD88C):
    // noon sky 0 block 8 is (0.5861, 0.5068, 0.3523); midnight (factor 0.24, colour 0x7a7aff)
    // sky 10 block 3 is (0.3245, 0.3072, 0.3582).
    {
        let h = |c: [f32; 3], w: [f32; 3]| (0..3).all(|k| (c[k] - w[k]).abs() < 0.0005);
        out.push(("lightmap hand texel torch 8", format!("{}", h(t[8], [0.5861, 0.5068, 0.3523])), "true"));
        let mut n = [[0f32; 3]; 256];
        compute(&p(OVERWORLD, 18000.0, 0.0, 0.0).input(), &mut n);
        out.push(("lightmap hand texel night", format!("{}", h(n[10 * 16 + 3], [0.3245, 0.3072, 0.3582])), "true"));
    }
    // Midnight: factor 0.24 and the blue-ish colour.
    let (f, c, am) = environment(OVERWORLD, 18000.0, 0.0, 0.0);
    out.push(("lightmap night environment", format!("{:.2} {:.2} {:.2} {:.3}", f, c[0], c[2], am[0]), "0.24 0.48 1.00 0.039"));
    // Noon, then sunset's slope: 11270 is still 1.0, 12205 is halfway to 0.24.
    out.push(("lightmap day slope", format!("{:.3} {:.3} {:.3}", environment(OVERWORLD, 11270.0, 0.0, 0.0).0, environment(OVERWORLD, 12205.0, 0.0, 0.0).0, environment(OVERWORLD, 13140.0, 0.0, 0.0).0), "1.000 0.620 0.240"));
    // Across midnight: 0.24 -> 1.0 over 22860..24730, halfway at 23795.
    out.push(("lightmap dawn wrap", format!("{:.3}", environment(OVERWORLD, 23795.0, 0.0, 0.0).0), "0.620"));
    // Rain: factor = lerp(rain, f, lerp(0.3125, f, 0.24)) at noon = 1 - 0.7600 * 0.3125.
    out.push(("lightmap rain", format!("{:.4}", environment(OVERWORLD, 6000.0, 1.0, 0.0).0), "0.7625"));
    out.push(("lightmap thunder", format!("{:.4}", environment(OVERWORLD, 6000.0, 1.0, 1.0).0), "0.5992"));
    // Dimensions.
    let (f, c, am) = environment(NETHER, 0.0, 0.0, 0.0);
    out.push(("lightmap nether environment", format!("{} {:.2} {:.3}", f, c[0], am[0]), "0 0.48 0.188"));
    let (f, _, am) = environment(END, 0.0, 0.0, 0.0);
    out.push(("lightmap end environment", format!("{} {:.3}", f, am[1]), "0 0.278"));
    // Night vision lifts the dark to 0.6 (max with the ambient); brightness 1 is notGamma.
    let mut i = p(OVERWORLD, 18000.0, 0.0, 0.0).input();
    i.brightness = 0.0;
    i.night_vision_factor = 1.0;
    compute(&i, &mut t);
    out.push(("lightmap night vision", format!("{:.2}", t[0][0]), "0.60"));
    // Darkness: subtracted (after the sum, 1.039 - 0.3), floors at 0.
    let mut i = p(OVERWORLD, 6000.0, 0.0, 0.0).input();
    i.brightness = 0.0;
    i.darkness_scale = 0.3;
    compute(&i, &mut t);
    out.push(("lightmap darkness", format!("{:.2} {:.2}", t[15 * 16][0], t[0][0]), "0.74 0.00"));
    // The boss bar's darkening scales green and blue by 0.6, red by 0.7.
    let mut i = p(OVERWORLD, 6000.0, 0.0, 0.0).input();
    i.brightness = 0.0;
    i.boss_darkening = 1.0;
    i.sky_factor = 0.5;
    compute(&i, &mut t);
    // sky 15: 0.0392 + 0.5 = 0.5392 -> red * 0.7, green * 0.6
    out.push(("lightmap boss darkening", format!("{:.3} {:.3}", t[15 * 16][0], t[15 * 16][1]), "0.377 0.324"));
    // The darkness pulse and the brightness option's darkness term.
    let q = Params { darkness_blend: 1.0, tick: 0.0, gamma: 0.5, ..p(OVERWORLD, 6000.0, 0.0, 0.0) }.input();
    out.push(("lightmap darkness params", format!("{:.3} {:.3}", q.darkness_scale, q.brightness), "0.450 0.000"));
    // The flicker walks within sensible bounds.
    let mut l = Lightmap::new();
    let mut worst = 0f32;
    for _ in 0..2000 {
        l.tick();
        worst = worst.max(l.flicker().abs());
    }
    out.push(("lightmap flicker bounded", format!("{}", worst < 0.2), "true"));

    // Things: the colour of light levels is the table's (sky * 16 + block), a vertex colour
    // times it rounds, and an unloaded place is lit as sky 0, block 0.
    let mut tab = [[0f32; 3]; 256];
    tab[3 * 16 + 9] = [0.5, 0.25, 1.0];
    tab[0] = [0.2, 0.2, 0.2];
    let w = crate::world::World::new(10, 4);
    let lit = Lit { world: &w, table: &tab, tex: 0 };
    out.push(("things light colour index", format!("{:?}", lit.colour(3, 9)), "[0.5, 0.25, 1.0]"));
    out.push(("things light tint", format!("{:?}", tint([200, 200, 200, 255], [0.5, 0.25, 1.0])), "[100, 50, 200, 255]"));
    out.push(("things light unloaded", format!("{:?}", lit.at([1.5, 70.0, 2.5])), "[0.2, 0.2, 0.2]"));
    out.push(("things light minimum block", format!("{:?}", lit.at_min_block([1.5, 70.0, 2.5], 9).iter().all(|&v| v == 0.0)), "true"));
    out
}

trait Pow4 {
    fn powi4(self) -> f32;
}
impl Pow4 for f32 {
    fn powi4(self) -> f32 {
        let s = self * self;
        s * s
    }
}
