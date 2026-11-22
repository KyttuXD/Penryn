//! Player movement as vanilla does it (LivingEntity.travel, Entity.move, collide), with the
//! block shapes from blocks.bin. The server checks movement, so the constants are vanilla's.

use crate::blocks::Blocks;
use crate::math;
use crate::world::World;
use alloc::vec::Vec;

pub const WIDTH: f64 = 0.6;
pub const HEIGHT: f64 = 1.8;
pub const SNEAK_HEIGHT: f64 = 1.5;
pub const EYE: f64 = 1.62;
pub const SNEAK_EYE: f64 = 1.27;
const STEP: f64 = 0.6;
const GRAVITY: f64 = 0.08;

#[derive(Clone, Copy)]
pub struct Aabb {
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

impl Aabb {
    pub fn player(x: f64, y: f64, z: f64, h: f64) -> Aabb {
        let r = WIDTH / 2.0;
        Aabb { lo: [x - r, y, z - r], hi: [x + r, y + h, z + r] }
    }
    fn offset(&self, d: [f64; 3]) -> Aabb {
        Aabb {
            lo: [self.lo[0] + d[0], self.lo[1] + d[1], self.lo[2] + d[2]],
            hi: [self.hi[0] + d[0], self.hi[1] + d[1], self.hi[2] + d[2]],
        }
    }
    fn expand(&self, d: [f64; 3]) -> Aabb {
        let mut a = *self;
        for i in 0..3 {
            if d[i] < 0.0 {
                a.lo[i] += d[i];
            } else {
                a.hi[i] += d[i];
            }
        }
        a
    }
    /// How far this box can move along `axis` by `d` before hitting `o`.
    fn clip(&self, o: &Aabb, axis: usize, mut d: f64) -> f64 {
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        if o.hi[a] <= self.lo[a] + 1e-7 || o.lo[a] >= self.hi[a] - 1e-7 || o.hi[b] <= self.lo[b] + 1e-7 || o.lo[b] >= self.hi[b] - 1e-7 {
            return d;
        }
        if d > 0.0 && o.lo[axis] >= self.hi[axis] - 1e-7 {
            d = d.min(o.lo[axis] - self.hi[axis]);
        } else if d < 0.0 && o.hi[axis] <= self.lo[axis] + 1e-7 {
            d = d.max(o.hi[axis] - self.lo[axis]);
        }
        d
    }
}

/// Who is colliding, for blocks whose shape depends on it (scaffolding, powder snow).
#[derive(Clone, Copy)]
pub struct Who {
    /// Feet height.
    pub feet: f64,
    /// Holding sneak (Entity.isDescending).
    pub descending: bool,
    pub fall_distance: f64,
    /// Wearing leather boots (walks on powder snow).
    pub boots: bool,
}

/// The collision boxes of the blocks inside `area`, for no one in particular. Unloaded
/// chunks are walls (as vanilla stops a player at the edge of what it has).
pub fn boxes(world: &World, b: &Blocks, area: &Aabb) -> Vec<Aabb> {
    boxes_for(world, b, area, None)
}

/// Vanilla's collision boxes (getCollisionShape) inside `area`, for `who`.
pub fn boxes_for(world: &World, b: &Blocks, area: &Aabb, who: Option<&Who>) -> Vec<Aabb> {
    let mut out = Vec::new();
    let mut walls: Vec<(i32, i32)> = Vec::new();
    let x0 = math::ifloor(area.lo[0]) - 1;
    let y0 = math::ifloor(area.lo[1]) - 1;
    let z0 = math::ifloor(area.lo[2]) - 1;
    let (x1, y1, z1) = (math::ifloor(area.hi[0]) + 1, math::ifloor(area.hi[1]) + 1, math::ifloor(area.hi[2]) + 1);
    for x in x0..=x1 {
        for z in z0..=z1 {
            if !world.loaded(x, z) {
                // One wall per chunk: a player already overlapping it can still slide out.
                let (cx, cz) = (x >> 4, z >> 4);
                if !walls.contains(&(cx, cz)) {
                    walls.push((cx, cz));
                    out.push(Aabb {
                        lo: [(cx * 16) as f64, -1e4, (cz * 16) as f64],
                        hi: [(cx * 16 + 16) as f64, 1e4, (cz * 16 + 16) as f64],
                    });
                }
                continue;
            }
            for y in y0..=y1 {
                cell_boxes(world, b, x, y, z, who, &mut out);
            }
        }
    }
    out
}

/// The collision boxes of the one block at x, y, z.
fn cell_boxes(world: &World, b: &Blocks, x: i32, y: i32, z: i32, who: Option<&Who>, out: &mut Vec<Aabb>) {
    use crate::blocks::{C_BAMBOO, C_OFFSET_OUTLINE, C_OUTLINE, C_POWDER_SNOW, C_SCAFFOLDING};
    // Pistons' moving blocks and opening shulker boxes (crate::moving) have their own shape.
    if world.moving.active() && world.moving.boxes_at(b, x, y, z, out) {
        return;
    }
    let s = world.get(x, y, z);
    if s == 0 || !b.collides(s) {
        return;
    }
    let block_box = |out: &mut Vec<Aabb>, lo: [f64; 3], hi: [f64; 3]| {
        out.push(Aabb {
            lo: [x as f64 + lo[0], y as f64 + lo[1], z as f64 + lo[2]],
            hi: [x as f64 + hi[0], y as f64 + hi[1], z as f64 + hi[2]],
        });
    };
    // CollisionContext.isAbove: the feet above the shape's top, by more than 1e-5f.
    let above = |top: f64| who.map_or(false, |w| w.feet > y as f64 + top - 1.0e-5f32 as f64);
    let outline = |out: &mut Vec<Aabb>, off: [f64; 3]| {
        for bx in b.shape(s) {
            let f = |i: usize| bx[i] as f64 / 16.0;
            block_box(out, [f(0) + off[0], f(1), f(2) + off[2]], [f(3) + off[0], f(4), f(5) + off[2]]);
        }
    };
    match b.state(s).coll_rule {
        C_BAMBOO => {
            // BambooStalkBlock: a 3/16 stalk at the block's offset.
            let o = b.offset_at(s, x, z);
            let (l, h) = (6.5 / 16.0, 9.5 / 16.0);
            block_box(out, [l + o[0], 0.0, l + o[2]], [h + o[0], 1.0, h + o[2]]);
        }
        C_SCAFFOLDING => {
            // ScaffoldingBlock: its top for someone above it and not sneaking down; the
            // bottom slab of an unsupported piece for someone above that; else nothing.
            let descending = who.map_or(false, |w| w.descending);
            if above(1.0) && !descending {
                block_box(out, [0.0, 0.875, 0.0], [1.0, 1.0, 1.0]);
            } else if above(0.0) && b.shape(s).iter().any(|bx| bx[1] == 0 && bx[4] == 2) {
                block_box(out, [0.0, 0.0, 0.0], [1.0, 0.125, 1.0]);
            }
        }
        C_POWDER_SNOW => {
            // PowderSnowBlock: catches a long fall; carries someone in leather boots.
            if let Some(w) = who {
                if w.fall_distance > 2.5 {
                    block_box(out, [0.0; 3], [1.0, 0.9f32 as f64, 1.0]);
                } else if w.boots && above(1.0) && !w.descending {
                    block_box(out, [0.0; 3], [1.0; 3]);
                }
            }
        }
        C_OFFSET_OUTLINE => outline(out, b.offset_at(s, x, z)),
        C_OUTLINE => outline(out, [0.0; 3]),
        _ => {
            for c in b.collision(s) {
                let f = |i: usize| c[i] as f64 / 32.0;
                block_box(out, [f(0), f(1), f(2)], [f(3), f(4), f(5)]);
            }
        }
    }
}

/// Whether a player box of height `h` at `pos` overlaps nothing (canPlayerFitWithinBlocks).
pub fn fits(world: &World, b: &Blocks, pos: [f64; 3], h: f64) -> bool {
    let bb = Aabb::player(pos[0], pos[1], pos[2], h);
    let s = Aabb { lo: [bb.lo[0] + 1e-7, bb.lo[1] + 1e-7, bb.lo[2] + 1e-7], hi: [bb.hi[0] - 1e-7, bb.hi[1] - 1e-7, bb.hi[2] - 1e-7] };
    let who = Who { feet: pos[1], descending: true, fall_distance: 0.0, boots: false };
    !boxes_for(world, b, &s, Some(&who)).iter().any(|o| overlaps(o, &s))
}

fn overlaps(o: &Aabb, s: &Aabb) -> bool {
    (0..3).all(|i| o.hi[i] > s.lo[i] && o.lo[i] < s.hi[i])
}

/// LivingEntity.onClimbable at a block: the #climbable tag, or an open trapdoor over a ladder
/// facing the same way.
fn climbable_at(world: &World, b: &Blocks, x: i32, y: i32, z: i32) -> bool {
    let s = world.get(x, y, z);
    if s == 0 {
        return false;
    }
    let name = b.of_state(s).name.as_str();
    if matches!(
        name,
        "ladder" | "vine" | "scaffolding" | "twisting_vines" | "twisting_vines_plant" | "weeping_vines"
            | "weeping_vines_plant" | "cave_vines" | "cave_vines_plant"
    ) {
        return true;
    }
    if name.ends_with("_trapdoor") && b.state(s).flags & crate::blocks::OPEN != 0 {
        let l = world.get(x, y - 1, z);
        return l != 0 && b.of_state(l).name == "ladder" && b.state(l).facing == b.state(s).facing;
    }
    false
}

/// Entity.collide without stepping: Y, then the larger horizontal axis first.
fn collide_plain(bb: &Aabb, d: [f64; 3], obstacles: &[Aabb]) -> [f64; 3] {
    let mut box_ = *bb;
    let mut out = [0.0; 3];
    let order = if d[0].abs() < d[2].abs() { [1, 2, 0] } else { [1, 0, 2] };
    for &axis in &order {
        let mut v = d[axis];
        if v != 0.0 {
            for o in obstacles {
                v = box_.clip(o, axis, v);
            }
            let mut m = [0.0; 3];
            m[axis] = v;
            box_ = box_.offset(m);
        }
        out[axis] = v;
    }
    out
}

pub struct Mover {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    pub horizontal_collision: bool,
    pub jump_delay: i32,
    /// Entity.fallDistance.
    pub fall_distance: f64,
    /// Entity.stuckSpeedMultiplier (cobwebs, berry bushes, powder snow), used by the next move.
    pub stuck: [f64; 3],
    /// Entity.mainSupportingBlockPos, and whether the last ground check found no block.
    pub support: Option<(i32, i32, i32)>,
    no_blocks: bool,
    /// This tick's moves (from, to, asked), for the blocks passed through.
    moves: Vec<([f64; 3], [f64; 3], [f64; 3])>,
    /// The status effects that change movement (set before each tick).
    pub effects: crate::effects::MoveEffects,
}

pub struct Input {
    pub forward: f64,
    pub strafe: f64,
    pub jump: bool,
    /// Crouching (the 1.5 box; also forced under a low ceiling).
    pub sneak: bool,
    /// The sneak key itself (edges, sinking through scaffolding, no bouncing).
    pub shift: bool,
    pub sprint: bool,
    /// Leather boots on (powder snow carries the player).
    pub boots: bool,
    /// The movement_speed attribute without sprinting (0.1), the flying speed from the
    /// abilities packet (0.05) and the jump_strength attribute (0.42).
    pub move_speed: f64,
    pub fly_speed: f32,
    pub jump_strength: f64,
}

impl Input {
    /// No keys held, vanilla's default attributes.
    pub fn idle() -> Input {
        Input {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            shift: false,
            sprint: false,
            boots: false,
            move_speed: 0.1f32 as f64,
            fly_speed: 0.05,
            jump_strength: 0.42f32 as f64,
        }
    }
}

const DRAG: f64 = 0.98f32 as f64;

impl Mover {
    pub fn new(pos: [f64; 3]) -> Mover {
        Mover {
            pos,
            vel: [0.0; 3],
            on_ground: false,
            horizontal_collision: false,
            jump_delay: 0,
            fall_distance: 0.0,
            stuck: [0.0; 3],
            support: None,
            no_blocks: false,
            moves: Vec::new(),
            effects: crate::effects::MoveEffects::default(),
        }
    }

    /// LivingEntity.getEffectiveGravity: slow falling falls at 0.01 while going down.
    fn gravity(&self) -> f64 {
        if self.vel[1] <= 0.0 && self.effects.slow_falling {
            GRAVITY.min(0.01)
        } else {
            GRAVITY
        }
    }

    /// Entity.move with MoverType.PISTON or SHULKER_BOX: pushed by `d` (no edge back-off and no
    /// stuck slowdown; the move is not one the blocks passed through see).
    pub fn push(&mut self, world: &World, b: &Blocks, d: [f64; 3], sneak: bool, flying: bool) {
        if self.stuck[0] * self.stuck[0] + self.stuck[1] * self.stuck[1] + self.stuck[2] * self.stuck[2] > 1e-7 {
            self.stuck = [0.0; 3];
            self.vel = [0.0; 3];
        }
        let inp = Input { sneak, ..Input::idle() };
        self.do_move(world, b, d, &inp, flying);
        self.moves.pop();
    }

    /// A teleport: the ground and supporting block are forgotten.
    pub fn reset(&mut self, pos: [f64; 3]) {
        self.pos = pos;
        self.vel = [0.0; 3];
        self.support = None;
        self.no_blocks = false;
        self.fall_distance = 0.0;
    }

    fn height(&self, sneak: bool) -> f64 {
        if sneak {
            SNEAK_HEIGHT
        } else {
            HEIGHT
        }
    }

    fn who(&self, inp: &Input) -> Who {
        Who { feet: self.pos[1], descending: inp.shift, fall_distance: self.fall_distance, boots: inp.boots }
    }

    fn feet_block(&self, world: &World) -> u16 {
        world.get(math::ifloor(self.pos[0]), math::ifloor(self.pos[1]), math::ifloor(self.pos[2]))
    }

    /// Entity.getOnPos(dy): through the supporting block when there is one (fences, walls and
    /// gates stand for themselves), else under the player's centre.
    fn on_pos(&self, world: &World, b: &Blocks, dy: f32) -> u16 {
        let dyd = dy as f64;
        if let Some((x, y, z)) = self.support {
            if dy > 1.0e-5 {
                let st = world.get(x, y, z);
                let n = b.of_state(st).name.as_str();
                let fence = n.ends_with("_fence");
                if (dyd <= 0.5 && fence) || n.ends_with("_wall") || n.ends_with("_fence_gate") {
                    return st;
                }
                return world.get(x, math::ifloor(self.pos[1] - dyd), z);
            }
            return world.get(x, y, z);
        }
        world.get(math::ifloor(self.pos[0]), math::ifloor(self.pos[1] - dyd), math::ifloor(self.pos[2]))
    }

    /// CollisionGetter.findSupportingBlock: of the blocks colliding with `probe`, the one
    /// nearest the player's position (ties: the larger position, by y, then z, then x).
    fn find_support(&self, world: &World, b: &Blocks, probe: &Aabb, who: &Who) -> Option<(i32, i32, i32)> {
        let mut best: Option<((i32, i32, i32), f64)> = None;
        let mut cell = Vec::new();
        for x in math::ifloor(probe.lo[0] - 1e-7) - 1..=math::ifloor(probe.hi[0] + 1e-7) + 1 {
            for y in math::ifloor(probe.lo[1] - 1e-7) - 1..=math::ifloor(probe.hi[1] + 1e-7) + 1 {
                for z in math::ifloor(probe.lo[2] - 1e-7) - 1..=math::ifloor(probe.hi[2] + 1e-7) + 1 {
                    cell.clear();
                    cell_boxes(world, b, x, y, z, Some(who), &mut cell);
                    if !cell.iter().any(|o| overlaps(o, probe)) {
                        continue;
                    }
                    let (ex, ey, ez) = (x as f64 + 0.5 - self.pos[0], y as f64 + 0.5 - self.pos[1], z as f64 + 0.5 - self.pos[2]);
                    let d = ex * ex + ey * ey + ez * ez;
                    let take = match best {
                        None => true,
                        Some((p, bd)) => d < bd || (d == bd && (y, z, x) > (p.1, p.2, p.0)),
                    };
                    if take {
                        best = Some(((x, y, z), d));
                    }
                }
            }
        }
        best.map(|(p, _)| p)
    }

    /// Entity.move: stuck-in-block slowdown, edge back-off, collision with vanilla's step-up,
    /// the supporting block, bounce, and the block's speed factor.
    fn do_move(&mut self, world: &World, b: &Blocks, mut d: [f64; 3], inp: &Input, flying: bool) {
        if self.stuck[0] * self.stuck[0] + self.stuck[1] * self.stuck[1] + self.stuck[2] * self.stuck[2] > 1e-7 {
            d = [d[0] * self.stuck[0], d[1] * self.stuck[1], d[2] * self.stuck[2]];
            self.stuck = [0.0; 3];
            self.vel = [0.0; 3];
        }
        let asked = d;
        let from = self.pos;
        let who = self.who(inp);
        let h = self.height(inp.sneak);
        let bb = Aabb::player(self.pos[0], self.pos[1], self.pos[2], h);
        // Player.maybeBackOffFromEdge: holding sneak above the ground never walks off an edge.
        let can_fall = |dx: f64, dz: f64, fall: f64| -> bool {
            let probe = Aabb {
                lo: [bb.lo[0] + 1e-7 + dx, bb.lo[1] - fall - 1e-7, bb.lo[2] + 1e-7 + dz],
                hi: [bb.hi[0] - 1e-7 + dx, bb.lo[1], bb.hi[2] - 1e-7 + dz],
            };
            !boxes_for(world, b, &probe, Some(&who)).iter().any(|o| overlaps(o, &probe))
        };
        let above_ground = self.on_ground || (self.fall_distance < STEP && !can_fall(0.0, 0.0, STEP - self.fall_distance));
        if inp.shift && !flying && d[1] <= 0.0 && above_ground {
            let step = 0.05;
            let less = |v: f64| if v.abs() <= step { 0.0 } else { v - step * v.signum() };
            while d[0] != 0.0 && can_fall(d[0], 0.0, STEP) {
                d[0] = less(d[0]);
            }
            while d[2] != 0.0 && can_fall(0.0, d[2], STEP) {
                d[2] = less(d[2]);
            }
            while d[0] != 0.0 && d[2] != 0.0 && can_fall(d[0], d[2], STEP) {
                d[0] = less(d[0]);
                d[2] = less(d[2]);
            }
        }
        let area = bb.expand(d).expand([0.0, STEP, 0.0]).expand([0.0, -1e-5, 0.0]);
        let obstacles = boxes_for(world, b, &area, Some(&who));
        let mut m = collide_plain(&bb, d, &obstacles);
        let hit_x = m[0] != d[0];
        let hit_z = m[2] != d[2];
        let fell_onto = d[1] != m[1] && d[1] < 0.0;
        // Entity.collide's step-up (maxUpStep 0.6): the lowest candidate height that gets
        // further, from where the fall stopped.
        if (self.on_ground || fell_onto) && (hit_x || hit_z) {
            let base = if fell_onto { bb.offset([0.0, m[1], 0.0]) } else { bb };
            let mut reach = base.expand([d[0], STEP, d[2]]);
            if !fell_onto {
                reach = reach.expand([0.0, -1.0e-5f32 as f64, 0.0]);
            }
            let mut hs: Vec<f32> = Vec::new();
            for o in obstacles.iter().filter(|o| overlaps(o, &reach)) {
                for y in [o.lo[1], o.hi[1]] {
                    let hh = (y - base.lo[1]) as f32;
                    if hh >= 0.0 && hh != m[1] as f32 && hh <= STEP as f32 && !hs.contains(&hh) {
                        hs.push(hh);
                    }
                }
            }
            hs.sort_by(|a, c| a.partial_cmp(c).unwrap_or(core::cmp::Ordering::Equal));
            for hh in hs {
                let c = collide_plain(&base, [d[0], hh as f64, d[2]], &obstacles);
                if c[0] * c[0] + c[2] * c[2] > m[0] * m[0] + m[2] * m[2] {
                    m = [c[0], c[1] - (bb.lo[1] - base.lo[1]), c[2]];
                    break;
                }
            }
        }
        self.pos = [self.pos[0] + m[0], self.pos[1] + m[1], self.pos[2] + m[2]];
        self.moves.push((from, self.pos, asked));
        self.horizontal_collision = m[0] != d[0] || m[2] != d[2];
        let vertical = m[1] != d[1];
        self.on_ground = vertical && d[1] < 0.0;
        // Entity.checkSupportingBlock.
        if self.on_ground {
            let nb = Aabb::player(self.pos[0], self.pos[1], self.pos[2], h);
            let probe = Aabb { lo: [nb.lo[0], nb.lo[1] - 1e-6, nb.lo[2]], hi: [nb.hi[0], nb.lo[1], nb.hi[2]] };
            let mut found = self.find_support(world, b, &probe, &who);
            if found.is_none() && !self.no_blocks {
                found = self.find_support(world, b, &probe.offset([-m[0], 0.0, -m[2]]), &who);
            }
            self.support = found;
            self.no_blocks = found.is_none();
        } else {
            self.no_blocks = false;
            self.support = None;
        }
        // checkFallDamage: falling adds up until the ground.
        if self.on_ground {
            self.fall_distance = 0.0;
        } else if m[1] < 0.0 {
            self.fall_distance -= m[1];
        }
        if m[0] != d[0] {
            self.vel[0] = 0.0;
        }
        if m[2] != d[2] {
            self.vel[2] = 0.0;
        }
        if vertical {
            // Entity.restituteMovementAfterCollisions: Block.bounceRestitution (slime 1, beds and
            // the shelf mushroom 0.75) bounces a landing faster than gravity, unless sneaking or
            // on honey; anything else stops it.
            let on = self.on_pos(world, b, 0.2);
            let on_name = if on != 0 { b.of_state(on).name.as_str() } else { "" };
            let rest = match on_name {
                "slime_block" => 1.0,
                "shelf_mushroom" => 0.75,
                n if n.ends_with("_bed") && n != "straw_bed" => 0.75,
                _ => 0.0,
            };
            let v = self.vel[1];
            let g = self.gravity();
            self.vel[1] = if d[1] < 0.0 && rest > 0.0 && -v > g && !inp.shift {
                let t = m[1] / v;
                (t * g - v) * (1.0 + t * (DRAG - 1.0)) * rest
            } else {
                0.0
            };
        }
        // getBlockSpeedFactor (soul sand, honey): the block at the feet, else the one below
        // (flying players: none).
        if !flying {
            let feet = self.feet_block(world);
            let mut f = b.of_state(feet).speed_factor;
            if f == 1.0 && b.kind(feet) != crate::blocks::WATER {
                f = b.of_state(self.on_pos(world, b, 0.500001)).speed_factor;
            }
            if f != 1.0 {
                self.vel[0] *= f as f64;
                self.vel[2] *= f as f64;
            }
        }
    }

    /// Entity.applyEffectsFromBlocks, after travel: the block stood on (slime), then the
    /// blocks passed through this tick (cobwebs, berry bushes and powder snow hold the player,
    /// honey slides it down, bubble columns push it).
    fn after_travel(&mut self, world: &World, b: &Blocks, inp: &Input, flying: bool) {
        // SlimeBlock.stepOn: walking on slime is slow (reads the speed after gravity).
        if self.on_ground && !inp.shift {
            let on = self.on_pos(world, b, 0.2);
            if on != 0 && b.of_state(on).name == "slime_block" {
                let a = self.vel[1].abs();
                if a < 0.1 {
                    let k = 0.4 + a * 0.2;
                    self.vel[0] *= k;
                    self.vel[2] *= k;
                }
            }
        }
        let h = self.height(inp.sneak);
        let mut moves = core::mem::take(&mut self.moves);
        if moves.is_empty() {
            moves.push((self.pos, self.pos, [0.0; 3]));
        }
        let mut visited: Vec<(i32, i32, i32)> = Vec::new();
        let deflate = |p: [f64; 3]| {
            let a = Aabb::player(p[0], p[1], p[2], h);
            Aabb { lo: [a.lo[0] + 1e-5, a.lo[1] + 1e-5, a.lo[2] + 1e-5], hi: [a.hi[0] - 1e-5, a.hi[1] - 1e-5, a.hi[2] - 1e-5] }
        };
        for (from, to, asked) in moves.iter() {
            // Each axis in turn (Direction.axisStepOrder: y, then the larger horizontal last).
            let order = if asked[0].abs() < asked[2].abs() { [1, 2, 0] } else { [1, 0, 2] };
            let mut segs: Vec<([f64; 3], [f64; 3])> = Vec::new();
            let mut s = *from;
            for &ax in &order {
                if to[ax] != from[ax] {
                    let mut e = s;
                    e[ax] = to[ax];
                    segs.push((s, e));
                    s = e;
                }
            }
            if segs.is_empty() {
                segs.push((*from, *to));
            }
            for (s, e) in segs {
                let (bs, be) = (deflate(s), deflate(e));
                let lo = [bs.lo[0].min(be.lo[0]), bs.lo[1].min(be.lo[1]), bs.lo[2].min(be.lo[2])];
                let hi = [bs.hi[0].max(be.hi[0]), bs.hi[1].max(be.hi[1]), bs.hi[2].max(be.hi[2])];
                let far = (0..3).map(|i| (e[i] - s[i]) * (e[i] - s[i])).sum::<f64>() > 0.99999f64 * 0.99999;
                for x in math::ifloor(lo[0])..=math::ifloor(hi[0]) {
                    for y in math::ifloor(lo[1])..=math::ifloor(hi[1]) {
                        for z in math::ifloor(lo[2])..=math::ifloor(hi[2]) {
                            if visited.contains(&(x, y, z)) {
                                continue;
                            }
                            visited.push((x, y, z));
                            let st = world.get(x, y, z);
                            if st != 0 {
                                let in_end = (be.hi[0] > x as f64 && be.lo[0] < x as f64 + 1.0)
                                    && (be.hi[1] > y as f64 && be.lo[1] < y as f64 + 1.0)
                                    && (be.hi[2] > z as f64 && be.lo[2] < z as f64 + 1.0);
                                self.entity_inside(world, b, st, x, y, z, flying, far || in_end);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Block.entityInside for the blocks that move a player.
    #[allow(clippy::too_many_arguments)]
    fn entity_inside(&mut self, world: &World, b: &Blocks, s: u16, x: i32, y: i32, z: i32, flying: bool, at_end: bool) {
        // Player.makeStuckInBlock and the bubble column pushes do nothing while flying.
        match b.of_state(s).name.as_str() {
            "cobweb" if !flying => {
                self.fall_distance = 0.0;
                self.stuck = [0.25, 0.05f32 as f64, 0.25];
            }
            "sweet_berry_bush" if !flying => {
                self.fall_distance = 0.0;
                self.stuck = [0.8f32 as f64, 0.75, 0.8f32 as f64];
            }
            "powder_snow" if !flying => {
                // Only when it is the block at the feet (getInBlockState).
                if (math::ifloor(self.pos[0]), math::ifloor(self.pos[1]), math::ifloor(self.pos[2])) == (x, y, z) {
                    self.fall_distance = 0.0;
                    self.stuck = [0.9f32 as f64, 1.5, 0.9f32 as f64];
                }
            }
            "honey_block" => {
                // HoneyBlock.isSlidingDown / doSlideMovement, on the speed before gravity.
                let old = self.vel[1] / DRAG + GRAVITY;
                let r = 0.4375 + (WIDTH as f32 / 2.0) as f64;
                let side = (x as f64 + 0.5 - self.pos[0]).abs() + 1e-7 > r || (z as f64 + 0.5 - self.pos[2]).abs() + 1e-7 > r;
                if !self.on_ground && self.pos[1] <= y as f64 + 0.9375 - 1e-7 && old < -0.08 && side {
                    let ny = (-0.05 - GRAVITY) * DRAG;
                    if old < -0.13 {
                        let k = -0.05 / old;
                        self.vel = [self.vel[0] * k, ny, self.vel[2] * k];
                    } else {
                        self.vel[1] = ny;
                    }
                    self.fall_distance = 0.0;
                }
            }
            "bubble_column" if !flying && at_end => {
                let down = b.state(s).flags & crate::blocks::DRAG_DOWN != 0;
                let up = world.get(x, y + 1, z);
                if up == 0 || (!b.collides(up) && !b.has_fluid(up) && b.kind(up) != crate::blocks::WATER && b.kind(up) != crate::blocks::LAVA) {
                    // Entity.onAboveBubbleColumn
                    self.vel[1] = if down { (self.vel[1] - 0.03).max(-0.9) } else { (self.vel[1] + 0.1).min(1.8) };
                } else {
                    // Entity.onInsideBubbleColumn
                    self.vel[1] = if down { (self.vel[1] - 0.03).max(-0.3) } else { (self.vel[1] + 0.06).min(0.7) };
                    self.fall_distance = 0.0;
                }
            }
            _ => {}
        }
    }

    fn move_relative(&mut self, speed: f64, inp: &Input, yaw: f32) {
        let (mut s, mut f) = (inp.strafe, inp.forward);
        let l = s * s + f * f;
        if l < 1e-7 {
            return;
        }
        if l > 1.0 {
            let n = math::sqrt(l);
            s /= n;
            f /= n;
        }
        s *= speed;
        f *= speed;
        let y = math::to_rad(yaw as f64);
        let (sin, cos) = (math::sin(y), math::cos(y));
        self.vel[0] += s * cos - f * sin;
        self.vel[2] += f * cos + s * sin;
    }

    /// One 20 Hz tick of player travel (LocalPlayer.aiStep and LivingEntity.travel).
    pub fn tick(&mut self, world: &World, b: &Blocks, inp: &Input, yaw: f32, flying: bool, fluid: u8) {
        self.moves.clear();
        if self.jump_delay > 0 {
            self.jump_delay -= 1;
        }
        // LivingEntity.aiStep: the delay goes when jump is let go (and while flying).
        if !inp.jump || flying {
            self.jump_delay = 0;
        }
        // A player's slow drift stops (horizontally as one).
        if self.vel[0] * self.vel[0] + self.vel[2] * self.vel[2] < 9.0e-6 {
            self.vel[0] = 0.0;
            self.vel[2] = 0.0;
        }
        if self.vel[1].abs() < 0.003 {
            self.vel[1] = 0.0;
        }
        let in_water = fluid == crate::blocks::WATER;
        let in_lava = fluid == crate::blocks::LAVA;
        if in_water || self.effects.slow_falling || self.effects.levitation.is_some() {
            self.fall_distance = 0.0;
        }
        if flying {
            // LocalPlayer.aiStep: up and down at 3x the flying speed; Player.travel: air
            // travel at the flying speed, then the old vertical speed x 0.6.
            let v = (inp.fly_speed * 3.0) as f64;
            if inp.jump {
                self.vel[1] += v;
            }
            if inp.shift {
                self.vel[1] -= v;
            }
            let vy = self.vel[1];
            let speed = (if inp.sprint { inp.fly_speed * 2.0 } else { inp.fly_speed }) as f64;
            self.move_relative(speed, inp, yaw);
            let d = self.vel;
            self.do_move(world, b, d, inp, true);
            self.vel = [self.vel[0] * 0.91f32 as f64, vy * 0.6, self.vel[2] * 0.91f32 as f64];
            self.fall_distance = 0.0;
            self.after_travel(world, b, inp, true);
            return;
        }
        if in_water || in_lava {
            if inp.jump {
                self.vel[1] += 0.04;
            }
            self.move_relative(0.02, inp, yaw);
            let oy = self.pos[1];
            let g = self.gravity();
            let d = self.vel;
            self.do_move(world, b, d, inp, false);
            let drag = if in_water { 0.8 } else { 0.5 };
            // travelInWater: dolphin's grace keeps 0.96 of the horizontal speed.
            let hdrag = if in_water && self.effects.dolphins_grace { 0.96f32 as f64 } else { drag };
            self.vel = [self.vel[0] * hdrag, self.vel[1] * drag, self.vel[2] * hdrag];
            self.vel[1] -= g / 16.0;
            // Climbing out at an edge: a hop, only when the player fits 0.6 higher and there
            // is no liquid there (travelInFluid's isFree).
            if self.horizontal_collision {
                let h = self.height(inp.sneak);
                let t = Aabb::player(self.pos[0], self.pos[1], self.pos[2], h)
                    .offset([self.vel[0], self.vel[1] + 0.6 - self.pos[1] + oy, self.vel[2]]);
                let blocked = boxes(world, b, &t).iter().any(|o| overlaps(o, &t));
                let mut wet = false;
                for x in math::ifloor(t.lo[0])..=math::ifloor(t.hi[0] - 1e-7) {
                    for y in math::ifloor(t.lo[1])..=math::ifloor(t.hi[1] - 1e-7) {
                        for z in math::ifloor(t.lo[2])..=math::ifloor(t.hi[2] - 1e-7) {
                            let k = b.kind(world.get(x, y, z));
                            wet |= k == crate::blocks::WATER || k == crate::blocks::LAVA;
                        }
                    }
                }
                if !blocked && !wet {
                    self.vel[1] = 0.3;
                }
            }
            self.after_travel(world, b, inp, false);
            return;
        }
        // LivingEntity.jumpFromGround: jump_strength x the block's jump factor (honey: half).
        if inp.jump && self.on_ground && self.jump_delay == 0 {
            let jf = b.of_state(self.feet_block(world)).jump_factor;
            let jf = if jf == 1.0 { b.of_state(self.on_pos(world, b, 0.500001)).jump_factor } else { jf };
            // getJumpPower: in floats, plus jump boost's 0.1 per level.
            let power = ((inp.jump_strength as f32) * jf + self.effects.jump_boost) as f64;
            if power > 1e-5 {
                self.vel[1] = self.vel[1].max(power);
                if inp.sprint {
                    let y = math::to_rad(yaw as f64);
                    self.vel[0] -= math::sin(y) * 0.2;
                    self.vel[2] += math::cos(y) * 0.2;
                }
            }
            self.jump_delay = 10;
        }
        // travelInAir: the friction of the block below (ice 0.98, slime 0.8) on the ground.
        let block_friction = if self.on_ground { b.of_state(self.on_pos(world, b, 0.500001)).friction as f64 } else { 1.0 };
        let speed = if self.on_ground {
            // getSpeed() (the attribute, +30% sprinting) x 0.21600002 / f^3, in floats.
            let walk = (if inp.sprint { inp.move_speed * 1.3 } else { inp.move_speed }) as f32;
            let bf = block_friction as f32;
            (walk * (0.21600002f32 / (bf * bf * bf))) as f64
        } else if inp.sprint {
            0.025999999f32 as f64
        } else {
            0.02f32 as f64
        };
        self.move_relative(speed, inp, yaw);
        // LivingEntity.handleOnClimbable: slow sideways, fall at most 0.15, hold on when sneaking.
        let (fx, fy, fz) = (math::ifloor(self.pos[0]), math::ifloor(self.pos[1]), math::ifloor(self.pos[2]));
        let on_climbable = climbable_at(world, b, fx, fy, fz);
        if on_climbable {
            self.fall_distance = 0.0;
            self.vel[0] = self.vel[0].clamp(-0.15, 0.15);
            self.vel[2] = self.vel[2].clamp(-0.15, 0.15);
            self.vel[1] = self.vel[1].max(-0.15);
            let scaffold = b.of_state(world.get(fx, fy, fz)).name == "scaffolding";
            if inp.shift && !scaffold && self.vel[1] < 0.0 {
                self.vel[1] = 0.0;
            }
        }
        let d = self.vel;
        self.do_move(world, b, d, inp, false);
        // Climbing up: into the wall or jumping, on what is climbable after the move; powder
        // snow too for someone in leather boots.
        let climb_now = climbable_at(world, b, math::ifloor(self.pos[0]), math::ifloor(self.pos[1]), math::ifloor(self.pos[2]));
        let feet_now = self.feet_block(world);
        let in_snow = feet_now != 0 && b.of_state(feet_now).name == "powder_snow";
        if (climb_now || (in_snow && inp.boots)) && (self.horizontal_collision || inp.jump) {
            self.vel[1] = 0.2;
        }
        let f = if self.on_ground { (block_friction as f32 * 0.91f32) as f64 } else { 0.91f32 as f64 };
        // travelInAir: levitation lifts towards 0.05 per level instead of gravity.
        let vy = match self.effects.levitation {
            Some(a) => self.vel[1] + (0.05 * (a + 1) as f64 - self.vel[1]) * 0.2,
            None => self.vel[1] - self.gravity(),
        };
        self.vel = [self.vel[0] * f, vy * DRAG, self.vel[2] * f];
        self.after_travel(world, b, inp, false);
    }
}
