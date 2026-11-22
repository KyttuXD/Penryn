//! Riding as vanilla's client does it: who rides what (ClientboundSetPassengersPacket), the seat
//! each passenger sits in (Entity.positionRider with the types' passenger attachments, less the
//! rider's own vehicle attachment), the camera riding along, getting off with sneak (the server
//! dismounts the player on the PlayerInput's shift; the client puts itself where
//! getDismountLocationForPassenger says), and the vehicles this client moves itself
//! (isLocalInstanceAuthoritative): boats and rafts (boat.rs), saddled horses, donkeys, mules and
//! camels, pigs and striders steered with their stick, and the harnessed happy ghast, sent as
//! ServerboundMoveVehiclePacket after ServerboundMovePlayerPacket.Rot each tick. Minecarts and
//! everything else are moved by the server; the player only sits in them.
//!
//! The horse's jump bar (LocalPlayer.jumpRidingScale, sent as START_RIDING_JUMP), the camel's
//! dash with its cooldown, the pig's and strider's boost (ItemBasedSteering) and the vehicle's
//! hearts in place of the food bar are here too.

use crate::blocks::Blocks;
use crate::client::Game;
use crate::entities::{Entity, EntityTable};
use crate::math;
use crate::physics::{self, Input, Mover};
use crate::wire::{In, Out};
use crate::world::World;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub const C_SET_PASSENGERS: i32 = 109;
pub const C_MOVE_VEHICLE: i32 = 58;
pub const C_SET_ENTITY_MOTION: i32 = 103;
pub const S_MOVE_PLAYER_ROT: i32 = 32;
pub const S_MOVE_VEHICLE: i32 = 34;
pub const S_PADDLE_BOAT: i32 = 35;
/// ServerboundPlayerCommandPacket.Action: START_RIDING_JUMP, START_FALL_FLYING.
pub const START_RIDING_JUMP: i32 = 3;
pub const START_FALL_FLYING: i32 = 6;

/// Client-side values kept in an entity's data map above the synced indices (these never come
/// from the server): the boat's paddle phases (f32 bits), the rider flag for the model's pose.
pub const D_PADDLE_LEFT: u8 = 0xF0;
pub const D_PADDLE_RIGHT: u8 = 0xF1;
pub const D_RIDING: u8 = 0xF2;
/// Ticks in the gliding or riptide pose (the model leans in over the first 10).
pub const D_FLY_TICKS: u8 = 0xF3;

/// Avatar.DEFAULT_VEHICLE_ATTACHMENT.
pub const PLAYER_VEHICLE_ATTACHMENT: f64 = 0.6;

/// Mth.sin as vanilla's 65536-step table has it (computed, not stored).
pub fn msin(x: f64) -> f32 {
    let i = ((x * 10430.378350470453) as i64 as i32) & 65535;
    math::sin(i as f64 * math::PI * 2.0 / 65536.0) as f32
}

pub fn mcos(x: f64) -> f32 {
    let i = (((x * 10430.378350470453) as i64 as i32).wrapping_add(16384)) & 65535;
    math::sin(i as f64 * math::PI * 2.0 / 65536.0) as f32
}

/// What the server told us about vehicles, kept by the session (client.rs).
#[derive(Default)]
pub struct Links {
    /// Passenger -> vehicle, and each vehicle's passengers in order (the first one steers).
    pub vehicle_of: BTreeMap<i32, i32>,
    pub passengers: BTreeMap<i32, Vec<i32>>,
    /// Other entities' movement_speed, jump_strength, step_height, max_health and flying_speed
    /// attributes (NaN: not sent).
    pub attrs: BTreeMap<i32, [f64; 5]>,
    /// ClientboundMoveVehiclePacket: the server put our vehicle back here.
    pub moved: Option<([f64; 3], f32, f32)>,
    /// The vehicle this client moves and where: sent each tick (ServerboundMoveVehiclePacket).
    pub driven: Option<(i32, [f64; 3], f32, f32, bool)>,
    /// The jump bar (scale, on cooldown) while riding a jumping mount.
    pub jump_bar: Option<(f32, bool)>,
    /// The ridden living vehicle's health and max health (its hearts replace the food bar).
    pub vehicle_health: Option<(f32, f32)>,
    /// ClientboundSetEntityMotion for us (a firework, a riptide are client-side, this is the
    /// server's knockback): the velocity, until the mover takes it.
    pub motion: Option<[f64; 3]>,
    /// Our own shared flags (Entity.DATA_SHARED_FLAGS_ID) as the server last sent them.
    pub own_flags: Option<u8>,
}

impl Links {
    pub fn on_passengers(&mut self, r: &mut In) {
        let Some(vehicle) = r.varint() else { return };
        let n = r.varint().unwrap_or(0).clamp(0, 64);
        let mut list = Vec::new();
        for _ in 0..n {
            match r.varint() {
                Some(p) => list.push(p),
                None => break,
            }
        }
        if let Some(old) = self.passengers.remove(&vehicle) {
            for p in old {
                if self.vehicle_of.get(&p) == Some(&vehicle) {
                    self.vehicle_of.remove(&p);
                }
            }
        }
        for &p in list.iter() {
            // A passenger of another vehicle moves (Entity.startRiding ends the old ride).
            if let Some(v) = self.vehicle_of.insert(p, vehicle) {
                if v != vehicle {
                    if let Some(l) = self.passengers.get_mut(&v) {
                        l.retain(|&q| q != p);
                    }
                }
            }
        }
        if !list.is_empty() {
            self.passengers.insert(vehicle, list);
        }
    }

    pub fn on_move_vehicle(&mut self, r: &mut In) {
        if let (Some(x), Some(y), Some(z), Some(yaw), Some(pitch)) = (r.f64(), r.f64(), r.f64(), r.f32(), r.f32()) {
            self.moved = Some(([x, y, z], yaw, pitch));
        }
    }

    /// ClientboundSetEntityMotion (LP vec3); only ours is kept.
    pub fn on_motion(&mut self, r: &mut In, me: i32) {
        if r.varint() == Some(me) {
            self.motion = read_lp_vec3(r);
        }
    }

    /// An attribute of an entity other than the player (worked out as client.rs does).
    pub fn on_attribute(&mut self, id: i32, attr: i32, v: f64) {
        let i = match attr {
            26 => 0,
            19 => 1,
            33 => 2,
            23 => 3,
            15 => 4,
            _ => return,
        };
        let a = self.attrs.entry(id).or_insert([f64::NAN; 5]);
        a[i] = v;
    }

    pub fn forget(&mut self, id: i32) {
        self.attrs.remove(&id);
        if let Some(old) = self.passengers.remove(&id) {
            for p in old {
                if self.vehicle_of.get(&p) == Some(&id) {
                    self.vehicle_of.remove(&p);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Links::default();
    }
}

/// Vec3.LP_STREAM_CODEC.
fn read_lp_vec3(r: &mut In) -> Option<[f64; 3]> {
    let lowest = r.u8()? as u64;
    if lowest == 0 {
        return Some([0.0; 3]);
    }
    let middle = r.u8()? as u64;
    let high = r.i32()? as u32 as u64;
    let buffer = lowest | middle << 8 | high << 16;
    let mut scale = lowest & 3;
    if lowest & 4 != 0 {
        scale |= (r.varint()? as u32 as u64) << 2;
    }
    let s = scale as f64;
    let unpack = |v: u64| ((v & 32767) as f64).min(32766.0) * 2.0 / 32766.0 - 1.0;
    Some([unpack(buffer >> 3) * s, unpack(buffer >> 18) * s, unpack(buffer >> 33) * s])
}

/// LocalPlayer.sendChanges while riding: ServerboundMovePlayerPacket.Rot, then the vehicle's
/// move when this client moves it. False when the player rides nothing (it sends its position).
pub fn send(g: &mut Game) -> Result<bool, String> {
    if !g.riding.vehicle_of.contains_key(&g.entity_id) {
        g.riding.driven = None;
        return Ok(false);
    }
    let mut o = Out::new();
    o.f32(g.player.yaw).f32(g.player.pitch).u8(0);
    g.conn.send(S_MOVE_PLAYER_ROT, &o)?;
    if let Some((_, p, yaw, pitch, on_ground)) = g.riding.driven {
        let mut o = Out::new();
        o.f64(p[0]).f64(p[1]).f64(p[2]).f32(yaw).f32(pitch).bool(on_ground);
        g.conn.send(S_MOVE_VEHICLE, &o)?;
    }
    Ok(true)
}

/// The vehicle kinds the client treats differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Boat,
    Raft,
    ChestBoat,
    Minecart,
    Horse,
    Camel,
    Pig,
    Strider,
    HappyGhast,
    Llama,
    Other,
}

pub fn kind_of(name: &str) -> Kind {
    if name.ends_with("_chest_boat") || name.ends_with("_chest_raft") {
        Kind::ChestBoat
    } else if name.ends_with("_raft") {
        Kind::Raft
    } else if name.ends_with("_boat") {
        Kind::Boat
    } else if name.ends_with("minecart") {
        Kind::Minecart
    } else {
        match name {
            "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => Kind::Horse,
            "camel" | "camel_husk" => Kind::Camel,
            "pig" => Kind::Pig,
            "strider" => Kind::Strider,
            "happy_ghast" => Kind::HappyGhast,
            "llama" | "trader_llama" => Kind::Llama,
            _ => Kind::Other,
        }
    }
}

fn type_name<'a>(t: &'a EntityTable, e: &Entity) -> &'a str {
    t.types.get(e.kind as usize).map_or("", |x| x.name.as_str())
}

/// EntityType passenger attachments (before the vehicle's turn), for the seat `index` of
/// `count` (EntityAttachments.get clamps to the last), as Horizon's riders.rs lists them.
pub fn passenger_attachment(t: &EntityTable, v: &Entity, index: usize, count: usize, rider_animal: bool) -> [f64; 3] {
    let name = type_name(t, v);
    let baby = t.is_baby(v);
    let up = |y: f32| [0.0, y as f64, 0.0];
    match kind_of(name) {
        Kind::Boat | Kind::Raft | Kind::ChestBoat => {
            // AbstractBoat.getPassengerAttachmentPoint: a third of the height up (a raft 8/9),
            // the second seat behind.
            let h = 0.5625f32;
            let y = if matches!(kind_of(name), Kind::Raft) || name == "bamboo_chest_raft" { h * 0.8888889 } else { h / 3.0 };
            let mut z = if kind_of(name) == Kind::ChestBoat { 0.15f32 } else { 0.0 };
            if count > 1 {
                z = if index == 0 { 0.2 } else { -0.6 };
                if rider_animal {
                    z += 0.2;
                }
            }
            return [0.0, y as f64, z as f64];
        }
        // AbstractMinecart's passenger attachment (villagers sit lower; not players).
        Kind::Minecart => return [0.0, 0.1875, 0.0],
        Kind::HappyGhast => {
            const SEATS: [[f64; 3]; 4] = [[0.0, 4.0, 1.7], [-1.7, 4.0, 0.0], [0.0, 4.0, -1.7], [1.7, 4.0, 0.0]];
            return SEATS[index.min(3)];
        }
        _ => {}
    }
    match name {
        "horse" => up(1.44375),
        "donkey" => up(1.1125),
        "mule" => up(1.2125),
        "skeleton_horse" | "zombie_horse" => up(1.31875),
        "pig" if baby => up(0.5),
        "pig" => up(0.86875),
        // Camel.getPassengerAttachmentPoint: half a block ahead at the body anchor (its height
        // less 0.375; sitting, 0.2 above the sitting box's), the second seat 0.7 behind, an
        // animal 0.2 further ahead; a baby's at 0.6 (pose transitions not followed).
        "camel" | "camel_husk" => {
            let sitting = v.d(19) < 0;
            let scale = if baby { 0.6f32 } else { 1.0 };
            let h = if sitting { if baby { 0.425 } else { 2.375 - 1.43 } } else { t.dims(v).1 };
            let y = h as f64 - if baby { 0.09375 } else { 0.375 } + if sitting { (scale * 1.43 - (scale * 1.43 - scale * 0.2)) as f64 } else { 0.0 };
            let mut z = 0.5f32;
            if count > 1 {
                if index != 0 {
                    z = -0.7;
                }
                if rider_animal {
                    z += 0.2;
                }
            }
            [0.0, y, (z * scale) as f64]
        }
        "strider" => [0.0, 1.6, 0.0],
        "llama" | "trader_llama" if baby => [0.0, 0.81f32 as f64, -0.15f32 as f64],
        "llama" | "trader_llama" => [0.0, 1.37, -0.3],
        "chicken" => [0.0, 0.7, -0.1],
        "spider" => up(0.765),
        "ravager" => [0.0, 2.2625, -0.0625],
        "hoglin" if baby => up(0.875),
        "hoglin" | "zoglin" => up(1.49375),
        "nautilus" | "zombie_nautilus" => up(1.1375),
        "cow" | "mooshroom" => up(1.36875),
        "sheep" => up(1.2375),
        "goat" => up(1.1125),
        "ghast" => up(4.0625),
        "player" => [0.0, t.dims(v).1 as f64, 0.0],
        _ => [0.0, t.dims(v).1 as f64, 0.0],
    }
}

/// Vec3.yRot(-yaw) of an attachment point (Mth's sine table).
fn rotated(a: [f64; 3], yaw: f32) -> [f64; 3] {
    let r = (-yaw * 0.017453292f32) as f64;
    let (s, c) = (msin(r) as f64, mcos(r) as f64);
    [a[0] * c + a[2] * s, a[1], a[2] * c - a[0] * s]
}

/// Entity.positionRider: where a rider sits on `v` at `at` turned `yaw`.
pub fn seat(t: &EntityTable, v: &Entity, at: [f64; 3], yaw: f32, index: usize, count: usize, rider_offset: f64, rider_animal: bool) -> [f64; 3] {
    let a = rotated(passenger_attachment(t, v, index, count, rider_animal), yaw);
    [at[0] + a[0], at[1] + a[1] - rider_offset, at[2] + a[2]]
}

/// A rider's own vehicle attachment (the humanoids hang their legs, a ghast sits up).
fn rider_offset(t: &EntityTable, r: &Entity) -> f64 {
    match type_name(t, r) {
        "player" | "mannequin" => PLAYER_VEHICLE_ATTACHMENT,
        "zombie" | "husk" | "drowned" if t.is_baby(r) => 0.1875,
        "zombie_villager" if t.is_baby(r) => 0.125,
        "piglin" if t.is_baby(r) => 0.1875,
        "zombie" | "husk" | "drowned" | "zombie_villager" | "zombified_piglin" | "piglin" | "piglin_brute" | "skeleton" | "stray" | "bogged" => 0.7f32 as f64,
        "wither_skeleton" => 0.875f32 as f64,
        "pillager" | "vindicator" | "evoker" | "illusioner" => 0.6f32 as f64,
        _ => 0.0,
    }
}

fn is_animal(name: &str) -> bool {
    matches!(
        name,
        "cow" | "pig" | "sheep" | "chicken" | "wolf" | "cat" | "ocelot" | "fox" | "rabbit" | "goat" | "llama" | "horse" | "donkey" | "mule" | "panda" | "polar_bear" | "frog" | "turtle" | "bee" | "parrot" | "mooshroom" | "axolotl" | "armadillo" | "sniffer" | "camel" | "strider" | "hoglin"
    )
}

/// The other entities riding something: sat on their seats (vanilla's client positions every
/// passenger itself; the server sends no position for them). `prev` sits on the vehicle's
/// last tick place, so they move smoothly with it.
pub fn seat_passengers(g: &mut Game, t: &EntityTable) {
    let links: Vec<(i32, Vec<i32>)> = g.riding.passengers.iter().map(|(v, l)| (*v, l.clone())).collect();
    for (vid, list) in links {
        let Some(v) = g.entities.get(&vid) else { continue };
        let (vpos, vprev, vyaw) = (v.pos, v.prev, vehicle_yaw(t, v));
        let n = list.len();
        let mut seats: Vec<(i32, [f64; 3], [f64; 3], f32)> = Vec::new();
        for (i, pid) in list.iter().enumerate() {
            if *pid == g.entity_id {
                continue;
            }
            let Some(p) = g.entities.get(pid) else { continue };
            let off = rider_offset(t, p);
            let animal = is_animal(type_name(t, p));
            seats.push((*pid, seat(t, v, vpos, vyaw, i, n, off, animal), seat(t, v, vprev, vyaw, i, n, off, animal), vyaw));
        }
        let boat = matches!(kind_of(type_name(t, v)), Kind::Boat | Kind::Raft | Kind::ChestBoat);
        for (pid, at, before, yaw) in seats {
            if let Some(p) = g.entities.get_mut(&pid) {
                p.pos = at;
                p.prev = before;
                p.data.insert(D_RIDING, 1);
                // AbstractBoat.clampRotation: the body faces the boat's way.
                if boat {
                    p.body_yaw = yaw;
                }
            }
        }
    }
    // Riders that got off stand again; gliders count their ticks.
    let riding = &g.riding.vehicle_of;
    for (id, e) in g.entities.iter_mut() {
        if e.data.contains_key(&D_RIDING) && !riding.contains_key(id) {
            e.data.remove(&D_RIDING);
        }
        let pose = e.data.get(&6).copied().unwrap_or(0);
        if pose == crate::poses::FALL_FLYING || pose == crate::poses::SPIN_ATTACK {
            *e.data.entry(D_FLY_TICKS).or_insert(0) += 1;
        } else if e.data.contains_key(&D_FLY_TICKS) {
            e.data.remove(&D_FLY_TICKS);
        }
    }
}

/// The yaw a vehicle's seats turn with (a living mount's body; a boat's or a cart's own).
fn vehicle_yaw(t: &EntityTable, v: &Entity) -> f32 {
    match kind_of(type_name(t, v)) {
        Kind::Boat | Kind::Raft | Kind::ChestBoat | Kind::Minecart | Kind::Other if v.health.is_none() && type_name(t, v) != "player" => v.yaw,
        _ => v.body_yaw,
    }
}

/// The keys the rider holds this tick.
#[derive(Clone, Copy, Default)]
pub struct Keys {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sprint: bool,
}

/// The local player's ride.
pub struct Rider {
    /// The vehicle ridden (-1: none).
    pub vehicle: i32,
    pub kind: Kind,
    /// The vehicle's place this tick and last tick, its yaw and pitch (when this client moves it).
    pub pos: [f64; 3],
    pub prev: [f64; 3],
    pub yaw: f32,
    pub prev_yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    /// Whether this client moves the vehicle (isLocalInstanceAuthoritative).
    pub controlled: bool,
    boat: crate::boat::Boat,
    mount: Mover,
    /// LocalPlayer.jumpRidingTicks and jumpRidingScale; the jump held last tick.
    jump_ticks: i32,
    pub jump_scale: f32,
    was_jump: bool,
    /// AbstractHorse.playerJumpPendingScale, allowStandSliding; Camel.dashCooldown.
    pending_jump: f32,
    dash_cooldown: i32,
    /// ItemBasedSteering: boosting, boostTime, boostTimeTotal, and the last synced boost time.
    boost: (bool, i32, i32, i64),
    /// The seat's place this tick and last (the camera).
    pub seat: [f64; 3],
    pub prev_seat: [f64; 3],
    /// The first tick on a vehicle (nothing to interpolate from yet).
    fresh: bool,
}

impl Rider {
    pub fn new() -> Rider {
        Rider {
            vehicle: -1,
            kind: Kind::Other,
            pos: [0.0; 3],
            prev: [0.0; 3],
            yaw: 0.0,
            prev_yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            controlled: false,
            boat: crate::boat::Boat::new(),
            mount: Mover::new([0.0; 3]),
            jump_ticks: 0,
            jump_scale: 0.0,
            was_jump: false,
            pending_jump: 0.0,
            dash_cooldown: 0,
            boost: (false, 0, 0, 0),
            seat: [0.0; 3],
            prev_seat: [0.0; 3],
            fresh: true,
        }
    }

    /// Whether this client moves vehicle `e` of kind `k` that we ride as passenger `index`
    /// (Entity.isLocalInstanceAuthoritative / getControllingPassenger).
    fn controls(g: &Game, t: &EntityTable, e: &Entity, k: Kind, index: usize) -> bool {
        if index != 0 || e.dying() {
            return false;
        }
        let saddled = e.equipment[7].is_some();
        let item = |slot: usize| e_item_name(g, g.inventory_item(slot));
        match k {
            Kind::Boat | Kind::Raft | Kind::ChestBoat => true,
            Kind::Horse | Kind::Camel => saddled,
            // Pig/Strider.getControllingPassenger: saddled, the rider holding its stick.
            Kind::Pig | Kind::Strider => {
                let stick = if k == Kind::Pig { "carrot_on_a_stick" } else { "warped_fungus_on_a_stick" };
                saddled && (item(36 + g.selected as usize) == stick || item(45) == stick)
            }
            // HappyGhast.getControllingPassenger: a harnessed adult.
            Kind::HappyGhast => !t.is_baby(e) && e.equipment[6].and_then(|(i, _)| g.blocks.items.get(i as usize)).map_or(false, |n| n.ends_with("_harness")),
            _ => false,
        }
    }

    /// One tick on a vehicle, in place of the player's own movement: true while riding.
    pub fn tick(&mut self, g: &mut Game, t: &EntityTable, k: Keys, mover: &mut Mover) -> Result<bool, String> {
        let me = g.entity_id;
        let vid = g.riding.vehicle_of.get(&me).copied();
        let vid = match vid {
            Some(v) if g.entities.contains_key(&v) => v,
            Some(v) => {
                // The vehicle is gone (out of sight, removed): off it.
                g.riding.forget(v);
                g.riding.vehicle_of.remove(&me);
                -1
            }
            None => -1,
        };
        if vid < 0 {
            if self.vehicle >= 0 {
                self.dismount(g, t, mover);
            }
            g.riding.jump_bar = None;
            g.riding.vehicle_health = None;
            return Ok(false);
        }
        let e = &g.entities[&vid];
        let name = String::from(type_name(t, e));
        let kind = kind_of(&name);
        if self.vehicle != vid {
            self.vehicle = vid;
            self.kind = kind;
            self.pos = e.pos;
            self.prev = e.pos;
            self.yaw = vehicle_yaw(t, e);
            self.prev_yaw = self.yaw;
            self.pitch = e.pitch;
            self.on_ground = e.on_ground;
            self.boat = crate::boat::Boat::new();
            self.boat.raft = name.ends_with("_raft");
            self.jump_ticks = 0;
            self.jump_scale = 0.0;
            self.pending_jump = 0.0;
            self.boost = (false, 0, 0, e.d(17));
            self.fresh = true;
            let (w, h) = t.dims(e);
            self.mount = Mover::new(e.pos);
            self.mount.dims = Some((w as f64, h as f64));
            if crate::sys::env("PENRYN_DEBUG").is_some() {
                crate::say!("riding {} {} ({:?}), attributes {:?}", name, vid, kind, g.riding.attrs.get(&vid));
            }
        }
        let index = g.riding.passengers.get(&vid).and_then(|l| l.iter().position(|&p| p == me)).unwrap_or(0);
        let count = g.riding.passengers.get(&vid).map_or(1, |l| l.len());
        let controlled = Self::controls(g, t, &g.entities[&vid], kind, index);
        if controlled && !self.controlled {
            // Taking over: start from where the server has it.
            let e = &g.entities[&vid];
            self.pos = e.pos;
            self.prev = e.pos;
            self.mount.reset(e.pos);
            self.mount.vel = [0.0; 3];
        }
        self.controlled = controlled;
        self.prev = self.pos;
        self.prev_yaw = self.yaw;
        if let Some((p, yaw, pitch)) = g.riding.moved.take() {
            // ClientboundMoveVehiclePacket: the server's correction.
            if crate::sys::env("PENRYN_DEBUG").is_some() {
                crate::say!("server moved the vehicle from {:.2} {:.2} {:.2} to {:.2} {:.2} {:.2}", self.pos[0], self.pos[1], self.pos[2], p[0], p[1], p[2]);
            }
            self.pos = p;
            self.prev = p;
            self.yaw = yaw;
            self.pitch = pitch;
            self.mount.reset(p);
        }
        let mut delta_yaw = 0.0;
        if controlled {
            match kind {
                Kind::Boat | Kind::Raft | Kind::ChestBoat => {
                    self.boat.pos = self.pos;
                    self.boat.yaw = self.yaw;
                    let input = [k.left, k.right, k.forward, k.back];
                    let paddles = self.boat.tick(&g.world, &g.blocks, Some(input));
                    let mut o = Out::new();
                    o.bool(paddles[0]).bool(paddles[1]);
                    g.conn.send(S_PADDLE_BOAT, &o)?;
                    self.pos = self.boat.pos;
                    self.yaw = self.boat.yaw;
                    self.on_ground = self.boat.on_ground;
                    delta_yaw = self.boat.delta_rotation;
                }
                _ => self.mount_tick(g, t, vid, kind, k),
            }
            g.riding.driven = Some((vid, self.pos, self.yaw, self.pitch, self.on_ground));
        } else {
            g.riding.driven = None;
            let e = &g.entities[&vid];
            self.pos = e.pos;
            self.prev = e.prev;
            self.yaw = vehicle_yaw(t, e);
        }
        // The jump bar (LocalPlayer.aiStep) for a saddled horse or camel this client steers.
        self.jump_bar(g, kind, controlled, k)?;
        // Boats turn their rider with them and keep its look within 105 degrees.
        if matches!(kind, Kind::Boat | Kind::Raft | Kind::ChestBoat) {
            if controlled {
                g.player.yaw += delta_yaw;
            }
            let d = wrap_deg(g.player.yaw - self.yaw);
            let c = d.clamp(-105.0, 105.0);
            g.player.yaw += c - d;
        }
        // Entity.positionRider: the player's seat.
        let v = &g.entities[&vid];
        let animal = false;
        self.seat = seat(t, v, self.pos, self.yaw, index, count, PLAYER_VEHICLE_ATTACHMENT, animal);
        self.prev_seat = seat(t, v, self.prev, self.prev_yaw, index, count, PLAYER_VEHICLE_ATTACHMENT, animal);
        if self.fresh {
            self.prev_seat = self.seat;
            self.fresh = false;
        }
        mover.pos = self.seat;
        mover.vel = [0.0; 3];
        mover.on_ground = false;
        mover.fall_distance = 0.0;
        g.player.x = self.seat[0];
        g.player.y = self.seat[1];
        g.player.z = self.seat[2];
        // Gui.renderVehicleHealth: a living vehicle's hearts.
        let v = &g.entities[&vid];
        let max = g.riding.attrs.get(&vid).map(|a| a[3]).filter(|m| !m.is_nan());
        g.riding.vehicle_health = match (max, v.health) {
            (Some(m), h) if m > 0.0 => Some((h.unwrap_or(m as f32), m as f32)),
            (None, Some(h)) => Some((h, h)),
            _ => None,
        };
        if matches!(kind, Kind::Boat | Kind::Raft | Kind::ChestBoat | Kind::Minecart | Kind::Other) && v.health.is_none() {
            g.riding.vehicle_health = None;
        }
        Ok(true)
    }

    /// After the session's tick (entities' last positions taken): the vehicle this client moves
    /// is put where it went, and the other passengers on their seats.
    pub fn after_tick(&mut self, g: &mut Game, t: &EntityTable) {
        if self.controlled && self.vehicle >= 0 {
            if let Some(e) = g.entities.get_mut(&self.vehicle) {
                e.pos = self.pos;
                e.yaw = self.yaw;
                e.pitch = self.pitch;
                e.body_yaw = self.yaw;
                e.head_yaw = self.yaw;
                e.on_ground = self.on_ground;
            }
        }
        crate::boat::animate_paddles(g, t, if self.controlled { Some((self.vehicle, self.boat.paddling)) } else { None });
        seat_passengers(g, t);
    }

    /// The camera's feet, `partial` of the way into the tick, while riding.
    pub fn camera(&self, g: &Game, t: &EntityTable, partial: f64) -> Option<[f64; 3]> {
        if self.vehicle < 0 {
            return None;
        }
        let lerp = |a: [f64; 3], b: [f64; 3]| [a[0] + (b[0] - a[0]) * partial, a[1] + (b[1] - a[1]) * partial, a[2] + (b[2] - a[2]) * partial];
        if self.controlled {
            return Some(lerp(self.prev_seat, self.seat));
        }
        // A vehicle the server moves: its drawn place (entities lerp from their last tick).
        let v = g.entities.get(&self.vehicle)?;
        let me = g.entity_id;
        let list = g.riding.passengers.get(&self.vehicle);
        let index = list.and_then(|l| l.iter().position(|&p| p == me)).unwrap_or(0);
        let count = list.map_or(1, |l| l.len());
        Some(seat(t, v, lerp(v.prev, v.pos), vehicle_yaw(t, v), index, count, PLAYER_VEHICLE_ATTACHMENT, false))
    }

    /// LivingEntity.travelRidden for a mount: tickRidden (turn to the rider, the pending jump,
    /// the boost), then travel with getRiddenInput at getRiddenSpeed.
    fn mount_tick(&mut self, g: &mut Game, t: &EntityTable, vid: i32, kind: Kind, k: Keys) {
        let e = &g.entities[&vid];
        let attrs = g.riding.attrs.get(&vid).copied().unwrap_or([f64::NAN; 5]);
        let name = type_name(t, e);
        let (def_speed, def_jump, def_step) = match name {
            "horse" | "zombie_horse" => (0.225f32 as f64, 0.7, 1.0),
            "skeleton_horse" => (0.2f32 as f64, 0.7, 1.0),
            "donkey" | "mule" => (0.175f32 as f64, 0.5, 1.0),
            "camel" | "camel_husk" => (0.09f32 as f64, 0.42f32 as f64, 1.5),
            "pig" => (0.25, 0.42f32 as f64, 0.6),
            "strider" => (0.175f32 as f64, 0.42f32 as f64, 0.6),
            _ => (0.05, 0.42f32 as f64, 0.6),
        };
        let pick = |v: f64, d: f64| if v.is_nan() { d } else { v };
        let speed_attr = pick(attrs[0], def_speed);
        let jump_strength = pick(attrs[1], def_jump);
        self.mount.step = pick(attrs[2], def_step);
        // tickRidden: the mount turns to the rider's look (getRiddenRotation: pitch halved).
        self.yaw = g.player.yaw;
        self.pitch = g.player.pitch * 0.5;
        // The rider's xxa / zza (KeyboardInput, x 0.98).
        let zza = (k.forward as i32 - k.back as i32) as f32 * 0.98;
        let xxa = (k.left as i32 - k.right as i32) as f32 * 0.98;
        let (mut strafe, mut forward, speed);
        match kind {
            Kind::Pig | Kind::Strider => {
                // ItemBasedSteering: a boost starts when the server's boost time changes.
                let bt = e.d(17);
                if bt != self.boost.3 {
                    self.boost = (true, 0, bt as i32, bt);
                }
                if self.boost.0 {
                    self.boost.1 += 1;
                    if self.boost.1 > self.boost.2 {
                        self.boost.0 = false;
                    }
                }
                let factor = if self.boost.0 && self.boost.2 > 0 { 1.0 + 1.15 * msin((self.boost.1 as f32 / self.boost.2 as f32 * core::f32::consts::PI) as f64) } else { 1.0 };
                // Pig: 0.225 of its speed; Strider: 0.55 (0.35 shivering out of lava).
                let f = if kind == Kind::Pig { 0.225 } else if e.d(18) != 0 { 0.35 } else { 0.55 };
                speed = if kind == Kind::Pig { (speed_attr * f) as f32 * factor } else { speed_attr as f32 * f as f32 * factor };
                strafe = 0.0;
                forward = 1.0;
            }
            Kind::HappyGhast => {
                // HappyGhast flies where the rider looks (see happy_ghast below).
                self.ghast_tick(g, xxa, zza, k);
                return;
            }
            _ => {
                // AbstractHorse.getRiddenInput: half sideways, a quarter backwards; nothing while
                // rearing on the ground.
                strafe = xxa * 0.5;
                forward = if zza <= 0.0 { zza * 0.25 } else { zza };
                speed = if kind == Kind::Camel { speed_attr as f32 + if k.sprint && self.dash_cooldown == 0 { 0.1 } else { 0.0 } } else { speed_attr as f32 };
                if kind == Kind::Camel && e.d(19) < 0 {
                    // Sitting (refuseToMove): no input.
                    strafe = 0.0;
                    forward = 0.0;
                }
                // AbstractHorse.tickRidden: a pending jump goes off on the ground.
                if self.mount.on_ground && self.pending_jump > 0.0 {
                    let scale = self.pending_jump;
                    let jf = 1.0f32;
                    if kind == Kind::Camel {
                        // Camel.executeRidersJump: a dash along the look.
                        let d = (jump_strength as f32 * jf) as f64;
                        let yr = (self.yaw * (math::PI as f32 / 180.0)) as f64;
                        let (lx, lz) = (-msin(yr) as f64, mcos(yr) as f64);
                        let l = math::sqrt(lx * lx + lz * lz).max(1e-9);
                        let s = (22.2222f32 * scale) as f64 * speed_attr;
                        self.mount.vel[0] += lx / l * s;
                        self.mount.vel[1] += (1.4285f32 * scale) as f64 * d;
                        self.mount.vel[2] += lz / l * s;
                        self.dash_cooldown = 55;
                    } else {
                        // executeRidersJump: getJumpPower(scale) up, and forward 0.4 x scale.
                        self.mount.vel[1] = ((jump_strength as f32) * scale * jf) as f64;
                        if forward > 0.0 {
                            let yr = (self.yaw * 0.017453292f32) as f64;
                            self.mount.vel[0] += (-0.4f32 * msin(yr) * scale) as f64;
                            self.mount.vel[2] += (0.4f32 * mcos(yr) * scale) as f64;
                        }
                    }
                }
                if self.mount.on_ground {
                    self.pending_jump = 0.0;
                }
            }
        }
        if self.dash_cooldown > 0 {
            self.dash_cooldown -= 1;
        }
        let inp = Input {
            forward: forward as f64,
            strafe: strafe as f64,
            jump: false,
            sneak: false,
            shift: false,
            sprint: false,
            boots: false,
            move_speed: speed as f64,
            fly_speed: 0.05,
            jump_strength,
        };
        // getFlyingSpeed with a player in control: its speed x 0.1.
        self.mount.mount = Some((speed * 0.1) as f64);
        self.mount.pos = self.pos;
        let fluid = fluid_at(&g.world, &g.blocks, self.pos);
        if g.world.loaded(math::ifloor(self.pos[0]), math::ifloor(self.pos[2])) {
            self.mount.tick(&g.world, &g.blocks, &inp, self.yaw, false, fluid);
        }
        self.pos = self.mount.pos;
        self.on_ground = self.mount.on_ground;
    }

    /// HappyGhast.tickRidden / getRiddenInput / travel: it turns 8% of the way to the rider's
    /// yaw and flies along the look (forward tilted by the pitch, back at half, jump up 0.5),
    /// x 3.9 x its flying speed, moved at 5/3 of the flying speed with 0.91 friction (0.8 in
    /// water), no gravity.
    fn ghast_tick(&mut self, g: &mut Game, xxa: f32, zza: f32, k: Keys) {
        let fly = g.riding.attrs.get(&self.vehicle).map(|a| a[4]).filter(|v| !v.is_nan()).unwrap_or(0.05);
        self.yaw += wrap_deg(g.player.yaw - self.yaw) * 0.08;
        self.pitch = g.player.pitch * 0.5;
        let (mut f, mut up) = (0.0f32, 0.0f32);
        if zza != 0.0 {
            let pr = (g.player.pitch * 0.017453292f32) as f64;
            f = mcos(pr);
            up = -msin(pr);
            if zza < 0.0 {
                f *= -0.5;
                up *= -0.5;
            }
        }
        if k.jump {
            up += 0.5;
        }
        let scale = 3.9 * fly as f32;
        let input = [(xxa * scale) as f64, (up * scale) as f64, (f * scale) as f64];
        let in_water = fluid_at(&g.world, &g.blocks, self.pos) == crate::blocks::WATER;
        let speed = if in_water { 0.02 } else { (fly as f32 * 5.0 / 3.0) as f64 };
        // Entity.moveRelative / getInputVector (y unrotated).
        let l = input[0] * input[0] + input[1] * input[1] + input[2] * input[2];
        if l >= 1e-7 {
            let n = if l > 1.0 { math::sqrt(l) } else { 1.0 };
            let v = [input[0] / n * speed, input[1] / n * speed, input[2] / n * speed];
            let yr = (self.yaw * 0.017453292f32) as f64;
            let (sn, c) = (msin(yr) as f64, mcos(yr) as f64);
            self.mount.vel[0] += v[0] * c - v[2] * sn;
            self.mount.vel[1] += v[1];
            self.mount.vel[2] += v[2] * c + v[0] * sn;
        }
        self.mount.pos = self.pos;
        let d = self.mount.vel;
        self.mount.move_entity(&g.world, &g.blocks, d, &Input::idle());
        let fr = if in_water { 0.8f32 as f64 } else { 0.91f32 as f64 };
        self.mount.vel = [self.mount.vel[0] * fr, self.mount.vel[1] * fr, self.mount.vel[2] * fr];
        self.pos = self.mount.pos;
        self.on_ground = self.mount.on_ground;
    }

    /// LocalPlayer.aiStep's riding jump: holding jump fills the bar (0.1 a tick to 0.9, then
    /// it shrinks back towards 0.8), letting go sends START_RIDING_JUMP with the scale x 100
    /// and onPlayerJump sets the mount's pending jump.
    fn jump_bar(&mut self, g: &mut Game, kind: Kind, controlled: bool, k: Keys) -> Result<(), String> {
        let jumpable = controlled && matches!(kind, Kind::Horse | Kind::Camel);
        let was = self.was_jump;
        self.was_jump = k.jump;
        if !jumpable {
            self.jump_scale = 0.0;
            g.riding.jump_bar = None;
            return Ok(());
        }
        let cooldown = if kind == Kind::Camel { self.dash_cooldown } else { 0 };
        if cooldown == 0 {
            if self.jump_ticks < 0 {
                self.jump_ticks += 1;
                if self.jump_ticks == 0 {
                    self.jump_scale = 0.0;
                }
            }
            if was && !k.jump {
                self.jump_ticks = -10;
                let amount = math::floor((self.jump_scale * 100.0) as f64) as i32;
                // onPlayerJump (a camel dashes only from the ground, off cooldown).
                if kind != Kind::Camel || self.mount.on_ground {
                    self.pending_jump = pending_scale(amount.max(0));
                }
                let mut o = Out::new();
                o.varint(g.entity_id).varint(START_RIDING_JUMP).varint(amount);
                g.conn.send(crate::ids::play::S_PLAYER_COMMAND, &o)?;
            } else if !was && k.jump {
                self.jump_ticks = 0;
                self.jump_scale = 0.0;
            } else if was {
                self.jump_ticks += 1;
                self.jump_scale = bar_scale(self.jump_ticks);
            }
        } else {
            self.jump_scale = 0.0;
        }
        g.riding.jump_bar = Some((self.jump_scale, cooldown > 0));
        Ok(())
    }

    /// LivingEntity.stopRiding: dismountVehicle puts the player where the vehicle lets it off.
    fn dismount(&mut self, g: &mut Game, t: &EntityTable, mover: &mut Mover) {
        let vid = self.vehicle;
        self.vehicle = -1;
        self.controlled = false;
        g.riding.driven = None;
        let at = match g.entities.get(&vid) {
            Some(v) => dismount_location(g, t, v, self.kind, g.player.yaw),
            None => mover.pos,
        };
        mover.reset(at);
        g.player.x = at[0];
        g.player.y = at[1];
        g.player.z = at[2];
        self.seat = at;
        self.prev_seat = at;
        if crate::sys::env("PENRYN_DEBUG").is_some() {
            crate::say!("got off at {:.2} {:.2} {:.2}", at[0], at[1], at[2]);
        }
    }

    /// Eye height while riding (a passenger stands, unless sneaking).
    pub fn eye(&self, shift: bool) -> Option<f64> {
        if self.vehicle < 0 {
            None
        } else if shift {
            Some(physics::SNEAK_EYE)
        } else {
            Some(physics::EYE)
        }
    }
}

impl Game {
    /// The item in a slot of the player's inventory menu.
    pub fn inventory_item(&self, slot: usize) -> Option<i32> {
        self.inventory.get(slot).copied().flatten().map(|(i, _)| i)
    }
}

fn e_item_name(g: &Game, item: Option<i32>) -> &str {
    item.and_then(|i| g.blocks.items.get(i as usize)).map_or("", |s| s.as_str())
}

fn wrap_deg(a: f32) -> f32 {
    (math::wrap(a as f64 + 180.0, 360.0) - 180.0) as f32
}

/// The fluid kind where a mount stands (as play.rs's fluid_at: 0.4 above the feet).
fn fluid_at(world: &World, b: &Blocks, p: [f64; 3]) -> u8 {
    let k = b.kind(world.get(math::ifloor(p[0]), math::ifloor(p[1] + 0.4), math::ifloor(p[2])));
    if k == crate::blocks::WATER || k == crate::blocks::LAVA {
        k
    } else {
        0
    }
}

/// BlockGetter.getBlockFloorHeight: the top of the collision at `p`, or of the block under it
/// less one; -inf when neither has one.
fn floor_height(world: &World, b: &Blocks, x: i32, y: i32, z: i32) -> f64 {
    let top = |y: i32| -> Option<f64> {
        let a = physics::Aabb { lo: [x as f64 + 0.01, y as f64, z as f64 + 0.01], hi: [x as f64 + 0.99, y as f64 + 1.0, z as f64 + 0.99] };
        let bs = physics::boxes(world, b, &a);
        bs.iter()
            .filter(|o| o.lo[0] < a.hi[0] && o.hi[0] > a.lo[0] && o.lo[2] < a.hi[2] && o.hi[2] > a.lo[2] && o.lo[1] < y as f64 + 1.0 && o.hi[1] > y as f64 && o.hi[1] - o.lo[1] < 100.0)
            .map(|o| o.hi[1] - y as f64)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
    };
    match top(y) {
        Some(h) => h,
        None => match top(y - 1) {
            Some(h) if h >= 1.0 => h - 1.0,
            _ => f64::NEG_INFINITY,
        },
    }
}

/// getDismountLocationForPassenger, for the vehicle kinds that have their own.
pub fn dismount_location(g: &Game, t: &EntityTable, v: &Entity, kind: Kind, rider_yaw: f32) -> [f64; 3] {
    let (vw, vh) = t.dims(v);
    let on_top = [v.pos[0], v.pos[1] + vh as f64, v.pos[2]];
    let fits = |p: [f64; 3]| physics::fits(&g.world, &g.blocks, p, physics::HEIGHT) || physics::fits(&g.world, &g.blocks, p, physics::SNEAK_HEIGHT);
    match kind {
        Kind::Boat | Kind::Raft | Kind::ChestBoat => {
            // AbstractBoat: out to the side the rider faces (getCollisionHorizontalEscapeVector).
            let d = ((vw * core::f32::consts::SQRT_2) as f64 + 0.6f32 as f64 + 1.0e-5f32 as f64) / 2.0;
            let yr = (rider_yaw * 0.017453292f32) as f64;
            let (f, c) = (-msin(yr), mcos(yr));
            let h = f.abs().max(c.abs());
            let (x, z) = (v.pos[0] + f as f64 * d / h as f64, v.pos[2] + c as f64 * d / h as f64);
            let (bx, by, bz) = (math::ifloor(x), math::ifloor(v.pos[1] + vh as f64), math::ifloor(z));
            let below = g.world.get(bx, by - 1, bz);
            if g.blocks.kind(below) != crate::blocks::WATER && !g.blocks.has_fluid(below) {
                for y in [by, by - 1] {
                    let fl = floor_height(&g.world, &g.blocks, bx, y, bz);
                    if fl.is_finite() && fl < 1.0 {
                        let p = [x, y as f64 + fl, z];
                        if fits(p) {
                            return p;
                        }
                    }
                }
            }
            on_top
        }
        Kind::Horse | Kind::Camel | Kind::Pig | Kind::Llama | Kind::Minecart | Kind::Strider => {
            // DismountHelper.offsetsForDirection around the vehicle's facing (Animal,
            // AbstractHorse, AbstractMinecart): right, left, back right, back left, ...
            let yaw = if kind == Kind::Minecart { v.yaw } else { v.body_yaw };
            let facing = math::ifloor(yaw as f64 / 90.0 + 0.5);
            let (fx, fz) = [(0, 1), (-1, 0), (0, -1), (1, 0)][(facing & 3) as usize];
            let (rx, rz) = (-fz, fx);
            let offsets = [(rx, rz), (-rx, -rz), (rx - fx, rz - fz), (-rx - fx, -rz - fz), (fx + rx, fz + rz), (fx - rx, fz - rz), (-fx, -fz), (fx, fz)];
            let (bx, by, bz) = (math::ifloor(v.pos[0]), math::ifloor(v.pos[1]), math::ifloor(v.pos[2]));
            for (dx, dz) in offsets {
                let (x, z) = (bx + dx, bz + dz);
                for y in [by, by + 1, by - 1] {
                    let fl = floor_height(&g.world, &g.blocks, x, y, z);
                    if fl.is_finite() && fl < 1.0 {
                        let p = [x as f64 + 0.5, y as f64 + fl, z as f64 + 0.5];
                        if fits(p) {
                            return p;
                        }
                    }
                }
            }
            on_top
        }
        _ => on_top,
    }
}

/// Gui: the jump bar in place of the experience bar, the vehicle's hearts in place of food.
/// Returns (experience bar drawn, food drawn) so the HUD skips its own.
pub fn hud(gui: &mut crate::gui::Gui, g: &Game) -> (bool, bool) {
    let (w, h) = (gui.width, gui.height);
    let x = (math::floor((w / 2.0) as f64) as f32) - 91.0;
    let mut xp = false;
    if let Some((scale, cooldown)) = g.riding.jump_bar {
        // Gui.renderJumpMeter / JumpableVehicleBarRenderer.
        let by = h - 32.0 + 3.0;
        gui.blit("hud/jump_bar_background", x, by);
        if cooldown {
            gui.blit("hud/jump_bar_cooldown", x, by);
        } else {
            let fill = (scale * 183.0) as i32 as f32;
            if fill > 0.0 {
                gui.blit_part("hud/jump_bar_progress", x, by, 0.0, 0.0, fill, 5.0);
            }
        }
        xp = true;
    }
    let mut food = false;
    if let (Some((health, max)), true) = (g.riding.vehicle_health, g.game_mode == 0 || g.game_mode == 2) {
        // Gui.renderVehicleHealth: up to 30 hearts in rows of 10 from the right.
        let hearts = ((max + 0.5) as i32 / 2).clamp(1, 30);
        let health = math::floor(health as f64 + 0.999) as i32;
        let right = (math::floor((w / 2.0) as f64) as f32) + 91.0;
        let mut top = h - 39.0;
        let mut i = 0;
        while i < hearts {
            let row = (hearts - i).min(10);
            for j in 0..row {
                let fx = right - j as f32 * 8.0 - 9.0;
                let n = i + j;
                gui.blit("hud/heart/vehicle_container", fx, top);
                if n * 2 + 1 < health {
                    gui.blit("hud/heart/vehicle_full", fx, top);
                } else if n * 2 + 1 == health {
                    gui.blit("hud/heart/vehicle_half", fx, top);
                }
            }
            top -= 10.0;
            i += 10;
        }
        food = true;
    }
    (xp, food)
}

/// AbstractHorse.getPlayerJumpPendingScale.
fn pending_scale(a: i32) -> f32 {
    if a >= 90 {
        1.0
    } else {
        0.4 + 0.4 * a as f32 / 90.0
    }
}

/// The jump bar's fill after holding jump `ticks` ticks.
fn bar_scale(ticks: i32) -> f32 {
    if ticks < 10 {
        ticks as f32 * 0.1
    } else {
        0.8 + 2.0 / (ticks - 9) as f32 * 0.1
    }
}

/// Checks for `penryn selftest`: seats, the jump bar, LP vectors, a boat's float and turn.
pub fn selftest(b: &Blocks, w: &World) -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut v = Vec::new();
    // LocalPlayer.aiStep's bar: 0.1 a tick, 0.9 at 9 ticks, then 0.8 + 0.2 / (ticks - 9).
    let scales: Vec<String> = (1..=12).map(|t| format!("{:.2}", bar_scale(t))).collect();
    v.push(("riding jump bar", scales.join(" "), "0.10 0.20 0.30 0.40 0.50 0.60 0.70 0.80 0.90 1.00 0.90 0.87"));
    // getPlayerJumpPendingScale.

    v.push(("horse jump scale", format!("{:.4} {:.4} {:.4}", pending_scale(0), pending_scale(45), pending_scale(95)), "0.4000 0.6000 1.0000"));
    // Mth.sin's table.
    v.push(("Mth.sin table", format!("{:.7} {:.7}", msin(1.0), mcos(math::PI)), "0.8414514 -1.0000000"));
    // The LP vector codec round trip (client.rs writes, we read).
    let mut buf = Out::new();
    crate::client::lp_vec3_out(&mut buf, [0.5, -1.25, 3.0]);
    let bytes = buf.b;
    let mut rd = In::new(&bytes);
    let got = read_lp_vec3(&mut rd).unwrap_or([9.0; 3]);
    v.push(("LP vec3 read", format!("{:.3} {:.3} {:.3}", got[0], got[1], got[2]), "0.500 -1.250 3.000"));
    for (a, b2, c) in crate::boat::selftest(b, w) {
        v.push((a, b2, c));
    }
    v
}
