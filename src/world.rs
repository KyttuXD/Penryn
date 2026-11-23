//! The chunks the server has sent: block states per 16x16x16 section.

use crate::wire::In;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

pub struct Section {
    pub states: Box<[u16; 4096]>,
    /// Blocks that are not air.
    pub solid: u32,
}

/// One section's light of one channel (sky or block): a single level when every block has it
/// (the lit air above ground, solid dark rock), else 4096 nibbles in the wire's order
/// (index = y << 8 | z << 4 | x, the even index in the low half of its byte).
#[derive(Clone, PartialEq)]
pub enum Light {
    Uniform(u8),
    Nibbles(Box<[u8; 2048]>),
}

impl Column {
    /// Gives every sky section the server has not sent what vanilla's light engine would read
    /// there: top down, the bottom layer (y = 0) of the section above, 15 over the top.
    /// Pushes the light sections whose light changed.
    fn resolve_sky(&mut self, changed: &mut Vec<usize>) {
        let mut above = [15u8; 256];
        for i in (0..self.sky.len()).rev() {
            if !self.sky_given[i] {
                let uniform = above.iter().all(|&v| v == above[0]);
                let new = if uniform {
                    Light::Uniform(above[0])
                } else {
                    let mut n = Box::new([0u8; 2048]);
                    for y in 0..16 {
                        for c in 0..256 {
                            n[(y * 256 + c) >> 1] |= above[c] << (((y * 256 + c) & 1) * 4);
                        }
                    }
                    Light::Nibbles(n)
                };
                if self.sky[i] != new {
                    self.sky[i] = new;
                    if !changed.contains(&i) {
                        changed.push(i);
                    }
                }
            }
            match &self.sky[i] {
                Light::Uniform(v) => above = [*v; 256],
                l => {
                    for c in 0..256 {
                        above[c] = l.at(c);
                    }
                }
            }
        }
    }

    /// A column with no light yet (every section dark): one light section more than the block
    /// sections at each end.
    pub fn dark(sections: Vec<Option<Section>>, biomes: Vec<[u16; 64]>, version: u32) -> Column {
        let n = sections.len() + 2;
        Column { sections, sky: alloc::vec![Light::Uniform(0); n], blk: alloc::vec![Light::Uniform(0); n], sky_given: alloc::vec![false; n], has_sky: false, biomes, version }
    }
}

impl Light {
    /// The level at a section-local index (y << 8 | z << 4 | x).
    #[inline]
    pub fn at(&self, i: usize) -> u8 {
        match self {
            Light::Uniform(v) => *v,
            Light::Nibbles(n) => n[i >> 1] >> ((i & 1) * 4) & 15,
        }
    }

    /// From the wire's 2048 bytes; all-equal data is kept as a single level.
    pub fn from_bytes(b: &[u8]) -> Option<Light> {
        if b.len() != 2048 {
            return None;
        }
        let f = b[0];
        if f & 15 == f >> 4 && b.iter().all(|&x| x == f) {
            return Some(Light::Uniform(f & 15));
        }
        let mut n = Box::new([0u8; 2048]);
        n.copy_from_slice(b);
        Some(Light::Nibbles(n))
    }

    fn is_nibbles(&self) -> bool {
        matches!(self, Light::Nibbles(_))
    }
}

/// A BitSet of the wire (26.3: BitSet.valueOf of a byte array): a VarInt byte count and the
/// bytes, bit i being byte i / 8, bit i % 8 (little-endian). Checked against a live server's
/// packet: the older "VarInt count of big-endian longs" layout does not parse.
pub fn read_bitset(r: &mut In) -> Option<Vec<u8>> {
    let n = r.varint()?;
    if !(0..=1024).contains(&n) {
        return None;
    }
    Some(r.take(n as usize)?.to_vec())
}

fn bit(set: &[u8], i: usize) -> bool {
    set.get(i / 8).map_or(false, |w| w >> (i % 8) & 1 != 0)
}

pub struct Column {
    pub sections: Vec<Option<Section>>,
    /// Sky light and block light per light section: height / 16 + 2 of them, index 0 being the
    /// section below the world, so block section sy is light section sy + 1.
    pub sky: Vec<Light>,
    pub blk: Vec<Light>,
    /// Which sky sections the server has sent (mask or empty mask). The others hold what vanilla's
    /// light engine reads for a missing sky section: the bottom layer of the section above,
    /// or 15 above the highest one. Only used once the column has had any sky data at all.
    pub sky_given: Vec<bool>,
    pub has_sky: bool,
    /// Each section's 4x4x4 biome cells (registry ids), ((y * 4 + z) * 4 + x).
    pub biomes: Vec<[u16; 64]>,
    /// Bumped on every change, so the renderer knows to rebuild.
    pub version: u32,
}

pub struct World {
    /// The server's dimension types (registry order): name, min_y, height.
    pub dim_types: Vec<(String, i32, i32)>,
    pub min_y: i32,
    pub height: i32,
    pub columns: BTreeMap<(i32, i32), Column>,
    pub block_bits: u8,
    pub biome_bits: u8,
    /// Sections to mesh again: (column x, section index from the bottom, column z).
    pub dirty: BTreeSet<(i32, i32, i32)>,
    /// Bumped when every chunk goes (a new dimension): the renderer drops all its meshes.
    pub epoch: u32,
    /// Which dimension's light attributes apply (lightmap::OVERWORLD, NETHER or END), from
    /// the dimension type's name (else the dimension's).
    pub light_env: u8,
    /// Per biome registry id: grass, foliage, water and sky colours.
    pub biome_colours: Vec<[u32; 4]>,
    /// The server's biome names, by registry id.
    pub biome_names: Vec<String>,
    /// Blocks in motion: pistons' moving blocks, shulker box lids.
    pub moving: crate::moving::Moving,
}

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    Blocks,
    Biomes,
}

/// One paletted container (1.21.5+ layout: no long count), decoded into `out` if given.
fn paletted(r: &mut In, s: Strategy, global_bits: u8, out: Option<&mut [u16]>) -> Option<()> {
    let wire = r.u8()?;
    let entries = if s == Strategy::Blocks { 4096 } else { 64 };
    let (has_palette, bits) = match (s, wire) {
        (_, 0) => (true, 0u8),
        (Strategy::Blocks, 1..=4) => (true, 4),
        (Strategy::Blocks, 5..=8) => (true, wire),
        (Strategy::Biomes, 1..=3) => (true, wire),
        _ => (false, global_bits.max(wire)),
    };
    if bits > 32 {
        return None;
    }
    let mut palette: Vec<i32> = Vec::new();
    if has_palette {
        if bits == 0 {
            palette.push(r.varint()?);
        } else {
            let n = r.varint()?;
            if n < 0 || n > 1 << bits {
                return None;
            }
            for _ in 0..n {
                palette.push(r.varint()?);
            }
        }
    }
    let longs = if bits == 0 { 0 } else { (entries + (64 / bits as usize) - 1) / (64 / bits as usize) };
    let data = r.take(longs * 8)?;
    if let Some(out) = out {
        if bits == 0 {
            let v = palette[0] as u16;
            out.iter_mut().for_each(|x| *x = v);
            return Some(());
        }
        let per = 64 / bits as usize;
        let mask = (1u64 << bits) - 1;
        for i in 0..entries {
            let cell = i / per;
            let off = (i % per) * bits as usize;
            let l = u64::from_be_bytes(data[cell * 8..cell * 8 + 8].try_into().ok()?);
            let v = ((l >> off) & mask) as usize;
            out[i] = if has_palette { *palette.get(v).unwrap_or(&0) as u16 } else { v as u16 };
        }
    }
    Some(())
}

/// For one axis of `copy_light`: (first, end, offset) of the cells of the neighbour at -1, 0 or 1
/// that land in the 18-wide copy, local to that neighbour, and the shift into the copy's coordinates.
fn axis(d: i32) -> (i32, i32, i32) {
    match d {
        -1 => (15, 16, -16),
        0 => (0, 16, 0),
        _ => (0, 1, 16),
    }
}

impl World {
    pub fn new(block_bits: u8, biome_bits: u8) -> World {
        World {
            dim_types: Vec::new(),
            min_y: -64,
            height: 384,
            columns: BTreeMap::new(),
            block_bits,
            biome_bits,
            dirty: BTreeSet::new(),
            epoch: 0,
            light_env: crate::lightmap::OVERWORLD,
            biome_colours: Vec::new(),
            biome_names: Vec::new(),
            moving: crate::moving::Moving::new(),
        }
    }

    /// The dimension of the Join Game packet: its type (an id in the server's dimension_type
    /// registry, with the height that type sets) else the vanilla height of its name.
    pub fn set_dimension(&mut self, name: &str, type_id: i32) {
        let (min_y, height) = match self.dim_types.get(type_id.max(0) as usize) {
            Some((_, min_y, height)) => (*min_y, *height),
            None => match name {
                "minecraft:the_nether" | "minecraft:the_end" => (0, 256),
                _ => (-64, 384),
            },
        };
        self.min_y = min_y;
        self.height = height;
        let ty = self.dim_types.get(type_id.max(0) as usize).map(|t| t.0.as_str()).unwrap_or(name);
        self.light_env = if ty.contains("nether") || name.contains("nether") {
            crate::lightmap::NETHER
        } else if ty.contains("the_end") || name.contains("the_end") {
            crate::lightmap::END
        } else {
            crate::lightmap::OVERWORLD
        };
        crate::say!("dimension {} (type {}): y {} to {}", name, type_id, min_y, min_y + height - 1);
        self.columns.clear();
        self.dirty.clear();
        self.epoch += 1;
    }

    /// Reads the sections of a chunk packet's buffer. `is_air(state)` tells empty blocks.
    pub fn load_column(&mut self, x: i32, z: i32, buf: &[u8], is_air: &dyn Fn(u16) -> bool) -> Option<()> {
        let n = (self.height / 16) as usize;
        let mut r = In::new(buf);
        let mut sections = Vec::with_capacity(n);
        let mut biomes = Vec::with_capacity(n);
        for _ in 0..n {
            let non_empty = r.u16()? as i16;
            let _fluids = r.u16()?;
            if non_empty == 0 {
                // Still has a (single-value) container on the wire.
                paletted(&mut r, Strategy::Blocks, self.block_bits, None)?;
                sections.push(None);
            } else {
                let mut states = Box::new([0u16; 4096]);
                paletted(&mut r, Strategy::Blocks, self.block_bits, Some(&mut states[..]))?;
                let solid = states.iter().filter(|&&s| !is_air(s)).count() as u32;
                sections.push(if solid == 0 { None } else { Some(Section { states, solid }) });
            }
            let mut b = [0u16; 64];
            paletted(&mut r, Strategy::Biomes, self.biome_bits, Some(&mut b[..]))?;
            biomes.push(b);
        }
        let version = self.columns.get(&(x, z)).map_or(0, |c| c.version + 1);
        self.columns.insert((x, z), Column::dark(sections, biomes, version));
        self.mark(x, z);
        Some(())
    }

    /// Reads the light half of a chunk packet or a light update (after its chunk position): the
    /// four masks, then the sky and block arrays. A section in a data mask takes its array, one
    /// in an "empty" mask goes dark, any other keeps its light. Always consumes the data; stores
    /// it only for a loaded column and marks the meshes that read the changed sections.
    pub fn read_light(&mut self, x: i32, z: i32, r: &mut In) -> Option<()> {
        let sky_m = read_bitset(r)?;
        let blk_m = read_bitset(r)?;
        let sky_e = read_bitset(r)?;
        let blk_e = read_bitset(r)?;
        let lights = (self.height / 16 + 2) as usize;
        let mut changed: Vec<usize> = Vec::new();
        for ch in 0..2 {
            let (mask, empty) = if ch == 0 { (&sky_m, &sky_e) } else { (&blk_m, &blk_e) };
            let count = r.varint()?;
            let mut arrays: Vec<Light> = Vec::with_capacity(count.clamp(0, 64) as usize);
            for _ in 0..count.max(0) {
                let len = r.varint()?;
                let data = r.take(len.max(0) as usize)?;
                arrays.push(Light::from_bytes(data)?);
            }
            let Some(col) = self.columns.get_mut(&(x, z)) else { continue };
            let layer = if ch == 0 { &mut col.sky } else { &mut col.blk };
            let mut next = arrays.into_iter();
            // Arrays belong to the set bits in ascending order, even past the world's top.
            for i in 0..(mask.len().max(empty.len()) * 8).max(lights) {
                let new = if bit(mask, i) {
                    next.next()?
                } else if bit(empty, i) {
                    Light::Uniform(0)
                } else {
                    continue;
                };
                if i >= lights {
                    continue;
                }
                if ch == 0 {
                    col.sky_given[i] = true;
                    col.has_sky = true;
                }
                if layer[i] != new {
                    layer[i] = new;
                    if !changed.contains(&i) {
                        changed.push(i);
                    }
                }
            }
        }
        if let Some(col) = self.columns.get_mut(&(x, z)) {
            if col.has_sky {
                col.resolve_sky(&mut changed);
            }
            if !changed.is_empty() {
                col.version += 1;
            }
        }
        for i in changed {
            self.mark_light(x, z, i);
        }
        Some(())
    }

    /// The meshes that read light section `i` of a column: its block section (i - 1) and the
    /// 26 around it, since the smooth light reads a block across every face, edge and corner.
    fn mark_light(&mut self, x: i32, z: i32, i: usize) {
        let n = self.height / 16;
        for dz in -1..=1 {
            for dx in -1..=1 {
                if !self.columns.contains_key(&(x + dx, z + dz)) {
                    continue;
                }
                for sy in (i as i32 - 2).max(0)..=(i as i32).min(n - 1) {
                    self.dirty.insert((x + dx, sy, z + dz));
                }
            }
        }
    }

    /// Sky and block light at a block; (0, 0) where nothing is loaded. The sections just
    /// below and above the world are included (they carry the light the world's faces see).
    pub fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        let ly = y - self.min_y + 16;
        if ly >= self.height + 32 {
            // Far above the world (a flier at the build limit): open sky where the dimension has any.
            return match self.columns.get(&(x >> 4, z >> 4)) {
                Some(c) if c.has_sky => (15, 0),
                _ => (0, 0),
            };
        }
        if ly < 0 {
            return (0, 0);
        }
        match self.columns.get(&(x >> 4, z >> 4)) {
            None => (0, 0),
            Some(c) => {
                let k = (ly >> 4) as usize;
                let i = (((ly & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize;
                (c.sky[k].at(i), c.blk[k].at(i))
            }
        }
    }

    /// Copies the light of block section (cx, sy, cz) and a one-block border into `out`
    /// (18 x 18 x 18, index ((y + 1) * 18 + (z + 1)) * 18 + (x + 1) for local x, y, z in -1..=16),
    /// each byte sky << 4 | block. Unloaded cells are 0. Works section by section, with
    /// single-level sections filled without reading nibbles.
    pub fn copy_light(&self, cx: i32, sy: i32, cz: i32, out: &mut [u8; 18 * 18 * 18]) {
        out.fill(0);
        let lights = self.height / 16 + 2;
        for dz in -1..=1i32 {
            for dx in -1..=1i32 {
                let Some(c) = self.columns.get(&(cx + dx, cz + dz)) else { continue };
                // Local range inside the neighbour along an axis: the whole section for 0, the
                // edge cell next to us otherwise; `o` is where it lands in `out` (minus the border).
                let (x0, x1, ox) = axis(dx);
                let (z0, z1, oz) = axis(dz);
                for dy in -1..=1i32 {
                    let k = sy + dy + 1;
                    if k < 0 || k >= lights {
                        continue;
                    }
                    let (y0, y1, oy) = axis(dy);
                    let (s, b) = (&c.sky[k as usize], &c.blk[k as usize]);
                    for y in y0..y1 {
                        for z in z0..z1 {
                            let row = (((y + oy + 1) * 18 + (z + oz + 1)) * 18 + (ox + 1)) as usize;
                            if let (Light::Uniform(sv), Light::Uniform(bv)) = (s, b) {
                                let v = sv << 4 | bv;
                                for x in x0..x1 {
                                    out[row + x as usize] = v;
                                }
                            } else {
                                for x in x0..x1 {
                                    let i = ((y << 8) | (z << 4) | x) as usize;
                                    out[row + x as usize] = s.at(i) << 4 | b.at(i);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// For PENRYN_DEBUG: how many sections of a column have sky and block light, and the
    /// light at a block.
    pub fn light_stats(&self, x: i32, z: i32) -> Option<[usize; 4]> {
        let c = self.columns.get(&(x, z))?;
        let lit = |v: &Vec<Light>| v.iter().filter(|l| *l != &Light::Uniform(0)).count();
        Some([lit(&c.sky), c.sky.iter().filter(|l| l.is_nibbles()).count(), lit(&c.blk), c.blk.iter().filter(|l| l.is_nibbles()).count()])
    }

    fn mark(&mut self, x: i32, z: i32) {
        let n = self.height / 16;
        for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            if self.columns.contains_key(&(x + dx, z + dz)) {
                for sy in 0..n {
                    self.dirty.insert((x + dx, sy, z + dz));
                }
            }
        }
    }

    pub fn forget(&mut self, x: i32, z: i32) {
        self.columns.remove(&(x, z));
        for sy in 0..self.height / 16 {
            self.dirty.insert((x, sy, z));
        }
    }

    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        let ly = y - self.min_y;
        if ly < 0 || ly >= self.height {
            return 0;
        }
        match self.columns.get(&(x >> 4, z >> 4)) {
            None => 0,
            Some(c) => match &c.sections[(ly >> 4) as usize] {
                None => 0,
                Some(s) => s.states[(((ly & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize],
            },
        }
    }

    /// Maps the server's biome ids (the order of its registry) to the colour table.
    pub fn set_biomes(&mut self, names: &[String], blocks: &crate::blocks::Blocks) {
        let plains = [0x91BD59, 0x77AB2F, 0x3F76E4, 0x78A7FF];
        self.biome_colours = names.iter().map(|n| blocks.biomes.get(n).copied().unwrap_or(plains)).collect();
        self.biome_names = names.to_vec();
    }

    /// The biome registry id at a block (its 4x4x4 cell); 0 where nothing is loaded.
    pub fn biome(&self, x: i32, y: i32, z: i32) -> u16 {
        let ly = (y - self.min_y).clamp(0, self.height - 1);
        match self.columns.get(&(x >> 4, z >> 4)) {
            None => u16::MAX,
            Some(c) => {
                let s = &c.biomes[(ly >> 4) as usize];
                s[((((ly & 15) >> 2) * 4 + ((z & 15) >> 2)) * 4 + ((x & 15) >> 2)) as usize]
            }
        }
    }

    /// Grass, foliage, water and sky colour at a block, blended over 5x5 columns (vanilla's
    /// default biome blend) — `kind` 0 grass, 1 foliage, 2 water, 3 sky.
    pub fn colour(&self, x: i32, y: i32, z: i32, kind: usize) -> [u8; 3] {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for dz in -2..=2 {
            for dx in -2..=2 {
                let id = self.biome(x + dx, y, z + dz);
                if let Some(c) = self.biome_colours.get(id as usize) {
                    let v = c[kind];
                    r += v >> 16 & 255;
                    g += v >> 8 & 255;
                    b += v & 255;
                    n += 1;
                }
            }
        }
        if n == 0 {
            return [0x91, 0xBD, 0x59];
        }
        [(r / n) as u8, (g / n) as u8, (b / n) as u8]
    }

    pub fn loaded(&self, x: i32, z: i32) -> bool {
        self.columns.contains_key(&(x >> 4, z >> 4))
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, state: u16, is_air: &dyn Fn(u16) -> bool) {
        let ly = y - self.min_y;
        if ly < 0 || ly >= self.height {
            return;
        }
        let (cx, cz) = (x >> 4, z >> 4);
        let Some(c) = self.columns.get_mut(&(cx, cz)) else { return };
        let slot = &mut c.sections[(ly >> 4) as usize];
        if slot.is_none() {
            if is_air(state) {
                return;
            }
            *slot = Some(Section { states: Box::new([0u16; 4096]), solid: 0 });
        }
        let s = slot.as_mut().unwrap();
        let i = (((ly & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize;
        let was = !is_air(s.states[i]);
        s.states[i] = state;
        let now = !is_air(state);
        if was && !now {
            s.solid -= 1;
        } else if !was && now {
            s.solid += 1;
        }
        c.version += 1;
        // This section, and the neighbouring ones when the block is on their edge.
        let sy = ly >> 4;
        let (lx, lyy, lz) = (x & 15, ly & 15, z & 15);
        let n = self.height / 16;
        self.dirty.insert((cx, sy, cz));
        for (cond, k) in [
            (lx == 0, (cx - 1, sy, cz)),
            (lx == 15, (cx + 1, sy, cz)),
            (lz == 0, (cx, sy, cz - 1)),
            (lz == 15, (cx, sy, cz + 1)),
            (lyy == 0 && sy > 0, (cx, sy - 1, cz)),
            (lyy == 15 && sy + 1 < n, (cx, sy + 1, cz)),
        ] {
            if cond {
                self.dirty.insert(k);
            }
        }
    }
}

/// Checks for `penryn selftest`: nibble unpacking, bit sets, the light update masks, the border copy.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut v = Vec::new();
    // Nibbles: index 0 is the low half of byte 0, index 1 the high half.
    let mut b = [0u8; 2048];
    b[0] = 0x21;
    b[2047] = 0xF5;
    let l = Light::from_bytes(&b).unwrap();
    v.push(("light nibbles", format!("{} {} {} {}", l.at(0), l.at(1), l.at(4094), l.at(4095)), "1 2 5 15"));
    v.push(("light uniform", format!("{}", Light::from_bytes(&[0xFF; 2048]) == Some(Light::Uniform(15))), "true"));
    v.push(("light mixed halves", format!("{}", Light::from_bytes(&[0x1F; 2048]).map_or(false, |l| l.is_nibbles() && l.at(0) == 15 && l.at(1) == 1)), "true"));
    v.push(("light bad length", format!("{}", Light::from_bytes(&[0; 100]).is_none()), "true"));
    // A BitSet: two bytes [0x21, 0x01]: bits 0, 5 and 8.
    let raw = [2u8, 0x21, 0x01];
    let mut r = In::new(&raw);
    let bs = read_bitset(&mut r).unwrap();
    v.push(("bitset", format!("{} {} {} {} {}", bit(&bs, 0), bit(&bs, 1), bit(&bs, 5), bit(&bs, 8), bit(&bs, 200)), "true false true true false"));
    // A light update (world y -64..320: 26 light sections): sky arrays in sections 1 (all 15)
    // and 2 (nibbles), block section 1 emptied.
    let mut w = World::new(15, 6);
    w.columns.insert((0, 0), Column::dark((0..24).map(|_| None).collect(), Vec::new(), 0));
    for l in w.columns.get_mut(&(0, 0)).unwrap().blk.iter_mut() {
        *l = Light::Uniform(9);
    }
    let mut d: Vec<u8> = Vec::new();
    d.extend([1u8, 0x06]); // sky mask: bits 1 and 2
    d.extend([0u8]); // block mask: none
    d.extend([0u8]); // empty sky: none
    d.extend([1u8, 0x02]); // empty block: bit 1
    d.push(2); // two sky arrays
    d.extend([0x80, 0x10]); // length 2048
    d.extend([0xFFu8; 2048]);
    d.extend([0x80, 0x10]);
    let mut nb = [0u8; 2048];
    nb[0] = 0xA3;
    d.extend(nb);
    d.push(0); // no block arrays
    let mut r = In::new(&d);
    let ok = w.read_light(0, 0, &mut r).is_some() && r.left() == 0;
    let c = &w.columns[&(0, 0)];
    v.push(("light update parse", format!("{} {} {} {}", ok, c.sky[1] == Light::Uniform(15), c.sky[2].at(0), c.sky[2].at(1)), "true true 3 10"));
    v.push(("light update empty mask", format!("{} {}", c.blk[1] == Light::Uniform(0), c.blk[2] == Light::Uniform(9)), "true true"));
    // Sections never sent read as vanilla's engine does: 15 over the top, 15 from the section above.
    v.push(("light missing sky sections", format!("{} {}", c.sky[25] == Light::Uniform(15), c.sky[3] == Light::Uniform(15)), "true true"));
    // The second update darkens sky section 2 only: section 0 (missing) stays 15 and only the
    // meshes around sections 1..=2 are dirty.
    w.dirty.clear();
    let d: Vec<u8> = alloc::vec![0u8, 0, 1, 0x04, 0, 0, 0];
    let mut r = In::new(&d);
    let ok = w.read_light(0, 0, &mut r).is_some() && r.left() == 0;
    let want: alloc::collections::BTreeSet<(i32, i32, i32)> = [(0, 0, 0), (0, 1, 0), (0, 2, 0)].into_iter().collect();
    let c = &w.columns[&(0, 0)];
    v.push(("light update dirty", format!("{} {} {}", ok, w.dirty == want, c.sky[2] == Light::Uniform(0) && c.sky[0] == Light::Uniform(15)), "true true true"));
    // A hole under a varying section copies its bottom layer (light 3 at x = 0 for section 2 here).
    let mut w2 = World::new(15, 6);
    w2.columns.insert((0, 0), Column::dark((0..24).map(|_| None).collect(), Vec::new(), 0));
    let mut d: Vec<u8> = alloc::vec![1u8, 0x08, 0, 0, 0, 1, 0x80, 0x10];
    d.extend(nb);
    d.push(0);
    let mut r = In::new(&d);
    let ok = w2.read_light(0, 0, &mut r).is_some() && r.left() == 0;
    let c = &w2.columns[&(0, 0)];
    v.push(("light hole under varying", format!("{} {} {} {}", ok, c.sky[2].at(0), c.sky[2].at(1), c.sky[2].at(4095)), "true 3 10 0"));
    // light_at and copy_light agree, borders included (min_y -64: y = -64 is section 1).
    w.min_y = -64;
    w.height = 384;
    let at = |x, y, z| w.light_at(x, y, z);
    v.push(("light_at", format!("{:?} {:?} {:?}", at(0, -64, 0), at(3, -48, 0), at(100, 0, 0)), "(15, 0) (0, 9) (0, 0)"));
    v.push(("light_at far above", format!("{:?} {:?}", at(0, 900, 0), at(100, 900, 0)), "(15, 0) (0, 0)"));
    let mut out = alloc::vec![0u8; 18 * 18 * 18];
    let out: &mut [u8; 18 * 18 * 18] = (&mut out[..]).try_into().unwrap();
    w.copy_light(0, 1, 0, out);
    // Section 1 here is y -48..-33; its cell (0,0,0) is light section 2, cell 0 -> nibble 3 (sky), block 9.
    let cell = |x: i32, y: i32, z: i32| out[(((y + 1) * 18 + (z + 1)) * 18 + (x + 1)) as usize];
    let (s0, b0) = w.light_at(0, -48, 0);
    let (s1, b1) = w.light_at(0, -49, 0);
    v.push(("copy_light", format!("{} {} {}", cell(0, 0, 0) == s0 << 4 | b0, cell(0, -1, 0) == s1 << 4 | b1, cell(-1, 0, 0)), "true true 0"));
    // Malformed light data must never panic or leave a column with the wrong shape: random
    // bytes, and truncations of a good packet, through read_light.
    {
        let mut good: Vec<u8> = alloc::vec![1u8, 0x06, 1, 0x02, 0, 0, 2, 0x80, 0x10];
        good.extend([0x35u8; 2048]);
        good.extend([0x80, 0x10]);
        good.extend(nb);
        good.push(1);
        good.extend([0x80, 0x10]);
        good.extend([0xFFu8; 2048]);
        let mut w3 = World::new(15, 6);
        w3.min_y = -64;
        w3.height = 384;
        w3.columns.insert((0, 0), Column::dark((0..24).map(|_| None).collect(), Vec::new(), 0));
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut shape_ok = true;
        for round in 0..3000 {
            let mut d: Vec<u8> = if round % 3 == 0 {
                (0..(rnd() % 300) as usize).map(|_| rnd() as u8).collect()
            } else {
                good.clone()
            };
            if round % 3 == 1 {
                d.truncate(rnd() as usize % (good.len() + 1));
            } else if round % 3 == 2 {
                for _ in 0..4 {
                    let i = rnd() as usize % d.len();
                    d[i] = rnd() as u8;
                }
            }
            let mut r = In::new(&d);
            let _ = w3.read_light(0, 0, &mut r);
            let c = &w3.columns[&(0, 0)];
            shape_ok &= c.sky.len() == 26 && c.blk.len() == 26 && c.sky_given.len() == 26;
        }
        v.push(("light fuzz", format!("{}", shape_ok), "true"));
        // Unloading a chunk drops its light with the column.
        w3.forget(0, 0);
        v.push(("light forget", format!("{} {:?}", w3.columns.len(), w3.light_at(0, 0, 0)), "0 (0, 0)"));
    }
    v
}
