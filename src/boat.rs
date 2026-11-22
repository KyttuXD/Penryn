//! Boats and rafts on the client that rows them, as vanilla's AbstractBoat.tick: the status
//! (in water, under water, under flowing water, on land, in the air), floatBoat (buoyancy,
//! 0.9 friction in water, the land's friction averaged under the hull, halved for the rower,
//! 0.45 under water, coming up to the surface from the air), controlBoat (turning 1 degree a
//! tick per key into the rotation that keeps 0.9 of itself, 0.04 forward, 0.005 back or
//! while only turning), and Entity.move with the boat's box. The paddles' phases turn PI/8 a
//! tick while rowing (every boat: the others' paddle states are synced data); the stroke
//! sounds come from the server.

use crate::blocks::{Blocks, WATER};
use crate::client::Game;
use crate::entities::EntityTable;
use crate::math;
use crate::physics::{self, Aabb, Input, Mover};
use crate::world::World;
use alloc::string::String;
use alloc::vec::Vec;

pub const WIDTH: f64 = 1.375;
pub const HEIGHT: f64 = 0.5625;
/// AbstractBoat.getDefaultGravity.
const GRAVITY: f64 = 0.04;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    InAir,
}

pub struct Boat {
    pub pos: [f64; 3],
    pub yaw: f32,
    pub vel: [f64; 3],
    pub delta_rotation: f32,
    pub status: Status,
    old_status: Option<Status>,
    water_level: f64,
    land_friction: f32,
    pub on_ground: bool,
    pub paddling: [bool; 2],
    pub raft: bool,
    mover: Mover,
    /// The water block's first state (its level 0), found once.
    water_base: Option<u16>,
}

/// The water block's first state: its states are its levels 0..15.
fn water_base(b: &Blocks) -> u16 {
    let mut first = 0u16;
    for (i, s) in b.states.iter().enumerate() {
        if b.blocks.get(s.block as usize).map_or(false, |k| k.kind == WATER && k.name == "water") {
            first = i as u16;
            break;
        }
    }
    first
}

/// FluidState.getHeight for water at a block, and whether it is a source: 1 under more water,
/// else its amount / 9 (waterlogged blocks hold a source).
pub fn water_height(world: &World, b: &Blocks, base: u16, x: i32, y: i32, z: i32) -> Option<(f32, bool)> {
    let s = world.get(x, y, z);
    let wet = |s: u16| s != 0 && (b.kind(s) == WATER || b.has_fluid(s));
    if !wet(s) {
        return None;
    }
    let (amount, source) = if b.kind(s) == WATER && b.of_state(s).name == "water" {
        let level = s.wrapping_sub(base) & 15;
        match level {
            0 => (8, true),
            1..=7 => (8 - level, false),
            _ => (8, false),
        }
    } else {
        (8, true)
    };
    let h = if wet(world.get(x, y + 1, z)) { 1.0 } else { amount as f32 / 9.0 };
    Some((h, source))
}

fn ceil(x: f64) -> i32 {
    -math::ifloor(-x)
}

impl Boat {
    pub fn new() -> Boat {
        let mut mover = Mover::new([0.0; 3]);
        mover.dims = Some((WIDTH, HEIGHT));
        mover.step = 0.0;
        Boat {
            pos: [0.0; 3],
            yaw: 0.0,
            vel: [0.0; 3],
            delta_rotation: 0.0,
            status: Status::InAir,
            old_status: None,
            water_level: 0.0,
            land_friction: 0.0,
            on_ground: false,
            paddling: [false; 2],
            raft: false,
            mover,
            water_base: None,
        }
    }

    fn bounds(&self) -> Aabb {
        let r = WIDTH / 2.0;
        Aabb { lo: [self.pos[0] - r, self.pos[1], self.pos[2] - r], hi: [self.pos[0] + r, self.pos[1] + HEIGHT, self.pos[2] + r] }
    }

    /// getWaterLevelAbove: the top of the water column the hull is in (a float, as vanilla's).
    fn water_level_above(&self, world: &World, b: &Blocks, base: u16) -> f32 {
        let bb = self.bounds();
        let (y0, y1) = (math::ifloor(bb.hi[1]), ceil(bb.hi[1]));
        'rows: for y in y0..y1 {
            let mut height = 0.0f32;
            for x in math::ifloor(bb.lo[0])..ceil(bb.hi[0]) {
                for z in math::ifloor(bb.lo[2])..ceil(bb.hi[2]) {
                    if let Some((h, _)) = water_height(world, b, base, x, y, z) {
                        height = height.max(h);
                    }
                    if height >= 1.0 {
                        continue 'rows;
                    }
                }
            }
            if height < 1.0 {
                return y as f32 + height;
            }
        }
        (y1 + 1) as f32
    }

    /// getStatus: isUnderwater, checkInWater, then the ground's friction.
    fn find_status(&mut self, world: &World, b: &Blocks, base: u16) -> Status {
        let bb = self.bounds();
        let max_y = bb.hi[1] + 0.001;
        let mut under = false;
        for x in math::ifloor(bb.lo[0])..ceil(bb.hi[0]) {
            for y in math::ifloor(bb.hi[1])..ceil(max_y) {
                for z in math::ifloor(bb.lo[2])..ceil(bb.hi[2]) {
                    if let Some((h, source)) = water_height(world, b, base, x, y, z) {
                        if max_y < (y as f32 + h) as f64 {
                            if !source {
                                self.water_level = bb.hi[1];
                                return Status::UnderFlowingWater;
                            }
                            under = true;
                        }
                    }
                }
            }
        }
        if under {
            self.water_level = bb.hi[1];
            return Status::UnderWater;
        }
        let mut in_water = false;
        self.water_level = f64::MIN;
        for x in math::ifloor(bb.lo[0])..ceil(bb.hi[0]) {
            for y in math::ifloor(bb.lo[1])..ceil(bb.lo[1] + 0.001) {
                for z in math::ifloor(bb.lo[2])..ceil(bb.hi[2]) {
                    if let Some((h, _)) = water_height(world, b, base, x, y, z) {
                        let level = (y as f32 + h) as f64;
                        self.water_level = self.water_level.max(level);
                        in_water |= bb.lo[1] < level;
                    }
                }
            }
        }
        if in_water {
            return Status::InWater;
        }
        let f = ground_friction(world, b, &bb);
        if f > 0.0 {
            self.land_friction = f;
            Status::OnLand
        } else {
            Status::InAir
        }
    }

    /// AbstractBoat.tick for the boat this client rows (`input`: left, right, forward,
    /// back). The paddle states it sends (ServerboundPaddleBoatPacket).
    pub fn tick(&mut self, world: &World, b: &Blocks, input: Option<[bool; 4]>) -> [bool; 2] {
        let base = *self.water_base.get_or_insert_with(|| water_base(b));
        let old = self.old_status.replace(self.status);
        let was = self.status;
        self.status = self.find_status(world, b, base);
        let controlled = input.is_some();
        // floatBoat
        let mut vspeed = -GRAVITY;
        let mut buoyancy = 0.0;
        if old.is_some() && was == Status::InAir && self.status != Status::InAir && self.status != Status::OnLand {
            self.water_level = self.pos[1] + HEIGHT;
            let target = (self.water_level_above(world, b, base) - HEIGHT as f32) as f64 + 0.101;
            let mut moved = self.bounds();
            moved.lo[1] = target;
            moved.hi[1] = target + HEIGHT;
            let s = Aabb { lo: [moved.lo[0] + 1e-7, moved.lo[1] + 1e-7, moved.lo[2] + 1e-7], hi: [moved.hi[0] - 1e-7, moved.hi[1] - 1e-7, moved.hi[2] - 1e-7] };
            let blocked = physics::boxes(world, b, &s).iter().any(|o| (0..3).all(|i| o.hi[i] > s.lo[i] && o.lo[i] < s.hi[i]));
            if !blocked {
                self.pos[1] = target;
                self.vel[1] = 0.0;
            }
            self.status = Status::InWater;
        } else {
            let inv_friction: f32 = match self.status {
                Status::InWater => {
                    buoyancy = (self.water_level - self.pos[1]) / HEIGHT;
                    0.9
                }
                Status::UnderFlowingWater => {
                    vspeed = -7.0e-4;
                    0.9
                }
                Status::UnderWater => {
                    buoyancy = 0.01f32 as f64;
                    0.45
                }
                Status::InAir => 0.9,
                Status::OnLand => {
                    let f = self.land_friction;
                    if controlled {
                        self.land_friction /= 2.0;
                    }
                    f
                }
            };
            let f = inv_friction as f64;
            self.vel = [self.vel[0] * f, self.vel[1] + vspeed, self.vel[2] * f];
            self.delta_rotation *= inv_friction;
            if buoyancy > 0.0 {
                self.vel[1] = (self.vel[1] + buoyancy * (GRAVITY / 0.65)) * 0.75;
            }
        }
        // controlBoat
        self.paddling = [false; 2];
        if let Some([left, right, up, down]) = input {
            let mut acc = 0.0f32;
            if left {
                self.delta_rotation -= 1.0;
            }
            if right {
                self.delta_rotation += 1.0;
            }
            if right != left && !up && !down {
                acc += 0.005;
            }
            self.yaw += self.delta_rotation;
            if up {
                acc += 0.04;
            }
            if down {
                acc -= 0.005;
            }
            let r = -self.yaw * 0.017453292f32;
            self.vel[0] += (crate::riding::msin(r as f64) * acc) as f64;
            self.vel[2] += (crate::riding::mcos((self.yaw * 0.017453292f32) as f64) * acc) as f64;
            self.paddling = [right && !left || up, left && !right || up];
        }
        // Entity.move(SELF) with the boat's box.
        self.mover.pos = self.pos;
        self.mover.vel = self.vel;
        self.mover.on_ground = self.on_ground;
        let d = self.vel;
        self.mover.move_entity(world, b, d, &Input::idle());
        self.pos = self.mover.pos;
        self.vel = self.mover.vel;
        self.on_ground = self.mover.on_ground;
        self.paddling
    }
}

/// AbstractBoat.getGroundFriction: the average friction of the blocks whose collision meets a
/// thin slab under the hull (lily pads not counted), 0 over nothing.
fn ground_friction(world: &World, b: &Blocks, bb: &Aabb) -> f32 {
    let slab = Aabb { lo: [bb.lo[0], bb.lo[1] - 0.001, bb.lo[2]], hi: [bb.hi[0], bb.lo[1], bb.hi[2]] };
    let (x0, x1) = (math::ifloor(slab.lo[0]) - 1, ceil(slab.hi[0]) + 1);
    let (y0, y1) = (math::ifloor(slab.lo[1]) - 1, ceil(slab.hi[1]) + 1);
    let (z0, z1) = (math::ifloor(slab.lo[2]) - 1, ceil(slab.hi[2]) + 1);
    let (mut sum, mut n) = (0.0f32, 0);
    for x in x0..x1 {
        for z in z0..z1 {
            let edges = (x == x0 || x == x1 - 1) as i32 + (z == z0 || z == z1 - 1) as i32;
            if edges == 2 {
                continue;
            }
            for y in y0..y1 {
                if edges == 1 && (y == y0 || y == y1 - 1) {
                    continue;
                }
                let s = world.get(x, y, z);
                if s == 0 || !b.collides(s) || b.of_state(s).name == "lily_pad" {
                    continue;
                }
                let cell = Aabb { lo: [x as f64, y as f64, z as f64], hi: [x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0] };
                let hit = physics::boxes(world, b, &cell).iter().any(|o| {
                    o.lo[0] >= x as f64 - 1e-9 && o.hi[0] <= x as f64 + 1.0 + 1e-9 && o.lo[2] >= z as f64 - 1e-9 && o.hi[2] <= z as f64 + 1.0 + 1e-9
                        && o.lo[1] >= y as f64 - 1e-9 && o.lo[1] < y as f64 + 1.0
                        && (0..3).all(|i| o.hi[i] > slab.lo[i] && o.lo[i] < slab.hi[i])
                });
                if hit {
                    sum += b.of_state(s).friction;
                    n += 1;
                }
            }
        }
    }
    if n > 0 {
        sum / n as f32
    } else {
        0.0
    }
}

/// Every boat's paddles (AbstractBoat.tick, on every client): a rowing side turns PI/8 a tick,
/// an idle one rests at 0. `ours` is the boat this client rows and its paddles (the others'
/// come from their synced data, 11 and 12). The stroke sounds are the server's: a client's
/// Level.playSound with no player plays nothing (ClientLevel.playSeededSound), and the
/// server's boat broadcasts them to everyone near, the rower included.
pub fn animate_paddles(g: &mut Game, t: &EntityTable, ours: Option<(i32, [bool; 2])>) {
    use core::f32::consts::PI;
    for (&id, e) in g.entities.iter_mut() {
        let name = t.types.get(e.kind as usize).map_or("", |x| x.name.as_str());
        if !(name.ends_with("_boat") || name.ends_with("_raft")) {
            continue;
        }
        // getPaddleState: the synced flags, only while a player is aboard to row.
        let rowing = match ours {
            Some((v, p)) if v == id => p,
            _ => [e.d(11) != 0, e.d(12) != 0],
        };
        for (i, key) in [crate::riding::D_PADDLE_LEFT, crate::riding::D_PADDLE_RIGHT].iter().enumerate() {
            let mut phase = f32::from_bits(e.data.get(key).copied().unwrap_or(0) as u32);
            if rowing[i] {
                phase += PI / 8.0;
            } else {
                phase = 0.0;
            }
            e.data.insert(*key, phase.to_bits() as i64);
        }
    }
}

/// AbstractBoatModel.animatePaddle: the paddle's x and y turns for a rowing time.
pub fn paddle_angles(time: f32, side: usize) -> (f32, f32) {
    let lerp = |t: f32, a: f32, b: f32| a + t.clamp(0.0, 1.0) * (b - a);
    let x = lerp((crate::riding::msin(-time as f64) + 1.0) / 2.0, -1.0471976, -0.2617994);
    let mut y = lerp((crate::riding::msin((-time + 1.0) as f64) + 1.0) / 2.0, -0.7853982, 0.7853982);
    if side == 1 {
        y = core::f32::consts::PI - y;
    }
    (x, y)
}

/// Checks: a boat dropped on still water floats up and stays; one rowed forward goes along
/// its yaw; on stone it slides with the stone's friction.
pub fn selftest(b: &Blocks, w: &World) -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut v = Vec::new();
    // The test world is stone at y 63 (main.rs test_world); a boat on it is on land.
    let mut boat = Boat::new();
    boat.pos = [8.5, 64.0, 8.5];
    boat.vel = [0.2, 0.0, 0.0];
    boat.on_ground = true;
    boat.tick(w, b, Some([false; 4]));
    v.push(("boat on stone", format!("{:?} {:.5}", boat.status, boat.vel[0]), "OnLand 0.12000"));
    // Forward from rest: 0.04 along the yaw (south at yaw 0).
    let mut boat = Boat::new();
    boat.pos = [8.5, 64.0, 8.5];
    boat.on_ground = true;
    boat.tick(w, b, Some([false, false, true, false]));
    v.push(("boat paddles forward", format!("{:.4} {:.4} {:?}", boat.vel[0], boat.vel[2], boat.paddling), "0.0000 0.0400 [true, true]"));
    // Turning left turns the boat 1 degree, and rows the right paddle.
    let mut boat = Boat::new();
    boat.pos = [8.5, 64.0, 8.5];
    boat.on_ground = true;
    boat.tick(w, b, Some([true, false, false, false]));
    v.push(("boat turns", format!("{:.2} {:?}", boat.yaw, boat.paddling), "-1.00 [false, true]"));
    let (x, y) = paddle_angles(0.0, 0);
    v.push(("paddle at rest", format!("{:.4} {:.4}", x, y), "-0.6545 0.6609"));
    v
}
