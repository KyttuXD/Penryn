//! The block table (blocks.bin, made by tools/make_assets.py from the player's own game):
//! every state's block, hardness, outline shape and baked model quads; every block's kind,
//! tint and tool rules; item names.

use alloc::string::String;
use alloc::vec::Vec;

pub const AIR: u8 = 0;
pub const SOLID: u8 = 1;
pub const CUTOUT: u8 = 2;
pub const PLANT: u8 = 3;
pub const WATER: u8 = 4;
pub const LAVA: u8 = 5;
pub const INVISIBLE_SOLID: u8 = 6;

pub const NEEDS_TOOL: u8 = 1;
pub const FULL_COLLISION: u8 = 2;
pub const REPLACEABLE: u8 = 4;
pub const OPAQUE_CUBE: u8 = 8;
/// A bubble column that pulls down (over magma).
pub const DRAG_DOWN: u8 = 16;
/// Holds a fluid (waterlogged, seagrass, kelp, bubble columns).
pub const HAS_FLUID: u8 = 32;
/// open=true (trapdoors, doors, gates).
pub const OPEN: u8 = 64;

/// Collision rules of their own (blocks with a dynamic shape in vanilla).
pub const C_BAMBOO: u8 = 1;
pub const C_SCAFFOLDING: u8 = 2;
pub const C_POWDER_SNOW: u8 = 3;
pub const C_OFFSET_OUTLINE: u8 = 4;
pub const C_OUTLINE: u8 = 5;

/// Offset kinds (BlockBehaviour.OffsetType): the model moves, the outline moves, and also down.
pub const OFF_MODEL: u8 = 1;
pub const OFF_OUTLINE: u8 = 2;
pub const OFF_VERTICAL: u8 = 4;

pub const Q_TINTED: u8 = 1;
pub const Q_CUTOUT: u8 = 2;
pub const Q_TRANSLUCENT: u8 = 4;
pub const Q_ON_EDGE: u8 = 16;

pub struct Block {
    pub kind: u8,
    /// 0 none, 1 grass, 2 foliage, 3 birch, 4 spruce, 5 water, 6 redstone, 7 stem.
    pub tint: u8,
    /// Tools that mine it fast: 1 pickaxe, 2 axe, 4 shovel, 8 hoe, 16 sword.
    pub mineable: u8,
    /// Tool tiers that cannot harvest it: 1 wooden, 2 stone, 4 copper, 8 iron, 16 gold,
    /// 32 diamond, 64 netherite.
    pub incorrect: u8,
    pub map_colour: [u8; 3],
    pub name: String,
    /// Its SoundType: break, step, place, hit and fall sound events (registry ids; 65535 none).
    pub sounds: [u16; 5],
    pub sound_volume: f32,
    pub sound_pitch: f32,
    /// Block.friction (0.6; ice 0.98), speedFactor (soul sand, honey 0.4), jumpFactor (honey 0.5).
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
    /// OFF_* bits, and the largest horizontal offset.
    pub offset: u8,
    pub max_offset: f32,
    /// getMaxVerticalOffset (0.2; small dripleaf 0.1).
    pub max_v_offset: f32,
}

#[derive(Clone, Copy)]
pub struct State {
    pub block: u16,
    /// Hardness x100; 65535 cannot be broken.
    pub hardness: u16,
    pub flags: u8,
    pub boxes: u8,
    pub first_box: u16,
    pub first_quad: u32,
    pub quads: u16,
    /// Vanilla's collision shape: boxes in `coll`, or a rule (C_*).
    pub first_coll: u32,
    pub colls: u8,
    pub coll_rule: u8,
    /// facing: 0 north, 1 south, 2 west, 3 east, 4 down, 5 up, 255 none.
    pub facing: u8,
}

/// A baked model face: four corners (position in 1/1024 block, atlas uv in 1/65535).
#[derive(Clone, Copy)]
pub struct Quad {
    pub pos: [[i16; 3]; 4],
    pub uv: [[u16; 2]; 4],
    /// Direction id of the face that hides it (6: never hidden).
    pub cull: u8,
    pub flags: u8,
    /// Directional shade x255.
    pub light: u8,
}

pub struct Blocks {
    pub states: Vec<State>,
    /// Boxes in sixteenths: x0 y0 z0 x1 y1 z1.
    pub boxes: Vec<[u8; 6]>,
    /// Collision boxes in 1/32 block (can reach below 0 and above 1).
    pub coll: Vec<[i8; 6]>,
    pub quads: Vec<Quad>,
    pub blocks: Vec<Block>,
    /// Item names by protocol id.
    pub items: Vec<String>,
    pub water_uv: [f32; 4],
    pub lava_uv: [f32; 4],
    /// The ten block-breaking stages (destroy_stage_0..9).
    pub crack_uv: [[f32; 4]; 10],
    /// Biomes by name: grass, foliage, water and sky colours (0xRRGGBB).
    pub biomes: alloc::collections::BTreeMap<String, [u32; 4]>,
}

const EMPTY: State =
    State { block: 0, hardness: 0, flags: 0, boxes: 0, first_box: 0, first_quad: 0, quads: 0, first_coll: 0, colls: 0, coll_rule: 0, facing: 255 };

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?]))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

impl Blocks {
    pub fn parse(d: &[u8]) -> Option<Blocks> {
        if d.get(..4)? != crate::account::BLOCKS_MAGIC {
            return None;
        }
        let ns = u32_at(d, 4)? as usize;
        let nb = u32_at(d, 8)? as usize;
        let nx = u32_at(d, 12)? as usize;
        let nq = u32_at(d, 16)? as usize;
        let ni = u32_at(d, 20)? as usize;
        let nc = u32_at(d, 24)? as usize;
        let mut p = 28;
        let mut states = Vec::with_capacity(ns);
        for _ in 0..ns {
            let r = d.get(p..p + 24)?;
            states.push(State {
                block: u16_at(r, 0)?,
                hardness: u16_at(r, 2)?,
                flags: r[4],
                boxes: r[5],
                first_box: u16_at(r, 6)?,
                first_quad: u32_at(r, 8)?,
                quads: u16_at(r, 12)?,
                first_coll: u32_at(r, 16)?,
                colls: r[20],
                coll_rule: r[21],
                facing: r[14],
            });
            p += 24;
        }
        let mut boxes = Vec::with_capacity(nx);
        for _ in 0..nx {
            let r = d.get(p..p + 6)?;
            boxes.push([r[0], r[1], r[2], r[3], r[4], r[5]]);
            p += 6;
        }
        let mut coll = Vec::with_capacity(nc);
        for _ in 0..nc {
            let r = d.get(p..p + 6)?;
            coll.push([r[0] as i8, r[1] as i8, r[2] as i8, r[3] as i8, r[4] as i8, r[5] as i8]);
            p += 6;
        }
        let mut quads = Vec::with_capacity(nq);
        for _ in 0..nq {
            let r = d.get(p..p + 48)?;
            let mut q = Quad { pos: [[0; 3]; 4], uv: [[0; 2]; 4], cull: r[40], flags: r[41], light: r[42] };
            for k in 0..4 {
                let o = k * 10;
                for a in 0..3 {
                    q.pos[k][a] = u16_at(r, o + 2 * a)? as i16;
                }
                q.uv[k] = [u16_at(r, o + 6)?, u16_at(r, o + 8)?];
            }
            quads.push(q);
            p += 48;
        }
        let mut blocks = Vec::with_capacity(nb);
        for _ in 0..nb {
            let r = d.get(p..p + 8)?;
            let n = r[7] as usize;
            let name = String::from_utf8_lossy(d.get(p + 8..p + 8 + n)?).into_owned();
            let sp = p + 8 + n;
            let snd = d.get(sp..sp + 33)?;
            let ff = |o: usize| -> Option<f32> { Some(f32::from_bits(u32_at(snd, o)?)) };
            let sounds = [u16_at(snd, 0)?, u16_at(snd, 2)?, u16_at(snd, 4)?, u16_at(snd, 6)?, u16_at(snd, 8)?];
            blocks.push(Block {
                kind: r[0],
                tint: r[1],
                mineable: r[2],
                incorrect: r[3],
                map_colour: [r[4], r[5], r[6]],
                name,
                sounds,
                sound_volume: snd[10] as f32 / 100.0,
                sound_pitch: snd[11] as f32 / 100.0,
                friction: ff(12)?,
                speed_factor: ff(16)?,
                jump_factor: ff(20)?,
                offset: snd[24],
                max_offset: ff(25)?,
                max_v_offset: ff(29)?,
            });
            p = sp + 33;
        }
        let mut items = Vec::with_capacity(ni);
        for _ in 0..ni {
            let n = *d.get(p)? as usize;
            items.push(String::from_utf8_lossy(d.get(p + 1..p + 1 + n)?).into_owned());
            p += 1 + n;
        }
        let f = |o: usize| -> Option<f32> { Some(f32::from_bits(u32_at(d, o)?)) };
        let water_uv = [f(p)?, f(p + 4)?, f(p + 8)?, f(p + 12)?];
        let lava_uv = [f(p + 16)?, f(p + 20)?, f(p + 24)?, f(p + 28)?];
        let mut crack_uv = [[0f32; 4]; 10];
        for (i, c) in crack_uv.iter_mut().enumerate() {
            let o = p + 32 + i * 16;
            *c = [f(o)?, f(o + 4)?, f(o + 8)?, f(o + 12)?];
        }
        let mut q = p + 32 + 160;
        let mut biomes = alloc::collections::BTreeMap::new();
        if let Some(n) = u32_at(d, q) {
            q += 4;
            for _ in 0..n {
                let l = *d.get(q)? as usize;
                let name = String::from_utf8_lossy(d.get(q + 1..q + 1 + l)?).into_owned();
                q += 1 + l;
                biomes.insert(name, [u32_at(d, q)?, u32_at(d, q + 4)?, u32_at(d, q + 8)?, u32_at(d, q + 12)?]);
                q += 16;
            }
        }
        Some(Blocks { states, boxes, coll, quads, blocks, items, water_uv, lava_uv, crack_uv, biomes })
    }

    pub fn state(&self, s: u16) -> &State {
        self.states.get(s as usize).unwrap_or(&EMPTY)
    }

    pub fn of_state(&self, s: u16) -> &Block {
        &self.blocks[self.state(s).block as usize]
    }

    pub fn kind(&self, s: u16) -> u8 {
        self.of_state(s).kind
    }

    pub fn shape(&self, s: u16) -> &[[u8; 6]] {
        let st = self.state(s);
        let a = st.first_box as usize;
        self.boxes.get(a..a + st.boxes as usize).unwrap_or(&[])
    }

    pub fn model(&self, s: u16) -> &[Quad] {
        let st = self.state(s);
        let a = st.first_quad as usize;
        self.quads.get(a..a + st.quads as usize).unwrap_or(&[])
    }

    /// A whole opaque cube: hides the faces of its neighbours.
    pub fn opaque_cube(&self, s: u16) -> bool {
        self.state(s).flags & OPAQUE_CUBE != 0
    }

    /// Whether it has any collision for the player (its own boxes or a rule).
    pub fn collides(&self, s: u16) -> bool {
        let st = self.state(s);
        st.colls > 0 || st.coll_rule != 0
    }

    pub fn has_fluid(&self, s: u16) -> bool {
        self.state(s).flags & HAS_FLUID != 0
    }

    /// Vanilla's collision boxes (1/32 block), for states without a rule of their own.
    pub fn collision(&self, s: u16) -> &[[i8; 6]] {
        let st = self.state(s);
        let a = st.first_coll as usize;
        self.coll.get(a..a + st.colls as usize).unwrap_or(&[])
    }

    /// BlockBehaviour.getOffset: the random shift of plants, bamboo and dripstone at a position
    /// (zero for blocks without one).
    pub fn offset_at(&self, s: u16, x: i32, z: i32) -> [f64; 3] {
        let blk = self.of_state(s);
        if blk.offset & OFF_MODEL == 0 {
            return [0.0; 3];
        }
        offset(x, z, blk.max_offset, if blk.offset & OFF_VERTICAL != 0 { blk.max_v_offset } else { 0.0 })
    }

    /// Bits a block state takes in a chunk section sent without a palette.
    pub fn global_bits(&self) -> u8 {
        ceil_log2(self.states.len()).max(1)
    }
}

/// Mth.getSeed.
pub fn seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    l = l.wrapping_mul(l).wrapping_mul(42317861).wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// BlockBehaviour.getOffset for OffsetType XZ, or XYZ when `max_v` (getMaxVerticalOffset) is not 0.
pub fn offset(x: i32, z: i32, max_h: f32, max_v: f32) -> [f64; 3] {
    let l = seed(x, 0, z);
    let h = |v: i64| -> f64 {
        let f = ((v & 15) as f32 / 15.0) as f64;
        ((f - 0.5) * 0.5).clamp(-(max_h as f64), max_h as f64)
    };
    let y = if max_v != 0.0 { ((((l >> 4) & 15) as f32 / 15.0) as f64 - 1.0) * max_v as f64 } else { 0.0 };
    [h(l), y, h(l >> 8)]
}

pub fn ceil_log2(n: usize) -> u8 {
    let mut b = 0u8;
    while (1usize << b) < n {
        b += 1;
    }
    b
}

/// Plains' colours for the tinted blocks (biome colours are not read yet).
pub fn tint_colour(t: u8) -> [u8; 3] {
    match t {
        1 => [0x91, 0xBD, 0x59],
        2 => [0x77, 0xAB, 0x2F],
        3 => [0x80, 0xA7, 0x55],
        4 => [0x61, 0x99, 0x61],
        5 => [0x3F, 0x76, 0xE4],
        6 => [0xC8, 0x10, 0x10],
        7 => [0xB0, 0xA0, 0x30],
        _ => [255, 255, 255],
    }
}
