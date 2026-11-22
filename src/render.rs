//! Drawing with OpenGL 1.5 fixed function: one vertex buffer per 16x16x16 section (opaque and
//! water apart), coloured faces with ambient occlusion instead of textures, fog, the block
//! outline and a 2D layer for the HUD.

use crate::blocks::{self, Blocks};
use crate::font;
use crate::math;
use crate::sdl::*;
use crate::world::World;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct V {
    pub p: [f32; 3],
    pub uv: [f32; 2],
    pub c: [u8; 4],
}

const STRIDE: i32 = 24;

/// A chunk vertex: position in 1/1024 block from the section's corner, uv in 1/32767 of the
/// atlas (the modelview and texture matrices scale them back), colour. 16 bytes, two thirds of
/// a float vertex: less to upload and to fetch on old cards.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CV {
    pub p: [i16; 3],
    pub _pad: i16,
    pub uv: [i16; 2],
    pub c: [u8; 4],
}

const CSTRIDE: i32 = 16;

fn cv(p: [f32; 3], uv: [f32; 2], c: [u8; 4]) -> CV {
    CV {
        p: [(p[0] * 1024.0) as i16, (p[1] * 1024.0) as i16, (p[2] * 1024.0) as i16],
        _pad: 0,
        uv: [(uv[0] * 32767.0) as i16, (uv[1] * 32767.0) as i16],
        c,
    }
}

struct Mesh {
    vbo: u32,
    count: i32,
}

struct SectionMesh {
    opaque: Mesh,
    water: Mesh,
}

pub struct Renderer {
    meshes: BTreeMap<(i32, i32, i32), SectionMesh>,
    epoch: u32,
    cache: Vec<u16>,
    opaque: Vec<CV>,
    water: Vec<CV>,
    pub ui: Vec<V>,
    pub sections_drawn: u32,
    pub vertices_drawn: u32,
    pub atlas: u32,
    pub atlas_size: (f32, f32),
    /// The atlas' alpha, kept to extrude flat items held in the hand.
    pub atlas_alpha: Vec<u8>,
    /// The rain's fog (AtmosphericFogEnvironment.rainFogMultiplier, 0 to 1): pulls the fog in.
    pub rain_fog: f32,
    /// The vertical field of view, degrees (the FOV option with its effects).
    pub fov: f32,
}

/// Faces: normal, the four corners (0 min, 1 max per axis, counter-clockwise from outside),
/// brightness.
const FACES: [([i32; 3], [[u8; 3]; 4], f32); 6] = [
    ([0, 1, 0], [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]], 1.0),
    ([0, -1, 0], [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]], 0.5),
    ([1, 0, 0], [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]], 0.6),
    ([-1, 0, 0], [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]], 0.6),
    ([0, 0, 1], [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], 0.8),
    ([0, 0, -1], [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]], 0.8),
];
const AO: [f32; 4] = [0.5, 0.68, 0.84, 1.0];

const P: i32 = 18; // padded section side
fn ci(x: i32, y: i32, z: i32) -> usize {
    (((y + 1) * P + (z + 1)) * P + (x + 1)) as usize
}

fn shade(c: [u8; 3], k: f32, a: u8) -> [u8; 4] {
    let f = |v: u8| (v as f32 * k).min(255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2]), a]
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            meshes: BTreeMap::new(),
            epoch: 0,
            cache: alloc::vec![0; (P * P * P) as usize],
            opaque: Vec::new(),
            water: Vec::new(),
            ui: Vec::new(),
            sections_drawn: 0,
            vertices_drawn: 0,
            atlas: 0,
            atlas_size: (1.0, 1.0),
            atlas_alpha: Vec::new(),
            rain_fog: 0.0,
            fov: 70.0,
        }
    }

    /// Uploads atlas.bin (16x16 tiles) with mipmaps made per tile, so tiles never bleed.
    pub fn load_atlas(&mut self, gl: &Gl, d: &[u8]) -> Result<(), String> {
        if d.get(..4) != Some(b"PNA1") || d.len() < 8 {
            return Err(String::from("atlas.bin is damaged"));
        }
        let w = u16::from_le_bytes([d[4], d[5]]) as usize;
        let h = u16::from_le_bytes([d[6], d[7]]) as usize;
        let mut level: Vec<u8> = d.get(8..8 + w * h * 4).ok_or("atlas.bin is short")?.to_vec();
        self.atlas_size = (w as f32, h as f32);
        self.atlas_alpha = level.iter().skip(3).step_by(4).copied().collect();
        unsafe {
            (gl.GenTextures)(1, &mut self.atlas);
            (gl.BindTexture)(TEXTURE_2D, self.atlas);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST_MIPMAP_LINEAR);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 4);
        }
        let (mut lw, mut lh) = (w, h);
        for lv in 0..5 {
            unsafe {
                (gl.TexImage2D)(TEXTURE_2D, lv, RGBA as i32, lw as i32, lh as i32, 0, RGBA, UNSIGNED_BYTE, level.as_ptr() as *const c_void);
            }
            if lv == 4 {
                break;
            }
            // Halve: 2x2 boxes, colour weighted by alpha so cut-out edges do not darken.
            let (nw, nh) = (lw / 2, lh / 2);
            let mut next = alloc::vec![0u8; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    let mut c = [0u32; 4];
                    let mut wsum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let o = ((2 * y + dy) * lw + 2 * x + dx) * 4;
                        let a = level[o + 3] as u32;
                        for k in 0..3 {
                            c[k] += level[o + k] as u32 * a;
                        }
                        c[3] += a;
                        wsum += a;
                    }
                    let o = (y * nw + x) * 4;
                    for k in 0..3 {
                        next[o + k] = if wsum > 0 { (c[k] / wsum) as u8 } else { 0 };
                    }
                    next[o + 3] = (c[3] / 4) as u8;
                }
            }
            level = next;
            lw = nw;
            lh = nh;
        }
        Ok(())
    }

    fn drop_mesh(gl: &Gl, m: SectionMesh) {
        for b in [m.opaque.vbo, m.water.vbo] {
            if b != 0 {
                unsafe { (gl.DeleteBuffers)(1, &b) };
            }
        }
    }

    /// Meshes the dirty sections nearest the camera, for up to `budget_ms`.
    pub fn update(&mut self, gl: &Gl, world: &mut World, b: &Blocks, eye: [f64; 3], budget_ms: u64) {
        if world.epoch != self.epoch {
            self.epoch = world.epoch;
            let old = core::mem::take(&mut self.meshes);
            for (_, m) in old {
                Self::drop_mesh(gl, m);
            }
        }
        let start = crate::sys::now_ms();
        let (ex, ey, ez) = (eye[0] as i32 >> 4, (eye[1] as i32 - world.min_y) >> 4, eye[2] as i32 >> 4);
        loop {
            if world.dirty.is_empty() || crate::sys::now_ms() - start > budget_ms {
                break;
            }
            // The nearest dirty section.
            let mut best = None;
            let mut best_d = i32::MAX;
            for &(x, y, z) in world.dirty.iter() {
                let d = (x - ex).abs().max((z - ez).abs()) * 4 + (y - ey).abs();
                if d < best_d {
                    best_d = d;
                    best = Some((x, y, z));
                }
            }
            let k = best.unwrap();
            world.dirty.remove(&k);
            if let Some(m) = self.meshes.remove(&k) {
                Self::drop_mesh(gl, m);
            }
            if world.columns.contains_key(&(k.0, k.2)) {
                if let Some(m) = self.build(gl, world, b, k) {
                    self.meshes.insert(k, m);
                }
            }
        }
    }

    fn fill_cache(&mut self, world: &World, (cx, sy, cz): (i32, i32, i32)) -> bool {
        let n = world.height / 16;
        let mut any = false;
        let mut refs: [[[Option<&[u16; 4096]>; 3]; 3]; 3] = [[[None; 3]; 3]; 3];
        let mut below = [[false; 3]; 3];
        for dx in 0..3 {
            for dz in 0..3 {
                let col = world.columns.get(&(cx + dx as i32 - 1, cz + dz as i32 - 1));
                for dy in 0..3 {
                    let s = sy + dy as i32 - 1;
                    if s < 0 {
                        below[dx][dz] = true;
                        continue;
                    }
                    if s >= n {
                        continue;
                    }
                    if let Some(c) = col {
                        if let Some(sec) = &c.sections[s as usize] {
                            refs[dx][dy][dz] = Some(&sec.states);
                            if dx == 1 && dy == 1 && dz == 1 {
                                any = true;
                            }
                        }
                    }
                }
            }
        }
        if !any {
            return false;
        }
        for y in -1..17 {
            let (dy, ly) = if y < 0 { (0, 15) } else if y > 15 { (2, 0) } else { (1, y) };
            for z in -1..17 {
                let (dz, lz) = if z < 0 { (0, 15) } else if z > 15 { (2, 0) } else { (1, z) };
                for x in -1..17 {
                    let (dx, lx) = if x < 0 { (0, 15) } else if x > 15 { (2, 0) } else { (1, x) };
                    let v = match refs[dx][dy][dz] {
                        Some(s) => s[((ly << 8) | (lz << 4) | lx) as usize],
                        // Below the world: as if stone, so its floor is not drawn.
                        None if dy == 0 && below[dx][dz] => 1,
                        None => 0,
                    };
                    self.cache[ci(x, y, z)] = v;
                }
            }
        }
        true
    }

    fn build(&mut self, gl: &Gl, world: &World, b: &Blocks, k: (i32, i32, i32)) -> Option<SectionMesh> {
        if !self.fill_cache(world, k) {
            return None;
        }
        self.opaque.clear();
        self.water.clear();
        // Biome colours for this section, made on first need: per 4-block layer, the biome ids
        // of a 20 x 20 area, blended over 5 x 5 (vanilla's default biome blend).
        let mut tints: Option<Vec<[[u8; 3]; 3]>> = None;
        let section_y = world.min_y + k.1 * 16;
        let make_tints = || -> Vec<[[u8; 3]; 3]> {
            let mut out = alloc::vec![[[0u8; 3]; 3]; 4 * 256];
            for layer in 0..4 {
                let y = section_y + layer * 4 + 2;
                let mut ids = [0u16; 400];
                for gz in 0..20 {
                    for gx in 0..20 {
                        ids[gz * 20 + gx] = world.biome(k.0 * 16 + gx as i32 - 2, y, k.2 * 16 + gz as i32 - 2);
                    }
                }
                for z in 0..16 {
                    for x in 0..16 {
                        let mut acc = [[0u32; 3]; 3];
                        let mut n = 0;
                        for dz in 0..5 {
                            for dx in 0..5 {
                                if let Some(c) = world.biome_colours.get(ids[(z + dz) * 20 + x + dx] as usize) {
                                    for kind in 0..3 {
                                        acc[kind][0] += c[kind] >> 16 & 255;
                                        acc[kind][1] += c[kind] >> 8 & 255;
                                        acc[kind][2] += c[kind] & 255;
                                    }
                                    n += 1;
                                }
                            }
                        }
                        let mut col = [[0x91, 0xBD, 0x59], [0x77, 0xAB, 0x2F], [0x3F, 0x76, 0xE4]];
                        if n > 0 {
                            for kind in 0..3 {
                                col[kind] = [(acc[kind][0] / n) as u8, (acc[kind][1] / n) as u8, (acc[kind][2] / n) as u8];
                            }
                        }
                        out[layer as usize * 256 + z * 16 + x] = col;
                    }
                }
            }
            out
        };
        let cache = &self.cache;
        let at = |x: i32, y: i32, z: i32| cache[ci(x, y, z)];
        let occ = |x: i32, y: i32, z: i32| b.opaque_cube(at(x, y, z));
        const DIR: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let s = at(x, y, z);
                    if s == 0 {
                        continue;
                    }
                    let blk = b.of_state(s);
                    let kind = blk.kind;
                    if kind == blocks::AIR || kind == blocks::INVISIBLE_SOLID {
                        continue;
                    }
                    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
                    if kind == blocks::WATER || kind == blocks::LAVA {
                        let water = kind == blocks::WATER;
                        let same = |s2: u16| b.kind(s2) == kind;
                        let top = if same(at(x, y + 1, z)) { 1.0 } else { 0.875 };
                        let r = if water { b.water_uv } else { b.lava_uv };
                        let tint = if water {
                            let t = tints.get_or_insert_with(&make_tints);
                            t[((y >> 2) * 256 + z * 16 + x) as usize][2]
                        } else {
                            [255, 255, 255]
                        };
                        let corner_uv = [[r[0], r[1]], [r[0], r[3]], [r[2], r[3]], [r[2], r[1]]];
                        for (nrm, corners, light) in FACES.iter() {
                            let (nx, ny, nz) = (x + nrm[0], y + nrm[1], z + nrm[2]);
                            if same(at(nx, ny, nz)) || occ(nx, ny, nz) {
                                continue;
                            }
                            let c = shade(tint, *light, if water { 190 } else { 255 });
                            let out = if water { &mut self.water } else { &mut self.opaque };
                            for (i, cr) in corners.iter().enumerate() {
                                let p = [fx + cr[0] as f32, fy + if cr[1] == 1 { top } else { 0.0 }, fz + cr[2] as f32];
                                out.push(cv(p, corner_uv[i], c));
                            }
                        }
                        continue;
                    }
                    // Grass, foliage and water take their biome's colour; birch, spruce,
                    // redstone and stems keep their fixed ones.
                    let tint = match blk.tint {
                        1 | 2 | 5 => {
                            let t = tints.get_or_insert_with(&make_tints);
                            t[((y >> 2) * 256 + z * 16 + x) as usize][match blk.tint { 1 => 0, 2 => 1, _ => 2 }]
                        }
                        0 => [255, 255, 255],
                        n => blocks::tint_colour(n),
                    };
                    let glassy = kind == blocks::CUTOUT && !blk.name.ends_with("leaves");
                    // Plants, bamboo and dripstone stand at their random offset (getOffset).
                    let off = b.offset_at(s, k.0 * 16 + x, k.2 * 16 + z);
                    let off = [(off[0] * 1024.0) as i16, (off[1] * 1024.0) as i16, (off[2] * 1024.0) as i16];
                    for q in b.model(s) {
                        if q.cull < 6 {
                            let d = DIR[q.cull as usize];
                            let ns = at(x + d[0], y + d[1], z + d[2]);
                            if b.opaque_cube(ns) || (glassy && b.state(ns).block == b.state(s).block) {
                                continue;
                            }
                        }
                        let base = if q.flags & blocks::Q_TINTED != 0 { tint } else { [255, 255, 255] };
                        let light = q.light as f32 / 255.0;
                        // The face's outward direction, for ambient occlusion on boundary faces.
                        let edge = q.flags & blocks::Q_ON_EDGE != 0 && q.cull < 6;
                        let out = if q.flags & blocks::Q_TRANSLUCENT != 0 { &mut self.water } else { &mut self.opaque };
                        for i in 0..4 {
                            let p = q.pos[i];
                            let (px, py, pz) = (p[0] as f32 / 1024.0, p[1] as f32 / 1024.0, p[2] as f32 / 1024.0);
                            let mut k = light;
                            if edge {
                                let n = DIR[q.cull as usize];
                                let (nx, ny, nz) = (x + n[0], y + n[1], z + n[2]);
                                let cr = [px > 0.5, py > 0.5, pz > 0.5];
                                let mut t = [0i32; 3];
                                for a in 0..3 {
                                    if n[a] == 0 {
                                        t[a] = if cr[a] { 1 } else { -1 };
                                    }
                                }
                                let mut sides = [false; 2];
                                let mut j = 0;
                                for a in 0..3 {
                                    if t[a] != 0 && j < 2 {
                                        let mut dd = [nx, ny, nz];
                                        dd[a] += t[a];
                                        sides[j] = occ(dd[0], dd[1], dd[2]);
                                        j += 1;
                                    }
                                }
                                let c = occ(nx + t[0], ny + t[1], nz + t[2]);
                                let ao = if sides[0] && sides[1] { 0 } else { 3 - (sides[0] as usize + sides[1] as usize + c as usize) };
                                k *= AO[ao];
                            }
                            out.push(CV {
                                p: [
                                    (x * 1024) as i16 + q.pos[i][0] + off[0],
                                    (y * 1024) as i16 + q.pos[i][1] + off[1],
                                    (z * 1024) as i16 + q.pos[i][2] + off[2],
                                ],
                                _pad: 0,
                                uv: [(q.uv[i][0] >> 1) as i16, (q.uv[i][1] >> 1) as i16],
                                c: shade(base, k, 255),
                            });
                        }
                    }
                }
            }
        }
        if self.opaque.is_empty() && self.water.is_empty() {
            return None;
        }
        let up = |v: &Vec<CV>| -> Mesh {
            if v.is_empty() {
                return Mesh { vbo: 0, count: 0 };
            }
            let mut id = 0u32;
            unsafe {
                (gl.GenBuffers)(1, &mut id);
                (gl.BindBuffer)(ARRAY_BUFFER, id);
                (gl.BufferData)(ARRAY_BUFFER, (v.len() * CSTRIDE as usize) as isize, v.as_ptr() as *const c_void, STATIC_DRAW);
                (gl.BindBuffer)(ARRAY_BUFFER, 0);
            }
            Mesh { vbo: id, count: v.len() as i32 }
        };
        Some(SectionMesh { opaque: up(&self.opaque), water: up(&self.water) })
    }

    /// Draws the world from `eye` looking along yaw/pitch (degrees, Minecraft's convention).
    pub fn draw_world(&mut self, gl: &Gl, world: &World, eye: [f64; 3], yaw: f32, pitch: f32, w: i32, h: i32, view_chunks: i32, sky: [f32; 3], before_water: &mut dyn FnMut(&[f32; 16]), sky_pass: &mut dyn FnMut(&[f32; 16])) {
        let far = (view_chunks as f32 + 1.0) * 16.0;
        let proj = perspective(self.fov, w as f32 / h as f32, 0.05, (far * 1.5).max(400.0));
        unsafe {
            (gl.Viewport)(0, 0, w, h);
            (gl.ClearColor)(sky[0], sky[1], sky[2], 1.0);
            (gl.Clear)(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT);
            (gl.Enable)(DEPTH_TEST);
            (gl.Enable)(CULL_FACE);
            (gl.CullFace)(BACK);
            (gl.Enable)(FOG);
            (gl.Fogi)(FOG_MODE, LINEAR_FOG);
            // Rain adds vanilla's environmental fog (0 to 1024 blocks, pulled in by 160 and
            // 256 under full rain): already 17 % at the eye. One linear fog for both: from
            // there to the render distance's end.
            let (s0, e) = (far * 0.55, far * 0.95);
            (gl.Fogf)(FOG_START, s0 + (-0.208 * e - s0) * self.rain_fog);
            (gl.Fogf)(FOG_END, e);
            let fc = [sky[0], sky[1], sky[2], 1.0];
            (gl.Fogfv)(FOG_COLOR, fc.as_ptr());
            (gl.MatrixMode)(PROJECTION);
            (gl.LoadMatrixf)(proj.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.atlas);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.5);
            let t: [f32; 16] = [1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadMatrixf)(t.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
        }
        let (yr, pr) = (math::to_rad(yaw as f64), math::to_rad(pitch as f64));
        let dir = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let view = look(dir);
        {
            let mut vf = [0f32; 16];
            for i in 0..16 {
                vf[i] = view[i] as f32;
            }
            unsafe {
                (gl.Disable)(TEXTURE_2D);
                (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
                (gl.Disable)(ALPHA_TEST);
                // The sky's float uvs: no 16-bit scale on the texture matrix.
                (gl.MatrixMode)(TEXTURE);
                (gl.LoadIdentity)();
                (gl.MatrixMode)(MODELVIEW);
            }
            sky_pass(&vf);
            unsafe {
                let t: [f32; 16] = [1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
                (gl.MatrixMode)(TEXTURE);
                (gl.LoadMatrixf)(t.as_ptr());
                (gl.MatrixMode)(MODELVIEW);
                (gl.EnableClientState)(VERTEX_ARRAY);
                (gl.EnableClientState)(COLOR_ARRAY);
                (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
                (gl.Enable)(TEXTURE_2D);
                (gl.BindTexture)(TEXTURE_2D, self.atlas);
                (gl.Enable)(ALPHA_TEST);
                (gl.AlphaFunc)(GREATER, 0.5);
                (gl.Enable)(FOG);
            }
        }
        let planes = frustum(&proj, &view);
        let mut order: Vec<((i32, i32, i32), f64)> = Vec::new();
        let maxd = (view_chunks as f64 + 1.0) * 16.0;
        for (&k, _) in self.meshes.iter() {
            let c = [
                (k.0 * 16 + 8) as f64 - eye[0],
                (world.min_y + k.1 * 16 + 8) as f64 - eye[1],
                (k.2 * 16 + 8) as f64 - eye[2],
            ];
            let d2 = c[0] * c[0] + c[2] * c[2];
            if d2 > maxd * maxd {
                continue;
            }
            if !in_frustum(&planes, [c[0] - 8.0, c[1] - 8.0, c[2] - 8.0], [c[0] + 8.0, c[1] + 8.0, c[2] + 8.0]) {
                continue;
            }
            order.push((k, d2 + c[1] * c[1]));
        }
        self.sections_drawn = order.len() as u32;
        let set = |k: &(i32, i32, i32)| {
            let t = [
                (k.0 * 16) as f64 - eye[0],
                (world.min_y + k.1 * 16) as f64 - eye[1],
                (k.2 * 16) as f64 - eye[2],
            ];
            let mut m = translate(&view, t);
            for i in 0..12 {
                m[i] /= 1024.0;
            }
            unsafe { (gl.LoadMatrixf)(m.as_ptr()) };
        };
        let draw = |m: &Mesh| unsafe {
            (gl.BindBuffer)(ARRAY_BUFFER, m.vbo);
            (gl.VertexPointer)(3, SHORT, CSTRIDE, core::ptr::null());
            (gl.TexCoordPointer)(2, SHORT, CSTRIDE, 8 as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, CSTRIDE, 12 as *const c_void);
            (gl.DrawArrays)(QUADS, 0, m.count);
        };
        self.vertices_drawn = order.iter().map(|(k, _)| (self.meshes[k].opaque.count + self.meshes[k].water.count) as u32).sum();
        for (k, _) in order.iter() {
            let m = &self.meshes[k];
            if m.opaque.count > 0 {
                set(k);
                draw(&m.opaque);
            }
        }
        // Entities, between the opaque world and the water (vanilla's order).
        unsafe {
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadIdentity)();
            (gl.MatrixMode)(MODELVIEW);
        }
        let mut vf = [0f32; 16];
        for i in 0..16 {
            vf[i] = view[i] as f32;
        }
        before_water(&vf);
        unsafe {
            let t: [f32; 16] = [1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0 / 32767.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadMatrixf)(t.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.atlas);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(ALPHA_TEST);
        }
        // Water, far to near, blended, without writing depth.
        order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(core::cmp::Ordering::Equal));
        unsafe {
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.DepthMask)(0);
            (gl.AlphaFunc)(GREATER, 0.004);
        }
        for (k, _) in order.iter() {
            let m = &self.meshes[k];
            if m.water.count > 0 {
                set(k);
                draw(&m.water);
            }
        }
        unsafe {
            (gl.DepthMask)(1);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.Disable)(BLEND);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
            (gl.MatrixMode)(TEXTURE);
            (gl.LoadIdentity)();
            (gl.MatrixMode)(MODELVIEW);
        }
    }

    /// The outline of the block being looked at.
    pub fn draw_outline(&mut self, gl: &Gl, eye: [f64; 3], yaw: f32, pitch: f32, pos: [i32; 3], boxes: &[[u8; 6]]) {
        let (yr, pr) = (math::to_rad(yaw as f64), math::to_rad(pitch as f64));
        let dir = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let m = translate(&look(dir), [pos[0] as f64 - eye[0], pos[1] as f64 - eye[1], pos[2] as f64 - eye[2]]);
        let mut v: Vec<V> = Vec::new();
        let c = [0, 0, 0, 160];
        for bx in boxes {
            let e = 0.002;
            let lo = [bx[0] as f32 / 16.0 - e, bx[1] as f32 / 16.0 - e, bx[2] as f32 / 16.0 - e];
            let hi = [bx[3] as f32 / 16.0 + e, bx[4] as f32 / 16.0 + e, bx[5] as f32 / 16.0 + e];
            let p = |i: u8| [if i & 1 != 0 { hi[0] } else { lo[0] }, if i & 2 != 0 { hi[1] } else { lo[1] }, if i & 4 != 0 { hi[2] } else { lo[2] }];
            for (a, b2) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                v.push(V { p: p(a), uv: [0.0; 2], c });
                v.push(V { p: p(b2), uv: [0.0; 2], c });
            }
        }
        unsafe {
            (gl.LoadMatrixf)(m.as_ptr());
            (gl.Disable)(FOG);
            (gl.LineWidth)(2.0);
            (gl.VertexPointer)(3, FLOAT, STRIDE, v.as_ptr() as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, STRIDE, (v.as_ptr() as *const u8).add(20) as *const c_void);
            (gl.DrawArrays)(LINES, 0, v.len() as i32);
        }
    }

    /// The breaking cracks on a block: its shape's faces with the stage's texture, multiplied
    /// into what is there (vanilla's crumbling render type: blend DST_COLOR, SRC_COLOR).
    pub fn draw_cracks(&mut self, gl: &Gl, eye: [f64; 3], yaw: f32, pitch: f32, pos: [i32; 3], boxes: &[[u8; 6]], uv: [f32; 4]) {
        let (yr, pr) = (math::to_rad(yaw as f64), math::to_rad(pitch as f64));
        let dir = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let m = translate(&look(dir), [pos[0] as f64 - eye[0], pos[1] as f64 - eye[1], pos[2] as f64 - eye[2]]);
        let mut v: Vec<V> = Vec::new();
        let c = [255, 255, 255, 255];
        for bx in boxes {
            let e = 0.003;
            let lo = [bx[0] as f32 / 16.0 - e, bx[1] as f32 / 16.0 - e, bx[2] as f32 / 16.0 - e];
            let hi = [bx[3] as f32 / 16.0 + e, bx[4] as f32 / 16.0 + e, bx[5] as f32 / 16.0 + e];
            for (fi, (_, corners, _)) in FACES.iter().enumerate() {
                for cr in corners {
                    let p = [if cr[0] == 1 { hi[0] } else { lo[0] }, if cr[1] == 1 { hi[1] } else { lo[1] }, if cr[2] == 1 { hi[2] } else { lo[2] }];
                    // The face's own two axes pick the texture coordinates (FACES: +Y -Y +X -X +Z -Z).
                    let (a, b) = match fi {
                        0 | 1 => (p[0], p[2]),
                        2 | 3 => (p[2], 1.0 - p[1]),
                        _ => (p[0], 1.0 - p[1]),
                    };
                    v.push(V { p, uv: [uv[0] + (uv[2] - uv[0]) * a.clamp(0.0, 1.0), uv[1] + (uv[3] - uv[1]) * b.clamp(0.0, 1.0)], c });
                }
            }
        }
        unsafe {
            (gl.LoadMatrixf)(m.as_ptr());
            (gl.Disable)(FOG);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.atlas);
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(DST_COLOR, SRC_COLOR);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.1);
            (gl.DepthMask)(0);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.VertexPointer)(3, FLOAT, STRIDE, v.as_ptr() as *const c_void);
            (gl.TexCoordPointer)(2, FLOAT, STRIDE, (v.as_ptr() as *const u8).add(12) as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, STRIDE, (v.as_ptr() as *const u8).add(20) as *const c_void);
            (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            (gl.DepthMask)(1);
            (gl.Disable)(BLEND);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
    }

    // ---------------------------------------------------------------- 2D

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [u8; 4]) {
        for p in [[x, y], [x, y + h], [x + w, y + h], [x + w, y]] {
            self.ui.push(V { p: [p[0], p[1], 0.0], uv: [0.0; 2], c });
        }
    }

    pub fn text(&mut self, x: f32, y: f32, s: &str, scale: f32, c: [u8; 4]) {
        let shadow = [c[0] / 4, c[1] / 4, c[2] / 4, c[3]];
        for (pass, col, off) in [(0, shadow, scale), (1, c, 0.0)] {
            let _ = pass;
            let mut cx = x;
            for ch in s.chars() {
                let g = font::glyph(ch);
                for (gx, colbits) in g.iter().enumerate() {
                    for gy in 0..7 {
                        if colbits >> gy & 1 != 0 {
                            self.rect(cx + gx as f32 * scale + off, y + gy as f32 * scale + off, scale, scale, col);
                        }
                    }
                }
                cx += font::W as f32 * scale;
            }
        }
    }

    pub fn flush_ui(&mut self, gl: &Gl, w: i32, h: i32) {
        let m: [f32; 16] = [2.0 / w as f32, 0.0, 0.0, 0.0, 0.0, -2.0 / h as f32, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, -1.0, 1.0, 0.0, 1.0];
        unsafe {
            (gl.Disable)(DEPTH_TEST);
            (gl.Disable)(CULL_FACE);
            (gl.Disable)(FOG);
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.MatrixMode)(PROJECTION);
            (gl.LoadMatrixf)(m.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadIdentity)();
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            if !self.ui.is_empty() {
                (gl.VertexPointer)(3, FLOAT, STRIDE, self.ui.as_ptr() as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, STRIDE, (self.ui.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, self.ui.len() as i32);
            }
            (gl.Disable)(BLEND);
        }
        self.ui.clear();
    }
}

pub fn perspective_pub(fov_deg: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    perspective(fov_deg, aspect, near, far)
}

fn perspective(fov_deg: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let half = math::to_rad(fov_deg as f64 / 2.0);
    let f = (math::cos(half) / math::sin(half)) as f32;
    [
        f / aspect, 0.0, 0.0, 0.0,
        0.0, f, 0.0, 0.0,
        0.0, 0.0, (far + near) / (near - far), -1.0,
        0.0, 0.0, 2.0 * far * near / (near - far), 0.0,
    ]
}

/// A view matrix looking along `dir` from the origin (column-major).
fn look(dir: [f64; 3]) -> [f64; 16] {
    let n = math::sqrt(dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).max(1e-9);
    let f = [dir[0] / n, dir[1] / n, dir[2] / n];
    let up = if f[1].abs() > 0.999 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    let mut s = [f[1] * up[2] - f[2] * up[1], f[2] * up[0] - f[0] * up[2], f[0] * up[1] - f[1] * up[0]];
    let sl = math::sqrt(s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).max(1e-9);
    s = [s[0] / sl, s[1] / sl, s[2] / sl];
    let u = [s[1] * f[2] - s[2] * f[1], s[2] * f[0] - s[0] * f[2], s[0] * f[1] - s[1] * f[0]];
    [s[0], u[0], -f[0], 0.0, s[1], u[1], -f[1], 0.0, s[2], u[2], -f[2], 0.0, 0.0, 0.0, 0.0, 1.0]
}

fn translate(m: &[f64; 16], t: [f64; 3]) -> [f32; 16] {
    let mut r = [0f32; 16];
    for i in 0..16 {
        r[i] = m[i] as f32;
    }
    for row in 0..3 {
        r[12 + row] = (m[row] * t[0] + m[4 + row] * t[1] + m[8 + row] * t[2]) as f32;
    }
    r
}

/// The six clip planes (a, b, c, d) of projection x view, for points relative to the eye.
fn frustum(p: &[f32; 16], v: &[f64; 16]) -> [[f64; 4]; 6] {
    let mut m = [0f64; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += p[k * 4 + r] as f64 * v[c * 4 + k];
            }
            m[c * 4 + r] = s;
        }
    }
    let row = |r: usize| [m[r], m[4 + r], m[8 + r], m[12 + r]];
    let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
    let add = |a: [f64; 4], b: [f64; 4], k: f64| [a[0] + k * b[0], a[1] + k * b[1], a[2] + k * b[2], a[3] + k * b[3]];
    [add(r3, r0, 1.0), add(r3, r0, -1.0), add(r3, r1, 1.0), add(r3, r1, -1.0), add(r3, r2, 1.0), add(r3, r2, -1.0)]
}

fn in_frustum(planes: &[[f64; 4]; 6], lo: [f64; 3], hi: [f64; 3]) -> bool {
    for p in planes {
        // The box corner furthest along the plane's normal.
        let x = if p[0] >= 0.0 { hi[0] } else { lo[0] };
        let y = if p[1] >= 0.0 { hi[1] } else { lo[1] };
        let z = if p[2] >= 0.0 { hi[2] } else { lo[2] };
        if p[0] * x + p[1] * y + p[2] * z + p[3] < 0.0 {
            return false;
        }
    }
    true
}
