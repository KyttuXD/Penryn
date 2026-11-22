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
use crate::terrain::{RUNS, TRANSLUCENT};
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
    /// Per 4-block layer, the biome ids of the 20 x 20 columns around the section.
    pub ids: Vec<u16>,
    /// Grass, foliage, water and sky colour per biome id.
    pub colours: Vec<[u32; 4]>,
    /// The block table; it outlives every job (Renderer::wait_jobs runs before it goes).
    pub blocks: *const Blocks,
}

unsafe impl Send for Job {}

pub struct Done {
    pub key: (i32, i32, i32),
    pub gen: u64,
    pub data: Vec<CV>,
    pub first: [u32; RUNS + 1],
    pub vis: u64,
}

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
                        for (i, cr) in corners.iter().enumerate() {
                            let p = [fx + cr[0] as f32, fy + if cr[1] == 1 { top } else { 0.0 }, fz + cr[2] as f32];
                            quad[i] = cv(p, corner_uv[i], c);
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
                    let mut quad = [cv([0.0; 3], [0.0; 2], [0; 4]); 4];
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
                        };
                    }
                    let run = if q.flags & blocks::Q_TRANSLUCENT != 0 { TRANSLUCENT } else { facing(&quad) };
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
/// threads cannot be made (then meshing stays on the main thread).
fn pool() -> Option<&'static Shared> {
    unsafe {
        if let Some(p) = *core::ptr::addr_of!(POOL) {
            return if p.workers > 0 { Some(p) } else { None };
        }
        let n = (crate::sys::processors() - 1).clamp(1, 4);
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
    crate::sys::env("PENRYN_MESH_THREADS").map_or(true, |v| v.trim() != "0") && pool().is_some()
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
