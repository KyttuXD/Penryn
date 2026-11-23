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

pub struct Column {
    pub sections: Vec<Option<Section>>,
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
        self.columns.insert((x, z), Column { sections, biomes, version });
        self.mark(x, z);
        Some(())
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
