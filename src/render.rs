//! Drawing with OpenGL 1.5 fixed function: one vertex buffer per 16x16x16 section (opaque and
//! water apart), coloured faces with ambient occlusion instead of textures, fog, the block
//! outline and a 2D layer for the HUD.

use crate::blocks::Blocks;
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


pub fn cv(p: [f32; 3], uv: [f32; 2], c: [u8; 4]) -> CV {
    CV {
        p: [(p[0] * 1024.0) as i16, (p[1] * 1024.0) as i16, (p[2] * 1024.0) as i16],
        _pad: 0,
        uv: [(uv[0] * 32767.0) as i16, (uv[1] * 32767.0) as i16],
        c,
    }
}

use crate::terrain::{Terrain, RUNS};

pub struct Renderer {
    /// The meshed sections, on the card (terrain.rs).
    pub terrain: Terrain,
    epoch: u32,
    cache: Vec<u16>,
    /// A section's quads by run while it is meshed: facing down, up, north, south, west, east,
    /// any (solid and cut-out together), then translucent.
    runs: [Vec<CV>; RUNS],
    /// Sections being meshed on the worker threads, and the number of their newest job.
    inflight: BTreeMap<(i32, i32, i32), u64>,
    gen: u64,
    done: Vec<crate::mesher::Done>,
    batch: Vec<(i32, i32, i32)>,
    /// Cave culling.
    pub graph: crate::occlusion::Graph,
    eye_in_opaque: bool,
    /// Vanilla's Cutout Leaves option (set by the caller each frame), and the value the
    /// meshes were made with: a change meshes everything again.
    pub cutout_leaves: bool,
    meshed_leaves: bool,
    vis_stack: Vec<u16>,
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
pub const FACES: [([i32; 3], [[u8; 3]; 4], f32); 6] = [
    ([0, 1, 0], [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]], 1.0),
    ([0, -1, 0], [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]], 0.5),
    ([1, 0, 0], [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]], 0.6),
    ([-1, 0, 0], [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]], 0.6),
    ([0, 0, 1], [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], 0.8),
    ([0, 0, -1], [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]], 0.8),
];
pub const AO: [f32; 4] = [0.5, 0.68, 0.84, 1.0];

const P: i32 = 18; // padded section side
pub fn ci(x: i32, y: i32, z: i32) -> usize {
    (((y + 1) * P + (z + 1)) * P + (x + 1)) as usize
}

pub fn shade(c: [u8; 3], k: f32, a: u8) -> [u8; 4] {
    let f = |v: u8| (v as f32 * k).min(255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2]), a]
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            terrain: Terrain::new(),
            epoch: 0,
            cache: alloc::vec![0; (P * P * P) as usize],
            runs: Default::default(),
            inflight: BTreeMap::new(),
            gen: 0,
            done: Vec::new(),
            batch: Vec::new(),
            graph: crate::occlusion::Graph::new(),
            eye_in_opaque: false,
            cutout_leaves: true,
            meshed_leaves: true,
            vis_stack: Vec::new(),
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

    /// Meshes the dirty sections nearest the camera: hands them to the worker threads and
    /// puts what they finished on the card (for up to `budget_ms`), or without threads meshes
    /// them here for up to `budget_ms`.
    pub fn update(&mut self, gl: &Gl, world: &mut World, b: &Blocks, eye: [f64; 3], budget_ms: u64) {
        if world.epoch != self.epoch {
            self.epoch = world.epoch;
            crate::mesher::cancel_and_wait();
            self.inflight.clear();
            self.done.clear();
            self.terrain.clear(gl);
            self.graph.clear();
        }
        // Vanilla turns cave culling off for a spectator inside a full block (LevelRenderer
        // .setupRender): the camera then sees through the ground.
        self.eye_in_opaque = b.opaque_cube(world.get(math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2])));
        if self.cutout_leaves != self.meshed_leaves {
            self.meshed_leaves = self.cutout_leaves;
            let n = world.height / 16;
            let keys: Vec<(i32, i32)> = world.columns.keys().copied().collect();
            for (x, z) in keys {
                for sy in 0..n {
                    world.dirty.insert((x, sy, z));
                }
            }
        }
        let start = crate::sys::now_ms();
        let threaded = crate::mesher::threaded();
        if threaded {
            crate::mesher::collect(&mut self.done);
            let mut used = 0;
            for d in self.done.iter_mut() {
                if used > 0 && crate::sys::now_ms() - start > budget_ms {
                    break;
                }
                used += 1;
                if self.inflight.get(&d.key) != Some(&d.gen) {
                    continue; // its blocks changed again: a newer job is on the way
                }
                self.inflight.remove(&d.key);
                self.graph.set(d.key, d.vis);
                if d.first[RUNS] == 0 {
                    self.terrain.remove(gl, d.key);
                } else {
                    self.terrain.put(gl, d.key, core::mem::take(&mut d.data), d.first);
                }
            }
            self.done.drain(..used);
        }
        let (ex, ey, ez) = (eye[0] as i32 >> 4, (eye[1] as i32 - world.min_y) >> 4, eye[2] as i32 >> 4);
        let most = crate::mesher::workers() as usize * 3 + 2;
        // The nearest dirty sections, a batch at a time (one pass over the set per batch).
        'batches: loop {
            nearest(&world.dirty, (ex, ey, ez), &mut self.batch);
            if self.batch.is_empty() {
                break;
            }
            for i in 0..self.batch.len() {
                if crate::sys::now_ms() - start > budget_ms || (threaded && crate::mesher::pending() >= most) {
                    break 'batches;
                }
                let k = self.batch[i];
                world.dirty.remove(&k);
                let Some(job) = self.job(gl, world, b, k) else { continue };
                if threaded {
                    self.inflight.insert(k, job.gen);
                    crate::mesher::submit(job);
                } else {
                    let d = crate::mesher::mesh(&job, &mut self.runs, &mut self.vis_stack);
                    self.graph.set(k, d.vis);
                    if d.first[RUNS] == 0 {
                        self.terrain.remove(gl, k);
                    } else {
                        self.terrain.put(gl, k, d.data, d.first);
                    }
                }
            }
        }
    }

    /// What meshing section `k` needs, or None when it has nothing to draw (then it is
    /// dropped here).
    fn job(&mut self, gl: &Gl, world: &World, b: &Blocks, k: (i32, i32, i32)) -> Option<crate::mesher::Job> {
        if !world.columns.contains_key(&(k.0, k.2)) || !self.fill_cache(world, k) {
            self.inflight.remove(&k);
            self.terrain.remove(gl, k);
            self.graph.set(k, crate::occlusion::ALL);
            return None;
        }
        // The biome ids of the 20 x 20 columns around, per 4-block layer (as World::biome).
        let mut cols: [[Option<&crate::world::Column>; 3]; 3] = [[None; 3]; 3];
        for dz in 0..3 {
            for dx in 0..3 {
                cols[dz][dx] = world.columns.get(&(k.0 + dx as i32 - 1, k.2 + dz as i32 - 1));
            }
        }
        let mut ids = alloc::vec![0u16; 1600];
        for layer in 0..4 {
            let ly = (k.1 * 16 + layer * 4 + 2).clamp(0, world.height - 1);
            let (sec, cy) = ((ly >> 4) as usize, (ly & 15) >> 2);
            for gz in 0..20 {
                let wz = k.2 * 16 + gz - 2;
                let (cz, lz) = (((wz >> 4) - k.2 + 1) as usize, wz & 15);
                for gx in 0..20 {
                    let wx = k.0 * 16 + gx - 2;
                    let (cx, lx) = (((wx >> 4) - k.0 + 1) as usize, wx & 15);
                    ids[(layer * 400 + gz * 20 + gx) as usize] = match cols[cz][cx] {
                        Some(c) => c.biomes[sec][((cy * 4 + (lz >> 2)) * 4 + (lx >> 2)) as usize],
                        None => u16::MAX,
                    };
                }
            }
        }
        self.gen += 1;
        Some(crate::mesher::Job { key: k, gen: self.gen, cache: self.cache.clone(), ids, colours: world.biome_colours.clone(), blocks: b as *const Blocks, cutout_leaves: self.meshed_leaves })
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
            crate::perf::lap(crate::perf::S::Sky);
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
        let maxd = (view_chunks as f64 + 1.0) * 16.0;
        let layers = world.height / 16;
        let cam = (math::ifloor(eye[0]) >> 4, (math::ifloor(eye[1]) - world.min_y) >> 4, math::ifloor(eye[2]) >> 4);
        self.graph.update(cam, view_chunks + 1, layers, !self.eye_in_opaque);
        let graph = &self.graph;
        let mut keep = |k: (i32, i32, i32), lo: [f64; 3]| {
            let (cx, cz) = (lo[0] + 8.0, lo[2] + 8.0);
            cx * cx + cz * cz <= maxd * maxd && in_frustum(&planes, lo, [lo[0] + 16.0, lo[1] + 16.0, lo[2] + 16.0]) && graph.visible(k)
        };
        self.terrain.select(eye, world.min_y, &mut keep, &faces_seen);
        self.sections_drawn = self.terrain.sections_drawn;
        // Solid and cut-out quads together, with the alpha test, near to far: on the cards
        // measured one pass beats two (solid without the test, then cut-out with it).
        unsafe { (gl.Enable)(ALPHA_TEST) };
        self.terrain.draw_opaque(gl, &view, eye, world.min_y);
        crate::perf::lap(crate::perf::S::World);
        // Entities, between the opaque world and the water (vanilla's order).
        unsafe {
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
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(ALPHA_TEST);
        }
        // Water, far to near, blended, without writing depth.
        unsafe {
            (gl.Enable)(BLEND);
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
            (gl.DepthMask)(0);
            (gl.AlphaFunc)(GREATER, 0.004);
        }
        self.terrain.draw_translucent(gl, &view, eye, world.min_y);
        self.vertices_drawn = self.terrain.quads_drawn * 4;
        crate::perf::lap(crate::perf::S::Water);
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

/// Which facing runs (bit per BUCKETS entry) of a section whose corner is at `lo` (relative to
/// the eye) can face the camera: a face looking up is seen only from above its plane, and so on.
/// Faces stay within a block or two of their section (models reach out a block, plants move).
fn faces_seen(lo: [f64; 3]) -> u8 {
    const M: f64 = 2.0;
    let mut f = 0u8;
    // 0 down, 1 up, 2 north (-z), 3 south (+z), 4 west (-x), 5 east (+x)
    for (axis, neg, pos) in [(1, 0, 1), (2, 2, 3), (0, 4, 5)] {
        if lo[axis] + 16.0 + M > 0.0 {
            f |= 1 << neg;
        }
        if lo[axis] - M < 0.0 {
            f |= 1 << pos;
        }
    }
    f
}

/// Which facing run a quad goes in: its normal (from the diagonals, counter-clockwise front)
/// when it lies along an axis, else 6.
pub fn facing(q: &[CV; 4]) -> usize {
    let d = |a: usize, b: usize| [q[b].p[0] as i32 - q[a].p[0] as i32, q[b].p[1] as i32 - q[a].p[1] as i32, q[b].p[2] as i32 - q[a].p[2] as i32];
    let (a, b) = (d(0, 2), d(1, 3));
    let n = [
        a[1] as i64 * b[2] as i64 - a[2] as i64 * b[1] as i64,
        a[2] as i64 * b[0] as i64 - a[0] as i64 * b[2] as i64,
        a[0] as i64 * b[1] as i64 - a[1] as i64 * b[0] as i64,
    ];
    match (n[0].signum(), n[1].signum(), n[2].signum()) {
        (0, -1, 0) => 0,
        (0, 1, 0) => 1,
        (0, 0, -1) => 2,
        (0, 0, 1) => 3,
        (-1, 0, 0) => 4,
        (1, 0, 0) => 5,
        _ => 6,
    }
}

/// Draws the runs `base..base + 7` picked by `mask` (or just `base` for the translucent run,
/// mask 1) of a section in one call, merging runs that touch. Returns the quads drawn.
/// The (up to) 64 dirty sections nearest the camera section `e`, nearest first (horizontal
/// distance counts four times the vertical, as the order chunks come in).
fn nearest(dirty: &alloc::collections::BTreeSet<(i32, i32, i32)>, e: (i32, i32, i32), out: &mut Vec<(i32, i32, i32)>) {
    const K: usize = 64;
    let mut best: [(i32, (i32, i32, i32)); K] = [(i32::MAX, (0, 0, 0)); K];
    let mut n = 0;
    for &k in dirty.iter() {
        let d = (k.0 - e.0).abs().max((k.2 - e.2).abs()) * 4 + (k.1 - e.1).abs();
        if n == K && d >= best[K - 1].0 {
            continue;
        }
        // Insert in order.
        let mut i = if n < K { n } else { K - 1 };
        while i > 0 && best[i - 1].0 > d {
            best[i] = best[i - 1];
            i -= 1;
        }
        best[i] = (d, k);
        if n < K {
            n += 1;
        }
    }
    out.clear();
    out.extend(best[..n].iter().map(|b| b.1));
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

/// Checks for `penryn selftest`: quads land in the right facing run, and a run is drawn
/// exactly when its faces can be seen.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut names = String::new();
    for (_, corners, _) in FACES.iter() {
        let mut q = [cv([0.0; 3], [0.0; 2], [0; 4]); 4];
        for (i, c) in corners.iter().enumerate() {
            q[i] = cv([c[0] as f32 + 3.0, c[1] as f32 + 5.0, c[2] as f32 + 7.0], [0.0; 2], [0; 4]);
        }
        names.push_str(&alloc::format!("{}", facing(&q)));
    }
    // FACES: up, down, east, west, south, north.
    out.push(("render facing runs", names, "105432"));
    let cross = [cv([0.0, 0.0, 0.0], [0.0; 2], [0; 4]), cv([1.0, 0.0, 1.0], [0.0; 2], [0; 4]), cv([1.0, 1.0, 1.0], [0.0; 2], [0; 4]), cv([0.0, 1.0, 0.0], [0.0; 2], [0; 4])];
    out.push(("render slanted run", alloc::format!("{}", facing(&cross)), "6"));
    // A section far below, east and south of the eye: its tops, west and north faces show.
    out.push(("render faces seen", alloc::format!("{:06b} {:06b}", faces_seen([40.0, -60.0, 30.0]), faces_seen([-8.0, -8.0, -8.0])), "010110 111111"));
    out
}
