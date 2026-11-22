//! Cave culling, as vanilla: each section knows which of its six faces can see each other
//! through blocks that are not full opaque cubes (VisGraph / VisibilitySet), and a breadth-first
//! walk from the camera's section through those openings (SectionOcclusionGraph) finds the
//! sections that can possibly be seen. Sections it never reaches (caves under the ground, rooms
//! behind walls) are not drawn. The walk only moves away from the camera (never back along a
//! direction already taken), as vanilla's does, and it is redone only when the camera enters
//! another section or a section's openings change.
//!
//! Faces are numbered as the block models' cull faces: 0 down, 1 up, 2 north (-z),
//! 3 south (+z), 4 west (-x), 5 east (+x).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Every face sees every other: an empty section, or one not meshed yet.
pub const ALL: u64 = (1 << 36) - 1;

const STEP: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];

fn opposite(d: usize) -> usize {
    d ^ 1
}

#[inline]
pub fn sees(vis: u64, a: usize, b: usize) -> bool {
    vis >> (a * 6 + b) & 1 != 0
}

/// VisGraph.resolve: which faces of a 16x16x16 section see each other. `solid(i)` tells the
/// full opaque cubes, at i = (y * 16 + z) * 16 + x. `stack` is scratch space, reused.
pub fn section_vis(solid: &dyn Fn(usize) -> bool, stack: &mut Vec<u16>) -> u64 {
    let mut blocked = [0u64; 64];
    let mut count = 0;
    for i in 0..4096 {
        if solid(i) {
            blocked[i >> 6] |= 1 << (i & 63);
            count += 1;
        }
    }
    // Vanilla: with fewer than 256 opaque blocks, everything sees everything.
    if count < 256 {
        return ALL;
    }
    if count == 4096 {
        return 0;
    }
    let mut vis = 0u64;
    // Flood each open region that touches the section's sides, from its edge cells.
    for i in 0..4096usize {
        let (x, y, z) = (i & 15, i >> 8, (i >> 4) & 15);
        if !(x == 0 || x == 15 || y == 0 || y == 15 || z == 0 || z == 15) {
            continue;
        }
        if blocked[i >> 6] >> (i & 63) & 1 != 0 {
            continue;
        }
        let mut faces = 0u8;
        stack.clear();
        stack.push(i as u16);
        blocked[i >> 6] |= 1 << (i & 63);
        while let Some(c) = stack.pop() {
            let c = c as usize;
            let (x, y, z) = (c & 15, c >> 8, (c >> 4) & 15);
            if y == 0 {
                faces |= 1;
            }
            if y == 15 {
                faces |= 2;
            }
            if z == 0 {
                faces |= 4;
            }
            if z == 15 {
                faces |= 8;
            }
            if x == 0 {
                faces |= 16;
            }
            if x == 15 {
                faces |= 32;
            }
            let mut go = |n: usize| {
                if blocked[n >> 6] >> (n & 63) & 1 == 0 {
                    blocked[n >> 6] |= 1 << (n & 63);
                    stack.push(n as u16);
                }
            };
            if x > 0 {
                go(c - 1);
            }
            if x < 15 {
                go(c + 1);
            }
            if z > 0 {
                go(c - 16);
            }
            if z < 15 {
                go(c + 16);
            }
            if y > 0 {
                go(c - 256);
            }
            if y < 15 {
                go(c + 256);
            }
        }
        for a in 0..6 {
            if faces >> a & 1 != 0 {
                for b in 0..6 {
                    if faces >> b & 1 != 0 {
                        vis |= 1 << (a * 6 + b);
                    }
                }
            }
        }
    }
    vis
}

/// The sections the camera may see, kept between frames.
pub struct Graph {
    /// Which faces see each other, per section (column x, section index, column z); missing
    /// sections see through.
    pub vis: BTreeMap<(i32, i32, i32), u64>,
    /// Bumped whenever `vis` changes.
    changed: bool,
    /// The walk's result: one bit per section of the box around `centre`.
    seen: Vec<u64>,
    centre: (i32, i32, i32),
    radius: i32,
    layers: i32,
    /// False when the camera is above or below the world: then everything counts as seen.
    active: bool,
    queue: Vec<u32>,
    entered: Vec<u8>,
    went: Vec<u8>,
    /// How many sections the last walk reached (F3 and PENRYN_PERF).
    pub reached: u32,
}

impl Graph {
    pub fn new() -> Graph {
        Graph {
            vis: BTreeMap::new(),
            changed: true,
            seen: Vec::new(),
            centre: (0, i32::MIN, 0),
            radius: -1,
            layers: 0,
            active: false,
            queue: Vec::new(),
            entered: Vec::new(),
            went: Vec::new(),
            reached: 0,
        }
    }

    pub fn clear(&mut self) {
        self.vis.clear();
        self.changed = true;
    }

    pub fn set(&mut self, k: (i32, i32, i32), v: u64) {
        let old = if v == ALL { self.vis.remove(&k) } else { self.vis.insert(k, v) };
        if old.unwrap_or(ALL) != v {
            self.changed = true;
        }
    }

    fn get(&self, k: (i32, i32, i32)) -> u64 {
        self.vis.get(&k).copied().unwrap_or(ALL)
    }

    /// Walks again if the camera changed section or a section's openings changed.
    pub fn update(&mut self, cam: (i32, i32, i32), radius: i32, layers: i32) {
        if !self.changed && cam == self.centre && radius == self.radius && layers == self.layers {
            return;
        }
        self.changed = false;
        self.centre = cam;
        self.radius = radius;
        self.layers = layers;
        self.active = cam.1 >= 0 && cam.1 < layers;
        if !self.active {
            return;
        }
        let side = (2 * radius + 1) as usize;
        let n = side * side * layers as usize;
        self.seen.clear();
        self.seen.resize((n + 63) / 64, 0);
        self.entered.clear();
        self.entered.resize(n, 0);
        self.went.clear();
        self.went.resize(n, 0);
        self.queue.clear();
        let idx = |x: i32, y: i32, z: i32| -> usize { ((y * side as i32 + (z - cam.2 + radius)) * side as i32 + (x - cam.0 + radius)) as usize };
        let start = idx(cam.0, cam.1, cam.2);
        self.seen[start >> 6] |= 1 << (start & 63);
        self.queue.push(start as u32);
        let mut head = 0;
        while head < self.queue.len() {
            let i = self.queue[head] as usize;
            head += 1;
            let x = (i % side) as i32 + cam.0 - radius;
            let z = ((i / side) % side) as i32 + cam.2 - radius;
            let y = (i / (side * side)) as i32;
            let first = i == start;
            let vis = if first { ALL } else { self.get((x, y, z)) };
            let (entered, went) = (self.entered[i], self.went[i]);
            for d in 0..6 {
                if went >> opposite(d) & 1 != 0 {
                    continue;
                }
                if !first {
                    let mut ok = false;
                    for e in 0..6 {
                        if entered >> e & 1 != 0 && sees(vis, e, d) {
                            ok = true;
                            break;
                        }
                    }
                    if !ok {
                        continue;
                    }
                }
                let s = STEP[d];
                let (nx, ny, nz) = (x + s[0], y + s[1], z + s[2]);
                if ny < 0 || ny >= layers || (nx - cam.0).abs() > radius || (nz - cam.2).abs() > radius {
                    continue;
                }
                let j = idx(nx, ny, nz);
                if self.seen[j >> 6] >> (j & 63) & 1 != 0 {
                    // Reached again: one more face it can be entered by (vanilla's
                    // addSourceDirection), which counts if it is still waiting in the queue.
                    self.entered[j] |= 1 << opposite(d);
                    continue;
                }
                self.seen[j >> 6] |= 1 << (j & 63);
                self.entered[j] = 1 << opposite(d);
                self.went[j] = went | 1 << d;
                self.queue.push(j as u32);
            }
        }
        self.reached = self.queue.len() as u32;
    }

    /// Whether the last walk reached section `k`.
    #[inline]
    pub fn visible(&self, k: (i32, i32, i32)) -> bool {
        if !self.active {
            return true;
        }
        let (dx, dz) = (k.0 - self.centre.0, k.2 - self.centre.2);
        if dx.abs() > self.radius || dz.abs() > self.radius || k.1 < 0 || k.1 >= self.layers {
            return false;
        }
        let side = 2 * self.radius + 1;
        let i = ((k.1 * side + dz + self.radius) * side + dx + self.radius) as usize;
        self.seen[i >> 6] >> (i & 63) & 1 != 0
    }
}

/// Checks for `penryn selftest`.
pub fn selftest() -> Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    let mut out = Vec::new();
    let mut check = |a: &'static str, b: alloc::string::String, c: &'static str| out.push((a, b, c));
    let mut stack = Vec::new();
    // Solid but for a tunnel along x through the middle.
    let tunnel = |i: usize| !(((i >> 8) == 8) && (((i >> 4) & 15) == 8));
    let v = section_vis(&tunnel, &mut stack);
    check("occlusion tunnel", format!("{} {} {}", sees(v, 4, 5), sees(v, 0, 1), sees(v, 4, 0)), "true false false");
    check("occlusion empty", format!("{}", section_vis(&|_| false, &mut stack) == ALL), "true");
    check("occlusion full", format!("{}", section_vis(&|_| true, &mut stack)), "0");
    // An L bend: open from the top face down to y 8, then out the east face.
    let bend = |i: usize| {
        let (x, y, z) = (i & 15, i >> 8, (i >> 4) & 15);
        !((x == 8 && z == 8 && y >= 8) || (y == 8 && z == 8 && x >= 8))
    };
    let v = section_vis(&bend, &mut stack);
    check("occlusion bend", format!("{} {} {}", sees(v, 1, 5), sees(v, 5, 1), sees(v, 1, 0)), "true true false");
    // A walk: the camera in section (0, 5, 0) above solid ground (layers 0 to 3 solid).
    let mut g = Graph::new();
    for x in -3..=3 {
        for z in -3..=3 {
            for y in 0..4 {
                g.set((x, y, z), 0);
            }
        }
    }
    g.update((0, 5, 0), 3, 8);
    check("occlusion walk", format!("{} {} {} {}", g.visible((0, 3, 0)), g.visible((2, 3, -1)), g.visible((0, 2, 0)), g.visible((3, 7, 3))), "true true false true");
    // A shaft opened down through the ground under the camera: the cave below shows.
    for y in 0..4 {
        g.set((0, y, 0), ALL);
    }
    g.update((0, 5, 0), 3, 8);
    check("occlusion shaft", format!("{} {} {}", g.visible((0, 0, 0)), g.visible((1, 1, 0)), g.visible((2, 2, 2))), "true true false");
    out
}
