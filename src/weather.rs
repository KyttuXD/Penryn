//! Weather as vanilla 26.3 draws and plays it: rain and snow columns around the camera
//! (WeatherEffectRenderer), which biome rains or snows where (Biome.getPrecipitationAt, with
//! its height and frozen-ocean temperature noise), the sky, fog and clouds darkening with the
//! rain and thunder levels (WeatherAttributes, AtmosphericFogEnvironment), the rain sounds
//! (ClientLevel.tickWeatherEffects) and lightning bolts (LightningBolt, LightningBoltRenderer:
//! the bolt, its thunder and the sky's flash).
//!
//! The textures and the biome climate come from weather.bin (optional: without it the sky
//! still darkens, but no rain or snow falls).

use crate::blocks::Blocks;
use crate::client::{Game, Sound, SoundRef};
use crate::math;
use crate::sdl::*;
use crate::world::World;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

pub const MAGIC: &[u8; 4] = b"PNW1";
/// Options.weatherRadius's default.
pub const RADIUS: i32 = 10;
/// SoundSource.WEATHER.
const WEATHER: usize = 3;
const SEA_LEVEL: i32 = 63;

// ---------------------------------------------------------------- vanilla's randomness

/// java.util.Random / LegacyRandomSource: the 48-bit LCG and its derived values.
pub struct JRand {
    seed: u64,
    gaussian: Option<f64>,
}

impl JRand {
    pub fn new(seed: i64) -> JRand {
        JRand { seed: (seed as u64 ^ 0x5DEECE66D) & ((1 << 48) - 1), gaussian: None }
    }
    pub fn set_seed(&mut self, seed: i64) {
        *self = JRand::new(seed);
    }
    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(0x5DEECE66D).wrapping_add(0xB) & ((1 << 48) - 1);
        (self.seed >> (48 - bits)) as i64 as i32
    }
    pub fn next_int(&mut self, bound: i32) -> i32 {
        if bound & -bound == bound {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let v = bits % bound;
            if bits.wrapping_sub(v).wrapping_add(bound - 1) >= 0 {
                return v;
            }
        }
    }
    pub fn next_i32(&mut self) -> i32 {
        self.next(32)
    }
    pub fn next_long(&mut self) -> i64 {
        ((self.next(32) as i64) << 32).wrapping_add(self.next(32) as i64)
    }
    pub fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.9604645e-8
    }
    pub fn next_double(&mut self) -> f64 {
        ((((self.next(26) as i64) << 27) + self.next(27) as i64) as f64) * (1.110223e-16f32 as f64)
    }
    /// MarsagliaPolarGaussian, as Random.nextGaussian.
    pub fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.gaussian.take() {
            return g;
        }
        loop {
            let a = 2.0 * self.next_double() - 1.0;
            let b = 2.0 * self.next_double() - 1.0;
            let s = a * a + b * b;
            if s < 1.0 && s != 0.0 {
                let m = math::sqrt(-2.0 * ln(s) / s);
                self.gaussian = Some(b * m);
                return a * m;
            }
        }
    }
}

/// The natural logarithm (no libm): split off the binary exponent, then the atanh series.
pub fn ln(x: f64) -> f64 {
    if x <= 0.0 {
        return f64::NEG_INFINITY;
    }
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000);
    if m > 1.4142135623730951 {
        m /= 2.0;
        e += 1;
    }
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let (mut term, mut sum) = (t, 0.0);
    let mut k = 1.0;
    while k < 40.0 {
        sum += term / k;
        term *= t2;
        k += 2.0;
    }
    2.0 * sum + e as f64 * core::f64::consts::LN_2
}

fn trunc(x: f32) -> f32 {
    x as i64 as f32
}

/// Java's float %: the sign of the dividend.
fn frem(a: f32, b: f32) -> f32 {
    a - trunc(a / b) * b
}

// ---------------------------------------------------------------- SimplexNoise (2D)

const GRAD: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0], [0.0, -1.0, 1.0], [-1.0, 1.0, 0.0], [0.0, -1.0, -1.0],
];

/// net.minecraft.world.level.levelgen.synth.SimplexNoise, made with `new SimplexNoise(r, true)`
/// (GradientNoise with no offsets: the three doubles are drawn and multiplied by 0).
pub struct Simplex {
    perm: [u8; 256],
}

impl Simplex {
    pub fn new(r: &mut JRand) -> Simplex {
        for _ in 0..3 {
            r.next_double();
        }
        let mut perm = [0u8; 256];
        for (i, p) in perm.iter_mut().enumerate() {
            *p = i as u8;
        }
        for i in 0..256 {
            let j = r.next_int(256 - i as i32) as usize;
            perm.swap(i, i + j);
        }
        Simplex { perm }
    }
    fn p(&self, i: i32) -> i32 {
        self.perm[(i & 255) as usize] as i32
    }
    fn corner(g: i32, x: f64, y: f64) -> f64 {
        let t = 0.5 - x * x - y * y;
        if t < 0.0 {
            0.0
        } else {
            let t = t * t;
            let gr = GRAD[g as usize];
            t * t * (gr[0] * x + gr[1] * y)
        }
    }
    pub fn get(&self, x: f64, y: f64) -> f32 {
        let sqrt3 = 1.7320508075688772;
        let f2 = 0.5 * (sqrt3 - 1.0);
        let g2 = (3.0 - sqrt3) / 6.0;
        let s = (x + y) * f2;
        let i = math::floor(x + s) as i32;
        let j = math::floor(y + s) as i32;
        let t = (i + j) as f64 * g2;
        let (x0, y0) = (x - (i as f64 - t), y - (j as f64 - t));
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let (x1, y1) = (x0 - i1 as f64 + g2, y0 - j1 as f64 + g2);
        let (x2, y2) = (x0 - 1.0 + 2.0 * g2, y0 - 1.0 + 2.0 * g2);
        let (ii, jj) = (i & 255, j & 255);
        let g0 = self.p(ii + self.p(jj)) % 12;
        let g1 = self.p(ii + i1 + self.p(jj + j1)) % 12;
        let g2i = self.p(ii + 1 + self.p(jj + 1)) % 12;
        (70.0 * (Self::corner(g0, x0, y0) + Self::corner(g1, x1, y1) + Self::corner(g2i, x2, y2))) as f32
    }
}

// ---------------------------------------------------------------- biome climate

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Precipitation {
    None,
    Rain,
    Snow,
}

#[derive(Clone, Copy)]
pub struct Climate {
    pub temperature: f32,
    pub precipitation: bool,
    pub frozen: bool,
}

/// Biome's static noises: TEMPERATURE_NOISE (seed 1234), FROZEN_TEMPERATURE_NOISE (three
/// layers, seed 3456) and BIOME_INFO_NOISE (seed 2345).
pub struct ClimateNoise {
    temperature: Simplex,
    frozen: [Simplex; 3],
    info: Simplex,
}

impl ClimateNoise {
    pub fn new() -> ClimateNoise {
        let temperature = Simplex::new(&mut JRand::new(1234));
        let mut r = JRand::new(3456);
        let frozen = [Simplex::new(&mut r), Simplex::new(&mut r), Simplex::new(&mut r)];
        let info = Simplex::new(&mut JRand::new(2345));
        ClimateNoise { temperature, frozen, info }
    }

    fn frozen_noise(&self, x: f64, z: f64) -> f32 {
        let layers = [(1.0, 0.14285715f32), (0.5, 0.2857143), (0.25, 0.5714286)];
        let mut v = 0.0f32;
        for (n, (freq, amp)) in self.frozen.iter().zip(layers) {
            v += amp * n.get(x * freq, z * freq);
        }
        v
    }

    /// Biome.getHeightAdjustedTemperature.
    pub fn temperature(&self, c: &Climate, x: i32, y: i32, z: i32) -> f32 {
        let mut t = c.temperature;
        if c.frozen {
            // TemperatureModifier.FROZEN: patches of frozen ocean are not so cold.
            let d = (self.frozen_noise(x as f64 * 0.05, z as f64 * 0.05) * 7.0) as f64;
            let e = self.info.get(x as f64 * 0.2, z as f64 * 0.2) as f64;
            if d + e < 0.3 && (self.info.get(x as f64 * 0.09, z as f64 * 0.09) as f64) < 0.8 {
                t = 0.2;
            }
        }
        let top = SEA_LEVEL + 17;
        if y > top {
            let g = self.temperature.get(x as f32 as f64 / 8.0, z as f32 as f64 / 8.0) * 8.0;
            t -= (g + y as f32 - top as f32) * 0.05 / 40.0;
        }
        t
    }

    /// Biome.getPrecipitationAt.
    pub fn precipitation(&self, c: &Climate, x: i32, y: i32, z: i32) -> Precipitation {
        if !c.precipitation {
            Precipitation::None
        } else if self.temperature(c, x, y, z) < 0.15 {
            Precipitation::Snow
        } else {
            Precipitation::Rain
        }
    }
}

// ---------------------------------------------------------------- weather data and state

/// A lightning bolt the server spawned: LightningBolt's client-side life, flashes and seed.
struct Bolt {
    life: i32,
    flashes: i32,
    seed: i64,
    rng: JRand,
    pos: [f64; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

pub struct Weather {
    pub bolt_kind: i32,
    rain_tex: u32,
    snow_tex: u32,
    climates: BTreeMap<String, Climate>,
    noise: Option<ClimateNoise>,
    /// The climate of each of the server's biome ids (World::biome_names), and the names it
    /// was made from.
    table: Vec<Option<Climate>>,
    table_of: usize,
    rain_sound_time: i32,
    /// ClientLevel.skyFlashTime.
    pub flash: i32,
    bolts: BTreeMap<i32, Bolt>,
    rng: JRand,
    /// AtmosphericFogEnvironment.rainFogMultiplier.
    pub fog: f32,
    /// PENRYN_TEST_WEATHER=rain[,thunder]: local rain and thunder levels for tests.
    test: Option<(f32, f32)>,
    /// The level's game time, ticked here between the server's time packets (one a second).
    time: i64,
    server_time: i64,
    /// PENRYN_DEBUG: print the weather's sounds.
    debug: bool,
    /// The columns around the camera: their origin, heights and precipitation, made again
    /// each tick or when the camera crosses a block.
    cols_at: Option<(i32, i32, i32)>,
    dirty: bool,
    heights: Vec<i32>,
    precip: Vec<Precipitation>,
    verts: Vec<V>,
}

/// The rain and thunder levels as the game uses them (Level.getRainLevel, getThunderLevel:
/// the thunder level scaled by the rain).
pub fn levels(g: &Game) -> (f32, f32) {
    let rain = g.rain_level.clamp(0.0, 1.0);
    (rain, g.thunder_level.clamp(0.0, 1.0) * rain)
}

fn lerp(t: f32, a: f32, b: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(t: f32, a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [lerp(t, a[0], b[0]), lerp(t, a[1], b[1]), lerp(t, a[2], b[2])]
}

/// ColorModifier.BLEND_TO_GRAY: towards the colour's grey (ARGB.greyscale) times `brightness`.
fn blend_to_grey(c: [f32; 3], brightness: f32, factor: f32) -> [f32; 3] {
    let g = (c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11) * brightness;
    lerp3(factor, c, [g, g, g])
}

impl Weather {
    pub fn new() -> Weather {
        let test = crate::sys::env("PENRYN_TEST_WEATHER").map(|s| {
            let thunder = s.contains("thunder");
            (if s.contains("rain") || thunder { 1.0 } else { 0.0 }, if thunder { 1.0 } else { 0.0 })
        });
        Weather {
            bolt_kind: -1,
            rain_tex: 0,
            snow_tex: 0,
            climates: BTreeMap::new(),
            noise: None,
            table: Vec::new(),
            table_of: usize::MAX,
            rain_sound_time: 0,
            flash: 0,
            bolts: BTreeMap::new(),
            rng: JRand::new(crate::sys::now_ms() as i64),
            fog: 0.0,
            test,
            time: 0,
            server_time: i64::MIN,
            debug: crate::sys::env("PENRYN_DEBUG").is_some(),
            cols_at: None,
            dirty: true,
            heights: Vec::new(),
            precip: Vec::new(),
            verts: Vec::new(),
        }
    }

    /// Reads weather.bin's biome table and lightning id (and, with `gl`, its textures).
    pub fn parse(&mut self, d: &[u8], gl: Option<&Gl>) -> Option<()> {
        if d.get(..4)? != MAGIC {
            return None;
        }
        let u16_at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?)) };
        self.bolt_kind = u16_at(4)? as i32;
        let mut p = 6;
        for k in 0..2 {
            let (w, h) = (u16_at(p)? as usize, u16_at(p + 2)? as usize);
            let px = d.get(p + 4..p + 4 + w * h * 4)?;
            if let Some(gl) = gl {
                let mut tex = 0u32;
                // Loaded again (a resource pack): the old texture goes.
                let old = if k == 0 { self.rain_tex } else { self.snow_tex };
                unsafe {
                    if old != 0 {
                        (gl.DeleteTextures)(1, &old);
                    }
                    (gl.GenTextures)(1, &mut tex);
                    (gl.BindTexture)(TEXTURE_2D, tex);
                    (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
                    (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
                }
                if k == 0 {
                    self.rain_tex = tex;
                } else {
                    self.snow_tex = tex;
                }
            }
            p += 4 + w * h * 4;
        }
        let n = u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?) as usize;
        p += 4;
        for _ in 0..n {
            let l = *d.get(p)? as usize;
            let name = String::from_utf8_lossy(d.get(p + 1..p + 1 + l)?).into_owned();
            p += 1 + l;
            let t = f32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?);
            let flags = *d.get(p + 4)?;
            p += 5;
            self.climates.insert(name, Climate { temperature: t, precipitation: flags & 1 != 0, frozen: flags & 2 != 0 });
        }
        self.noise = Some(ClimateNoise::new());
        self.table_of = usize::MAX;
        Some(())
    }

    pub fn climate(&self, name: &str) -> Option<Climate> {
        self.climates.get(name).copied()
    }

    /// The levels to use: the server's, or the test override.
    pub fn levels(&self, g: &Game) -> (f32, f32) {
        self.test.unwrap_or_else(|| levels(g))
    }

    fn sync_table(&mut self, world: &World) {
        let key = world.biome_names.len() ^ world.biome_names.first().map_or(0, |n| n.len() << 16);
        if key == self.table_of {
            return;
        }
        self.table = world.biome_names.iter().map(|n| self.climates.get(n).copied()).collect();
        self.table_of = key;
        self.dirty = true;
    }

    /// The precipitation at a block (ClientLevel.getPrecipitationAt).
    pub fn precipitation_at(&self, world: &World, x: i32, y: i32, z: i32) -> Precipitation {
        let Some(noise) = self.noise.as_ref() else { return Precipitation::None };
        let id = world.biome(x, y, z);
        match self.table.get(id as usize).copied().flatten() {
            Some(c) => noise.precipitation(&c, x, y, z),
            None => Precipitation::None,
        }
    }

    /// One client tick (20 a second): bolts, the sky flash, the rain fog and rain sounds.
    pub fn tick(&mut self, g: &mut Game, eye: [f64; 3]) {
        self.sync_table(&g.world);
        self.dirty = true;
        if g.game_time != self.server_time {
            self.server_time = g.game_time;
            self.time = g.game_time;
        } else {
            self.time += 1;
        }
        if self.flash > 0 {
            self.flash -= 1;
        }
        // Lightning bolts: LightningBolt.tick on the client.
        let kind = self.bolt_kind;
        self.bolts.retain(|id, _| g.entities.get(id).map_or(false, |e| e.kind == kind));
        for (id, e) in g.entities.iter() {
            if e.kind != kind || kind < 0 || self.bolts.contains_key(id) {
                continue;
            }
            let mut rng = JRand::new(self.rng.next_long());
            let seed = rng.next_long();
            let flashes = rng.next_int(3) + 1;
            self.bolts.insert(*id, Bolt { life: 2, flashes, seed, rng, pos: e.pos });
        }
        for b in self.bolts.values_mut() {
            if b.life == 2 {
                if self.debug {
                    crate::say!("weather: lightning bolt at {:.1} {:.1} {:.1}, {} flashes", b.pos[0], b.pos[1], b.pos[2], b.flashes);
                }
                let p1 = 0.8 + b.rng.next_float() * 0.2;
                let p2 = 0.5 + b.rng.next_float() * 0.2;
                g.sounds.push(Sound { sound: SoundRef::Name(String::from("entity.lightning_bolt.thunder")), pos: Some(b.pos), volume: 10000.0, pitch: p1, source: WEATHER });
                g.sounds.push(Sound { sound: SoundRef::Name(String::from("entity.lightning_bolt.impact")), pos: Some(b.pos), volume: 2.0, pitch: p2, source: WEATHER });
            }
            b.life -= 1;
            if b.life < 0 && b.flashes > 0 && b.life < -b.rng.next_int(10) {
                b.flashes -= 1;
                b.life = 1;
                b.seed = b.rng.next_long();
            }
            if b.life >= 0 {
                self.flash = 2;
            }
        }
        let (rain, _) = self.levels(g);
        let world = &g.world;
        let blocks = &g.blocks;
        let cam = [math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2])];
        // The rain fog: thicker under the open sky, half as thick where it never rains.
        let open = height(world, blocks, cam[0], cam[2]) <= cam[1];
        let precip = self
            .table
            .get(world.biome(cam[0], cam[1], cam[2]) as usize)
            .copied()
            .flatten()
            .map_or(false, |c| c.precipitation);
        let target = rain * if open { 1.0 } else { 0.0 } * if precip { 1.0 } else { 0.5 };
        self.fog += (target - self.fog) * 0.2;
        if rain <= 0.0 || self.noise.is_none() {
            return;
        }
        // ClientLevel.tickWeatherEffects: random columns near the camera; the last one rained
        // on is where the rain sound plays (from above when it falls on a roof overhead).
        let mut r = JRand::new(self.time.wrapping_mul(312987231));
        let n = 2 * RADIUS + 1;
        let tries = (0.225 * (n * n) as f32 * rain * rain) as i32;
        let mut hit: Option<[i32; 3]> = None;
        for _ in 0..tries {
            let dx = r.next_int(n) - RADIUS;
            let dz = r.next_int(n) - RADIUS;
            let (x, z) = (cam[0] + dx, cam[2] + dz);
            let y = height(world, blocks, x, z);
            if y <= world.min_y || y > cam[1] + 10 || y < cam[1] - 10 {
                continue;
            }
            if self.precipitation_at(world, x, y, z) != Precipitation::Rain {
                continue;
            }
            hit = Some([x, y - 1, z]);
            // The splash particle's place (no particles drawn yet).
            r.next_double();
            r.next_double();
        }
        if let Some(p) = hit {
            let t = self.rain_sound_time;
            self.rain_sound_time += 1;
            if r.next_int(3) < t {
                self.rain_sound_time = 0;
                let at = [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5];
                let above = p[1] > cam[1] + 1 && height(world, blocks, cam[0], cam[2]) > cam[1];
                let (name, volume, pitch) = if above { ("weather.rain.above", 0.1, 0.5) } else { ("weather.rain", 0.2, 1.0) };
                if self.debug {
                    crate::say!("weather: {} at {} {} {}", name, p[0], p[1], p[2]);
                }
                g.sounds.push(Sound { sound: SoundRef::Name(String::from(name)), pos: Some(at), volume, pitch, source: WEATHER });
            }
        }
    }

    /// The sky (and fog) colour under the rain, thunder and lightning flash.
    pub fn sky_colour(&self, sky: [f32; 3], rain: f32, thunder: f32) -> [f32; 3] {
        let mut c = sky;
        let only_rain = rain - thunder;
        if only_rain > 0.0 {
            c = lerp3(only_rain, c, blend_to_grey(c, 0.6, 0.75));
        }
        if thunder > 0.0 {
            c = lerp3(thunder, c, blend_to_grey(c, 0.24, 0.94));
        }
        if self.flash > 0 {
            c = lerp3(0.22, c, [0.8, 0.8, 1.0]);
        }
        c
    }

    /// The sky light factor (clouds, rain and stars use it as daylight).
    pub fn daylight(&self, day: f32, rain: f32, thunder: f32) -> f32 {
        if self.flash > 0 {
            return 1.0;
        }
        let mut d = day;
        let only_rain = rain - thunder;
        if only_rain > 0.0 {
            d = lerp(only_rain, d, lerp(0.3125, d, 0.24));
        }
        if thunder > 0.0 {
            d = lerp(thunder, d, lerp(0.52734375, d, 0.24));
        }
        d
    }

    /// Makes the columns' heights and precipitation around `eye` again when needed.
    fn columns(&mut self, world: &World, blocks: &Blocks, eye: [f64; 3]) {
        let at = (math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2]));
        if !self.dirty && self.cols_at == Some(at) {
            return;
        }
        self.dirty = false;
        self.cols_at = Some(at);
        let n = (2 * RADIUS + 1) as usize;
        self.heights.clear();
        self.precip.clear();
        for dz in -RADIUS..=RADIUS {
            for dx in -RADIUS..=RADIUS {
                let (x, z) = (at.0 + dx, at.2 + dz);
                self.heights.push(height(world, blocks, x, z));
                self.precip.push(self.precipitation_at(world, x, at.1, z));
            }
        }
        debug_assert!(self.heights.len() == n * n);
    }

    /// WeatherEffectRenderer: one quad per column within the weather radius, turned across
    /// the line to the camera, from the ground (or 10 below) to 10 above the camera, the
    /// texture sliding down (rain) or drifting (snow); fading out towards the edge.
    /// Returns the number of columns drawn.
    pub fn build(&mut self, world: &World, blocks: &Blocks, eye: [f64; 3], partial: f32, rain: f32, light: f32) -> (usize, usize) {
        let game_time = self.time;
        self.verts.clear();
        if rain <= 0.0 || self.noise.is_none() {
            return (0, 0);
        }
        self.sync_table(world);
        self.columns(world, blocks, eye);
        let (ex, ey, ez) = (math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2]));
        let n = 2 * RADIUS + 1;
        let r2 = (RADIUS * RADIUS) as f32;
        let ticks = (game_time & 131071) as i32;
        let mut r = JRand::new(0);
        let mut counts = (0, 0);
        let lc = (light.clamp(0.0, 1.0) * 255.0) as u8;
        for pass in [Precipitation::Rain, Precipitation::Snow] {
            for dz in -RADIUS..=RADIUS {
                for dx in -RADIUS..=RADIUS {
                    let i = ((dz + RADIUS) * n + dx + RADIUS) as usize;
                    if self.precip[i] != pass {
                        continue;
                    }
                    let (x, z) = (ex + dx, ez + dz);
                    let h = self.heights[i];
                    let bottom = (ey - RADIUS).max(h);
                    let top = (ey + RADIUS).max(h);
                    // The camera's own column: vanilla's size there is NaN, so it draws nothing.
                    if top == bottom || (dx == 0 && dz == 0) {
                        continue;
                    }
                    let seed = (x.wrapping_mul(x).wrapping_mul(3121).wrapping_add(x.wrapping_mul(45238971)))
                        ^ (z.wrapping_mul(z).wrapping_mul(418711).wrapping_add(z.wrapping_mul(13761)));
                    r.set_seed(seed as i64);
                    let (u_off, v_off, base) = if pass == Precipitation::Rain {
                        let j = (x.wrapping_mul(x).wrapping_mul(3121).wrapping_add(x.wrapping_mul(45238971))
                            .wrapping_add(z.wrapping_mul(z).wrapping_mul(418711))
                            .wrapping_add(z.wrapping_mul(13761)))
                            & 255;
                        let f = 3.0 + r.next_float();
                        let g = -((ticks + j) as f32 + partial) / 32.0 * f;
                        counts.0 += 1;
                        (0.0, frem(g, 32.0), 1.0)
                    } else {
                        let f = ticks as f32 + partial;
                        let g = (r.next_double() + (f * 0.01 * r.next_gaussian() as f32) as f64) as f32;
                        let h2 = (r.next_double() + (f * r.next_gaussian() as f32 * 0.001) as f64) as f32;
                        let k = -(((game_time & 511) as f32) + partial) / 512.0;
                        counts.1 += 1;
                        (g, k + h2, 0.8)
                    };
                    let cx = (x as f64 + 0.5 - eye[0]) as f32;
                    let cz = (z as f64 + 0.5 - eye[2]) as f32;
                    let d2 = cx * cx + cz * cz;
                    let alpha = lerp((d2 / r2).min(1.0), base, 0.5) * rain;
                    // columnSizeX/Z: across the line from the camera's block (half a block each side).
                    let (sx, sz) = (dz as f32, dx as f32);
                    let len = math::sqrtf(sx * sx + sz * sz);
                    let (hx, hz) = (-sx / len / 2.0, sz / len / 2.0);
                    let (x0, x1) = (cx - hx, cx + hx);
                    let (z0, z1) = (cz - hz, cz + hz);
                    let (y1, y0) = ((top as f64 - eye[1]) as f32, (bottom as f64 - eye[1]) as f32);
                    let (v0, v1) = (bottom as f32 * 0.25 + v_off, top as f32 * 0.25 + v_off);
                    let c = [lc, lc, lc, (alpha.clamp(0.0, 1.0) * 255.0) as u8];
                    self.verts.push(V { p: [x0, y1, z0], uv: [u_off, v0], c });
                    self.verts.push(V { p: [x1, y1, z1], uv: [u_off + 1.0, v0], c });
                    self.verts.push(V { p: [x1, y0, z1], uv: [u_off + 1.0, v1], c });
                    self.verts.push(V { p: [x0, y0, z0], uv: [u_off, v1], c });
                }
            }
        }
        counts
    }

    /// Draws the rain and snow built by `build`, and the lightning bolts, in the world's
    /// view (`view`: rotation only, positions relative to the eye).
    pub fn draw(&mut self, gl: &Gl, view: &[f32; 16], world: &World, blocks: &Blocks, eye: [f64; 3], partial: f32, rain: f32, light: f32) {
        let (rain_n, snow_n) = self.build(world, blocks, eye, partial, rain, light);
        let has_bolts = !self.bolts.is_empty();
        if rain_n + snow_n == 0 && !has_bolts {
            return;
        }
        unsafe {
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadIdentity)();
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(view.as_ptr());
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.Disable)(CULL_FACE);
            (gl.Enable)(DEPTH_TEST);
            (gl.DepthMask)(0);
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.004);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
        }
        if rain_n + snow_n > 0 {
            let v = &self.verts;
            unsafe {
                (gl.Enable)(TEXTURE_2D);
                (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
                (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
                (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
                if rain_n > 0 {
                    (gl.BindTexture)(TEXTURE_2D, self.rain_tex);
                    (gl.DrawArrays)(QUADS, 0, rain_n as i32 * 4);
                }
                if snow_n > 0 {
                    (gl.BindTexture)(TEXTURE_2D, self.snow_tex);
                    (gl.DrawArrays)(QUADS, rain_n as i32 * 4, snow_n as i32 * 4);
                }
                (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
                (gl.Disable)(TEXTURE_2D);
            }
        }
        if has_bolts {
            self.verts.clear();
            for b in self.bolts.values() {
                bolt_quads(&mut self.verts, b.seed, [(b.pos[0] - eye[0]) as f32, (b.pos[1] - eye[1]) as f32, (b.pos[2] - eye[2]) as f32]);
            }
            let v = &self.verts;
            unsafe {
                // RenderTypes.lightning: additive, no culling.
                (gl.BlendFunc)(SRC_ALPHA, ONE);
                (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
                (gl.Disable)(TEXTURE_2D);
                (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            }
        }
        unsafe {
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.Disable)(BLEND);
            (gl.Disable)(ALPHA_TEST);
            (gl.DepthMask)(1);
            (gl.Enable)(CULL_FACE);
        }
    }
}

/// The MOTION_BLOCKING heightmap: one above the highest block that blocks motion or holds a
/// fluid: water and lava themselves, or waterlogged blocks (the world's bottom where there is
/// none, or the column is not loaded).
pub fn height(world: &World, blocks: &Blocks, x: i32, z: i32) -> i32 {
    let Some(c) = world.columns.get(&(x >> 4, z >> 4)) else { return world.min_y };
    for si in (0..c.sections.len()).rev() {
        let Some(s) = c.sections[si].as_ref() else { continue };
        if s.solid == 0 {
            continue;
        }
        for ly in (0..16).rev() {
            let st = s.states[((ly << 8) | ((z & 15) << 4) | (x & 15)) as usize];
            if st != 0 && (blocks.collides(st) || blocks.has_fluid(st) || matches!(blocks.kind(st), crate::blocks::WATER | crate::blocks::LAVA)) {
                return world.min_y + si as i32 * 16 + ly + 1;
            }
        }
    }
    world.min_y
}

/// LightningBoltRenderer: a jagged main bolt 8 x 16 blocks high and two branches, four
/// nested widths each, from the bolt's seed.
fn bolt_quads(out: &mut Vec<V>, seed: i64, at: [f32; 3]) {
    let mut xs = [0f32; 8];
    let mut zs = [0f32; 8];
    let (mut fx, mut fz) = (0f32, 0f32);
    let mut r = JRand::new(seed);
    for h in (0..8).rev() {
        xs[h] = fx;
        zs[h] = fz;
        fx += (r.next_int(11) - 5) as f32;
        fz += (r.next_int(11) - 5) as f32;
    }
    let c = [(0.45 * 255.0) as u8, (0.45 * 255.0) as u8, (0.5 * 255.0) as u8, (0.3 * 255.0) as u8];
    for k in 0..4 {
        let mut r2 = JRand::new(seed);
        for l in 0..3 {
            let main = l == 0;
            let m = 7 - l;
            let n = if main { 0 } else { m as i32 - 2 };
            let mut o = xs[m] - fx;
            let mut p = zs[m] - fz;
            let mut q = m as i32;
            while q >= n {
                let (pr, ps) = (o, p);
                if main {
                    o += (r2.next_int(11) - 5) as f32;
                    p += (r2.next_int(11) - 5) as f32;
                } else {
                    o += (r2.next_int(31) - 15) as f32;
                    p += (r2.next_int(31) - 15) as f32;
                }
                let mut w0 = 0.1 + k as f32 * 0.2;
                let mut w1 = w0;
                if main {
                    w0 *= q as f32 * 0.1 + 1.0;
                    w1 *= (q - 1) as f32 * 0.1 + 1.0;
                }
                for f in [[false, false, true, false], [true, false, true, true], [true, true, false, true], [false, true, false, false]] {
                    let s = |b: bool, w: f32| if b { w } else { -w };
                    let y0 = at[1] + q as f32 * 16.0;
                    let y1 = at[1] + (q + 1) as f32 * 16.0;
                    out.push(V { p: [at[0] + o + s(f[0], w1), y0, at[2] + p + s(f[1], w1)], uv: [0.0; 2], c });
                    out.push(V { p: [at[0] + pr + s(f[0], w0), y1, at[2] + ps + s(f[1], w0)], uv: [0.0; 2], c });
                    out.push(V { p: [at[0] + pr + s(f[2], w0), y1, at[2] + ps + s(f[3], w0)], uv: [0.0; 2], c });
                    out.push(V { p: [at[0] + o + s(f[2], w1), y0, at[2] + p + s(f[3], w1)], uv: [0.0; 2], c });
                }
                q -= 1;
            }
        }
    }
}

/// Self-test checks that need no server: (what, got, want).
pub fn checks(data: Option<&[u8]>, blocks: Option<&Blocks>) -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut out = Vec::new();
    let mut r = JRand::new(42);
    out.push(("weather: Java Random(42).nextInt()", format!("{}", r.next_i32()), "-1170105035"));
    let mut r = JRand::new(42);
    out.push(("weather: Java Random(42).nextGaussian()", format!("{:.6}", r.next_gaussian()), "1.141905"));
    out.push(("weather: ln(10)", format!("{:.9}", ln(10.0)), "2.302585093"));
    out.push(("weather: frem(-33.5, 32)", format!("{}", frem(-33.5, 32.0)), "-1.5"));
    let Some(d) = data else { return out };
    let mut w = Weather::new();
    w.test = None;
    out.push(("weather: weather.bin parses", format!("{}", w.parse(d, None).is_some()), "true"));
    out.push(("weather: lightning bolt entity id", format!("{}", w.bolt_kind), "78"));
    let noise = ClimateNoise::new();
    let at = |name: &str, y: i32| -> String {
        match w.climate(name) {
            Some(c) => format!("{:?}", noise.precipitation(&c, 100, y, 100)),
            None => String::from("missing"),
        }
    };
    out.push(("weather: plains rains", at("minecraft:plains", 70), "Rain"));
    out.push(("weather: snowy plains snow", at("minecraft:snowy_plains", 70), "Snow"));
    out.push(("weather: desert stays dry", at("minecraft:desert", 70), "None"));
    out.push(("weather: windswept hills snow high up", at("minecraft:windswept_hills", 160), "Snow"));
    out.push(("weather: windswept hills rain low down", at("minecraft:windswept_hills", 70), "Rain"));
    let mut sum = 0.0f32;
    for i in 0..64 {
        sum += noise.temperature.get(i as f64 * 1.37, i as f64 * 0.73).abs();
    }
    out.push(("weather: simplex noise varies within [-1, 1]", format!("{}", sum > 5.0 && sum < 64.0), "true"));
    // Columns over a flat floor (a test world: stone at y 63 in chunk 0, 0) as plains.
    if let Some(b) = blocks {
        let mut world = crate::world::World::new(b.global_bits(), 6);
        let n = (world.height / 16) as usize;
        let mut sections = Vec::new();
        for _ in 0..n {
            sections.push(None);
        }
        world.columns.insert((0, 0), crate::world::Column::dark(sections, alloc::vec![[0u16; 64]; n], 0));
        let stone = (1..b.states.len() as u16).find(|&s| b.of_state(s).name == "stone").unwrap_or(1);
        for x in 0..16 {
            for z in 0..16 {
                world.set(x, 63, z, stone, &|s| s == 0);
            }
        }
        world.biome_names = alloc::vec![String::from("minecraft:plains")];
        out.push(("weather: heightmap over the floor", format!("{}", height(&world, b, 5, 5)), "64"));
        // Water counts (rain falls on the sea, not on its floor): a pool at (0, 64, 0).
        if let Some(water) = (1..b.states.len() as u16).find(|&s| b.of_state(s).name == "water") {
            world.set(0, 64, 0, water, &|s| s == 0);
            out.push(("weather: heightmap over water", format!("{}", height(&world, b, 0, 0)), "65"));
            world.set(0, 64, 0, 0, &|s| s == 0);
        }
        w.time = 1000;
        let (rn, sn) = w.build(&world, b, [8.5, 70.0, 8.5], 0.0, 1.0, 1.0);
        // Within the 16x16 chunk loaded around (8, 8): the camera's own column has no quad.
        out.push(("weather: rain columns around the camera", format!("{} rain, {} snow", rn, sn), "255 rain, 0 snow"));
        let v = &w.verts[0];
        out.push(("weather: a column reaches 10 above the camera", format!("{}", v.p[1]), "10"));
    }
    out
}
