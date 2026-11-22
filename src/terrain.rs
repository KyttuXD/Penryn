//! Where the meshed sections live on the card, and drawing them with few calls.
//!
//! Old drivers (Apple's above all) spend several microseconds of CPU on every draw call, so a
//! call per section (hundreds a frame) costs more than the drawing itself. Sections are
//! therefore packed into regions of 8 x 4 x 8 sections (128 x 64 x 128 blocks), one vertex
//! buffer each, and a frame draws every visible section of a region with one
//! glMultiDrawElements. Positions are stored relative to the region's centre in 1/256 block
//! (16-bit), so one matrix serves the whole region.
//!
//! Inside a region each section has a slot with some room to grow; a rebuilt section is
//! written in place with glBufferSubData when it fits, else at the end, and the region is
//! packed again into a bigger buffer when it runs out of room (read back from the card: no
//! copy of the meshes is kept in memory).
//!
//! A section's quads come in runs (render.rs): seven by facing (down, up, north, south,
//! west, east, any) for the solid and cut-out quads, then the translucent ones.

use crate::render::CV;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ffi::c_void;

pub const RUNS: usize = 8;
pub const TRANSLUCENT: usize = 7;
/// The run of quads facing any way (always drawn).
pub const ANY: usize = 6;
const CSTRIDE: i32 = 16;

/// Region size in sections.
const RX: i32 = 8;
const RY: i32 = 4;
/// Units per block of the stored positions.
pub const UNITS: f64 = 256.0;

type SKey = (i32, i32, i32);
type RKey = (i32, i32, i32);

fn region_of(k: SKey) -> RKey {
    (k.0.div_euclid(RX), k.1.div_euclid(RY), k.2.div_euclid(RX))
}

/// The region's centre, in blocks: x and z from 0, y from the bottom of the world.
fn centre(r: RKey) -> [f64; 3] {
    [(r.0 * RX * 16 + RX * 8) as f64, (r.1 * RY * 16 + RY * 8) as f64, (r.2 * RX * 16 + RX * 8) as f64]
}

/// A section-local position (1/1024 block) as stored in its region (1/256 block from the
/// region's centre).
pub fn to_region(k: SKey, p: [i16; 3]) -> [i16; 3] {
    let r = region_of(k);
    let off = [
        ((k.0 - r.0 * RX) * 16 - RX * 8) * 256,
        ((k.1 - r.1 * RY) * 16 - RY * 8) * 256,
        ((k.2 - r.2 * RX) * 16 - RX * 8) * 256,
    ];
    let mut out = [0i16; 3];
    for a in 0..3 {
        out[a] = (((p[a] as i32 + 2) >> 2) + off[a]) as i16;
    }
    out
}

struct Region {
    vbo: u32,
    /// Room, and the end of the last slot, in quads.
    cap: u32,
    top: u32,
    /// Quads held by live slots (with their room to grow).
    live: u32,
}

struct Slot {
    at: u32,
    cap: u32,
    /// Where each run starts within the slot, in quads; first[RUNS] is the total.
    first: [u32; RUNS + 1],
}

/// A section to draw: its region, slot, runs and the runs facing the camera.
#[derive(Clone, Copy)]
struct Item {
    rank: u32,
    d2: f64,
    region: RKey,
    first: [u32; RUNS + 1],
    runs: u16,
}

pub struct Terrain {
    regions: BTreeMap<RKey, Region>,
    slots: BTreeMap<SKey, Slot>,
    /// The shared quad index buffer (0 1 2, 2 3 0 per quad, as vanilla's), 32-bit.
    index: u32,
    index_quads: u32,
    items: Vec<Item>,
    seen: Vec<(RKey, f64)>,
    counts: Vec<i32>,
    offsets: Vec<*const c_void>,
    pub sections_drawn: u32,
    pub quads_drawn: u32,
    /// Draw calls in the last frame (PENRYN_PERF).
    pub calls: u32,
}

impl Terrain {
    pub fn new() -> Terrain {
        Terrain {
            regions: BTreeMap::new(),
            slots: BTreeMap::new(),
            index: 0,
            index_quads: 0,
            items: Vec::new(),
            seen: Vec::new(),
            counts: Vec::new(),
            offsets: Vec::new(),
            sections_drawn: 0,
            quads_drawn: 0,
            calls: 0,
        }
    }

    pub fn clear(&mut self, gl: &Gl) {
        for (_, r) in core::mem::take(&mut self.regions) {
            unsafe { (gl.DeleteBuffers)(1, &r.vbo) };
        }
        self.slots.clear();
    }

    pub fn remove(&mut self, gl: &Gl, k: SKey) {
        let Some(s) = self.slots.remove(&k) else { return };
        let rk = region_of(k);
        if let Some(r) = self.regions.get_mut(&rk) {
            r.live -= s.cap;
            if r.live == 0 {
                unsafe { (gl.DeleteBuffers)(1, &r.vbo) };
                self.regions.remove(&rk);
            }
        }
    }

    /// Stores a section's quads (positions already region-relative, see `to_region`).
    pub fn put(&mut self, gl: &Gl, k: SKey, data: Vec<CV>, first: [u32; RUNS + 1]) {
        let quads = first[RUNS];
        if quads == 0 {
            self.remove(gl, k);
            return;
        }
        let rk = region_of(k);
        // In place, when it fits its old slot.
        if let (Some(s), Some(r)) = (self.slots.get_mut(&k), self.regions.get(&rk)) {
            if quads <= s.cap {
                upload(gl, r.vbo, s.at, &data);
                s.first = first;
                return;
            }
        }
        let old_cap = self.slots.get(&k).map_or(0, |s| s.cap);
        let need = quads + quads / 4 + 16;
        let r = self.regions.entry(rk).or_insert(Region { vbo: 0, cap: 0, top: 0, live: 0 });
        r.live = r.live - old_cap + need;
        if r.vbo != 0 && r.top + need <= r.cap {
            let at = r.top;
            r.top += need;
            upload(gl, r.vbo, at, &data);
            self.slots.insert(k, Slot { at, cap: need, first });
            return;
        }
        // Out of room: pack the region again in a bigger buffer, the other sections read back
        // from the old one (rare: the room grows by half each time).
        self.slots.remove(&k);
        let r = self.regions.get_mut(&rk).unwrap();
        let cap = (r.live * 3 / 2).max(4096).next_power_of_two();
        let old = r.vbo;
        let mut vbo = 0u32;
        unsafe {
            (gl.GenBuffers)(1, &mut vbo);
            (gl.BindBuffer)(ARRAY_BUFFER, vbo);
            (gl.BufferData)(ARRAY_BUFFER, cap as isize * 4 * CSTRIDE as isize, core::ptr::null(), STATIC_DRAW);
        }
        r.vbo = vbo;
        r.cap = cap;
        let mut top = 0;
        let lo = (rk.0 * RX, rk.1 * RY, rk.2 * RX);
        let hi = (lo.0 + RX - 1, lo.1 + RY - 1, lo.2 + RX - 1);
        let mut tmp: Vec<CV> = Vec::new();
        for (sk, s) in self.slots.range_mut(lo..=hi) {
            if region_of(*sk) != rk {
                continue;
            }
            let n = s.first[RUNS] as usize * 4;
            tmp.clear();
            tmp.reserve(n);
            unsafe {
                tmp.set_len(n);
                (gl.BindBuffer)(ARRAY_BUFFER, old);
                (gl.GetBufferSubData)(ARRAY_BUFFER, s.at as isize * 4 * CSTRIDE as isize, (n * CSTRIDE as usize) as isize, tmp.as_mut_ptr() as *mut c_void);
                (gl.BindBuffer)(ARRAY_BUFFER, vbo);
                (gl.BufferSubData)(ARRAY_BUFFER, top as isize * 4 * CSTRIDE as isize, (n * CSTRIDE as usize) as isize, tmp.as_ptr() as *const c_void);
            }
            s.at = top;
            top += s.cap;
        }
        unsafe {
            (gl.BufferSubData)(ARRAY_BUFFER, top as isize * 4 * CSTRIDE as isize, (data.len() * CSTRIDE as usize) as isize, data.as_ptr() as *const c_void);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            if old != 0 {
                (gl.DeleteBuffers)(1, &old);
            }
        }
        self.slots.insert(k, Slot { at: top, cap: need, first });
        r.top = top + need;
        self.ensure_index(gl, cap);
    }

    fn ensure_index(&mut self, gl: &Gl, quads: u32) {
        if quads <= self.index_quads {
            return;
        }
        let n = quads.next_power_of_two();
        let mut ix: Vec<u32> = Vec::with_capacity(n as usize * 6);
        for q in 0..n {
            let v = q * 4;
            ix.extend_from_slice(&[v, v + 1, v + 2, v + 2, v + 3, v]);
        }
        unsafe {
            if self.index == 0 {
                (gl.GenBuffers)(1, &mut self.index);
            }
            (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, self.index);
            (gl.BufferData)(ELEMENT_ARRAY_BUFFER, (ix.len() * 4) as isize, ix.as_ptr() as *const c_void, STATIC_DRAW);
            (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, 0);
        }
        self.index_quads = n;
    }

    /// Picks the sections to draw this frame: `keep(k, lo)` says whether section k (corner
    /// `lo` relative to the eye) is in view; `faces(lo)` which facing runs can face the eye.
    pub fn select(&mut self, eye: [f64; 3], min_y: i32, keep: &mut dyn FnMut((i32, i32, i32), [f64; 3]) -> bool, faces: &dyn Fn([f64; 3]) -> u8) {
        self.items.clear();
        for (&k, s) in self.slots.iter() {
            let lo = [(k.0 * 16) as f64 - eye[0], (min_y + k.1 * 16) as f64 - eye[1], (k.2 * 16) as f64 - eye[2]];
            if !keep(k, lo) {
                continue;
            }
            let c = [lo[0] + 8.0, lo[1] + 8.0, lo[2] + 8.0];
            let d2 = c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
            let mut first = s.first;
            for f in first.iter_mut() {
                *f += s.at;
            }
            self.items.push(Item { rank: 0, d2, region: region_of(k), first, runs: faces(lo) as u16 | 1 << ANY });
        }
        // Near to far, and each region's sections together (ranked by its nearest section),
        // so a region is one call and what is in front is drawn first.
        self.items.sort_unstable_by(|a, b| a.d2.partial_cmp(&b.d2).unwrap_or(core::cmp::Ordering::Equal));
        self.seen.clear();
        for it in self.items.iter_mut() {
            let rank = match self.seen.iter().position(|s| s.0 == it.region) {
                Some(i) => i,
                None => {
                    self.seen.push((it.region, it.d2));
                    self.seen.len() - 1
                }
            };
            it.rank = rank as u32;
        }
        self.sections_drawn = self.items.len() as u32;
        self.quads_drawn = 0;
        self.calls = 0;
    }

    /// Draws the solid and cut-out runs of the selected sections (the caller has set up the
    /// textures, fog and alpha test; `view` is the camera's rotation).
    pub fn draw_opaque(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32) {
        let mut items = core::mem::take(&mut self.items);
        items.sort_unstable_by(|a, b| a.rank.cmp(&b.rank).then(a.d2.partial_cmp(&b.d2).unwrap_or(core::cmp::Ordering::Equal)));
        self.draw_list(gl, view, eye, min_y, &items, false);
        self.items = items;
    }

    /// Draws the translucent runs, far to near (strictly, for the blending).
    pub fn draw_translucent(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32) {
        let mut items = core::mem::take(&mut self.items);
        items.retain(|it| it.first[TRANSLUCENT] != it.first[RUNS]);
        items.sort_unstable_by(|a, b| b.d2.partial_cmp(&a.d2).unwrap_or(core::cmp::Ordering::Equal));
        self.draw_list(gl, view, eye, min_y, &items, true);
        self.items = items;
    }

    fn draw_list(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32, items: &[Item], translucent: bool) {
        if items.is_empty() {
            return;
        }
        unsafe { (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, self.index) };
        let mut i = 0;
        while i < items.len() {
            let rk = items[i].region;
            self.counts.clear();
            self.offsets.clear();
            let mut open: Option<(u32, u32)> = None;
            while i < items.len() && items[i].region == rk {
                let it = &items[i];
                i += 1;
                let mask = if translucent { 1u16 << TRANSLUCENT } else { it.runs };
                for run in 0..RUNS {
                    if mask >> run & 1 == 0 {
                        continue;
                    }
                    let (s, e) = (it.first[run], it.first[run + 1]);
                    if s == e {
                        continue;
                    }
                    self.quads_drawn += e - s;
                    open = match open {
                        Some((os, oc)) if os + oc == s => Some((os, oc + e - s)),
                        Some((os, oc)) => {
                            self.counts.push((oc * 6) as i32);
                            self.offsets.push((os as usize * 24) as *const c_void);
                            Some((s, e - s))
                        }
                        None => Some((s, e - s)),
                    };
                }
            }
            if let Some((os, oc)) = open {
                self.counts.push((oc * 6) as i32);
                self.offsets.push((os as usize * 24) as *const c_void);
            }
            let Some(r) = self.regions.get(&rk) else { continue };
            if self.counts.is_empty() {
                continue;
            }
            let c = centre(rk);
            let t = [c[0] - eye[0], c[1] + min_y as f64 - eye[1], c[2] - eye[2]];
            let mut m = [0f32; 16];
            for k in 0..16 {
                m[k] = view[k] as f32;
            }
            for row in 0..3 {
                m[12 + row] = (view[row] * t[0] + view[4 + row] * t[1] + view[8 + row] * t[2]) as f32;
            }
            for k in 0..12 {
                m[k] /= UNITS as f32;
            }
            unsafe {
                (gl.LoadMatrixf)(m.as_ptr());
                (gl.BindBuffer)(ARRAY_BUFFER, r.vbo);
                (gl.VertexPointer)(3, SHORT, CSTRIDE, core::ptr::null());
                (gl.TexCoordPointer)(2, SHORT, CSTRIDE, 8 as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, CSTRIDE, 12 as *const c_void);
                if self.counts.len() == 1 {
                    (gl.DrawElements)(TRIANGLES, self.counts[0], UNSIGNED_INT, self.offsets[0]);
                } else {
                    (gl.MultiDrawElements)(TRIANGLES, self.counts.as_ptr(), UNSIGNED_INT, self.offsets.as_ptr(), self.counts.len() as i32);
                }
            }
            self.calls += 1;
        }
        unsafe {
            (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, 0);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
        }
    }
}

fn upload(gl: &Gl, vbo: u32, at: u32, data: &[CV]) {
    unsafe {
        (gl.BindBuffer)(ARRAY_BUFFER, vbo);
        (gl.BufferSubData)(ARRAY_BUFFER, at as isize * 4 * CSTRIDE as isize, (data.len() * CSTRIDE as usize) as isize, data.as_ptr() as *const c_void);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
    }
}

/// Checks for `penryn selftest`: positions survive the move into their region.
pub fn selftest() -> Vec<(&'static str, alloc::string::String, &'static str)> {
    let mut out = Vec::new();
    // Section (9, 5, -1) is in region (1, 1, -1); its far corner sits at x 16+16-64 = -32,
    // y 16+16-32 = 0, z 7*16+16-64 = 64 blocks from the region's centre.
    let p = to_region((9, 5, -1), [16 * 1024, 16 * 1024, 16 * 1024]);
    let q = to_region((0, 0, 0), [-1024, 0, 513]);
    out.push(("terrain region position", alloc::format!("{:?} {:?} {:?}", region_of((9, 5, -1)), p, q), "(1, 1, -1) [-8192, 0, 16384] [-16640, -8192, -16256]"));
    out
}
