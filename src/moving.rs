//! Blocks in motion, from ClientboundBlockEventPacket: pistons pushing and pulling and shulker
//! box lids. As vanilla's client does on the block event, the piston runs here too
//! (PistonBaseBlock.triggerEvent, moveBlocks, PistonStructureResolver): the blocks it moves
//! become moving_piston blocks with a PistonMovingBlockEntity each, until the server's own
//! blocks arrive. Their collision while they move, the player pushed along
//! (moveCollidedEntities, a slime block launching), the moving blocks drawn, and the shulker
//! box lid (ShulkerBoxBlockEntity) growing its collision as it opens.

use crate::blocks::{self, Blocks};
use crate::physics::{Aabb, Mover};
use crate::sdl::*;
use crate::wire::In;
use crate::world::World;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ffi::c_void;

/// blocks.rs facing codes (0 north, 1 south, 2 west, 3 east, 4 down, 5 up): step and axis.
const STEP: [[i32; 3]; 6] = [[0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0]];
const AXIS: [usize; 6] = [2, 2, 0, 0, 1, 1];
const NONE: u8 = 255;

fn sign(d: u8) -> f64 {
    if d & 1 == 1 {
        1.0
    } else {
        -1.0
    }
}

fn add(p: [i32; 3], d: u8, k: i32) -> [i32; 3] {
    let s = STEP[d as usize];
    [p[0] + s[0] * k, p[1] + s[1] * k, p[2] + s[2] * k]
}

/// The states of one block: [first, end).
#[derive(Clone, Copy, Default)]
struct Range {
    first: u16,
    end: u16,
    block: u16,
}

impl Range {
    fn of(b: &Blocks, name: &str) -> Range {
        let Some(block) = b.blocks.iter().position(|k| k.name == name) else {
            return Range { first: 0, end: 0, block: u16::MAX };
        };
        let (mut first, mut end) = (u16::MAX, 0u16);
        for (i, s) in b.states.iter().enumerate() {
            if s.block as usize == block {
                first = first.min(i as u16);
                end = end.max(i as u16 + 1);
            }
        }
        if first == u16::MAX {
            first = 0;
        }
        Range { first, end, block: block as u16 }
    }
    fn has(&self, s: u16) -> bool {
        s >= self.first && s < self.end
    }
    /// The state at `stride` slot `slot` (vanilla's property order) facing `f`.
    fn find(&self, b: &Blocks, f: u8, stride: u16, slot: u16, from: u16, to: u16) -> u16 {
        (self.first + from..(self.first + to).min(self.end))
            .find(|&s| (s - self.first) % stride == slot && b.state(s).facing == f)
            .unwrap_or(0)
    }
}

struct Ids {
    moving: Range,
    piston: Range,
    sticky: Range,
    head: Range,
    slime: u16,
    honey: u16,
    /// Obsidian, crying obsidian, respawn anchors, reinforced deepslate: never pushed.
    fixed: [u16; 4],
    shulkers: Vec<u16>,
    /// Per state: the push reaction (effects.bin), + 128 with a block entity.
    push: Vec<u8>,
}

impl Ids {
    fn new(b: &Blocks) -> Ids {
        let shulkers = b.blocks.iter().enumerate().filter(|(_, k)| k.name.ends_with("shulker_box")).map(|(i, _)| i as u16).collect();
        Ids {
            moving: Range::of(b, "moving_piston"),
            piston: Range::of(b, "piston"),
            sticky: Range::of(b, "sticky_piston"),
            head: Range::of(b, "piston_head"),
            slime: Range::of(b, "slime_block").block,
            honey: Range::of(b, "honey_block").block,
            fixed: [
                Range::of(b, "obsidian").block,
                Range::of(b, "crying_obsidian").block,
                Range::of(b, "respawn_anchor").block,
                Range::of(b, "reinforced_deepslate").block,
            ],
            shulkers,
            push: load_push(b.states.len()),
        }
    }
    /// moving_piston: facing, type (normal first).
    fn moving_state(&self, b: &Blocks, f: u8, sticky: bool) -> u16 {
        self.moving.find(b, f, 2, sticky as u16, 0, 12)
    }
    /// BlockState.getPistonPushReaction: 0 push and pull, 1 destroyed, 2 blocks, 4 push only.
    /// Without effects.bin: plants and replaceable blocks break, the rest move.
    fn reaction(&self, b: &Blocks, s: u16) -> u8 {
        match self.push.get(s as usize) {
            Some(r) => r & 7,
            None if b.kind(s) == blocks::PLANT || b.state(s).flags & blocks::REPLACEABLE != 0 => 1,
            None => 0,
        }
    }
    fn block_entity(&self, s: u16) -> bool {
        self.push.get(s as usize).map_or(false, |r| r & 128 != 0)
    }
    fn sticky_block(&self, b: &Blocks, s: u16) -> bool {
        let k = b.state(s).block;
        s != 0 && (k == self.slime || k == self.honey)
    }
    /// piston_head: facing, short (true first), type (normal first).
    fn head_state(&self, b: &Blocks, f: u8, short: bool, sticky: bool) -> u16 {
        self.head.find(b, f, 4, if short { 0 } else { 2 } + sticky as u16, 0, 24)
    }
    /// piston / sticky_piston: extended (true first), facing.
    fn piston_state(&self, b: &Blocks, sticky: bool, extended: bool, f: u8) -> u16 {
        let r = if sticky { self.sticky } else { self.piston };
        if extended {
            r.find(b, f, 1, 0, 0, 6)
        } else {
            r.find(b, f, 1, 0, 6, 12)
        }
    }
    fn is_piston(&self, s: u16) -> bool {
        self.piston.has(s) || self.sticky.has(s)
    }
}

/// PistonMovingBlockEntity.
#[derive(Clone, Copy)]
pub struct Piston {
    pub pos: [i32; 3],
    pub moved: u16,
    /// The piston's facing (the entity's direction).
    pub facing: u8,
    pub extending: bool,
    pub source: bool,
    pub progress: f32,
    pub progress_o: f32,
    death: u8,
}

impl Piston {
    /// getMovementDirection.
    fn movement(&self) -> u8 {
        if self.extending {
            self.facing
        } else {
            self.facing ^ 1
        }
    }
    /// getExtendedProgress.
    fn extended(&self, p: f32) -> f32 {
        if self.extending {
            p - 1.0
        } else {
            1.0 - p
        }
    }
    fn offset(&self, p: f32) -> [f64; 3] {
        let e = self.extended(p);
        let s = STEP[self.facing as usize];
        [(s[0] as f32 * e) as f64, (s[1] as f32 * e) as f64, (s[2] as f32 * e) as f64]
    }
}

/// ShulkerBoxBlockEntity's lid: 0 closed, 1 opening, 2 opened, 3 closing.
#[derive(Clone, Copy)]
pub struct Shulker {
    pub pos: [i32; 3],
    pub facing: u8,
    status: u8,
    pub progress: f32,
    pub progress_o: f32,
}

pub struct Moving {
    ids: Option<Ids>,
    pub pistons: Vec<Piston>,
    pub shulkers: Vec<Shulker>,
    ticks: u64,
    /// PistonMovingBlockEntity.NOCLIP: the direction of the push under way.
    noclip: Cell<u8>,
    /// Entity.pistonDeltas this tick.
    deltas: [f64; 3],
    deltas_tick: u64,
}

impl Moving {
    pub fn new() -> Moving {
        Moving { ids: None, pistons: Vec::new(), shulkers: Vec::new(), ticks: 0, noclip: Cell::new(NONE), deltas: [0.0; 3], deltas_tick: 0 }
    }

    fn ensure(&mut self, b: &Blocks) {
        if self.ids.is_none() {
            self.ids = Some(Ids::new(b));
        }
    }

    /// Whether a piston is half way (for tests).
    pub fn in_motion(&self) -> bool {
        self.pistons.iter().any(|p| p.progress > 0.0 && p.progress < 1.0)
    }

    pub fn active(&self) -> bool {
        !self.pistons.is_empty() || !self.shulkers.is_empty()
    }

    /// How far a shulker box's lid is open (0..1) at `partial`: for whoever draws its model
    /// (ShulkerBoxRenderer lifts the lid by 0.5 x this and turns it 270 degrees).
    #[allow(dead_code)]
    pub fn shulker_progress(&self, pos: [i32; 3], partial: f32) -> f32 {
        self.shulkers.iter().find(|s| s.pos == pos).map_or(0.0, |s| s.progress_o + (s.progress - s.progress_o) * partial)
    }

    /// The collision of a moving block (PistonMovingBlockEntity.getCollisionShape) or an open
    /// shulker box (ShulkerBoxBlockEntity.getBoundingBox) at x y z; false when neither is there.
    pub fn boxes_at(&self, b: &Blocks, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) -> bool {
        let pos = [x, y, z];
        let at = |out: &mut Vec<Aabb>, lo: [f64; 3], hi: [f64; 3], o: [f64; 3]| {
            out.push(Aabb {
                lo: [x as f64 + lo[0] + o[0], y as f64 + lo[1] + o[1], z as f64 + lo[2] + o[2]],
                hi: [x as f64 + hi[0] + o[0], y as f64 + hi[1] + o[1], z as f64 + hi[2] + o[2]],
            });
        };
        if let (Some(p), Some(ids)) = (self.pistons.iter().find(|p| p.pos == pos), self.ids.as_ref()) {
            // A retracting piston's own block: its base already stands.
            if !p.extending && p.source && ids.is_piston(p.moved) {
                let base = ids.piston_state(b, ids.sticky.has(p.moved), true, p.facing);
                each_box(b, base, |lo, hi| at(out, lo, hi, [0.0; 3]));
            }
            // What the piston itself pushes does not stop the push.
            if p.progress < 1.0 && self.noclip.get() == p.movement() {
                return true;
            }
            let st = if p.source { ids.head_state(b, p.facing, p.extending != (1.0 - p.progress < 0.25), false) } else { p.moved };
            let o = p.offset(p.progress);
            each_box(b, st, |lo, hi| at(out, lo, hi, o));
            return true;
        }
        if let Some(s) = self.shulkers.iter().find(|s| s.pos == pos && s.progress > 0.0) {
            // Shulker.getProgressAabb(1, facing, 0.5 x progress): the cube grown towards facing.
            let (mut lo, mut hi) = ([0.0; 3], [1.0; 3]);
            let grow = (0.5f32 * s.progress) as f64;
            let a = AXIS[s.facing as usize];
            if sign(s.facing) > 0.0 {
                hi[a] += grow;
            } else {
                lo[a] -= grow;
            }
            at(out, lo, hi, [0.0; 3]);
            return true;
        }
        false
    }
}

/// A state's collision boxes in the block (getCollisionShape for no one in particular).
fn each_box(b: &Blocks, s: u16, mut f: impl FnMut([f64; 3], [f64; 3])) {
    if s == 0 || !b.collides(s) {
        return;
    }
    match b.state(s).coll_rule {
        0 => {
            for c in b.collision(s) {
                let v = |i: usize| c[i] as f64 / 32.0;
                f([v(0), v(1), v(2)], [v(3), v(4), v(5)]);
            }
        }
        blocks::C_POWDER_SNOW => {}
        _ => {
            for c in b.shape(s) {
                let v = |i: usize| c[i] as f64 / 16.0;
                f([v(0), v(1), v(2)], [v(3), v(4), v(5)]);
            }
        }
    }
}

/// ClientboundBlockEventPacket: position, b0, b1, block.
pub fn on_block_event(world: &mut World, b: &Blocks, r: &mut In) {
    let (Some(v), Some(b0), Some(b1), Some(block)) = (r.i64(), r.u8(), r.u8(), r.varint()) else { return };
    let pos = [(v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32];
    block_event(world, b, pos, b0, b1, block);
}

/// PistonBaseBlock.triggerEvent (0 extend, 1 and 2 retract; b1 the facing) and
/// ShulkerBoxBlockEntity.triggerEvent (1, how many have it open).
pub fn block_event(world: &mut World, b: &Blocks, pos: [i32; 3], b0: u8, b1: u8, _block: i32) {
    // Level.blockEvent: the event goes to the block the client has there (the packet's block
    // only names it), and a piston's direction is its own state's facing.
    let here = world.get(pos[0], pos[1], pos[2]);
    world.moving.ensure(b);
    let ids = world.moving.ids.as_ref().unwrap();
    if ids.is_piston(here) {
        let sticky = ids.sticky.has(here);
        let f = b.state(here).facing;
        if crate::sys::env("PENRYN_DEBUG").is_some() {
            crate::say!("piston event at {:?}: {} {}", pos, b0, b1);
        }
        if f < 6 && b0 <= 2 {
            piston_event(world, b, pos, f, sticky, b0);
        }
    } else if b0 == 1 && ids.shulkers.contains(&b.state(here).block) {
        let facing = match b.state(here).facing {
            f if f < 6 => f,
            _ => 5,
        };
        let m = &mut world.moving;
        let i = match m.shulkers.iter().position(|s| s.pos == pos) {
            Some(i) => i,
            None => {
                m.shulkers.push(Shulker { pos, facing, status: 0, progress: 0.0, progress_o: 0.0 });
                m.shulkers.len() - 1
            }
        };
        let s = &mut m.shulkers[i];
        s.facing = facing;
        match b1 {
            0 => s.status = 3,
            1 => s.status = 1,
            _ => {}
        }
    }
}

fn put(world: &mut World, b: &Blocks, p: [i32; 3], s: u16) {
    world.set(p[0], p[1], p[2], s, &|t| b.kind(t) == blocks::AIR);
}

/// A new PistonMovingBlockEntity (with its moving_piston block already placed).
fn add_piston(world: &mut World, p: Piston) {
    world.moving.pistons.retain(|q| q.pos != p.pos);
    world.moving.pistons.push(p);
}

/// PistonMovingBlockEntity.finalTick: the moving block at `p` put in place now.
fn finish(world: &mut World, b: &Blocks, p: [i32; 3]) {
    let Some(i) = world.moving.pistons.iter().position(|q| q.pos == p) else { return };
    let e = world.moving.pistons.swap_remove(i);
    let here = world.get(p[0], p[1], p[2]);
    if world.moving.ids.as_ref().map_or(false, |ids| ids.moving.has(here)) {
        put(world, b, p, if e.source { 0 } else { e.moved });
    }
}

/// PistonBaseBlock.triggerEvent on the client.
fn piston_event(world: &mut World, b: &Blocks, pos: [i32; 3], f: u8, sticky: bool, b0: u8) {
    if b0 == 0 {
        if move_blocks(world, b, pos, f, true, sticky) {
            let ids = world.moving.ids.as_ref().unwrap();
            let s = ids.piston_state(b, sticky, true, f);
            put(world, b, pos, s);
        }
        return;
    }
    // Retracting: an extension still under way ends at once; the piston itself moves back
    // into its base, a sticky one pulling the block two ahead.
    finish(world, b, add(pos, f, 1));
    let ids = world.moving.ids.as_ref().unwrap();
    let mp = ids.moving_state(b, f, sticky);
    let base = ids.piston_state(b, sticky, false, f);
    put(world, b, pos, mp);
    add_piston(world, Piston { pos, moved: base, facing: f, extending: false, source: true, progress: 0.0, progress_o: 0.0, death: 0 });
    let ids = world.moving.ids.as_ref().unwrap();
    if sticky {
        let p2 = add(pos, f, 2);
        let st = world.get(p2[0], p2[1], p2[2]);
        let finishing = ids.moving.has(st) && world.moving.pistons.iter().any(|q| q.pos == p2 && q.facing == f && q.extending);
        if finishing {
            finish(world, b, p2);
        } else if b0 == 1 && b.kind(st) != blocks::AIR && pushable(world, b, st, p2, f ^ 1, false, f) && (ids.reaction(b, st) == 0 || ids.is_piston(st)) {
            move_blocks(world, b, pos, f, false, sticky);
        } else {
            put(world, b, add(pos, f, 1), 0);
        }
    } else {
        put(world, b, add(pos, f, 1), 0);
    }
}

/// PistonBaseBlock.isPushable.
fn pushable(world: &World, b: &Blocks, s: u16, p: [i32; 3], dir: u8, destroy: bool, facing: u8) -> bool {
    let Some(ids) = world.moving.ids.as_ref() else { return false };
    let (lo, hi) = (world.min_y, world.min_y + world.height - 1);
    if p[1] < lo || p[1] > hi {
        return false;
    }
    if b.kind(s) == blocks::AIR {
        return true;
    }
    if ids.fixed.contains(&b.state(s).block) || (dir == 4 && p[1] == lo) || (dir == 5 && p[1] == hi) {
        return false;
    }
    if !ids.is_piston(s) {
        if b.state(s).hardness == 65535 {
            return false;
        }
        match ids.reaction(b, s) {
            2 => return false,
            1 => return destroy,
            4 => return dir == facing,
            _ => {}
        }
    } else if !ids.piston_state(b, ids.sticky.has(s), false, b.state(s).facing).eq(&s) {
        // An extended piston stays.
        return false;
    }
    !ids.block_entity(s)
}

/// PistonStructureResolver: what a piston at `piston` facing `facing` moves.
struct Resolver<'a> {
    world: &'a World,
    b: &'a Blocks,
    piston: [i32; 3],
    facing: u8,
    dir: u8,
    push: Vec<[i32; 3]>,
    destroy: Vec<[i32; 3]>,
}

impl<'a> Resolver<'a> {
    fn get(&self, p: [i32; 3]) -> u16 {
        self.world.get(p[0], p[1], p[2])
    }
    fn ids(&self) -> &'a Ids {
        self.world.moving.ids.as_ref().unwrap()
    }
    fn air(&self, s: u16) -> bool {
        self.b.kind(s) == blocks::AIR
    }
    fn sticky(&self, s: u16) -> bool {
        self.ids().sticky_block(self.b, s)
    }
    /// canStickToEachOther: slime and honey do not hold each other.
    fn stick(&self, a: u16, c: u16) -> bool {
        let ids = self.ids();
        let (ka, kc) = (self.b.state(a).block, self.b.state(c).block);
        if (ka == ids.honey && kc == ids.slime) || (ka == ids.slime && kc == ids.honey) {
            return false;
        }
        self.sticky(a) || self.sticky(c)
    }

    fn resolve(&mut self, extending: bool) -> bool {
        let start = add(self.piston, self.facing, if extending { 1 } else { 2 });
        let s = self.get(start);
        if !pushable(self.world, self.b, s, start, self.dir, false, self.facing) {
            if extending && self.ids().reaction(self.b, s) == 1 {
                self.destroy.push(start);
                return true;
            }
            return false;
        }
        if !self.line(start, self.dir) {
            return false;
        }
        let mut i = 0;
        while i < self.push.len() {
            let p = self.push[i];
            if self.sticky(self.get(p)) && !self.branch(p) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// addBlockLine: the block at `origin`, what sticks behind it, and what it pushes ahead.
    fn line(&mut self, origin: [i32; 3], direction: u8) -> bool {
        let mut s = self.get(origin);
        if self.air(s) || !pushable(self.world, self.b, s, origin, self.dir, false, direction) || origin == self.piston || self.push.contains(&origin) {
            return true;
        }
        let back = self.dir ^ 1;
        let mut i = 1;
        if i + self.push.len() > 12 {
            return false;
        }
        while self.sticky(s) {
            let p = add(origin, back, i as i32);
            let old = s;
            s = self.get(p);
            if self.air(s) || !self.stick(old, s) || !pushable(self.world, self.b, s, p, self.dir, false, back) || p == self.piston {
                break;
            }
            i += 1;
            if i + self.push.len() > 12 {
                return false;
            }
        }
        let mut j = 0;
        for k in (0..i).rev() {
            self.push.push(add(origin, back, k as i32));
            j += 1;
        }
        let mut k = 1;
        loop {
            let p = add(origin, self.dir, k);
            if let Some(l) = self.push.iter().position(|q| *q == p) {
                self.reorder(j, l);
                for m in 0..=(l + j).min(self.push.len() - 1) {
                    let q = self.push[m];
                    if self.sticky(self.get(q)) && !self.branch(q) {
                        return false;
                    }
                }
                return true;
            }
            let s = self.get(p);
            if self.air(s) {
                return true;
            }
            if !pushable(self.world, self.b, s, p, self.dir, true, self.dir) || p == self.piston {
                return false;
            }
            if self.ids().reaction(self.b, s) == 1 {
                self.destroy.push(p);
                return true;
            }
            if self.push.len() >= 12 {
                return false;
            }
            self.push.push(p);
            j += 1;
            k += 1;
        }
    }

    /// reorderListAtCollision.
    fn reorder(&mut self, i: usize, j: usize) {
        let n = self.push.len();
        let mut out = Vec::with_capacity(n);
        out.extend_from_slice(&self.push[..j]);
        out.extend_from_slice(&self.push[n - i..]);
        out.extend_from_slice(&self.push[j..n - i]);
        self.push = out;
    }

    /// addBranchingBlocks: sticky blocks take their neighbours across the push.
    fn branch(&mut self, p: [i32; 3]) -> bool {
        let s = self.get(p);
        // Direction.values(): down, up, north, south, west, east.
        for d in [4u8, 5, 0, 1, 2, 3] {
            if AXIS[d as usize] != AXIS[self.dir as usize] {
                let q = add(p, d, 1);
                if self.stick(self.get(q), s) && !self.line(q, d) {
                    return false;
                }
            }
        }
        true
    }
}

/// PistonBaseBlock.moveBlocks on the client: the blocks become moving_piston blocks one step
/// along, the head comes out (extending), what was left behind turns to air.
fn move_blocks(world: &mut World, b: &Blocks, pos: [i32; 3], f: u8, extending: bool, sticky: bool) -> bool {
    let head = add(pos, f, 1);
    let ids = world.moving.ids.as_ref().unwrap();
    if !extending && ids.head.has(world.get(head[0], head[1], head[2])) {
        put(world, b, head, 0);
    }
    let dir = if extending { f } else { f ^ 1 };
    let mut r = Resolver { world: &*world, b, piston: pos, facing: f, dir, push: Vec::new(), destroy: Vec::new() };
    if !r.resolve(extending) {
        return false;
    }
    let (push, destroy) = (r.push, r.destroy);
    let states: Vec<u16> = push.iter().map(|p| world.get(p[0], p[1], p[2])).collect();
    let mut vacated = push.clone();
    for p in destroy.iter().rev() {
        put(world, b, *p, 0);
    }
    let ids = world.moving.ids.as_ref().unwrap();
    let mp = ids.moving_state(b, f, false);
    let mph = ids.moving_state(b, f, sticky);
    let head_state = ids.head_state(b, f, false, sticky);
    for k in (0..push.len()).rev() {
        let p2 = add(push[k], dir, 1);
        vacated.retain(|q| *q != p2);
        put(world, b, p2, mp);
        add_piston(world, Piston { pos: p2, moved: states[k], facing: f, extending, source: false, progress: 0.0, progress_o: 0.0, death: 0 });
    }
    if extending {
        vacated.retain(|q| *q != head);
        put(world, b, head, mph);
        add_piston(world, Piston { pos: head, moved: head_state, facing: f, extending: true, source: true, progress: 0.0, progress_o: 0.0, death: 0 });
    }
    for p in vacated {
        put(world, b, p, 0);
    }
    true
}

/// A block change from the server: a moving block the client already runs stays; anything
/// else replaces it.
pub fn set_block(world: &mut World, b: &Blocks, x: i32, y: i32, z: i32, s: u16) {
    let q = [x, y, z];
    put(world, b, q, s);
    if !world.moving.active() {
        return;
    }
    world.moving.ensure(b);
    let ids = world.moving.ids.as_ref().unwrap();
    if ids.moving.has(s) {
        return;
    }
    let shulker = ids.shulkers.contains(&b.state(s).block);
    world.moving.pistons.retain(|p| p.pos != q);
    world.moving.shulkers.retain(|sh| sh.pos != q || shulker);
}

/// effects.bin's push reactions, when it has them.
fn load_push(states: usize) -> Vec<u8> {
    let Some(d) = crate::sys::read_file(&alloc::format!("{}/effects.bin", crate::data_dir())) else { return Vec::new() };
    if d.get(..4) != Some(&crate::account::EFFECTS_MAGIC[..]) || d.len() < 10 {
        return Vec::new();
    }
    let rd = |o: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?])) };
    let parse = || -> Option<Vec<u8>> {
        let (n, aw, ah) = (rd(4)? as usize, rd(6)? as usize, rd(8)? as usize);
        let mut p = 10;
        for _ in 0..n {
            p += 1 + *d.get(p)? as usize + 6;
        }
        p += aw * ah * 4;
        let count = u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?) as usize;
        let v = d.get(p + 4..p + 4 + count)?.to_vec();
        if count == states {
            Some(v)
        } else {
            None
        }
    };
    parse().unwrap_or_default()
}

/// The intersection test of AABB.intersects.
fn meets(a: &Aabb, b: &Aabb) -> bool {
    (0..3).all(|i| a.lo[i] < b.hi[i] && a.hi[i] > b.lo[i])
}

/// PistonMath.getMovementArea: the slab the leading face of `b` sweeps going `delta` along `d`.
fn movement_area(b: &Aabb, d: u8, delta: f64) -> Aabb {
    let a = AXIS[d as usize];
    let v = delta * sign(d);
    let (lo, hi) = (v.min(0.0), v.max(0.0));
    let face = if sign(d) > 0.0 { b.hi[a] } else { b.lo[a] };
    let mut out = *b;
    out.lo[a] = face + lo;
    out.hi[a] = face + hi;
    out
}

/// PistonMovingBlockEntity.getMovement: how far into `e` the area reaches along `d`.
fn movement(area: &Aabb, d: u8, e: &Aabb) -> f64 {
    let a = AXIS[d as usize];
    if sign(d) > 0.0 {
        area.hi[a] - e.lo[a]
    } else {
        e.hi[a] - area.lo[a]
    }
}

fn player_box(mover: &Mover, sneak: bool) -> Aabb {
    Aabb::player(mover.pos[0], mover.pos[1], mover.pos[2], if sneak { crate::physics::SNEAK_HEIGHT } else { crate::physics::HEIGHT })
}

/// moveEntityByPiston: the player moved `amount` along `d`, with `noclip`'s pushers ignored,
/// limited as Entity.limitPistonMovement does (0.51 per axis per tick).
fn piston_push(world: &mut World, b: &Blocks, mover: &mut Mover, noclip: u8, amount: f64, d: u8, sneak: bool, flying: bool) {
    let m = &mut world.moving;
    if m.deltas_tick != m.ticks {
        m.deltas = [0.0; 3];
        m.deltas_tick = m.ticks;
    }
    let a = AXIS[d as usize];
    let want = amount * sign(d);
    if want * want <= 1e-7 {
        return;
    }
    let e = (want + m.deltas[a]).clamp(-0.51, 0.51);
    let v = e - m.deltas[a];
    m.deltas[a] = e;
    if v.abs() <= 1.0e-5f32 as f64 {
        return;
    }
    let mut dv = [0.0; 3];
    dv[a] = v;
    world.moving.noclip.set(noclip);
    mover.push(world, b, dv, sneak, flying);
    world.moving.noclip.set(NONE);
}

/// PistonMovingBlockEntity.moveCollidedEntities, for the player, as the progress goes to `new`.
fn move_collided(world: &mut World, b: &Blocks, mover: &mut Mover, p: &Piston, new: f32, sneak: bool, flying: bool) {
    let Some(ids) = world.moving.ids.as_ref() else { return };
    let dir = p.movement();
    let delta = (new - p.progress) as f64;
    // getCollisionRelatedBlockState: a retracting piston's head, else what moves.
    let st = if !p.extending && p.source && ids.is_piston(p.moved) {
        ids.head_state(b, p.facing, p.progress > 0.25, ids.sticky.has(p.moved))
    } else {
        p.moved
    };
    let slime = b.state(p.moved).block == ids.slime && p.moved != 0;
    let o = p.offset(p.progress);
    let mut boxes: Vec<Aabb> = Vec::new();
    each_box(b, st, |lo, hi| {
        boxes.push(Aabb {
            lo: [p.pos[0] as f64 + lo[0] + o[0], p.pos[1] as f64 + lo[1] + o[1], p.pos[2] as f64 + lo[2] + o[2]],
            hi: [p.pos[0] as f64 + hi[0] + o[0], p.pos[1] as f64 + hi[1] + o[1], p.pos[2] as f64 + hi[2] + o[2]],
        })
    });
    if boxes.is_empty() {
        return;
    }
    let mut bounds = boxes[0];
    for bx in boxes.iter() {
        for i in 0..3 {
            bounds.lo[i] = bounds.lo[i].min(bx.lo[i]);
            bounds.hi[i] = bounds.hi[i].max(bx.hi[i]);
        }
    }
    let swept = movement_area(&bounds, dir, delta);
    let mut area = bounds;
    for i in 0..3 {
        area.lo[i] = area.lo[i].min(swept.lo[i]);
        area.hi[i] = area.hi[i].max(swept.hi[i]);
    }
    let pb = player_box(mover, sneak);
    if !meets(&area, &pb) {
        return;
    }
    // A slime block moved into the player throws them along (pushed or pulled).
    if slime {
        mover.vel[AXIS[dir as usize]] = sign(dir);
    }
    let mut far = 0.0f64;
    for bx in boxes.iter() {
        let ma = movement_area(bx, dir, delta);
        if meets(&ma, &pb) {
            far = far.max(movement(&ma, dir, &pb));
            if far >= delta {
                break;
            }
        }
    }
    if far <= 0.0 {
        return;
    }
    piston_push(world, b, mover, dir, far.min(delta) + 0.01, dir, sneak, flying);
    // fixEntityWithinPistonBase: out of the base a retracting piston leaves behind.
    if !p.extending && p.source {
        let pb = player_box(mover, sneak);
        let cell = Aabb { lo: [p.pos[0] as f64, p.pos[1] as f64, p.pos[2] as f64], hi: [p.pos[0] as f64 + 1.0, p.pos[1] as f64 + 1.0, p.pos[2] as f64 + 1.0] };
        if meets(&pb, &cell) {
            let opp = dir ^ 1;
            let d1 = movement(&cell, opp, &pb) + 0.01;
            let mut inter = pb;
            for i in 0..3 {
                inter.lo[i] = pb.lo[i].max(cell.lo[i]);
                inter.hi[i] = pb.hi[i].min(cell.hi[i]);
            }
            let d2 = movement(&cell, opp, &inter) + 0.01;
            if (d1 - d2).abs() < 0.01 {
                piston_push(world, b, mover, dir, d1.min(delta) + 0.01, opp, sneak, flying);
            }
        }
    }
}

/// One client tick of the moving blocks (after the player's own move, as block entities tick
/// after entities): pistons go 0.5 further, shulker lids 0.1, pushing the player.
pub fn tick(world: &mut World, b: &Blocks, mover: &mut Mover, sneak: bool, flying: bool) {
    world.moving.ticks += 1;
    if !world.moving.active() {
        return;
    }
    let mut i = 0;
    while i < world.moving.pistons.len() {
        let mut p = world.moving.pistons[i];
        if !world.loaded(p.pos[0], p.pos[2]) {
            world.moving.pistons.swap_remove(i);
            continue;
        }
        p.progress_o = p.progress;
        if p.progress_o >= 1.0 {
            // The client keeps a finished piston 5 ticks, then puts the block in place itself.
            if p.death < 5 {
                p.death += 1;
                world.moving.pistons[i] = p;
                i += 1;
                continue;
            }
            world.moving.pistons.swap_remove(i);
            let here = world.get(p.pos[0], p.pos[1], p.pos[2]);
            if world.moving.ids.as_ref().map_or(false, |ids| ids.moving.has(here)) {
                world.set(p.pos[0], p.pos[1], p.pos[2], p.moved, &|t| b.kind(t) == blocks::AIR);
            }
            continue;
        }
        let new = p.progress + 0.5;
        move_collided(world, b, mover, &p, new, sneak, flying);
        p.progress = new.min(1.0);
        if let Some(e) = world.moving.pistons.get_mut(i) {
            *e = p;
        }
        i += 1;
    }
    let mut i = 0;
    while i < world.moving.shulkers.len() {
        let mut s = world.moving.shulkers[i];
        let here = world.get(s.pos[0], s.pos[1], s.pos[2]);
        let still = world.moving.ids.as_ref().map_or(false, |ids| ids.shulkers.contains(&b.state(here).block));
        if !still || (s.status == 0 && s.progress == 0.0 && s.progress_o == 0.0) {
            world.moving.shulkers.swap_remove(i);
            continue;
        }
        // ShulkerBoxBlockEntity.updateAnimation.
        s.progress_o = s.progress;
        match s.status {
            0 => s.progress = 0.0,
            1 => {
                s.progress += 0.1;
                if s.progress >= 1.0 {
                    s.status = 2;
                    s.progress = 1.0;
                }
                shulker_push(world, b, mover, &s, sneak, flying);
            }
            3 => {
                s.progress -= 0.1;
                if s.progress <= 0.0 {
                    s.status = 0;
                    s.progress = 0.0;
                }
            }
            _ => s.progress = 1.0,
        }
        world.moving.shulkers[i] = s;
        i += 1;
    }
}

/// ShulkerBoxBlockEntity.moveCollidedEntities: the lid opening this tick lifts what it meets.
fn shulker_push(world: &mut World, b: &Blocks, mover: &mut Mover, s: &Shulker, sneak: bool, flying: bool) {
    let (mx, mn) = (s.progress_o.max(s.progress) as f64, s.progress_o.min(s.progress) as f64);
    let a = AXIS[s.facing as usize];
    let mut lo = [s.pos[0] as f64, s.pos[1] as f64, s.pos[2] as f64];
    let mut hi = [lo[0] + 1.0, lo[1] + 1.0, lo[2] + 1.0];
    let base = lo[a];
    if sign(s.facing) > 0.0 {
        lo[a] = base + 1.0 + mn;
        hi[a] = base + 1.0 + mx;
    } else {
        lo[a] = base - mx;
        hi[a] = base - mn;
    }
    let area = Aabb { lo, hi };
    if !meets(&area, &player_box(mover, sneak)) {
        return;
    }
    let mut d = [0.0; 3];
    d[a] = (mx - mn + 0.01) * sign(s.facing);
    mover.push(world, b, d, sneak, flying);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// The moving blocks (PistonHeadRenderer), in the world pass with the entities: `view` the
/// camera's rotation, positions relative to `eye`.
pub fn draw(gl: &Gl, view: &[f32; 16], eye: [f64; 3], world: &World, b: &Blocks, atlas: u32, partial: f32) {
    let m = &world.moving;
    let Some(ids) = m.ids.as_ref() else { return };
    if m.pistons.is_empty() {
        return;
    }
    let mut solid: Vec<V> = Vec::new();
    let mut clear: Vec<V> = Vec::new();
    let mut emit = |s: u16, at: [f64; 3], cell: [i32; 3]| {
        if s == 0 {
            return;
        }
        let blk = b.of_state(s);
        let tint = match blk.tint {
            1 | 2 | 5 => world.colour(cell[0], cell[1], cell[2], match blk.tint { 1 => 0, 2 => 1, _ => 2 }),
            0 => [255, 255, 255],
            n => blocks::tint_colour(n),
        };
        for q in b.model(s) {
            let base = if q.flags & blocks::Q_TINTED != 0 { tint } else { [255, 255, 255] };
            let l = q.light as f32 / 255.0;
            let c = [(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255];
            let out = if q.flags & blocks::Q_TRANSLUCENT != 0 { &mut clear } else { &mut solid };
            for i in 0..4 {
                let p = q.pos[i];
                out.push(V {
                    p: [
                        (at[0] - eye[0] + p[0] as f64 / 1024.0) as f32,
                        (at[1] - eye[1] + p[1] as f64 / 1024.0) as f32,
                        (at[2] - eye[2] + p[2] as f64 / 1024.0) as f32,
                    ],
                    uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0],
                    c,
                });
            }
        }
    };
    for p in m.pistons.iter() {
        let pr = p.progress_o + (p.progress - p.progress_o) * partial.min(1.0);
        let o = p.offset(pr);
        let cell = [p.pos[0] as f64, p.pos[1] as f64, p.pos[2] as f64];
        let at = [cell[0] + o[0], cell[1] + o[1], cell[2] + o[2]];
        let near = (at[0] - eye[0]) * (at[0] - eye[0]) + (at[2] - eye[2]) * (at[2] - eye[2]);
        if near > 256.0 * 256.0 {
            continue;
        }
        if ids.head.has(p.moved) {
            let sticky = (p.moved - ids.head.first) % 2 == 1;
            emit(ids.head_state(b, p.facing, pr <= 0.5, sticky), at, p.pos);
        } else if p.source && !p.extending && ids.is_piston(p.moved) {
            let sticky = ids.sticky.has(p.moved);
            emit(ids.head_state(b, p.facing, pr >= 0.5, sticky), at, p.pos);
            emit(ids.piston_state(b, sticky, true, p.facing), cell, p.pos);
        } else {
            emit(p.moved, at, p.pos);
        }
    }
    if solid.is_empty() && clear.is_empty() {
        return;
    }
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, atlas);
        (gl.EnableClientState)(VERTEX_ARRAY);
        (gl.EnableClientState)(COLOR_ARRAY);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.Disable)(CULL_FACE);
        for (v, blend) in [(&solid, false), (&clear, true)] {
            if v.is_empty() {
                continue;
            }
            (gl.Enable)(ALPHA_TEST);
            if blend {
                (gl.AlphaFunc)(GREATER, 0.004);
                (gl.Enable)(BLEND);
                (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
                (gl.DepthMask)(0);
            } else {
                (gl.AlphaFunc)(GREATER, 0.5);
            }
            (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
            (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
            (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            if blend {
                (gl.DepthMask)(1);
                (gl.Disable)(BLEND);
            }
        }
        (gl.Enable)(CULL_FACE);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
    }
}

/// The checks `penryn selftest` runs (no server needed): a piston lifting a stone and the
/// player on it, a shulker box opening under the player.
pub fn selftest(b: &Blocks) -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut out: Vec<(&'static str, String, &'static str)> = Vec::new();
    let find = |name: &str, f: u8| (0..b.states.len() as u16).find(|&s| b.of_state(s).name == name && b.state(s).facing == f);
    let stone = (0..b.states.len() as u16).find(|&s| b.of_state(s).name == "stone").unwrap_or(1);
    let Some(piston_block) = b.blocks.iter().position(|k| k.name == "piston") else { return out };
    // An unextended piston facing up (vanilla's order: extended true first).
    let Some(piston) = (0..b.states.len() as u16).filter(|&s| b.of_state(s).name == "piston" && b.state(s).facing == 5).nth(1) else { return out };
    // A piston at y 63 facing up under a stone at 64; the player stands on the stone.
    let mut w = crate::test_world(b, &[(8, 63, 8, piston), (8, 64, 8, stone)]);
    let mut m = Mover::new([8.5, 65.0, 8.5]);
    m.on_ground = true;
    block_event(&mut w, b, [8, 63, 8], 0, 1, piston_block as i32);
    let moved: Vec<String> = [65, 64]
        .iter()
        .map(|&y| w.moving.pistons.iter().find(|p| p.pos == [8, y, 8]).map_or(String::from("-"), |p| b.of_state(p.moved).name.clone()))
        .collect();
    out.push(("piston: what moves", moved.join(" "), "stone piston_head"));    tick(&mut w, b, &mut m, false, false);
    let mut boxes = Vec::new();
    w.moving.boxes_at(b, 8, 65, 8, &mut boxes);
    let top = boxes.iter().map(|bx| bx.hi[1]).fold(0.0, f64::max);
    out.push(("piston: half way after a tick", format!("{:.2} {:.2}", top, m.pos[1]), "65.50 65.51"));
    tick(&mut w, b, &mut m, false, false);
    out.push(("piston: lifts the player a block", format!("{:.2}", m.pos[1]), "66.01"));
    // A sticky piston pulls the stone back: its base moves in, the stone follows a step behind.
    if let (Some(sticky), Some(head), Some(sp)) = (
        (0..b.states.len() as u16).find(|&s| b.of_state(s).name == "sticky_piston" && b.state(s).facing == 5),
        w.moving.ids.as_ref().map(|ids| ids.head_state(b, 5, false, true)),
        b.blocks.iter().position(|k| k.name == "sticky_piston"),
    ) {
        let mut w = crate::test_world(b, &[(8, 63, 8, sticky), (8, 64, 8, head), (8, 65, 8, stone)]);
        block_event(&mut w, b, [8, 63, 8], 1, 1, sp as i32);
        let moved: Vec<String> = [63, 64]
            .iter()
            .map(|&y| w.moving.pistons.iter().find(|p| p.pos == [8, y, 8]).map_or(String::from("-"), |p| b.of_state(p.moved).name.clone()))
            .collect();
        out.push(("sticky piston: what moves back", format!("{} {}", moved.join(" "), b.kind(w.get(8, 65, 8)) == blocks::AIR), "sticky_piston stone true"));
    }
    // A shulker box facing up opens under the player: the lid lifts them 0.11 a tick.
    if let (Some(sb), Some(sh)) = (find("shulker_box", 5), b.blocks.iter().position(|k| k.name == "shulker_box")) {
        let mut w = crate::test_world(b, &[(8, 64, 8, sb)]);
        let mut m = Mover::new([8.5, 65.0, 8.5]);
        m.on_ground = true;
        block_event(&mut w, b, [8, 64, 8], 1, 1, sh as i32);
        for _ in 0..10 {
            tick(&mut w, b, &mut m, false, false);
        }
        let mut boxes = Vec::new();
        w.moving.boxes_at(b, 8, 64, 8, &mut boxes);
        let top = boxes.iter().map(|bx| bx.hi[1]).fold(0.0, f64::max);
        out.push(("shulker box: open lid and the player on it", format!("{:.2} {:.2}", top, m.pos[1]), "65.50 66.10"));
    }
    out
}
