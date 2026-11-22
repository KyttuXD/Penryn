//! Where the meshed sections live on the card, and drawing them with few calls.
//!
//! Old drivers (Apple's above all) spend several microseconds of CPU on every draw call, so a
//! call per section (hundreds a frame) costs more than the drawing itself. Sections are
//! therefore grouped into regions of 8 x 4 x 8 sections (128 x 64 x 128 blocks) whose
//! positions are stored relative to the region's centre in 1/256 block (16-bit), so one matrix
//! serves the whole region, and packed into pages: vertex buffers of 16384 quads (65536
//! vertices, so 16-bit indices). A frame draws every visible section of a page with one
//! glMultiDrawElements.
//!
//! Each section has a slot in a page with a quarter more room than it needs; a rebuilt section
//! is written in place with glBufferSubData when it fits, else in the first free space of the
//! region's pages (or a new page). Nothing is ever moved, read back or kept in memory, so
//! loading chunks never waits for the card. A section bigger than a page gets a page of its own
//! (with 32-bit indices).
//!
//! A section's quads come in runs (mesher.rs): seven by facing (down, up, north, south,
//! west, east, any) for the solid and cut-out quads, then leaves forced opaque, then the
//! translucent quads.

use crate::render::CV;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ffi::c_void;

pub const RUNS: usize = 9;
/// Leaves drawn opaque (vanilla's Cutout Leaves option off): without the alpha test.
pub const OPAQUE: usize = 7;
pub const TRANSLUCENT: usize = 8;
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

/// Quads per page.
const PAGE: u32 = 16384;

struct Page {
    vbo: u32,
    cap: u32,
    /// Free space: (start, length) in quads, sorted, never touching each other.
    free: Vec<(u32, u32)>,
}

impl Page {
    fn take(&mut self, n: u32) -> Option<u32> {
        let i = self.free.iter().position(|f| f.1 >= n)?;
        let at = self.free[i].0;
        if self.free[i].1 == n {
            self.free.remove(i);
        } else {
            self.free[i] = (at + n, self.free[i].1 - n);
        }
        Some(at)
    }

    fn give(&mut self, at: u32, n: u32) {
        let i = self.free.iter().position(|f| f.0 > at).unwrap_or(self.free.len());
        self.free.insert(i, (at, n));
        // Merge with the next, then the previous.
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
    }

    fn empty(&self) -> bool {
        self.free.len() == 1 && self.free[0] == (0, self.cap)
    }
}

struct Slot {
    page: u16,
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
    page: u16,
    first: [u32; RUNS + 1],
    runs: u16,
}

pub struct Terrain {
    /// Each region's pages (an emptied page is kept for reuse until the region goes).
    regions: BTreeMap<RKey, Vec<Page>>,
    slots: BTreeMap<SKey, Slot>,
    /// The sections in each region, so a region out of view is skipped whole.
    members: BTreeMap<RKey, Vec<SKey>>,
    /// The shared quad index buffers (0 1 2, 2 3 0 per quad, as vanilla's): 16-bit for a page,
    /// 32-bit for the pages of oversized sections.
    index16: u32,
    index32: u32,
    index32_quads: u32,
    items: Vec<Item>,
    grouped: Vec<Item>,
    seen: Vec<(RKey, u16)>,
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
            members: BTreeMap::new(),
            index16: 0,
            index32: 0,
            index32_quads: 0,
            items: Vec::new(),
            grouped: Vec::new(),
            seen: Vec::new(),
            counts: Vec::new(),
            offsets: Vec::new(),
            sections_drawn: 0,
            quads_drawn: 0,
            calls: 0,
        }
    }

    pub fn clear(&mut self, gl: &Gl) {
        for (_, pages) in core::mem::take(&mut self.regions) {
            for p in pages {
                unsafe { (gl.DeleteBuffers)(1, &p.vbo) };
            }
        }
        self.slots.clear();
        self.members.clear();
    }

    pub fn remove(&mut self, gl: &Gl, k: SKey) {
        let Some(s) = self.slots.remove(&k) else { return };
        let rk = region_of(k);
        self.unlist(k);
        let Some(pages) = self.regions.get_mut(&rk) else { return };
        if let Some(p) = pages.get_mut(s.page as usize) {
            p.give(s.at, s.cap);
        }
        // A region with nothing left gives its buffers back.
        if pages.iter().all(|p| p.empty()) {
            for p in pages.iter() {
                unsafe { (gl.DeleteBuffers)(1, &p.vbo) };
            }
            self.regions.remove(&rk);
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
        if let Some(s) = self.slots.get_mut(&k) {
            if quads <= s.cap {
                if let Some(p) = self.regions.get(&rk).and_then(|pages| pages.get(s.page as usize)) {
                    upload(gl, p.vbo, s.at, &data);
                    s.first = first;
                    return;
                }
            }
        }
        if let Some(s) = self.slots.remove(&k) {
            if let Some(p) = self.regions.get_mut(&rk).and_then(|pages| pages.get_mut(s.page as usize)) {
                p.give(s.at, s.cap);
            }
        }
        let need = (quads + quads / 4 + 16).min(PAGE.max(quads));
        let pages = self.regions.entry(rk).or_insert_with(Vec::new);
        let mut spot = None;
        for (i, p) in pages.iter_mut().enumerate() {
            if let Some(at) = p.take(need) {
                spot = Some((i, at));
                break;
            }
        }
        let (pi, at) = match spot {
            Some(s) => s,
            None => {
                let cap = PAGE.max(need);
                let mut vbo = 0u32;
                unsafe {
                    (gl.GenBuffers)(1, &mut vbo);
                    (gl.BindBuffer)(ARRAY_BUFFER, vbo);
                    (gl.BufferData)(ARRAY_BUFFER, cap as isize * 4 * CSTRIDE as isize, core::ptr::null(), STATIC_DRAW);
                    (gl.BindBuffer)(ARRAY_BUFFER, 0);
                }
                let mut p = Page { vbo, cap, free: alloc::vec![(0, cap)] };
                let at = p.take(need).unwrap_or(0);
                pages.push(p);
                (pages.len() - 1, at)
            }
        };
        upload(gl, pages[pi].vbo, at, &data);
        self.slots.insert(k, Slot { page: pi as u16, at, cap: need, first });
        let m = self.members.entry(rk).or_insert_with(Vec::new);
        if !m.contains(&k) {
            m.push(k);
        }
        self.ensure_index(gl, need);
    }

    fn unlist(&mut self, k: SKey) {
        let rk = region_of(k);
        if let Some(m) = self.members.get_mut(&rk) {
            m.retain(|x| *x != k);
            if m.is_empty() {
                self.members.remove(&rk);
            }
        }
    }

    fn ensure_index(&mut self, gl: &Gl, quads: u32) {
        if self.index16 == 0 {
            let mut ix: Vec<u16> = Vec::with_capacity(PAGE as usize * 6);
            for q in 0..PAGE {
                let v = (q * 4) as u16;
                ix.extend_from_slice(&[v, v + 1, v + 2, v + 2, v + 3, v]);
            }
            unsafe {
                (gl.GenBuffers)(1, &mut self.index16);
                (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, self.index16);
                (gl.BufferData)(ELEMENT_ARRAY_BUFFER, (ix.len() * 2) as isize, ix.as_ptr() as *const c_void, STATIC_DRAW);
                (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, 0);
            }
        }
        if quads <= PAGE || quads <= self.index32_quads {
            return;
        }
        let n = quads.next_power_of_two();
        let mut ix: Vec<u32> = Vec::with_capacity(n as usize * 6);
        for q in 0..n {
            let v = q * 4;
            ix.extend_from_slice(&[v, v + 1, v + 2, v + 2, v + 3, v]);
        }
        unsafe {
            if self.index32 == 0 {
                (gl.GenBuffers)(1, &mut self.index32);
            }
            (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, self.index32);
            (gl.BufferData)(ELEMENT_ARRAY_BUFFER, (ix.len() * 4) as isize, ix.as_ptr() as *const c_void, STATIC_DRAW);
            (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, 0);
        }
        self.index32_quads = n;
    }

    /// Picks the sections to draw this frame: `keep(k, lo)` says whether section k (corner
    /// `lo` relative to the eye) is in view; `faces(lo)` which facing runs can face the eye.
    pub fn select(&mut self, eye: [f64; 3], min_y: i32, keep_box: &dyn Fn([f64; 3], [f64; 3]) -> bool, keep: &mut dyn FnMut((i32, i32, i32), [f64; 3]) -> bool, faces: &dyn Fn([f64; 3]) -> u8) {
        self.items.clear();
        for (&rk, members) in self.members.iter() {
            let lo = [(rk.0 * RX * 16) as f64 - eye[0], (min_y + rk.1 * RY * 16) as f64 - eye[1], (rk.2 * RX * 16) as f64 - eye[2]];
            let hi = [lo[0] + (RX * 16) as f64, lo[1] + (RY * 16) as f64, lo[2] + (RX * 16) as f64];
            if !keep_box(lo, hi) {
                continue;
            }
            for &k in members.iter() {
                let Some(s) = self.slots.get(&k) else { continue };
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
                self.items.push(Item { rank: 0, d2, region: rk, page: s.page, first, runs: faces(lo) as u16 | 1 << ANY });
            }
        }
        // Near to far (the translucent pass goes through this backwards); then, for the opaque
        // pass, each page's sections together, pages ranked by their nearest section, so a page
        // is one call and what is in front is drawn first.
        self.items.sort_unstable_by(|a, b| a.d2.partial_cmp(&b.d2).unwrap_or(core::cmp::Ordering::Equal));
        self.seen.clear();
        self.counts.clear();
        for it in self.items.iter_mut() {
            let rank = match self.seen.iter().position(|s| s.0 == it.region && s.1 == it.page) {
                Some(i) => i,
                None => {
                    self.seen.push((it.region, it.page));
                    self.counts.push(0);
                    self.seen.len() - 1
                }
            };
            it.rank = rank as u32;
            self.counts[rank] += 1;
        }
        // A counting sort by rank keeps the near-to-far order within each page.
        let mut at = 0;
        for c in self.counts.iter_mut() {
            let n = *c;
            *c = at;
            at += n;
        }
        self.grouped.clear();
        self.grouped.extend_from_slice(&self.items);
        for it in self.items.iter() {
            let r = it.rank as usize;
            self.grouped[self.counts[r] as usize] = *it;
            self.counts[r] += 1;
        }
        self.sections_drawn = self.items.len() as u32;
        self.quads_drawn = 0;
        self.calls = 0;
    }

    /// Draws the solid and cut-out runs of the selected sections (the caller has set up the
    /// textures, fog and the alpha test; `view` is the camera's rotation), then the leaves
    /// forced opaque without the alpha test.
    pub fn draw_opaque(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32) {
        let items = core::mem::take(&mut self.grouped);
        self.draw_list(gl, view, eye, min_y, &items, None);
        if items.iter().any(|it| it.first[OPAQUE] != it.first[OPAQUE + 1]) {
            unsafe { (gl.Disable)(ALPHA_TEST) };
            self.draw_list(gl, view, eye, min_y, &items, Some(1 << OPAQUE));
            unsafe { (gl.Enable)(ALPHA_TEST) };
        }
        self.grouped = items;
    }

    /// Draws the translucent runs, far to near (strictly, for the blending).
    pub fn draw_translucent(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32) {
        let mut items = core::mem::take(&mut self.items);
        items.retain(|it| it.first[TRANSLUCENT] != it.first[RUNS]);
        items.reverse();
        self.draw_list(gl, view, eye, min_y, &items, Some(1 << TRANSLUCENT));
        self.items = items;
    }

    /// Draws `items` (grouped by page), each with its facing runs or the runs of `mask`.
    fn draw_list(&mut self, gl: &Gl, view: &[f64; 16], eye: [f64; 3], min_y: i32, items: &[Item], mask: Option<u16>) {
        if items.is_empty() {
            return;
        }
        let mut bound = 0u32;
        let mut i = 0;
        while i < items.len() {
            let (rk, pg) = (items[i].region, items[i].page);
            let Some(page) = self.regions.get(&rk).and_then(|pages| pages.get(pg as usize)) else {
                i += 1;
                continue;
            };
            let (vbo, big) = (page.vbo, page.cap > PAGE);
            let isz = if big { 4 } else { 2 };
            self.counts.clear();
            self.offsets.clear();
            let mut open: Option<(u32, u32)> = None;
            while i < items.len() && items[i].region == rk && items[i].page == pg {
                let it = &items[i];
                i += 1;
                let mask = mask.unwrap_or(it.runs);
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
                            self.offsets.push((os as usize * 6 * isz) as *const c_void);
                            Some((s, e - s))
                        }
                        None => Some((s, e - s)),
                    };
                }
            }
            if let Some((os, oc)) = open {
                self.counts.push((oc * 6) as i32);
                self.offsets.push((os as usize * 6 * isz) as *const c_void);
            }
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
            let (index, ty) = if big { (self.index32, UNSIGNED_INT) } else { (self.index16, UNSIGNED_SHORT) };
            unsafe {
                if bound != index {
                    (gl.BindBuffer)(ELEMENT_ARRAY_BUFFER, index);
                    bound = index;
                }
                (gl.LoadMatrixf)(m.as_ptr());
                (gl.BindBuffer)(ARRAY_BUFFER, vbo);
                (gl.VertexPointer)(3, SHORT, CSTRIDE, core::ptr::null());
                (gl.TexCoordPointer)(2, SHORT, CSTRIDE, 8 as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, CSTRIDE, 12 as *const c_void);
                if self.counts.len() == 1 {
                    (gl.DrawElements)(TRIANGLES, self.counts[0], ty, self.offsets[0]);
                } else {
                    (gl.MultiDrawElements)(TRIANGLES, self.counts.as_ptr(), ty, self.offsets.as_ptr(), self.counts.len() as i32);
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
