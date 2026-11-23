//! Particles as vanilla's ParticleEngine draws them: block break and digging particles from the
//! block's own texture (TerrainParticle), sprinting and eating particles, and what the server
//! sends (ClientboundLevelParticlesPacket, explosions, entity and level events) with vanilla's
//! sprite sets from particles.bin. Each particle type moves as its vanilla class does (gravity,
//! friction, lifetime, size over its life, sprite by age); they are drawn as camera-facing
//! quads in three batches (block atlas, sprites cut out, sprites blended), at most `MAX`.

use crate::blocks::{self, Blocks};
use crate::client::Game;
use crate::gui::{Gui, Icon};
use crate::math;
use crate::sdl::*;
use crate::wire::In;
use crate::world::World;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

/// The most particles alive at once (vanilla: 16384 per group; a 2008 GPU wants fewer).
const MAX: usize = 4000;
/// minecraft:villager's entity type id (26.3 registries).
const ENTITY_VILLAGER: i32 = 143;
/// ClientExplosionTracker.MAX_PARTICLES_PER_TICK.
const EXPLOSION_PER_TICK: i32 = 512;

/// What the network brought for the particle engine, queued by the packet handlers.
pub enum Effect {
    /// A ClientboundLevelParticlesPacket or ClientboundExplodePacket body.
    Packet(i32, Vec<u8>),
    /// A block destroyed by someone else (level event 2001): where, and its state.
    Destroy([i32; 3], u16),
    /// Another level event (bone meal, spawner flames...): kind, block, data.
    Level(i32, [i32; 3], i32),
    /// An entity event: the event, where the entity is, its type.
    Entity(u8, [f64; 3], i32),
}

/// The local player, for the particles it makes itself (sprinting, eating).
pub struct PlayerFx {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub eye: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub sprinting: bool,
    pub on_ground: bool,
    pub in_fluid: bool,
    /// An item being eaten: its id, and ticks of use so far and in all.
    pub eating: Option<(i32, u32, u32)>,
}

/// The behaviour of a particle type (its vanilla class and provider).
#[derive(Clone, Copy, PartialEq)]
enum K {
    None,
    Generic,
    Terrain,
    DustPillar,
    Crumble,
    FallingDust,
    Item,
    Smoke,
    LargeSmoke,
    WhiteSmoke,
    Flame,
    SmallFlame,
    Bubble,
    Splash,
    Rain,
    Crit,
    Magic,
    Damage,
    Poof,
    Heart,
    Angry,
    Happy,
    Town,
    Note,
    Portal,
    Dust,
    DustTransition,
    Explosion,
    ExplosionSeed,
    Lava,
    Campfire,
    Signal,
    Sweep,
    Spell,
    Cloud,
    Sneeze,
    Drip,
}

fn kind_of(name: &str) -> K {
    match name {
        "block" | "block_marker" => K::Terrain,
        "dust_pillar" => K::DustPillar,
        "block_crumble" => K::Crumble,
        "falling_dust" => K::FallingDust,
        "item" | "item_slime" | "item_snowball" | "item_cobweb" => K::Item,
        "smoke" => K::Smoke,
        "large_smoke" => K::LargeSmoke,
        "white_smoke" => K::WhiteSmoke,
        "flame" | "soul_fire_flame" | "copper_fire_flame" => K::Flame,
        "small_flame" => K::SmallFlame,
        "bubble" | "bubble_column_up" | "current_down" => K::Bubble,
        "splash" => K::Splash,
        "rain" => K::Rain,
        "crit" => K::Crit,
        "enchanted_hit" => K::Magic,
        "damage_indicator" => K::Damage,
        "poof" => K::Poof,
        "heart" => K::Heart,
        "angry_villager" => K::Angry,
        "happy_villager" | "composter" | "egg_crack" => K::Happy,
        "dolphin" | "mycelium" => K::Town,
        "note" => K::Note,
        "portal" | "reverse_portal" => K::Portal,
        "dust" => K::Dust,
        "dust_color_transition" => K::DustTransition,
        "explosion" => K::Explosion,
        "explosion_emitter" => K::ExplosionSeed,
        "lava" => K::Lava,
        "campfire_cosy_smoke" => K::Campfire,
        "campfire_signal_smoke" => K::Signal,
        "sweep_attack" => K::Sweep,
        "effect" | "instant_effect" | "entity_effect" | "witch" => K::Spell,
        "cloud" => K::Cloud,
        "sneeze" => K::Sneeze,
        n if n.starts_with("dripping_") || n.starts_with("falling_") || n.starts_with("landing_") => K::Drip,
        "vibration" | "trail" | "shriek" | "sculk_charge" | "sculk_charge_pop" | "sonic_boom" | "elder_guardian"
        | "gust_emitter_large" | "gust_emitter_small" | "flash" => K::None,
        _ => K::Generic,
    }
}

/// A particle's options as the packet carries them.
#[derive(Clone, Copy)]
enum Opt {
    None,
    Block(u16),
    Item(i32),
    /// ARGB, as floats.
    Color([f32; 4]),
    Dust([f32; 3], f32),
    Transition([f32; 3], [f32; 3], f32),
}

// Size over the particle's life (each class's getQuadSize).
const SZ_CONST: u8 = 0;
const SZ_GROW: u8 = 1;
const SZ_FLAME: u8 = 2;
const SZ_LAVA: u8 = 3;
const SZ_PORTAL: u8 = 4;

// Flags.
const PHYSICS: u8 = 1;
const SPEED_UP: u8 = 2;
const STOPPED: u8 = 4;
const ON_GROUND: u8 = 8;
const BY_AGE: u8 = 16;

// Layers: the block atlas, the sprite sheet cut out, the sprite sheet blended.
const L_ATLAS: u8 = 0;
const L_SHEET: u8 = 1;
const L_BLEND: u8 = 2;

struct P {
    pos: [f64; 3],
    prev: [f64; 3],
    vel: [f64; 3],
    /// Where a portal particle started (it moves along a curve from there).
    start: [f64; 3],
    age: i32,
    life: i32,
    gravity: f32,
    friction: f32,
    size: f32,
    /// Half the collision box's width, and its height.
    half: f64,
    height: f64,
    col: [f32; 4],
    uv: [f32; 4],
    kind: K,
    ty: u16,
    layer: u8,
    flags: u8,
    size_mode: u8,
    /// Dust colour transitions: the colour it ends at.
    to: [f32; 3],
    from: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

pub struct Particles {
    list: Vec<P>,
    evict: usize,
    rng: u64,
    sheet: u32,
    sprites: Vec<[f32; 4]>,
    /// Per particle type (registry id): its sprites, and its behaviour.
    sets: Vec<Vec<u16>>,
    kinds: Vec<K>,
    /// ParticleType.getOverrideLimiter: shown however far from the camera.
    always: Vec<bool>,
    names: Vec<String>,
    /// Per block: its particle texture's tile in the atlas (0xFFFF: none), when particles.bin
    /// was made with this atlas.
    block_tile: Vec<u16>,
    atlas_size: (f32, f32),
    explosions: Vec<([f64; 3], f32, i32, Vec<(u16, Opt, f32, f32, i32)>)>,
    verts: [Vec<V>; 3],
    /// Type ids of the ones vanilla's own code makes.
    id_smoke: u16,
    id_poof: u16,
    id_heart: u16,
    id_flame: u16,
    id_angry: u16,
    id_happy: u16,
    id_explosion: u16,
    id_large_smoke: u16,
    id_totem: u16,
}

fn rd16(d: &[u8], o: usize) -> u16 {
    d.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

impl Particles {
    /// The engine, with particles.bin's sprites when it is there (block particles work
    /// without it).
    pub fn load(gl: &Gl, data: Option<&[u8]>, atlas_size: (f32, f32)) -> Particles {
        let mut p = Particles {
            list: Vec::new(),
            evict: 0,
            rng: 0x9E37_79B9_7F4A_7C15 ^ crate::sys::now_ms(),
            sheet: 0,
            sprites: Vec::new(),
            sets: Vec::new(),
            kinds: Vec::new(),
            always: Vec::new(),
            names: Vec::new(),
            block_tile: Vec::new(),
            atlas_size,
            explosions: Vec::new(),
            verts: [Vec::new(), Vec::new(), Vec::new()],
            id_smoke: 0xFFFF,
            id_poof: 0xFFFF,
            id_heart: 0xFFFF,
            id_flame: 0xFFFF,
            id_angry: 0xFFFF,
            id_happy: 0xFFFF,
            id_explosion: 0xFFFF,
            id_large_smoke: 0xFFFF,
            id_totem: 0xFFFF,
        };
        if let Some(d) = data {
            if p.parse(Some(gl), d).is_none() {
                crate::warn!("particles.bin is damaged: no particle sprites");
                p.sets.clear();
                p.kinds.clear();
                p.always.clear();
                p.block_tile.clear();
            }
        }
        p
    }

    fn parse(&mut self, gl: Option<&Gl>, d: &[u8]) -> Option<()> {
        if d.get(..4)? != crate::account::PARTICLES_MAGIC {
            return None;
        }
        let (w, h) = (rd16(d, 4) as usize, rd16(d, 6) as usize);
        let px = d.get(8..8 + w * h * 4)?;
        let mut o = 8 + w * h * 4;
        let n = rd16(d, o) as usize;
        o += 2;
        for _ in 0..n {
            let (x, y, sw, sh) = (rd16(d, o) as f32, rd16(d, o + 2) as f32, rd16(d, o + 4) as f32, rd16(d, o + 6) as f32);
            self.sprites.push([x / w as f32, y / h as f32, (x + sw) / w as f32, (y + sh) / h as f32]);
            o += 8;
        }
        let nt = rd16(d, o) as usize;
        o += 2;
        for i in 0..nt {
            let l = *d.get(o)? as usize;
            let name = String::from_utf8_lossy(d.get(o + 1..o + 1 + l)?).into_owned();
            o += 1 + l;
            let c = *d.get(o)? as usize;
            o += 1;
            let mut set = Vec::with_capacity(c);
            for k in 0..c {
                let s = rd16(d, o + 2 * k);
                if (s as usize) < self.sprites.len() {
                    set.push(s);
                }
            }
            o += 2 * c;
            let id = i as u16;
            match name.as_str() {
                "smoke" => self.id_smoke = id,
                "poof" => self.id_poof = id,
                "heart" => self.id_heart = id,
                "flame" => self.id_flame = id,
                "angry_villager" => self.id_angry = id,
                "happy_villager" => self.id_happy = id,
                "explosion" => self.id_explosion = id,
                "large_smoke" => self.id_large_smoke = id,
                "totem_of_undying" => self.id_totem = id,
                _ => {}
            }
            let mut k = kind_of(&name);
            if set.is_empty() && !matches!(k, K::Terrain | K::DustPillar | K::Crumble | K::Item | K::ExplosionSeed) {
                k = K::None;
            }
            self.kinds.push(k);
            self.always.push(override_limiter(&name));
            self.sets.push(set);
            self.names.push(name);
        }
        let (aw, ah) = (rd16(d, o), rd16(d, o + 2));
        let nb = u32::from_le_bytes(d.get(o + 4..o + 8)?.try_into().ok()?) as usize;
        o += 8;
        if aw as f32 == self.atlas_size.0 && ah as f32 == self.atlas_size.1 {
            self.block_tile = (0..nb).map(|i| rd16(d, o + 2 * i)).collect();
        } else {
            crate::warn!("particles.bin was made with another atlas.bin: block particles use the model's texture");
        }
        let Some(gl) = gl else { return Some(()) };
        unsafe {
            (gl.GenTextures)(1, &mut self.sheet);
            (gl.BindTexture)(TEXTURE_2D, self.sheet);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
            (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
        }
        Some(())
    }

    /// The sprite sheet's texture (0: none), to free when the particles are loaded again.
    pub fn sheet(&self) -> u32 {
        self.sheet
    }

    pub fn count(&self) -> usize {
        self.list.len()
    }

    // ------------------------------------------------------------ randomness

    fn next(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
    /// RandomSource.nextFloat.
    fn r(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn rd(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn rint(&mut self, n: i32) -> i32 {
        (self.next() % n.max(1) as u64) as i32
    }
    /// RandomSource.nextGaussian (Box-Muller).
    fn gauss(&mut self) -> f64 {
        let u = self.rd().max(1e-12);
        let v = self.rd();
        math::sqrt(-2.0 * ln(u)) * math::cos(2.0 * math::PI * v)
    }

    // ------------------------------------------------------------ making particles

    fn base(&mut self, x: f64, y: f64, z: f64, kind: K, ty: u16) -> P {
        let life = (4.0 / (self.r() * 0.9 + 0.1)) as i32;
        let size = 0.1 * (self.r() * 0.5 + 0.5) * 2.0;
        P {
            pos: [x, y, z],
            prev: [x, y, z],
            vel: [0.0; 3],
            start: [x, y, z],
            age: 0,
            life,
            gravity: 0.0,
            friction: 0.98,
            size,
            half: 0.1,
            height: 0.2,
            col: [1.0; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            kind,
            ty,
            layer: L_SHEET,
            flags: PHYSICS,
            size_mode: SZ_CONST,
            to: [1.0; 3],
            from: [1.0; 3],
        }
    }

    /// Particle(level, x, y, z, xa, ya, za): a random push about (xa, ya, za), a little up.
    fn base_v(&mut self, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64, kind: K, ty: u16) -> P {
        let mut p = self.base(x, y, z, kind, ty);
        p.vel = [
            xa + (self.r() * 2.0 - 1.0) as f64 * 0.4,
            ya + (self.r() * 2.0 - 1.0) as f64 * 0.4,
            za + (self.r() * 2.0 - 1.0) as f64 * 0.4,
        ];
        let speed = ((self.r() + self.r() + 1.0) * 0.15) as f64;
        let len = math::sqrt(p.vel[0] * p.vel[0] + p.vel[1] * p.vel[1] + p.vel[2] * p.vel[2]).max(1e-9);
        p.vel = [p.vel[0] / len * speed * 0.4, p.vel[1] / len * speed * 0.4 + 0.1, p.vel[2] / len * speed * 0.4];
        p
    }

    fn set_size(p: &mut P, w: f64, h: f64) {
        p.half = w / 2.0;
        p.height = h;
    }

    fn pick_sprite(&mut self, p: &mut P) {
        let n = self.sets.get(p.ty as usize).map_or(0, |s| s.len());
        if n > 0 {
            let i = self.rint(n as i32) as usize;
            p.uv = self.sprites[self.sets[p.ty as usize][i] as usize];
        }
    }

    fn sprite_from_age(&self, p: &mut P) {
        if let Some(set) = self.sets.get(p.ty as usize) {
            if !set.is_empty() {
                let i = (p.age.max(0) as usize * (set.len() - 1)) / p.life.max(1) as usize;
                p.uv = self.sprites[set[i.min(set.len() - 1)] as usize];
            }
        }
    }

    fn add(&mut self, p: P) {
        if self.list.len() < MAX {
            self.list.push(p);
        } else {
            // EvictingQueue: the oldest goes (roughly: the slots are reused in turn).
            let i = self.evict % MAX;
            self.list[i] = p;
            self.evict += 1;
        }
    }

    /// The atlas rectangle of a block state's particle texture.
    fn block_uv(&self, b: &Blocks, s: u16) -> Option<[f32; 4]> {
        let blk = b.state(s).block as usize;
        if let Some(&t) = self.block_tile.get(blk) {
            if t == 0xFFFF {
                return None;
            }
            return Some(self.tile_uv(t));
        }
        // Without particles.bin: the bounds of the model's first face.
        let q = b.model(s).first()?;
        let (mut u0, mut v0, mut u1, mut v1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
        for uv in q.uv.iter() {
            let (u, v) = (uv[0] as f32 / 65535.0, uv[1] as f32 / 65535.0);
            u0 = u0.min(u);
            v0 = v0.min(v);
            u1 = u1.max(u);
            v1 = v1.max(v);
        }
        Some([u0, v0, u1, v1])
    }

    fn tile_uv(&self, t: u16) -> [f32; 4] {
        let row = (self.atlas_size.0 / 16.0).max(1.0) as u32;
        let (x, y) = ((t as u32 % row) as f32 * 16.0, (t as u32 / row) as f32 * 16.0);
        let (w, h) = self.atlas_size;
        [x / w, y / h, (x + 16.0) / w, (y + 16.0) / h]
    }

    /// An item's icon in the atlas (its first layer, or its model's first face).
    fn item_uv(&self, b: &Blocks, gui: Option<&Gui>, item: i32) -> Option<[f32; 4]> {
        match gui?.icons.get(item as usize)? {
            Icon::Flat(layers) => Some(self.tile_uv(layers.first()?.0)),
            Icon::Model { first, .. } => {
                let q = b.quads.get(*first as usize)?;
                let (mut u0, mut v0, mut u1, mut v1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
                for uv in q.uv.iter() {
                    let (u, v) = (uv[0] as f32 / 65535.0, uv[1] as f32 / 65535.0);
                    u0 = u0.min(u);
                    v0 = v0.min(v);
                    u1 = u1.max(u);
                    v1 = v1.max(v);
                }
                Some([u0, v0, u1, v1])
            }
            _ => None,
        }
    }

    /// A quarter of the texture at a random place (TerrainParticle's uo, vo).
    fn sub_uv(&mut self, uv: [f32; 4]) -> [f32; 4] {
        let (uo, vo) = (self.r() * 3.0, self.r() * 3.0);
        let (du, dv) = (uv[2] - uv[0], uv[3] - uv[1]);
        [uv[0] + du * uo / 4.0, uv[1] + dv * vo / 4.0, uv[0] + du * (uo + 1.0) / 4.0, uv[1] + dv * (vo + 1.0) / 4.0]
    }

    /// TerrainParticle: a piece of the block's texture, tinted as the block is.
    fn terrain(&mut self, w: &World, b: &Blocks, s: u16, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) -> Option<P> {
        let kind = b.kind(s);
        if s == 0 || kind == blocks::AIR || b.of_state(s).name == "moving_piston" {
            return None;
        }
        let uv = self.block_uv(b, s)?;
        let mut p = self.base_v(x, y, z, xa, ya, za, K::Terrain, 0);
        p.layer = L_ATLAS;
        p.gravity = 1.0;
        let blk = b.of_state(s);
        let tint = if blk.name == "grass_block" {
            [255, 255, 255]
        } else {
            let (bx, by, bz) = (math::ifloor(x), math::ifloor(y), math::ifloor(z));
            match blk.tint {
                1 => w.colour(bx, by, bz, 0),
                2 => w.colour(bx, by, bz, 1),
                5 => w.colour(bx, by, bz, 2),
                0 => [255, 255, 255],
                n => blocks::tint_colour(n),
            }
        };
        p.col = [0.6 * tint[0] as f32 / 255.0, 0.6 * tint[1] as f32 / 255.0, 0.6 * tint[2] as f32 / 255.0, 1.0];
        p.size /= 2.0;
        p.uv = self.sub_uv(uv);
        Some(p)
    }

    /// ClientLevel.addDestroyBlockEffect: 4 x 4 x 4 pieces per box of the block's shape
    /// (at least 2 a side), each flying out from the middle.
    pub fn destroy(&mut self, w: &World, b: &Blocks, pos: [i32; 3], s: u16) {
        let k = b.kind(s);
        if s == 0 || k == blocks::AIR || k == blocks::WATER || k == blocks::LAVA {
            return;
        }
        let o = if b.of_state(s).offset & blocks::OFF_OUTLINE != 0 { b.offset_at(s, pos[0], pos[2]) } else { [0.0; 3] };
        let shape: Vec<[u8; 6]> = if b.shape(s).is_empty() { alloc::vec![[0, 0, 0, 16, 16, 16]] } else { b.shape(s).to_vec() };
        for bx in shape {
            let lo = [bx[0] as f64 / 16.0 + o[0], bx[1] as f64 / 16.0, bx[2] as f64 / 16.0 + o[2]];
            let d = [
                ((bx[3] - bx[0]) as f64 / 16.0).min(1.0),
                ((bx[4] - bx[1]) as f64 / 16.0).min(1.0),
                ((bx[5] - bx[2]) as f64 / 16.0).min(1.0),
            ];
            let n = [ceil_i(d[0] / 0.25).max(2), ceil_i(d[1] / 0.25).max(2), ceil_i(d[2] / 0.25).max(2)];
            for i in 0..n[0] {
                for j in 0..n[1] {
                    for kk in 0..n[2] {
                        let f = [(i as f64 + 0.5) / n[0] as f64, (j as f64 + 0.5) / n[1] as f64, (kk as f64 + 0.5) / n[2] as f64];
                        let p = [f[0] * d[0] + lo[0], f[1] * d[1] + lo[1], f[2] * d[2] + lo[2]];
                        if let Some(t) = self.terrain(
                            w,
                            b,
                            s,
                            pos[0] as f64 + p[0],
                            pos[1] as f64 + p[1],
                            pos[2] as f64 + p[2],
                            f[0] - 0.5,
                            f[1] - 0.5,
                            f[2] - 0.5,
                        ) {
                            self.add(t);
                        }
                    }
                }
            }
        }
    }

    /// ClientLevel.addBreakingParticles: one small piece on the face being dug, each tick.
    pub fn crack(&mut self, w: &World, b: &Blocks, pos: [i32; 3], face: i32) {
        let s = w.get(pos[0], pos[1], pos[2]);
        let k = b.kind(s);
        if s == 0 || k == blocks::AIR || k == blocks::WATER || k == blocks::LAVA {
            return;
        }
        let shape = b.shape(s);
        let (mut lo, mut hi) = ([1.0f64; 3], [0.0f64; 3]);
        for bx in shape {
            for a in 0..3 {
                lo[a] = lo[a].min(bx[a] as f64 / 16.0);
                hi[a] = hi[a].max(bx[a + 3] as f64 / 16.0);
            }
        }
        if shape.is_empty() {
            lo = [0.0; 3];
            hi = [1.0; 3];
        }
        let f = 0.1;
        let mut p = [0.0f64; 3];
        for a in 0..3 {
            p[a] = pos[a] as f64 + self.rd() * (hi[a] - lo[a] - f * 2.0) + f + lo[a];
        }
        match face {
            0 => p[1] = pos[1] as f64 + lo[1] - f,
            1 => p[1] = pos[1] as f64 + hi[1] + f,
            2 => p[2] = pos[2] as f64 + lo[2] - f,
            3 => p[2] = pos[2] as f64 + hi[2] + f,
            4 => p[0] = pos[0] as f64 + lo[0] - f,
            _ => p[0] = pos[0] as f64 + hi[0] + f,
        }
        if let Some(mut t) = self.terrain(w, b, s, p[0], p[1], p[2], 0.0, 0.0, 0.0) {
            // .setPower(0.2).scale(0.6)
            t.vel = [t.vel[0] * 0.2, (t.vel[1] - 0.1) * 0.2 + 0.1, t.vel[2] * 0.2];
            t.size *= 0.6;
            Self::set_size(&mut t, 0.2 * 0.6, 0.2 * 0.6);
            self.add(t);
        }
    }

    /// ClientLevel.addParticle: a particle of type `ty` (registry id), as its provider makes it.
    /// Far ones (over 32 blocks from the eye) are not made unless `force`.
    fn spawn(&mut self, g: &Game, gui: Option<&Gui>, eye: [f64; 3], ty: u16, opt: Opt, force: bool, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
        let Some(&kind) = self.kinds.get(ty as usize) else { return };
        if kind == K::None {
            return;
        }
        let (dx, dy, dz) = (x - eye[0], y - eye[1], z - eye[2]);
        let force = force || self.always.get(ty as usize).copied().unwrap_or(false);
        if !force && dx * dx + dy * dy + dz * dz > 1024.0 {
            return;
        }
        let (w, b) = (&g.world, &g.blocks);
        let mut p = match kind {
            K::Terrain | K::DustPillar | K::Crumble => {
                let Opt::Block(s) = opt else { return };
                let Some(mut p) = self.terrain(w, b, s, x, y, z, xa, ya, za) else { return };
                if kind == K::DustPillar {
                    p.vel = [self.gauss() / 30.0, ya + self.gauss() / 2.0, self.gauss() / 30.0];
                    p.life = self.rint(20) + 20;
                } else if kind == K::Crumble {
                    p.vel = [0.0; 3];
                    p.life = self.rint(10) + 1;
                }
                p
            }
            K::FallingDust => {
                let Opt::Block(s) = opt else { return };
                let mut p = self.base(x, y, z, kind, ty);
                let mc = b.of_state(s).map_colour;
                let f = self.r() * 0.4 + 0.6;
                p.col = [mc[0] as f32 / 255.0 * f, mc[1] as f32 / 255.0 * f, mc[2] as f32 / 255.0 * f, 1.0];
                p.size *= 0.75 * 0.9;
                p.life = (32.0 / (self.r() * 0.8 + 0.2)) as i32;
                p.flags |= BY_AGE;
                p.size_mode = SZ_GROW;
                p
            }
            K::Item => {
                let item = match opt {
                    Opt::Item(i) => i,
                    _ => {
                        let name = match self.names.get(ty as usize).map(|s| s.as_str()) {
                            Some("item_slime") => "slime_ball",
                            Some("item_snowball") => "snowball",
                            Some("item_cobweb") => "cobweb",
                            _ => return,
                        };
                        match b.items.iter().position(|n| n == name) {
                            Some(i) => i as i32,
                            None => return,
                        }
                    }
                };
                let Some(uv) = self.item_uv(b, gui, item) else { return };
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.vel = [p.vel[0] * 0.1 + xa, p.vel[1] * 0.1 + ya, p.vel[2] * 0.1 + za];
                p.gravity = 1.0;
                p.size /= 2.0;
                p.uv = self.sub_uv(uv);
                p.layer = L_ATLAS;
                p
            }
            K::Smoke | K::LargeSmoke | K::WhiteSmoke => {
                // BaseAshSmokeParticle(0.1, 0.1, 0.1, xa, ya, za, scale, colour 0.3, 8, -0.1, physics)
                let scale = if kind == K::LargeSmoke { 2.5 } else { 1.0 };
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.friction = 0.96;
                p.gravity = -0.1;
                p.flags |= SPEED_UP | BY_AGE;
                p.vel = [p.vel[0] * 0.1 + xa, p.vel[1] * 0.1 + ya, p.vel[2] * 0.1 + za];
                let c = if kind == K::WhiteSmoke { 0.7 + self.r() * 0.3 } else { self.r() * 0.3 };
                p.col = [c, c, c, 1.0];
                p.size *= 0.75 * scale;
                p.life = ((8.0 / (self.rd() * 0.8 + 0.2)) * scale as f64) as i32;
                p.life = p.life.max(1);
                p.size_mode = SZ_GROW;
                p
            }
            K::Flame | K::SmallFlame => {
                // RisingParticle; FlameParticle moves through blocks.
                let mut p = self.base_v(x, y, z, xa, ya, za, kind, ty);
                p.friction = 0.96;
                p.vel = [p.vel[0] * 0.01 + xa, p.vel[1] * 0.01 + ya, p.vel[2] * 0.01 + za];
                for a in 0..3 {
                    p.pos[a] += ((self.r() - self.r()) * 0.05) as f64;
                }
                p.prev = p.pos;
                p.life = (8.0 / (self.rd() * 0.8 + 0.2)) as i32 + 4;
                p.flags &= !PHYSICS;
                p.size_mode = SZ_FLAME;
                if kind == K::SmallFlame {
                    p.size *= 0.5;
                }
                self.pick_sprite(&mut p);
                p
            }
            K::Bubble => {
                let mut p = self.base(x, y, z, kind, ty);
                Self::set_size(&mut p, 0.02, 0.02);
                p.size *= self.r() * 0.6 + 0.2;
                p.vel = [
                    xa * 0.2 + ((self.r() * 2.0 - 1.0) * 0.02) as f64,
                    ya * 0.2 + ((self.r() * 2.0 - 1.0) * 0.02) as f64,
                    za * 0.2 + ((self.r() * 2.0 - 1.0) * 0.02) as f64,
                ];
                p.life = (8.0 / (self.r() * 0.8 + 0.2)) as i32;
                self.pick_sprite(&mut p);
                p
            }
            K::Splash | K::Rain => {
                // WaterDropParticle
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.vel[0] *= 0.3;
                p.vel[1] = (self.r() * 0.2 + 0.1) as f64;
                p.vel[2] *= 0.3;
                Self::set_size(&mut p, 0.01, 0.01);
                p.gravity = 0.06;
                p.life = (8.0 / (self.r() * 0.8 + 0.2)) as i32;
                if kind == K::Splash {
                    p.gravity = 0.04;
                    if ya == 0.0 && (xa != 0.0 || za != 0.0) {
                        p.vel = [xa, 0.1, za];
                    }
                }
                self.pick_sprite(&mut p);
                p
            }
            K::Crit | K::Magic | K::Damage => {
                // DamageIndicatorProvider: new CritParticle(x, y, z, xa, ya + 1, za).
                let ya = if kind == K::Damage { ya + 1.0 } else { ya };
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.friction = 0.7;
                p.gravity = 0.5;
                p.vel = [p.vel[0] * 0.1 + xa * 0.4, p.vel[1] * 0.1 + ya * 0.4, p.vel[2] * 0.1 + za * 0.4];
                let c = self.r() * 0.3 + 0.6;
                p.col = [c, c, c, 1.0];
                p.size *= 0.75;
                p.life = ((6.0 / (self.rd() * 0.8 + 0.6)) as i32).max(1);
                p.flags &= !PHYSICS;
                p.size_mode = SZ_GROW;
                if kind == K::Magic {
                    p.col[0] *= 0.3;
                    p.col[1] *= 0.8;
                }
                if kind == K::Damage {
                    p.life = 20;
                }
                self.pick_sprite(&mut p);
                self.tick_one(&mut p, w, b);
                p
            }
            K::Poof => {
                // ExplodeParticle
                let mut p = self.base_v(x, y, z, xa, ya, za, kind, ty);
                p.gravity = -0.1;
                p.friction = 0.9;
                p.vel = [
                    xa + ((self.r() * 2.0 - 1.0) * 0.05) as f64,
                    ya + ((self.r() * 2.0 - 1.0) * 0.05) as f64,
                    za + ((self.r() * 2.0 - 1.0) * 0.05) as f64,
                ];
                let c = self.r() * 0.3 + 0.7;
                p.col = [c, c, c, 1.0];
                let r1 = self.r();
                let r2 = self.r();
                p.size = 0.1 * (r1 * r2 * 6.0 + 1.0);
                p.life = (16.0 / (self.rd() * 0.8 + 0.2)) as i32 + 2;
                p.flags |= BY_AGE;
                p
            }
            K::Heart | K::Angry => {
                let y = if kind == K::Angry { y + 0.5 } else { y };
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.flags |= SPEED_UP;
                p.flags &= !PHYSICS;
                p.friction = 0.86;
                p.vel = [p.vel[0] * 0.01, p.vel[1] * 0.01 + 0.1, p.vel[2] * 0.01];
                p.size *= 1.5;
                p.life = 16;
                p.size_mode = SZ_GROW;
                self.pick_sprite(&mut p);
                p
            }
            K::Happy | K::Town => {
                // SuspendedTownParticle: drifts, its lifetime counting down.
                let mut p = self.base_v(x, y, z, xa, ya, za, kind, ty);
                let c = self.r() * 0.1 + 0.2;
                p.col = if kind == K::Happy { [1.0; 4] } else { [0.3, 0.5, 1.0, 1.0] };
                if kind == K::Town && self.names.get(ty as usize).map_or(false, |n| n == "mycelium") {
                    p.col = [c, c, c, 1.0];
                }
                Self::set_size(&mut p, 0.02, 0.02);
                p.size *= self.r() * 0.6 + 0.5;
                p.vel = [p.vel[0] * 0.02, p.vel[1] * 0.02, p.vel[2] * 0.02];
                p.life = (20.0 / (self.rd() * 0.8 + 0.2)) as i32;
                p.flags &= !PHYSICS;
                self.pick_sprite(&mut p);
                p
            }
            K::Note => {
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.friction = 0.66;
                p.flags |= SPEED_UP;
                p.vel = [p.vel[0] * 0.01, p.vel[1] * 0.01 + 0.2, p.vel[2] * 0.01];
                let c = xa as f32;
                let ch = |o: f32| (math::sin(((c + o) * 6.283_185_5) as f64) as f32 * 0.65 + 0.35).max(0.0);
                p.col = [ch(0.0), ch(1.0 / 3.0), ch(2.0 / 3.0), 1.0];
                p.size *= 1.5;
                p.life = 6;
                p.size_mode = SZ_GROW;
                self.pick_sprite(&mut p);
                p
            }
            K::Portal => {
                let mut p = self.base(x, y, z, kind, ty);
                p.vel = [xa, ya, za];
                p.size = 0.1 * (self.r() * 0.2 + 0.5);
                let c = self.r() * 0.6 + 0.4;
                p.col = [c * 0.9, c * 0.3, c, 1.0];
                p.life = (self.r() * 10.0) as i32 + 40;
                p.flags &= !PHYSICS;
                p.size_mode = SZ_PORTAL;
                self.pick_sprite(&mut p);
                p
            }
            K::Dust | K::DustTransition => {
                let (c, to, scale) = match opt {
                    Opt::Dust(c, s) => (c, c, s),
                    Opt::Transition(a, b2, s) => (a, b2, s),
                    _ => ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 1.0),
                };
                let mut p = self.base_v(x, y, z, xa, ya, za, kind, ty);
                p.friction = 0.96;
                p.flags |= SPEED_UP | BY_AGE;
                p.vel = [p.vel[0] * 0.1, p.vel[1] * 0.1, p.vel[2] * 0.1];
                let f = self.r() * 0.4 + 0.6;
                let mut rc = |v: f32| (self.r() * 0.2 + 0.8) * v * f;
                p.from = [rc(c[0]), rc(c[1]), rc(c[2])];
                p.to = if kind == K::DustTransition { to } else { p.from };
                p.col = [p.from[0], p.from[1], p.from[2], 1.0];
                p.size *= 0.75 * scale;
                p.life = (8.0 / (self.rd() * 0.8 + 0.2)) as i32;
                p.life = ((p.life as f32 * scale) as i32).max(1);
                p.size_mode = SZ_GROW;
                p
            }
            K::Explosion => {
                let mut p = self.base(x, y, z, kind, ty);
                p.life = 6 + self.rint(4);
                let c = self.r() * 0.6 + 0.4;
                p.col = [c, c, c, 1.0];
                p.size = 2.0 * (1.0 - xa as f32 * 0.5);
                p.flags = BY_AGE;
                p
            }
            K::ExplosionSeed => {
                let mut p = self.base(x, y, z, kind, ty);
                p.life = 8;
                p.flags = 0;
                p.size = 0.0;
                p
            }
            K::Lava => {
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.gravity = 0.75;
                p.friction = 0.999;
                p.vel[0] *= 0.8;
                p.vel[2] *= 0.8;
                p.vel[1] = (self.r() * 0.4 + 0.05) as f64;
                p.size *= self.r() * 2.0 + 0.2;
                p.life = (16.0 / (self.rd() * 0.8 + 0.2)) as i32;
                p.size_mode = SZ_LAVA;
                self.pick_sprite(&mut p);
                p
            }
            K::Campfire | K::Signal => {
                let mut p = self.base(x, y, z, kind, ty);
                p.size *= 3.0;
                Self::set_size(&mut p, 0.25, 0.25);
                p.life = if kind == K::Signal { self.rint(50) + 280 } else { self.rint(50) + 80 };
                p.gravity = 3.0e-6;
                p.vel = [xa, ya + (self.r() / 500.0) as f64, za];
                p.layer = L_BLEND;
                self.pick_sprite(&mut p);
                p
            }
            K::Sweep => {
                let mut p = self.base(x, y, z, kind, ty);
                p.life = 4;
                let c = self.r() * 0.6 + 0.4;
                p.col = [c, c, c, 1.0];
                p.size = 1.0 - xa as f32 * 0.5;
                p.flags = BY_AGE;
                p
            }
            K::Spell => {
                let (rx, rz) = (0.5 - self.rd(), 0.5 - self.rd());
                let mut p = self.base_v(x, y, z, rx, ya, rz, kind, ty);
                p.friction = 0.96;
                p.gravity = -0.1;
                p.flags |= SPEED_UP | BY_AGE;
                p.flags &= !PHYSICS;
                p.vel[1] *= 0.2;
                if xa == 0.0 && za == 0.0 {
                    p.vel[0] *= 0.1;
                    p.vel[2] *= 0.1;
                }
                p.size *= 0.75;
                p.life = (8.0 / (self.rd() * 0.8 + 0.2)) as i32;
                if let Opt::Color(c) = opt {
                    p.col = [c[1], c[2], c[3], c[0]];
                } else if self.names.get(ty as usize).map_or(false, |n| n == "witch") {
                    let f = self.r() * 0.5 + 0.35;
                    p.col = [f, 0.0, f, 1.0];
                }
                if p.col[3] < 1.0 {
                    p.layer = L_BLEND;
                }
                p
            }
            K::Cloud | K::Sneeze => {
                let mut p = self.base_v(x, y, z, 0.0, 0.0, 0.0, kind, ty);
                p.friction = 0.96;
                p.vel = [p.vel[0] * 0.1 + xa, p.vel[1] * 0.1 + ya, p.vel[2] * 0.1 + za];
                let c = 1.0 - self.r() * 0.3;
                p.col = if kind == K::Sneeze { [200.0 / 255.0, 50.0 / 255.0, 120.0 / 255.0, 0.4] } else { [c, c, c, 1.0] };
                if kind == K::Sneeze {
                    p.layer = L_BLEND;
                }
                p.size *= 1.875;
                p.life = (8.0 / (self.rd() * 0.8 + 0.3)) as i32;
                p.life = ((p.life as f32 * 2.5) as i32).max(1);
                p.flags |= BY_AGE;
                p.size_mode = SZ_GROW;
                p
            }
            K::Drip => {
                let name = self.names.get(ty as usize).cloned().unwrap_or_default();
                let mut p = self.base(x, y, z, kind, ty);
                Self::set_size(&mut p, 0.01, 0.01);
                p.gravity = if name.starts_with("dripping_") { 0.02 } else { 0.06 };
                p.life = if name.starts_with("dripping_") { 40 } else { (64.0 / (self.r() * 0.8 + 0.2)) as i32 };
                p.col = if name.contains("lava") {
                    [1.0, 0.2857143, 0.083333336, 1.0]
                } else if name.contains("honey") || name.contains("nectar") {
                    [0.582, 0.448, 0.082, 1.0]
                } else if name.contains("obsidian_tear") {
                    [0.511_718_75, 0.031_25, 0.890_625, 1.0]
                } else if name.contains("spore") {
                    [0.32, 0.5, 0.22, 1.0]
                } else {
                    [0.2, 0.3, 1.0, 1.0]
                };
                self.pick_sprite(&mut p);
                p
            }
            K::Generic | K::None => {
                let mut p = self.base_v(x, y, z, xa, ya, za, kind, ty);
                p.flags |= BY_AGE;
                p
            }
        };
        if p.flags & BY_AGE != 0 {
            self.sprite_from_age(&mut p);
        }
        self.add(p);
    }

    // ------------------------------------------------------------ the network

    /// Reads a ParticleOptions: the type and its options (None: an option this client cannot
    /// read past).
    fn read_options(&self, r: &mut In) -> Option<(u16, Opt)> {
        let ty = r.varint()?;
        let name = self.names.get(ty as usize)?.as_str();
        let opt = match name {
            "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => Opt::Block(r.varint()? as u16),
            "dust" => {
                let c = r.i32()? as u32;
                Opt::Dust(rgb(c), r.f32()?)
            }
            "dust_color_transition" => {
                let a = r.i32()? as u32;
                let b = r.i32()? as u32;
                Opt::Transition(rgb(a), rgb(b), r.f32()?)
            }
            "effect" | "instant_effect" => {
                let c = r.i32()? as u32;
                let _power = r.f32()?;
                let c3 = rgb(c);
                Opt::Color([1.0, c3[0], c3[1], c3[2]])
            }
            "entity_effect" | "tinted_leaves" | "flash" => {
                let c = r.i32()? as u32;
                let c3 = rgb(c);
                Opt::Color([(c >> 24) as f32 / 255.0, c3[0], c3[1], c3[2]])
            }
            "dragon_breath" | "sculk_charge" => {
                r.f32()?;
                Opt::None
            }
            "geyser" | "geyser_plume" => {
                r.i32()?;
                Opt::None
            }
            "geyser_base" | "geyser_poof" => {
                r.i32()?;
                r.f32()?;
                Opt::None
            }
            "shriek" => {
                r.varint()?;
                Opt::None
            }
            "trail" => {
                r.take(24 + 4)?;
                r.varint()?;
                Opt::None
            }
            "vibration" => {
                // PositionSource: a block (position) or an entity (id, y offset); then arrival ticks.
                match r.varint()? {
                    0 => {
                        r.i64()?;
                    }
                    _ => {
                        r.varint()?;
                        r.f32()?;
                    }
                }
                r.varint()?;
                Opt::None
            }
            "item" => {
                // ItemStackTemplate: the item, a count, and a component patch.
                let item = r.varint()?;
                r.varint()?;
                let (add, remove) = (r.varint()?, r.varint()?);
                for _ in 0..add {
                    let kind = r.varint()?;
                    crate::client::skip_component(kind, r)?;
                }
                for _ in 0..remove {
                    r.varint()?;
                }
                Opt::Item(item)
            }
            "sulfur_cube_goo" => return None,
            _ => Opt::None,
        };
        Some((ty as u16, opt))
    }

    /// Takes what the packet handlers queued: particles, explosions, entity and level events.
    /// Returns the explosion's push on the player, if any.
    pub fn handle(&mut self, g: &mut Game, gui: Option<&Gui>, sizes: &[(f32, f32)], eye: [f64; 3]) -> Option<[f64; 3]> {
        let effects = core::mem::take(&mut g.particle_events);
        let mut push = None;
        for e in effects.iter() {
            match e {
                Effect::Packet(id, body) if *id == crate::ids::play::C_LEVEL_PARTICLES => {
                    let _ = self.level_particles(g, gui, eye, body);
                }
                Effect::Packet(_, body) => {
                    if let Some(k) = self.explode(g, gui, eye, body) {
                        push = Some(k);
                    }
                }
                Effect::Destroy(pos, s) => self.destroy(&g.world, &g.blocks, *pos, *s),
                Effect::Level(kind, pos, data) => self.level_event(g, gui, eye, *kind, *pos, *data),
                Effect::Entity(ev, pos, kind) => self.entity_event(g, gui, eye, sizes, *ev, *pos, *kind),
            }
        }
        // Give the queue's buffer back (no allocation next time).
        let mut effects = effects;
        effects.clear();
        if g.particle_events.is_empty() {
            g.particle_events = effects;
        }
        push
    }

    /// ClientboundLevelParticlesPacket: the particle, overrideLimiter, alwaysShow, the place,
    /// the spread and the speeds (each axis), the count (a VarInt) and the randomization.
    #[allow(clippy::type_complexity)]
    fn read_level_particles(&self, body: &[u8]) -> Option<(u16, Opt, bool, bool, [f64; 3], [f64; 3], [f64; 3], i32, i32)> {
        let mut r = In::new(body);
        let (ty, opt) = self.read_options(&mut r)?;
        let force = r.bool()?;
        let always = r.bool()?;
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        let dist = [r.f32()? as f64, r.f32()? as f64, r.f32()? as f64];
        let speed = [r.f32()? as f64, r.f32()? as f64, r.f32()? as f64];
        let count = r.varint()?;
        let random = r.varint().unwrap_or(0);
        Some((ty, opt, force, always, pos, dist, speed, count, random))
    }

    /// ClientPacketListener.handleParticleEvent.
    fn level_particles(&mut self, g: &Game, gui: Option<&Gui>, eye: [f64; 3], body: &[u8]) -> Option<()> {
        let (ty, opt, force, always, [x, y, z], dist, speed, count, random) = self.read_level_particles(body)?;
        if crate::sys::env("PENRYN_DEBUG").is_some() {
            crate::say!("particles: {} x{} at {:.1} {:.1} {:.1} ({} alive)", self.names.get(ty as usize).map_or("?", |n| n.as_str()), count, x, y, z, self.list.len());
        }
        // alwaysShow only matters under the "decreased" particles option, which is not here.
        let _ = always;
        // RandomizationType's ids, out of range read as DEFAULT (ByIdMap's ZERO strategy).
        let random = if (0..=2).contains(&random) { random } else { 0 };
        if count == 0 {
            self.spawn(g, gui, eye, ty, opt, force, x, y, z, speed[0] * dist[0], speed[1] * dist[1], speed[2] * dist[2]);
            return Some(());
        }
        for _ in 0..count.min(MAX as i32) {
            if random != 0 {
                let o = [self.rd() * dist[0], self.rd() * dist[1], self.rd() * dist[2]];
                let mut v = speed;
                if random == 2 {
                    v = [v[0] * self.rd(), v[1] * self.rd(), v[2] * self.rd()];
                }
                self.spawn(g, gui, eye, ty, opt, force, x + o[0], y + o[1], z + o[2], v[0], v[1], v[2]);
            } else {
                let o = [self.gauss() * dist[0], self.gauss() * dist[1], self.gauss() * dist[2]];
                let v = [self.gauss() * speed[0], self.gauss() * speed[1], self.gauss() * speed[2]];
                self.spawn(g, gui, eye, ty, opt, force, x + o[0], y + o[1], z + o[2], v[0], v[1], v[2]);
            }
        }
        Some(())
    }

    /// ClientPacketListener.handleExplosion: the sound, the big particle, block particles over
    /// the next tick (ClientExplosionTracker), and the push on the player.
    fn explode(&mut self, g: &mut Game, gui: Option<&Gui>, eye: [f64; 3], body: &[u8]) -> Option<[f64; 3]> {
        let mut r = In::new(body);
        let c = [r.f64()?, r.f64()?, r.f64()?];
        let radius = r.f32()?;
        let count = r.i32()?;
        let push = if r.bool()? { Some([r.f64()?, r.f64()?, r.f64()?]) } else { None };
        let (ty, opt) = self.read_options(&mut r)?;
        // The sound: a registry id + 1, or 0 and an inline name with an optional range.
        let holder = r.varint()?;
        let sound = if holder == 0 {
            let n = r.string()?;
            if r.bool()? {
                r.f32()?;
            }
            crate::client::SoundRef::Name(String::from(n.trim_start_matches("minecraft:")))
        } else {
            crate::client::SoundRef::Id(holder as u32 - 1)
        };
        let n = r.varint()?;
        let mut infos = Vec::new();
        for _ in 0..n.clamp(0, 64) {
            let (pty, popt) = self.read_options(&mut r)?;
            let scaling = r.f32()?;
            let speed = r.f32()?;
            let weight = r.varint()?;
            infos.push((pty, popt, scaling, speed, weight.max(0)));
        }
        let play_sound = r.bool().unwrap_or(true);
        if crate::sys::env("PENRYN_DEBUG").is_some() {
            crate::say!("particles: explosion at {:.1} {:.1} {:.1}, radius {}, {} blocks, {} block particle kinds", c[0], c[1], c[2], radius, count, infos.len());
        }
        if play_sound {
            let pitch = (1.0 + (self.r() - self.r()) * 0.2) * 0.7;
            g.sounds.push(crate::client::Sound { sound, pos: Some(c), volume: 4.0, pitch, source: 4 });
        }
        self.spawn(g, gui, eye, ty, opt, true, c[0], c[1], c[2], 1.0, 0.0, 0.0);
        if !infos.is_empty() && count > 0 {
            self.explosions.push((c, radius, count, infos));
        }
        push
    }

    fn level_event(&mut self, g: &Game, gui: Option<&Gui>, eye: [f64; 3], kind: i32, pos: [i32; 3], data: i32) {
        let (x, y, z) = (pos[0] as f64, pos[1] as f64, pos[2] as f64);
        match kind {
            // PARTICLES_MOBBLOCK_SPAWN: smoke and flames all over the block.
            2004 => {
                for _ in 0..20 {
                    let (a, b, c) = (x + 0.5 + (self.rd() - 0.5) * 2.0, y + 0.5 + (self.rd() - 0.5) * 2.0, z + 0.5 + (self.rd() - 0.5) * 2.0);
                    self.spawn(g, gui, eye, self.id_smoke, Opt::None, false, a, b, c, 0.0, 0.0, 0.0);
                    self.spawn(g, gui, eye, self.id_flame, Opt::None, false, a, b, c, 0.0, 0.0, 0.0);
                }
            }
            // PARTICLES_SHOOT_SMOKE (dispensers): smoke out of the front.
            2000 => {
                let d = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]][data.clamp(0, 5) as usize];
                for _ in 0..10 {
                    let f = self.rd() * 0.2 + 0.01;
                    let p = [x + 0.5 + d[0] as f64 * 0.6, y + 0.5 + d[1] as f64 * 0.6, z + 0.5 + d[2] as f64 * 0.6];
                    let v = [d[0] as f64 * f + self.gauss() * 0.01, d[1] as f64 * f + self.gauss() * 0.01, d[2] as f64 * f + self.gauss() * 0.01];
                    self.spawn(g, gui, eye, self.id_smoke, Opt::None, false, p[0], p[1], p[2], v[0], v[1], v[2]);
                }
            }
            // PARTICLES_AND_SOUND_PLANT_GROWTH (bone meal): happy villagers in the block.
            1505 => {
                let n = if data == 0 { 15 } else { data.min(64) };
                for _ in 0..n {
                    let v = [self.gauss() * 0.02, self.gauss() * 0.02, self.gauss() * 0.02];
                    let (a, b, c) = (x + self.rd(), y + self.rd(), z + self.rd());
                    self.spawn(g, gui, eye, self.id_happy, Opt::None, false, a, b, c, v[0], v[1], v[2]);
                }
            }
            // LAVA_FIZZ: large smoke over the block.
            1501 => {
                for _ in 0..8 {
                    let (a, c) = (x + self.rd(), z + self.rd());
                    self.spawn(g, gui, eye, self.id_large_smoke, Opt::None, false, a, y + 1.2, c, 0.0, 0.0, 0.0);
                }
            }
            _ => {}
        }
    }

    /// Entity.handleEntityEvent's particles: death and spawn poofs, love and taming hearts,
    /// villagers' moods.
    fn entity_event(&mut self, g: &Game, gui: Option<&Gui>, eye: [f64; 3], sizes: &[(f32, f32)], ev: u8, pos: [f64; 3], kind: i32) {
        let (w, h) = sizes.get(kind.max(0) as usize).map_or((0.6, 1.8), |s| (s.0 as f64, s.1 as f64));
        // Villager's own events (the minecraft:entity_type id of villager in 26.3).
        let villager = kind == ENTITY_VILLAGER;
        let (ty, n, dy) = match ev {
            60 | 20 => (self.id_poof, 20, 0.0),
            18 | 7 => (self.id_heart, 7, 0.5),
            6 => (self.id_smoke, 7, 0.5),
            // Villager.addParticlesAroundSelf: 5, a block higher.
            12 if villager => (self.id_heart, 5, 1.0),
            13 if villager => (self.id_angry, 5, 1.0),
            14 if villager => (self.id_happy, 5, 1.0),
            // LivingEntity.handleEntityEvent 35 (a totem of undying saved the entity): 30 totems
            // over the body, flung faster than the poofs.
            35 => (self.id_totem, 30, 0.0),
            _ => return,
        };
        let speed = if ev == 35 { 0.25 } else { 0.02 };
        for _ in 0..n {
            let v = [self.gauss() * speed, self.gauss() * speed, self.gauss() * speed];
            let x = pos[0] + w * (2.0 * self.rd() - 1.0);
            let y = pos[1] + h * self.rd() + dy;
            let z = pos[2] + w * (2.0 * self.rd() - 1.0);
            if ev == 20 {
                // Mob.spawnAnim: the poof moves out from where it starts.
                self.spawn(g, gui, eye, ty, Opt::None, false, x - v[0] * 10.0, y - v[1] * 10.0, z - v[2] * 10.0, v[0], v[1], v[2]);
            } else {
                self.spawn(g, gui, eye, ty, Opt::None, false, x, y, z, v[0], v[1], v[2]);
            }
        }
    }

    // ------------------------------------------------------------ the tick

    /// The 20 Hz tick: every particle moves, ages and goes; the local player's own particles.
    pub fn tick(&mut self, g: &Game, gui: Option<&Gui>, fx: &PlayerFx) {
        // Explosion emitters and the explosions' block particles make particles of their own.
        let mut i = 0;
        while i < self.list.len() {
            if self.list[i].kind == K::ExplosionSeed {
                let (x, y, z) = (self.list[i].pos[0], self.list[i].pos[1], self.list[i].pos[2]);
                let f = self.list[i].age as f64 / self.list[i].life as f64;
                for _ in 0..6 {
                    let o = [(self.rd() - self.rd()) * 4.0, (self.rd() - self.rd()) * 4.0, (self.rd() - self.rd()) * 4.0];
                    self.spawn(g, gui, fx.eye, self.id_explosion, Opt::None, true, x + o[0], y + o[1], z + o[2], f, 0.0, 0.0);
                }
            }
            if self.list[i].kind == K::Lava && self.r() > self.list[i].age as f32 / self.list[i].life as f32 {
                let p = &self.list[i];
                let (pp, v) = (p.pos, p.vel);
                self.spawn(g, gui, fx.eye, self.id_smoke, Opt::None, false, pp[0], pp[1], pp[2], v[0], v[1], v[2]);
            }
            i += 1;
        }
        self.explosion_particles(g, gui, fx.eye);
        let (w, b) = (&g.world, &g.blocks);
        let mut list = core::mem::take(&mut self.list);
        let mut j = 0;
        while j < list.len() {
            if self.tick_one(&mut list[j], w, b) {
                j += 1;
            } else {
                list.swap_remove(j);
            }
        }
        self.list = list;
        // Entity.spawnSprintParticle: a piece of the block underfoot, kicked back.
        if fx.sprinting && fx.on_ground && !fx.in_fluid {
            let (bx, by, bz) = (math::ifloor(fx.pos[0]), math::ifloor(fx.pos[1] - 0.2), math::ifloor(fx.pos[2]));
            let s = w.get(bx, by, bz);
            if s != 0 && b.kind(s) != blocks::AIR && b.kind(s) != blocks::WATER && b.kind(s) != blocks::LAVA && b.kind(s) != blocks::INVISIBLE_SOLID {
                let x = fx.pos[0] + (self.rd() - 0.5) * 0.6;
                let z = fx.pos[2] + (self.rd() - 0.5) * 0.6;
                if let Some(mut p) = self.terrain(w, b, s, x, fx.pos[1] + 0.1, z, fx.vel[0] * -4.0, 1.5, fx.vel[2] * -4.0) {
                    // The block's own particle option at the block below (not where it lands).
                    p.prev = p.pos;
                    self.add(p);
                }
            }
        }
        // Consumable.emitParticlesAndSounds: crumbs of the food every 4 ticks once a fifth eaten.
        if let Some((item, t, d)) = fx.eating {
            let name = b.items.get(item as usize).map(|s| s.as_str()).unwrap_or("");
            let drink = name.contains("potion") || name.contains("bottle") || name == "milk_bucket";
            let remaining = d as i32 - t as i32;
            if !drink && t as i32 > (d as f32 * 0.21875) as i32 && remaining % 4 == 0 {
                self.eat_particles(g, gui, fx, item);
            }
        }
    }

    /// LivingEntity.spawnItemParticles: 5 crumbs of the item from the mouth.
    fn eat_particles(&mut self, g: &Game, gui: Option<&Gui>, fx: &PlayerFx, item: i32) {
        let Some(uv) = self.item_uv(&g.blocks, gui, item) else { return };
        let (yr, pr) = (math::to_rad(fx.yaw as f64), math::to_rad(fx.pitch as f64));
        // Vec3.xRot(-pitch) then yRot(-yaw).
        let rot = |v: [f64; 3]| {
            let (c, s) = (math::cos(-pr), math::sin(-pr));
            let v = [v[0], v[1] * c + v[2] * s, v[2] * c - v[1] * s];
            let (c, s) = (math::cos(-yr), math::sin(-yr));
            [v[0] * c + v[2] * s, v[1], v[2] * c - v[0] * s]
        };
        for _ in 0..5 {
            let v = rot([(self.rd() - 0.5) * 0.1, self.rd() * 0.1 + 0.1, 0.0]);
            let o = rot([(self.rd() - 0.5) * 0.3, -self.rd() * 0.6 - 0.3, 0.6]);
            let mut p = self.base_v(fx.eye[0] + o[0], fx.eye[1] + o[1], fx.eye[2] + o[2], 0.0, 0.0, 0.0, K::Item, 0);
            p.vel = [p.vel[0] * 0.1 + v[0], p.vel[1] * 0.1 + v[1] + 0.05, p.vel[2] * 0.1 + v[2]];
            p.gravity = 1.0;
            p.size /= 2.0;
            p.uv = self.sub_uv(uv);
            p.layer = L_ATLAS;
            self.add(p);
        }
    }

    fn explosion_particles(&mut self, g: &Game, gui: Option<&Gui>, eye: [f64; 3]) {
        if self.explosions.is_empty() {
            return;
        }
        let ex = core::mem::take(&mut self.explosions);
        let total: i32 = ex.iter().fold(0i32, |t, e| t.saturating_add(e.2));
        for _ in 0..total.min(EXPLOSION_PER_TICK) {
            let mut pick = self.rint(total.max(1));
            let Some(e) = ex.iter().find(|e| {
                pick -= e.2;
                pick < 0
            }) else {
                continue;
            };
            let mut d = [self.r() as f64 * 2.0 - 1.0, self.r() as f64 * 2.0 - 1.0, self.r() as f64 * 2.0 - 1.0];
            let len = math::sqrt(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
            if len < 1e-4 {
                continue;
            }
            d = [d[0] / len, d[1] / len, d[2] / len];
            let dist = cbrt(self.r() as f64) as f32 * e.1;
            let p = [e.0[0] + d[0] * dist as f64, e.0[1] + d[1] * dist as f64, e.0[2] + d[2] * dist as f64];
            let s = g.world.get(math::ifloor(p[0]), math::ifloor(p[1]), math::ifloor(p[2]));
            if s != 0 && g.blocks.kind(s) != blocks::AIR {
                continue;
            }
            let wsum: i32 = e.3.iter().map(|i| i.4).sum();
            let mut wp = self.rint(wsum.max(1));
            let Some(info) = e.3.iter().find(|i| {
                wp -= i.4;
                wp < 0
            }) else {
                continue;
            };
            let f = 0.5 / (dist / e.1 + 0.1) * self.r() * self.r() + 0.3;
            let at = [p[0] + d[0] * info.2 as f64, p[1] + d[1] * info.2 as f64, p[2] + d[2] * info.2 as f64];
            let v = (f * info.3) as f64;
            self.spawn(g, gui, eye, info.0, info.1, false, at[0], at[1], at[2], d[0] * v, d[1] * v, d[2] * v);
        }
    }

    /// One particle's tick, as its class does it; false when it is gone.
    fn tick_one(&self, p: &mut P, w: &World, b: &Blocks) -> bool {
        p.prev = p.pos;
        match p.kind {
            K::Happy | K::Town => {
                // if (lifetime-- <= 0) remove
                p.life -= 1;
                if p.life < 0 {
                    return false;
                }
                p.pos = [p.pos[0] + p.vel[0], p.pos[1] + p.vel[1], p.pos[2] + p.vel[2]];
                p.vel = [p.vel[0] * 0.99, p.vel[1] * 0.99, p.vel[2] * 0.99];
                return true;
            }
            K::Bubble => {
                p.life -= 1;
                if p.life < 0 {
                    return false;
                }
                p.vel[1] += 0.002;
                self.do_move(p, w, b);
                p.vel = [p.vel[0] * 0.85, p.vel[1] * 0.85, p.vel[2] * 0.85];
                let s = w.get(math::ifloor(p.pos[0]), math::ifloor(p.pos[1]), math::ifloor(p.pos[2]));
                return b.kind(s) == blocks::WATER || b.has_fluid(s);
            }
            K::Splash | K::Rain | K::Drip => {
                p.age += 1;
                if p.age >= p.life {
                    return false;
                }
                p.vel[1] -= p.gravity as f64;
                self.do_move(p, w, b);
                p.vel = [p.vel[0] * 0.98, p.vel[1] * 0.98, p.vel[2] * 0.98];
                if p.flags & ON_GROUND != 0 {
                    if p.kind == K::Drip || (p.age & 1) == 0 {
                        return false;
                    }
                    p.vel[0] *= 0.7;
                    p.vel[2] *= 0.7;
                }
                let s = w.get(math::ifloor(p.pos[0]), math::ifloor(p.pos[1]), math::ifloor(p.pos[2]));
                return !(b.kind(s) == blocks::WATER && p.kind != K::Drip);
            }
            K::Portal => {
                p.age += 1;
                if p.age >= p.life {
                    return false;
                }
                let f = p.age as f64 / p.life as f64;
                let g2 = 1.0 - (-f + f * f * 2.0);
                p.pos = [p.start[0] + p.vel[0] * g2, p.start[1] + p.vel[1] * g2 + (1.0 - f), p.start[2] + p.vel[2] * g2];
                return true;
            }
            K::Campfire | K::Signal => {
                p.age += 1;
                if p.age >= p.life || p.col[3] <= 0.0 {
                    return false;
                }
                // xd += random / 5000 * (random ? 1 : -1), the same for zd (deterministic here).
                let jx = ((p.age * 7919 + p.ty as i32) % 1000) as f64 / 1000.0;
                let jz = ((p.age * 104_729 + 17) % 1000) as f64 / 1000.0;
                p.vel[0] += jx / 5000.0 * if p.age & 1 == 0 { 1.0 } else { -1.0 };
                p.vel[2] += jz / 5000.0 * if p.age & 2 == 0 { 1.0 } else { -1.0 };
                p.vel[1] -= p.gravity as f64;
                self.do_move(p, w, b);
                if p.age >= p.life - 60 && p.col[3] > 0.01 {
                    p.col[3] -= 0.015;
                }
                return true;
            }
            K::Explosion | K::Sweep => {
                p.age += 1;
                if p.age >= p.life {
                    return false;
                }
                self.sprite_from_age(p);
                return true;
            }
            K::ExplosionSeed => {
                p.age += 1;
                return p.age < p.life;
            }
            _ => {}
        }
        // Particle.tick
        let age = p.age;
        p.age += 1;
        if age >= p.life {
            return false;
        }
        p.vel[1] -= 0.04 * p.gravity as f64;
        self.do_move(p, w, b);
        if p.flags & SPEED_UP != 0 && p.pos[1] == p.prev[1] {
            p.vel[0] *= 1.1;
            p.vel[2] *= 1.1;
        }
        let f = p.friction as f64;
        p.vel = [p.vel[0] * f, p.vel[1] * f, p.vel[2] * f];
        if p.flags & ON_GROUND != 0 {
            p.vel[0] *= 0.7;
            p.vel[2] *= 0.7;
        }
        match p.kind {
            K::Crit | K::Magic | K::Damage => {
                p.col[1] *= 0.96;
                p.col[2] *= 0.9;
            }
            K::Lava => {}
            K::FallingDust => {
                p.vel[1] = (p.vel[1] - 0.003).max(-0.14);
            }
            _ => {}
        }
        if p.flags & BY_AGE != 0 {
            self.sprite_from_age(p);
        }
        true
    }

    /// Particle.move: against the blocks' collision boxes when it has physics; a hit from above
    /// or below stops it for good (stoppedByCollision).
    fn do_move(&self, p: &mut P, w: &World, b: &Blocks) {
        if p.flags & STOPPED != 0 {
            return;
        }
        let want = p.vel;
        let mut d = want;
        if p.flags & PHYSICS != 0 && (d[0] != 0.0 || d[1] != 0.0 || d[2] != 0.0) && d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 10000.0 {
            let mut lo = [p.pos[0] - p.half, p.pos[1], p.pos[2] - p.half];
            let mut hi = [p.pos[0] + p.half, p.pos[1] + p.height, p.pos[2] + p.half];
            let order = if d[0].abs() < d[2].abs() { [1, 2, 0] } else { [1, 0, 2] };
            for &a in order.iter() {
                d[a] = collide_axis(w, b, &lo, &hi, a, d[a]);
                lo[a] += d[a];
                hi[a] += d[a];
            }
        }
        if d[0] != 0.0 || d[1] != 0.0 || d[2] != 0.0 {
            p.pos = [p.pos[0] + d[0], p.pos[1] + d[1], p.pos[2] + d[2]];
        }
        if want[1].abs() >= 1e-5 && d[1].abs() < 1e-5 {
            p.flags |= STOPPED;
        }
        if want[1] != d[1] && want[1] < 0.0 {
            p.flags |= ON_GROUND;
        } else {
            p.flags &= !ON_GROUND;
        }
        if want[0] != d[0] {
            p.vel[0] = 0.0;
        }
        if want[2] != d[2] {
            p.vel[2] = 0.0;
        }
    }

    // ------------------------------------------------------------ drawing

    /// Draws the particles as quads facing the camera (SingleQuadParticle, LOOKAT_XYZ), from
    /// the eye; `view` is the camera's rotation.
    pub fn draw(&mut self, gl: &Gl, view: &[f32; 16], eye: [f64; 3], partial: f32, atlas: u32, lit: &crate::lightmap::Lit) {
        if self.list.is_empty() {
            return;
        }
        for v in self.verts.iter_mut() {
            v.clear();
        }
        // The camera's right and up in the world (the rows of its rotation).
        let right = [view[0], view[4], view[8]];
        let up = [view[1], view[5], view[9]];
        let fwd = [-view[2], -view[6], -view[10]];
        let pt = partial as f64;
        // Most particles sit in the same block as the one drawn before: its light is kept.
        let mut last: ([i32; 3], u8, [f32; 3]) = ([i32::MIN; 3], 0, [1.0; 3]);
        for p in self.list.iter() {
            if p.kind == K::ExplosionSeed {
                continue;
            }
            let c = [
                (p.prev[0] + (p.pos[0] - p.prev[0]) * pt - eye[0]) as f32,
                (p.prev[1] + (p.pos[1] - p.prev[1]) * pt - eye[1]) as f32,
                (p.prev[2] + (p.pos[2] - p.prev[2]) * pt - eye[2]) as f32,
            ];
            // Behind the camera: not drawn.
            if c[0] * fwd[0] + c[1] * fwd[1] + c[2] * fwd[2] < -1.0 {
                continue;
            }
            let t = ((p.age as f32 + partial) / p.life.max(1) as f32).clamp(0.0, 1.0);
            let s = match p.size_mode {
                SZ_GROW => p.size * ((p.age as f32 + partial) / p.life.max(1) as f32 * 32.0).clamp(0.0, 1.0),
                SZ_FLAME => p.size * (1.0 - t * t * 0.5),
                SZ_LAVA => p.size * (1.0 - t * t),
                SZ_PORTAL => {
                    let f = 1.0 - t;
                    p.size * (1.0 - f * f)
                }
                _ => p.size,
            };
            let mut col = p.col;
            if p.kind == K::DustTransition {
                let f = (p.age as f32 + partial) / (p.life as f32 + 1.0);
                for k in 0..3 {
                    col[k] = p.from[k] + (p.to[k] - p.from[k]) * f;
                }
            }
            let cc = [
                (col[0].clamp(0.0, 1.0) * 255.0) as u8,
                (col[1].clamp(0.0, 1.0) * 255.0) as u8,
                (col[2].clamp(0.0, 1.0) * 255.0) as u8,
                (col[3].clamp(0.0, 1.0) * 255.0) as u8,
            ];
            // SingleQuadParticle.getLightCoords: the light of the block it is in. Flames
            // brighten their block light as they age, lava and explosions are fully lit.
            let life = ((p.age as f32 + partial) / p.life.max(1) as f32).clamp(0.0, 1.0);
            let cell = [math::ifloor(c[0] as f64 + eye[0]), math::ifloor(c[1] as f64 + eye[1]), math::ifloor(c[2] as f64 + eye[2])];
            let boost = match p.kind {
                K::Flame | K::SmallFlame => (life * 15.0) as u8,
                K::Lava | K::Explosion => 15,
                _ => 0,
            };
            let lc = if last.0 == cell && last.1 == boost {
                last.2
            } else {
                let (sky, blk) = lit.world.light_at(cell[0], cell[1], cell[2]);
                let (sky, blk) = if p.kind == K::Lava || p.kind == K::Explosion { (15, 15) } else { (sky, (blk + boost).min(15)) };
                let l = lit.colour(sky, blk);
                last = (cell, boost, l);
                l
            };
            let cc = crate::lightmap::tint(cc, lc);
            let uv = p.uv;
            let out = &mut self.verts[p.layer as usize];
            for (sx, sy, u, v) in [(1.0f32, -1.0f32, uv[2], uv[3]), (1.0, 1.0, uv[2], uv[1]), (-1.0, 1.0, uv[0], uv[1]), (-1.0, -1.0, uv[0], uv[3])] {
                out.push(V {
                    p: [
                        c[0] + (right[0] * sx + up[0] * sy) * s,
                        c[1] + (right[1] * sx + up[1] * sy) * s,
                        c[2] + (right[2] * sx + up[2] * sy) * s,
                    ],
                    uv: [u, v],
                    c: cc,
                });
            }
        }
        unsafe {
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadIdentity)();
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(view.as_ptr());
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.Disable)(CULL_FACE);
            (gl.Enable)(TEXTURE_2D);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.1);
            (gl.Disable)(BLEND);
        }
        let draw = |v: &Vec<V>| unsafe {
            if v.is_empty() {
                return;
            }
            let base = v.as_ptr() as *const u8;
            (gl.VertexPointer)(3, FLOAT, 24, base as *const c_void);
            (gl.TexCoordPointer)(2, FLOAT, 24, base.add(12) as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, base.add(20) as *const c_void);
            (gl.DrawArrays)(QUADS, 0, v.len() as i32);
        };
        unsafe { (gl.BindTexture)(TEXTURE_2D, atlas) };
        draw(&self.verts[L_ATLAS as usize]);
        if self.sheet != 0 {
            unsafe { (gl.BindTexture)(TEXTURE_2D, self.sheet) };
            draw(&self.verts[L_SHEET as usize]);
            unsafe {
                (gl.Enable)(BLEND);
                (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
                (gl.AlphaFunc)(GREATER, 0.004);
                (gl.DepthMask)(0);
            }
            draw(&self.verts[L_BLEND as usize]);
            unsafe {
                (gl.DepthMask)(1);
                (gl.Disable)(BLEND);
            }
        }
        unsafe {
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(CULL_FACE);
        }
    }
}

/// How far a box can move along `axis` by `d` through the blocks' collision boxes.
fn collide_axis(w: &World, b: &Blocks, lo: &[f64; 3], hi: &[f64; 3], axis: usize, mut d: f64) -> f64 {
    if d == 0.0 {
        return 0.0;
    }
    let (a1, a2) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut clo = *lo;
    let mut chi = *hi;
    if d < 0.0 {
        clo[axis] += d;
    } else {
        chi[axis] += d;
    }
    for x in math::ifloor(clo[0])..=math::ifloor(chi[0]) {
        for y in math::ifloor(clo[1])..=math::ifloor(chi[1]) {
            for z in math::ifloor(clo[2])..=math::ifloor(chi[2]) {
                let s = w.get(x, y, z);
                if s == 0 || !b.collides(s) {
                    continue;
                }
                let cell = [x as f64, y as f64, z as f64];
                let mut clip = |olo: [f64; 3], ohi: [f64; 3]| {
                    if ohi[a1] <= lo[a1] + 1e-7 || olo[a1] >= hi[a1] - 1e-7 || ohi[a2] <= lo[a2] + 1e-7 || olo[a2] >= hi[a2] - 1e-7 {
                        return;
                    }
                    if d > 0.0 && olo[axis] >= hi[axis] - 1e-7 {
                        d = d.min(olo[axis] - hi[axis]);
                    } else if d < 0.0 && ohi[axis] <= lo[axis] + 1e-7 {
                        d = d.max(ohi[axis] - lo[axis]);
                    }
                };
                let boxes = b.collision(s);
                if boxes.is_empty() {
                    // A shape of its own rule (scaffolding, bamboo...): its outline.
                    for bx in b.shape(s) {
                        let f = |i: usize| bx[i] as f64 / 16.0;
                        clip([cell[0] + f(0), cell[1] + f(1), cell[2] + f(2)], [cell[0] + f(3), cell[1] + f(4), cell[2] + f(5)]);
                    }
                } else {
                    for bx in boxes {
                        let f = |i: usize| bx[i] as f64 / 32.0;
                        clip([cell[0] + f(0), cell[1] + f(1), cell[2] + f(2)], [cell[0] + f(3), cell[1] + f(4), cell[2] + f(5)]);
                    }
                }
            }
        }
    }
    d
}

/// The types registered with overrideLimiter (ParticleTypes).
fn override_limiter(name: &str) -> bool {
    matches!(
        name,
        "block_marker" | "geyser" | "geyser_base" | "geyser_poof" | "geyser_plume" | "damage_indicator" | "elder_guardian"
            | "explosion_emitter" | "explosion" | "gust" | "gust_emitter_large" | "gust_emitter_small" | "sonic_boom"
            | "sculk_charge" | "sculk_charge_pop" | "vibration" | "poof" | "spit" | "squid_ink" | "sweep_attack"
            | "campfire_cosy_smoke" | "campfire_signal_smoke" | "glow_squid_ink" | "glow" | "wax_on" | "wax_off"
            | "electric_spark" | "scrape" | "trial_spawner_detection" | "trial_spawner_detection_ominous"
            | "vault_connection" | "ominous_spawning"
    )
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 255) as f32 / 255.0, (c >> 8 & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

fn ceil_i(x: f64) -> i32 {
    let f = math::floor(x);
    if f < x {
        f as i32 + 1
    } else {
        f as i32
    }
}

/// The natural logarithm (for the Gaussian): the exponent, then atanh's series on the rest.
fn ln(x: f64) -> f64 {
    if x <= 0.0 {
        return -1e30;
    }
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | (1023u64 << 52));
    let y = (m - 1.0) / (m + 1.0);
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;
    let mut k = 1.0;
    for _ in 0..12 {
        sum += term / k;
        term *= y2;
        k += 2.0;
    }
    e as f64 * core::f64::consts::LN_2 + 2.0 * sum
}

fn cbrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = if x < 1.0 { 0.5 + x * 0.5 } else { x / 3.0 + 0.7 };
    for _ in 0..12 {
        g = (2.0 * g + x / (g * g)) / 3.0;
    }
    g
}

/// The selftest's particle checks, in a made-up world (stone floor at y 63).
pub fn selftest(b: &Blocks, w: &World) -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let yes = |v: bool| String::from(if v { "yes" } else { "no" });
    let mut p = Particles {
        list: Vec::new(),
        evict: 0,
        rng: 12345,
        sheet: 0,
        sprites: Vec::new(),
        sets: Vec::new(),
        kinds: Vec::new(),
        always: Vec::new(),
        names: Vec::new(),
        block_tile: Vec::new(),
        atlas_size: (1024.0, 1024.0),
        explosions: Vec::new(),
        verts: [Vec::new(), Vec::new(), Vec::new()],
        id_smoke: 0xFFFF,
        id_poof: 0xFFFF,
        id_heart: 0xFFFF,
        id_flame: 0xFFFF,
        id_angry: 0xFFFF,
        id_happy: 0xFFFF,
        id_explosion: 0xFFFF,
        id_large_smoke: 0xFFFF,
        id_totem: 0xFFFF,
    };
    let stone = (0..b.states.len() as u16).find(|&s| b.of_state(s).name == "stone").unwrap_or(1);
    p.destroy(w, b, [8, 64, 8], stone);
    out.push(("stone breaks into 64 particles", alloc::format!("{}", p.count()), "64"));
    let mut ticks = 0;
    let mut low = 1e9f64;
    while p.count() > 0 && ticks < 100 {
        let list = core::mem::take(&mut p.list);
        let mut keep = Vec::new();
        for mut q in list {
            if p.tick_one(&mut q, w, b) {
                low = low.min(q.pos[1]);
                keep.push(q);
            }
        }
        p.list = keep;
        ticks += 1;
    }
    out.push(("block particles gone within 40 ticks", yes(ticks <= 41), "yes"));
    out.push(("block particles stop on the floor", yes(low >= 64.0 - 1e-6), "yes"));
    // A torch-sized shape (one box under 4/16 a side): 2 x 2 x 2 at least per box.
    if let Some(torch) = (0..b.states.len() as u16).find(|&s| b.of_state(s).name == "torch") {
        p.list.clear();
        p.destroy(w, b, [8, 64, 8], torch);
        let n = b.shape(torch).len() * 2 * 2 * 3;
        out.push(("a torch breaks into 2x2x3 per box", yes(p.count() == n), "yes"));
    }
    if let Some(d) = crate::sys::read_file(&alloc::format!("{}/particles.bin", crate::data_dir())) {
        let ok = d.get(..4) == Some(&crate::account::PARTICLES_MAGIC[..]);
        out.push(("particles.bin magic", yes(ok), "yes"));
        // A flame packet as a 26.3 server sends it (the count is a VarInt).
        if p.parse(None, &d).is_some() {
            let body: [u8; 53] = [
                39, 1, 0, 192, 144, 62, 0, 0, 0, 0, 0, 64, 89, 64, 0, 0, 0, 0, 0, 64, 18, 0, 0, 0, 0, 0, 0, 62, 76, 204, 205, 62, 76,
                204, 205, 62, 76, 204, 205, 60, 35, 215, 10, 60, 35, 215, 10, 60, 35, 215, 10, 50, 0,
            ];
            let got = p.read_level_particles(&body).map(|(ty, _, f, _, pos, _, _, n, _)| {
                alloc::format!("{} {} {} {} {}", p.names[ty as usize], f, pos[0], pos[1], n)
            });
            out.push(("level particles packet", got.unwrap_or_default(), "flame true -1039.5 101 50"));
        }
        out.push(("ln and cbrt", alloc::format!("{:.4} {:.4}", ln(10.0), cbrt(27.0)), "2.3026 3.0000"));
    }
    out
}

