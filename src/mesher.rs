//! Meshing sections into quads, on worker threads so that a frame never waits for it.
//!
//! The main thread copies what a section's mesh depends on (its blocks with a one-block
//! border, the biome ids around it, the biome colours) into a job; a worker turns the job into
//! quads (render.rs's vertex format, positions already in their region, see terrain.rs) and
//! the section's visibility set (occlusion.rs); the main thread takes finished meshes and puts
//! them on the card. Jobs carry a number, so a mesh made from blocks that changed again in the
//! meantime is thrown away.
//!
//! Without threads (if pthread_create fails) the same `mesh` runs on the main thread.

use crate::blocks::{self, Blocks};
use crate::render::{ci, cv, facing, shade, AO, CV, FACES};
use crate::terrain::{OPAQUE, RUNS, TRANSLUCENT};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub struct Job {
    pub key: (i32, i32, i32),
    pub gen: u64,
    /// The section's blocks and a one-block border, 18 x 18 x 18 (render.rs `ci`).
    pub cache: Vec<u16>,
    /// The same cells' light, sky << 4 | block (World::copy_light).
    pub light: Vec<u8>,
    /// Per 4-block layer, the biome ids of the 20 x 20 columns around the section.
    pub ids: Vec<u16>,
    /// Grass, foliage, water and sky colour per biome id.
    pub colours: Vec<[u32; 4]>,
    /// The block table; it outlives every job (mesher::cancel_and_wait runs before it goes).
    pub blocks: *const Blocks,
    /// Vanilla's Cutout Leaves option: off, leaves are opaque and hide each other's faces.
    pub cutout_leaves: bool,
}

unsafe impl Send for Job {}

pub struct Done {
    pub key: (i32, i32, i32),
    pub gen: u64,
    pub data: Vec<CV>,
    pub first: [u32; RUNS + 1],
    pub vis: u64,
}


/// A light byte (sky << 4 | block) as a vertex's light, (block, sky) in 1/16 steps of a level.
#[inline]
fn flat(l: u8) -> [i16; 2] {
    [((l & 15) as i16) << 4, ((l >> 4) as i16) << 4]
}

/// The smooth light of the four corners of a face: `centre` is the cell the face looks at,
/// `axis` its normal's axis. Corner i has the high side of the first in-plane axis when bit 0
/// is set and of the second when bit 1 is set (axes (axis + 1) % 3 and (axis + 2) % 3).
/// Vanilla's AO lighter: the corner's value is the mean of the face cell, the two cells
/// beside it along the corner and the diagonal one; a diagonal cell is only read when one of
/// the two beside it lets light through (else it counts as the first one), and a cell without
/// any light (an opaque one) takes the face cell's. Sky and block are averaged separately.
/// Values are (block, sky) in 1/16 steps of a level, 0 to 240.
pub fn face_corners(light: &impl Fn([i32; 3]) -> u8, clear: &impl Fn([i32; 3]) -> bool, centre: [i32; 3], axis: usize) -> [[i16; 2]; 4] {
    let (ua, va) = ((axis + 1) % 3, (axis + 2) % 3);
    let c = light(centre);
    let mut out = [[0i16; 2]; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let (su, sv) = (if i & 1 != 0 { 1 } else { -1 }, if i & 2 != 0 { 1 } else { -1 });
        let (mut pu, mut pv, mut pd) = (centre, centre, centre);
        pu[ua] += su;
        pv[va] += sv;
        pd[ua] += su;
        pd[va] += sv;
        let (lu, lv) = (light(pu), light(pv));
        let ld = if clear(pu) || clear(pv) { light(pd) } else { lu };
        let f = |v: u8| if v == 0 { c } else { v };
        let (lu, lv, ld) = (f(lu), f(lv), f(ld));
        let sky = (c >> 4) + (lu >> 4) + (lv >> 4) + (ld >> 4);
        let blk = (c & 15) + (lu & 15) + (lv & 15) + (ld & 15);
        // mean of four levels x 16
        *o = [blk as i16 * 4, sky as i16 * 4];
    }
    out
}

/// A vertex's light from a face's four corner values, by its place (0 to 1024) along the
/// face's two in-plane axes.
#[inline]
fn blend_corners(c: &[[i16; 2]; 4], pu: i16, pv: i16) -> [i16; 2] {
    if c[0] == c[1] && c[0] == c[2] && c[0] == c[3] {
        return c[0];
    }
    let (u, v) = ((pu as i32).clamp(0, 1024), (pv as i32).clamp(0, 1024));
    let w = [(1024 - u) * (1024 - v), u * (1024 - v), (1024 - u) * v, u * v];
    let mut o = [0i16; 2];
    for k in 0..2 {
        let sum: i64 = (0..4).map(|i| c[i][k] as i64 * w[i] as i64).sum();
        o[k] = ((sum + (1 << 19)) >> 20) as i16;
    }
    o
}

/// Where a model quad takes its light from: the axis of its plane and the cell it looks at
/// (offset from the block: the neighbour for a face on the block's boundary, else the block
/// itself). None for a quad that is not flat in a coordinate plane (cross plants).
fn quad_plane(q: &blocks::Quad) -> Option<(usize, [i32; 3])> {
    const DIR: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
    if q.cull < 6 {
        let edge = q.flags & blocks::Q_ON_EDGE != 0;
        return Some((AXIS_OF_DIR[q.cull as usize], if edge { DIR[q.cull as usize] } else { [0; 3] }));
    }
    for a in 0..3 {
        let e = q.pos[0][a];
        if q.pos.iter().all(|p| p[a] == e) {
            let mut d = [0; 3];
            if e == 0 {
                d[a] = -1;
            } else if e == 1024 {
                d[a] = 1;
            }
            return Some((a, d));
        }
    }
    None
}

/// The axis (0 x, 1 y, 2 z) of each direction id (down, up, north, south, west, east).
const AXIS_OF_DIR: [usize; 6] = [1, 1, 2, 2, 0, 0];

/// Meshes a job. `runs` and `stack` are the caller's scratch space.
pub fn mesh(job: &Job, runs: &mut [Vec<CV>; RUNS], stack: &mut Vec<u16>) -> Done {
    let b: &Blocks = unsafe { &*job.blocks };
    let k = job.key;
    let vis = {
        let cache = &job.cache;
        let solid = |i: usize| b.opaque_cube(cache[ci((i & 15) as i32, (i >> 8) as i32, ((i >> 4) & 15) as i32)]);
        crate::occlusion::section_vis(&solid, stack)
    };
    for r in runs.iter_mut() {
        r.clear();
    }
    // Biome colours for this section, made on first need: per 4-block layer, the biome ids
    // of a 20 x 20 area, blended over 5 x 5 (vanilla's default biome blend).
    let mut tints: Option<Vec<[[u8; 3]; 3]>> = None;
    let make_tints = || -> Vec<[[u8; 3]; 3]> {
        let mut out = alloc::vec![[[0u8; 3]; 3]; 4 * 256];
        for layer in 0..4usize {
            let ids = &job.ids[layer * 400..layer * 400 + 400];
            for z in 0..16 {
                for x in 0..16 {
                    let mut acc = [[0u32; 3]; 3];
                    let mut n = 0;
                    for dz in 0..5 {
                        for dx in 0..5 {
                            if let Some(c) = job.colours.get(ids[(z + dz) * 20 + x + dx] as usize) {
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
                    out[layer * 256 + z * 16 + x] = col;
                }
            }
        }
        out
    };
    let cache = &job.cache;
    let at = |x: i32, y: i32, z: i32| cache[ci(x, y, z)];
    let occ = |x: i32, y: i32, z: i32| b.opaque_cube(at(x, y, z));
    let lights = &job.light;
    let lt = |x: i32, y: i32, z: i32| lights[ci(x, y, z)];
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
                        let mut quad = [cv([0.0; 3], [0.0; 2], c); 4];
                        // FluidRenderer: the larger of the fluid's own cell and the one above,
                        // per channel; the floor looks at the cell below.
                        let l = if nrm[1] < 0 {
                            lt(x, y - 1, z)
                        } else {
                            let (a, u) = (lt(x, y, z), lt(x, y + 1, z));
                            (a >> 4).max(u >> 4) << 4 | (a & 15).max(u & 15)
                        };
                        for (i, cr) in corners.iter().enumerate() {
                            let p = [fx + cr[0] as f32, fy + if cr[1] == 1 { top } else { 0.0 }, fz + cr[2] as f32];
                            quad[i] = cv(p, corner_uv[i], c);
                            quad[i].light = flat(l);
                        }
                        let run = if water { TRANSLUCENT } else { facing(&quad) };
                        runs[run].extend_from_slice(&quad);
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
                let leaves = blk.name.ends_with("leaves");
                let glassy = kind == blocks::CUTOUT && !leaves;
                // LeavesBlock.skipRendering and ModelBlockRenderer.forceOpaque.
                let opaque_leaves = leaves && !job.cutout_leaves;
                // Plants, bamboo and dripstone stand at their random offset (getOffset).
                let off = b.offset_at(s, k.0 * 16 + x, k.2 * 16 + z);
                let off = [(off[0] * 1024.0) as i16, (off[1] * 1024.0) as i16, (off[2] * 1024.0) as i16];
                for q in b.model(s) {
                    if q.cull < 6 {
                        let d = DIR[q.cull as usize];
                        let ns = at(x + d[0], y + d[1], z + d[2]);
                        if b.opaque_cube(ns) || (glassy && b.state(ns).block == b.state(s).block) || (opaque_leaves && b.of_state(ns).name.ends_with("leaves")) {
                            continue;
                        }
                    }
                    let base = if q.flags & blocks::Q_TINTED != 0 { tint } else { [255, 255, 255] };
                    let light = q.light as f32 / 255.0;
                    // The face's outward direction, for ambient occlusion on boundary faces.
                    let edge = q.flags & blocks::Q_ON_EDGE != 0 && q.cull < 6;
                    let mut quad = [cv([0.0; 3], [0.0; 2], [0; 4]); 4];
                    // Smooth light: the four corners of the face's plane, blended by each
                    // vertex's place in it; a quad in no plane takes its block's light.
                    let plane = quad_plane(q);
                    let (own_light, corner_light) = match plane {
                        Some((a, d)) => {
                            let cell = [x + d[0], y + d[1], z + d[2]];
                            let l = |c: [i32; 3]| lt(c[0], c[1], c[2]);
                            let clear = |c: [i32; 3]| !occ(c[0], c[1], c[2]);
                            ([0; 2], Some((a, face_corners(&l, &clear, cell, a))))
                        }
                        None => (flat(lt(x, y, z)), None),
                    };
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
                        quad[i] = CV {
                            p: [
                                (x * 1024) as i16 + q.pos[i][0] + off[0],
                                (y * 1024) as i16 + q.pos[i][1] + off[1],
                                (z * 1024) as i16 + q.pos[i][2] + off[2],
                            ],
                            _pad: 0,
                            uv: [(q.uv[i][0] >> 1) as i16, (q.uv[i][1] >> 1) as i16],
                            c: shade(base, k, 255),
                            light: match &corner_light {
                                Some((a, c)) => blend_corners(c, q.pos[i][(a + 1) % 3], q.pos[i][(a + 2) % 3]),
                                None => own_light,
                            },
                        };
                    }
                    let run = if q.flags & blocks::Q_TRANSLUCENT != 0 {
                        TRANSLUCENT
                    } else if opaque_leaves {
                        OPAQUE
                    } else {
                        facing(&quad)
                    };
                    runs[run].extend_from_slice(&quad);
                }
            }
        }
    }
    let mut first = [0u32; RUNS + 1];
    let total: usize = runs.iter().map(|r| r.len()).sum();
    let mut data: Vec<CV> = Vec::with_capacity(total);
    for (i, r) in runs.iter().enumerate() {
        first[i] = (data.len() / 4) as u32;
        for v in r.iter() {
            data.push(CV { p: crate::terrain::to_region(k, v.p), ..*v });
        }
    }
    first[RUNS] = (data.len() / 4) as u32;
    Done { key: k, gen: job.gen, data, first, vis }
}

// ------------------------------------------------------------------------------ the pool

/// The queue between the main thread and the workers, behind a spin lock (held only to push
/// or pop: a few instructions).
struct Shared {
    lock: AtomicBool,
    jobs: UnsafeCell<VecDeque<Job>>,
    done: UnsafeCell<Vec<Done>>,
    /// Jobs taken by a worker and not finished yet.
    busy: AtomicU32,
    workers: u32,
}

unsafe impl Sync for Shared {}

impl Shared {
    fn with<R>(&self, f: impl FnOnce(&mut VecDeque<Job>, &mut Vec<Done>) -> R) -> R {
        while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let r = unsafe { f(&mut *self.jobs.get(), &mut *self.done.get()) };
        self.lock.store(false, Ordering::Release);
        r
    }
}

static mut POOL: Option<&'static Shared> = None;

extern "C" fn worker(arg: *mut c_void) -> *mut c_void {
    let sh: &'static Shared = unsafe { &*(arg as *const Shared) };
    let mut runs: [Vec<CV>; RUNS] = Default::default();
    let mut stack = Vec::new();
    loop {
        let job = sh.with(|jobs, _| {
            let j = jobs.pop_front();
            if j.is_some() {
                sh.busy.fetch_add(1, Ordering::AcqRel);
            }
            j
        });
        match job {
            Some(j) => {
                let d = mesh(&j, &mut runs, &mut stack);
                drop(j);
                sh.with(|_, done| done.push(d));
                sh.busy.fetch_sub(1, Ordering::AcqRel);
            }
            None => crate::sys::sleep_ms(1),
        }
    }
}

/// The workers, started on first use: one less than the processors, at most four. None when
/// threads cannot be made or PENRYN_MESH_THREADS=0 (then meshing stays on the main thread).
fn pool() -> Option<&'static Shared> {
    unsafe {
        if let Some(p) = *core::ptr::addr_of!(POOL) {
            return if p.workers > 0 { Some(p) } else { None };
        }
        let off = crate::sys::env("PENRYN_MESH_THREADS").map_or(false, |v| v.trim() == "0");
        let n = if off { 0 } else { (crate::sys::processors() - 1).clamp(1, 4) };
        let sh: &'static mut Shared = alloc::boxed::Box::leak(alloc::boxed::Box::new(Shared {
            lock: AtomicBool::new(false),
            jobs: UnsafeCell::new(VecDeque::new()),
            done: UnsafeCell::new(Vec::new()),
            busy: AtomicU32::new(0),
            workers: 0,
        }));
        let mut made = 0;
        for _ in 0..n {
            let mut t = 0usize;
            if crate::sys::pthread_create(&mut t, core::ptr::null(), worker, sh as *const Shared as *mut c_void) == 0 {
                crate::sys::pthread_detach(t);
                made += 1;
            }
        }
        sh.workers = made;
        POOL = Some(sh);
        if made > 0 {
            Some(sh)
        } else {
            None
        }
    }
}

/// Whether meshing runs on worker threads (else the caller meshes itself).
pub fn threaded() -> bool {
    pool().is_some()
}

pub fn workers() -> u32 {
    pool().map_or(0, |p| p.workers)
}

pub fn submit(job: Job) {
    if let Some(p) = pool() {
        p.with(|jobs, _| jobs.push_back(job));
    }
}

/// Moves the finished meshes into `out`.
pub fn collect(out: &mut Vec<Done>) {
    if let Some(p) = pool() {
        p.with(|_, done| out.append(done));
    }
}

/// Drops the jobs not started yet and waits for the running ones (before the block table
/// goes, or when every mesh is thrown away).
pub fn cancel_and_wait() {
    let Some(p) = pool() else { return };
    p.with(|jobs, _| jobs.clear());
    while p.busy.load(Ordering::Acquire) != 0 {
        crate::sys::sleep_ms(1);
    }
    p.with(|_, done| done.clear());
}

/// Jobs queued or running.
pub fn pending() -> usize {
    pool().map_or(0, |p| p.with(|jobs, _| jobs.len()) + p.busy.load(Ordering::Acquire) as usize)
}

// ------------------------------------------------------------------------------ selftest

pub fn selftest() -> Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    let mut v = Vec::new();
    let all = |_c: [i32; 3]| true;
    // A uniform field gives its own level, sky 15 and block 5: (80, 240).
    let c = face_corners(&|_| 0xF5, &all, [5, 5, 5], 1);
    v.push(("face light uniform", format!("{:?}", c), "[[80, 240], [80, 240], [80, 240], [80, 240]]"));
    // A cell without light beside the face cell counts as the face cell's (vanilla's blend).
    let c = face_corners(&|p| if p == [5, 5, 6] { 0 } else { 0xF0 }, &all, [5, 5, 5], 1);
    v.push(("face light zero cell", format!("{:?}", c[1]), "[0, 240]"));
    // Sky = x: the corners at x - 1 average 4, 5, 5, 4 (4.5 x 16 = 72), those at x + 1 5.5 (88).
    // Axis 1: first in-plane axis z (bit 0), second x (bit 1).
    let c = face_corners(&|p| (p[0] as u8) << 4, &all, [5, 5, 5], 1);
    v.push(("face light gradient", format!("{:?}", c.map(|a| a[1])), "[72, 72, 88, 88]"));
    v.push(("blend corners", format!("{:?} {:?} {:?}", blend_corners(&c, 0, 0), blend_corners(&c, 1024, 1024), blend_corners(&c, 512, 512)), "[0, 72] [0, 88] [0, 80]"));
    // The diagonal cell is only read when one of the two beside it lets light through.
    let lf = |p: [i32; 3]| if p == [6, 5, 6] || p == [5, 5, 5] { 0xF0 } else { 0x30 };
    let (shut, open) = (face_corners(&lf, &|_| false, [5, 5, 5], 1), face_corners(&lf, &all, [5, 5, 5], 1));
    v.push(("face light diagonal", format!("{} {}", shut[3][1], open[3][1]), "96 144"));
    // Quad planes: a cut face on the edge looks at the neighbour, a slab top at its own cell.
    let q = |pos: [[i16; 3]; 4], cull: u8, flags: u8| blocks::Quad { pos, uv: [[0; 2]; 4], cull, flags, light: 255 };
    let top = [[0, 512, 0], [0, 512, 1024], [1024, 512, 1024], [1024, 512, 0]];
    let a = quad_plane(&q(top, 6, 0));
    let b = quad_plane(&q([[0, 1024, 0]; 4].map(|p| p), 1, blocks::Q_ON_EDGE));
    let cross = quad_plane(&q([[0, 0, 0], [0, 1024, 0], [1024, 1024, 1024], [1024, 0, 1024]], 6, 0));
    v.push(("quad plane", format!("{:?} {:?} {:?}", a, b, cross), "Some((1, [0, 0, 0])) Some((1, [0, 1, 0])) None"));
    v
}
