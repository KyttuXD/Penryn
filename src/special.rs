//! The player's special movement as vanilla's client runs it: swimming (Entity.updateSwimming:
//! sprinting under water starts it, it lasts while sprinting in water; Player.travel steers the
//! swimmer up and down along the look), the pose and its box (Player.updatePlayerPose:
//! swimming, gliding and the riptide spin are 0.6 high with the eyes at 0.4; where neither
//! standing nor crouching fits, the player crawls), elytra gliding (LocalPlayer.aiStep's
//! tryToStartFallFlying with START_FALL_FLYING; LivingEntity.travelFallFlying's
//! updateFallFlyingMovement; the server ends the glide through the synced flag), firework
//! rockets attached to the gliding player (FireworkRocketEntity.tick's boost along the look)
//! and the trident's riptide (TridentItem.releaseUsing's push, the spin attack's 20 ticks).

use crate::boat::water_height;
use crate::client::Game;
use crate::math;
use crate::physics::{self, Input, Mover};
use crate::poses::{CROUCHING, FALL_FLYING, SPIN_ATTACK, SWIMMING};
use alloc::string::String;
use alloc::vec::Vec;

pub const STANDING: i64 = 0;
/// The swimming, gliding and spinning box and eye height (Avatar.POSES).
pub const SMALL: f64 = 0.6;
pub const SMALL_EYE: f64 = 0.4;

pub struct Special {
    pub swimming: bool,
    pub fall_flying: bool,
    /// The pose this tick (Pose ids as poses.rs has them).
    pub pose: i64,
    /// Touching water (wasTouchingWater) and the eyes in it (wasEyeInWater).
    pub in_water: bool,
    pub under_water: bool,
    was_jump: bool,
    /// LivingEntity.autoSpinAttackTicks (the riptide spin).
    pub spin_ticks: i32,
    /// The server's glide flag last seen, to tell when it ends the glide.
    server_glide: bool,
    water_base: Option<u16>,
    /// The enchantment registry's riptide entry (found once the registries are in).
    riptide_id: Option<i32>,
}

impl Special {
    pub fn new() -> Special {
        Special {
            swimming: false,
            fall_flying: false,
            pose: STANDING,
            in_water: false,
            under_water: false,
            was_jump: false,
            spin_ticks: 0,
            server_glide: false,
            water_base: None,
            riptide_id: None,
        }
    }

    /// The eye height of the pose (None: the caller's standing or crouching one).
    pub fn eye(&self) -> Option<f64> {
        match self.pose {
            SWIMMING | FALL_FLYING | SPIN_ATTACK => Some(SMALL_EYE),
            // Crouching where standing does not fit, sneak key up or not.
            CROUCHING => Some(physics::SNEAK_EYE),
            _ => None,
        }
    }

    /// The box is the small one (swimming, crawling, gliding, spinning).
    pub fn small(&self) -> bool {
        matches!(self.pose, SWIMMING | FALL_FLYING | SPIN_ATTACK)
    }

    /// Entity.updateInWaterStateAndDoFluidPushing / updateFluidOnEyes for the player box.
    fn water_state(&mut self, g: &Game, p: [f64; 3], eye: f64, h: f64) {
        let base = *self.water_base.get_or_insert_with(|| first_water(&g.blocks));
        let r = physics::WIDTH / 2.0 - 0.001;
        let (lo, hi) = ([p[0] - r, p[1] + 0.001, p[2] - r], [p[0] + r, p[1] + h - 0.001, p[2] + r]);
        let mut touching = false;
        'out: for x in math::ifloor(lo[0])..=math::ifloor(hi[0]) {
            for y in math::ifloor(lo[1])..=math::ifloor(hi[1]) {
                for z in math::ifloor(lo[2])..=math::ifloor(hi[2]) {
                    if let Some((wh, _)) = water_height(&g.world, &g.blocks, base, x, y, z) {
                        if y as f64 + wh as f64 >= lo[1] {
                            touching = true;
                            break 'out;
                        }
                    }
                }
            }
        }
        self.in_water = touching;
        let ey = p[1] + eye - 0.11111111f32 as f64;
        let (bx, by, bz) = (math::ifloor(p[0]), math::ifloor(ey), math::ifloor(p[2]));
        self.under_water = water_height(&g.world, &g.blocks, base, bx, by, bz).map_or(false, |(wh, _)| by as f64 + wh as f64 > ey);
    }

    /// LocalPlayer.isSprintingPossible's shallow water rule: in water a sprint needs the eyes
    /// under (or a swim already going); and shouldStopSwimSprinting.
    pub fn sprint_allowed(&self, sprinting_now: bool, forward: bool) -> bool {
        if self.in_water && !self.under_water && !self.swimming {
            return false;
        }
        if self.swimming && sprinting_now && (!self.in_water || !forward) {
            return false;
        }
        true
    }

    /// Before the player's travel: swimming, the pose and its box, starting a glide (jump in
    /// the air with an elytra on), the server ending one, the swimmer's vertical steering.
    #[allow(clippy::too_many_arguments)]
    pub fn before(&mut self, g: &mut Game, mover: &mut Mover, inp: &mut Input, jump_key: bool, shift_key: bool, riding: bool) -> Result<(), String> {
        let p = mover.pos;
        let h = if self.small() { SMALL } else if inp.sneak { physics::SNEAK_HEIGHT } else { physics::HEIGHT };
        let eye = self.eye().unwrap_or(if inp.sneak { physics::SNEAK_EYE } else { physics::EYE });
        self.water_state(g, p, eye, h);
        // Player.updateSwimming (Entity.updateSwimming).
        if g.flying || riding {
            self.swimming = false;
        } else if self.swimming {
            self.swimming = inp.sprint && self.in_water;
        } else {
            let feet = g.world.get(math::ifloor(p[0]), math::ifloor(p[1]), math::ifloor(p[2]));
            let water_here = feet != 0 && (g.blocks.kind(feet) == crate::blocks::WATER || g.blocks.has_fluid(feet));
            self.swimming = inp.sprint && self.under_water && water_here;
        }
        // The server's glide flag (shared flag 7): it ends a glide, and confirms one.
        if let Some(f) = g.riding.own_flags {
            let on = f & 0x80 != 0;
            if on != self.server_glide {
                self.server_glide = on;
                self.fall_flying = on;
            }
        }
        // LocalPlayer.aiStep: jump pressed in the air (not on a ladder) starts a glide.
        let pressed = jump_key && !self.was_jump;
        self.was_jump = jump_key;
        let elytra = g.inventory[6].and_then(|(i, _)| g.blocks.items.get(i as usize)).map_or(false, |n| n == "elytra");
        let fx = math::ifloor(p[0]);
        let climbing = climbable(g, fx, math::ifloor(p[1]), math::ifloor(p[2]));
        if pressed && !riding && !mover.on_ground && !g.flying && !self.fall_flying && elytra && !self.in_water && mover.effects.levitation.is_none() && !climbing {
            self.fall_flying = true;
            let mut o = crate::wire::Out::new();
            o.varint(g.entity_id).varint(crate::riding::START_FALL_FLYING).varint(0);
            g.conn.send(crate::ids::play::S_PLAYER_COMMAND, &o)?;
        }
        // Riding, or landed without the server ever taking the glide up.
        if riding || (mover.on_ground && !self.server_glide) {
            self.fall_flying = false;
        }
        if self.spin_ticks > 0 {
            self.spin_ticks -= 1;
        }
        // Player.updatePlayerPose: the wanted pose, else crouching, else crawling.
        let desired = if self.swimming {
            SWIMMING
        } else if self.fall_flying {
            FALL_FLYING
        } else if self.spin_ticks > 0 {
            SPIN_ATTACK
        } else if shift_key && !g.flying {
            CROUCHING
        } else {
            STANDING
        };
        let fits = |h: f64| physics::fits(&g.world, &g.blocks, p, h);
        let height_of = |pose: i64| match pose {
            SWIMMING | FALL_FLYING | SPIN_ATTACK => SMALL,
            CROUCHING => physics::SNEAK_HEIGHT,
            _ => physics::HEIGHT,
        };
        self.pose = if riding || g.game_mode == 3 || fits(height_of(desired)) {
            desired
        } else if fits(physics::SNEAK_HEIGHT) {
            CROUCHING
        } else {
            SWIMMING
        };
        if riding {
            self.pose = if shift_key { CROUCHING } else { STANDING };
        }
        mover.dims = if self.small() { Some((physics::WIDTH, SMALL)) } else { None };
        // Crouching, and crawling (slow as sneaking: LocalPlayer.isMovingSlowly's
        // isVisuallyCrawling); the box stays the small one.
        if self.pose == CROUCHING || (self.pose == SWIMMING && !self.in_water) {
            inp.sneak = true;
        }
        // Player.travel: a swimmer goes up or down along the look.
        if self.swimming && !riding {
            let look_y = -math::sin(math::to_rad(g.player.pitch as f64));
            let blend = if look_y < -0.2 { 0.085 } else { 0.06 };
            let base = self.water_base.unwrap_or(0);
            let surface = water_height(&g.world, &g.blocks, base, math::ifloor(p[0]), math::ifloor(p[1] + 1.0 - 0.1), math::ifloor(p[2])).is_some();
            if look_y <= 0.0 || inp.jump || surface {
                mover.vel[1] += (look_y - mover.vel[1]) * blend;
            }
        }
        // A knockback the server sent (ClientboundSetEntityMotion).
        if let Some(v) = g.riding.motion.take() {
            mover.vel = v;
        }
        Ok(())
    }

    /// LivingEntity.travel while gliding (travelFallFlying), with the attached rockets' boost.
    /// False when the normal travel should run (not gliding, or in a fluid).
    pub fn travel(&mut self, g: &Game, mover: &mut Mover, inp: &Input, fluid: u8, rocket_kind: i32) -> bool {
        if !self.fall_flying {
            return false;
        }
        // FireworkRocketEntity.tick: a rocket attached to a glider pushes it along its look.
        let me = g.entity_id;
        let look = look(g.player.yaw, g.player.pitch);
        for e in g.entities.values() {
            // DATA_ATTACHED_TO_TARGET (an OptionalInt: id + 1).
            if e.kind == rocket_kind && e.d(9) - 1 == me as i64 {
                let v = mover.vel;
                mover.vel = [
                    v[0] + look[0] * 0.1 + (look[0] * 1.5 - v[0]) * 0.5,
                    v[1] + look[1] * 0.1 + (look[1] * 1.5 - v[1]) * 0.5,
                    v[2] + look[2] * 0.1 + (look[2] * 1.5 - v[2]) * 0.5,
                ];
            }
        }
        if fluid != 0 {
            return false;
        }
        let p = mover.pos;
        if climbable(g, math::ifloor(p[0]), math::ifloor(p[1]), math::ifloor(p[2])) {
            self.fall_flying = false;
            return false;
        }
        // Entity.checkFallDistanceAccumulation: a glide not diving keeps the fall at 1.
        if mover.vel[1] > -0.5 && mover.fall_distance > 1.0 {
            mover.fall_distance = 1.0;
        }
        let g_eff = if mover.vel[1] <= 0.0 && mover.effects.slow_falling { 0.01 } else { 0.08 };
        mover.vel = fall_flying_movement(mover.vel, g.player.pitch, g.player.yaw, g_eff);
        let d = mover.vel;
        mover.move_entity(&g.world, &g.blocks, d, inp);
        true
    }

    /// TridentItem.releaseUsing on the client: a riptide trident held 10 ticks or more, let go
    /// in water or rain, throws the player along the look and starts the spin.
    pub fn release_use(&mut self, g: &Game, mover: &mut Mover, ticks: u32, raining: bool) {
        let slot = 36 + g.selected as usize;
        let Some((item, _)) = g.inventory[slot] else { return };
        if g.blocks.items.get(item as usize).map(|s| s.as_str()) != Some("trident") || ticks < 10 {
            return;
        }
        if self.riptide_id.is_none() {
            self.riptide_id = g.creative.registries.iter().find(|(n, _)| n.ends_with("enchantment")).and_then(|(_, l)| l.iter().position(|e| e.trim_start_matches("minecraft:") == "riptide")).map(|i| i as i32);
        }
        let Some(rid) = self.riptide_id else { return };
        let level = enchantment_level(&g.creative.patches[slot], rid);
        if level <= 0 || !(self.in_water || raining) {
            return;
        }
        // EnchantmentHelper.getTridentSpinAttackStrength: 1.5 + 0.75 a level after the first.
        let f = 1.5 + 0.75 * (level - 1) as f32;
        let (yr, xr) = (g.player.yaw * 0.017453292f32, g.player.pitch * 0.017453292f32);
        let (sy, cy, sx, cx) = (crate::riding::msin(yr as f64), crate::riding::mcos(yr as f64), crate::riding::msin(xr as f64), crate::riding::mcos(xr as f64));
        let (mut gx, mut hy, mut kz) = (-sy * cx, -sx, cy * cx);
        let l = math::sqrtf(gx * gx + hy * hy + kz * kz);
        gx *= f / l;
        hy *= f / l;
        kz *= f / l;
        mover.vel[0] += gx as f64;
        mover.vel[1] += hy as f64;
        mover.vel[2] += kz as f64;
        self.spin_ticks = 20;
        if mover.on_ground {
            mover.move_entity(&g.world, &g.blocks, [0.0, 1.1999999284744263, 0.0], &Input::idle());
        }
    }
}

/// The view vector (Entity.calculateViewVector) in Mth's sine table.
fn look(yaw: f32, pitch: f32) -> [f64; 3] {
    let f = pitch * 0.017453292f32;
    let g = -yaw * 0.017453292f32;
    let (h, i) = (crate::riding::mcos(g as f64), crate::riding::msin(g as f64));
    let (j, k) = (crate::riding::mcos(f as f64), crate::riding::msin(f as f64));
    [(i * j) as f64, -k as f64, (h * j) as f64]
}

/// LivingEntity.updateFallFlyingMovement.
pub fn fall_flying_movement(v: [f64; 3], pitch: f32, yaw: f32, gravity: f64) -> [f64; 3] {
    let lk = look(yaw, pitch);
    let lean = (pitch * 0.017453292f32) as f64;
    let look_h = math::sqrt(lk[0] * lk[0] + lk[2] * lk[2]);
    let move_h = math::sqrt(v[0] * v[0] + v[2] * v[2]);
    let c = math::cos(lean);
    let lift = c * c;
    let mut m = [v[0], v[1] + gravity * (-1.0 + lift * 0.75), v[2]];
    if m[1] < 0.0 && look_h > 0.0 {
        let convert = m[1] * -0.1 * lift;
        m = [m[0] + lk[0] * convert / look_h, m[1] + convert, m[2] + lk[2] * convert / look_h];
    }
    if lean < 0.0 && look_h > 0.0 {
        let convert = move_h * (-crate::riding::msin(lean)) as f64 * 0.04;
        m = [m[0] - lk[0] * convert / look_h, m[1] + convert * 3.2, m[2] - lk[2] * convert / look_h];
    }
    if look_h > 0.0 {
        m = [m[0] + (lk[0] / look_h * move_h - m[0]) * 0.1, m[1], m[2] + (lk[2] / look_h * move_h - m[2]) * 0.1];
    }
    [m[0] * 0.99f32 as f64, m[1] * 0.98f32 as f64, m[2] * 0.99f32 as f64]
}

fn first_water(b: &crate::blocks::Blocks) -> u16 {
    for (i, s) in b.states.iter().enumerate() {
        if b.blocks.get(s.block as usize).map_or(false, |k| k.name == "water") {
            return i as u16;
        }
    }
    0
}

fn climbable(g: &Game, x: i32, y: i32, z: i32) -> bool {
    let s = g.world.get(x, y, z);
    s != 0 && matches!(g.blocks.of_state(s).name.as_str(), "ladder" | "vine" | "scaffolding" | "twisting_vines" | "twisting_vines_plant" | "weeping_vines" | "weeping_vines_plant" | "cave_vines" | "cave_vines_plant")
}

/// The level of enchantment `id` in a slot's component patch (creative.rs's delimited form:
/// the counts, then each added component's type, length and value; minecraft:enchantments
/// is type 13: a list of (enchantment, level)).
pub fn enchantment_level(patch: &[u8], id: i32) -> i32 {
    let mut r = crate::wire::In::new(patch);
    let (Some(add), Some(_)) = (r.varint(), r.varint()) else { return 0 };
    for _ in 0..add.max(0) {
        let (Some(kind), Some(len)) = (r.varint(), r.varint()) else { return 0 };
        let Some(v) = r.take(len.max(0) as usize) else { return 0 };
        if kind == 13 {
            let mut e = crate::wire::In::new(v);
            let n = e.varint().unwrap_or(0);
            for _ in 0..n.clamp(0, 64) {
                let (Some(ench), Some(level)) = (e.varint(), e.varint()) else { break };
                if ench == id {
                    return level;
                }
            }
        }
    }
    0
}

/// Checks: a glide's first ticks, the enchantment reader.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use alloc::format;
    let mut v = Vec::new();
    // Gliding level from rest: gravity x (-1 + 0.75) less the lift's conversion.
    let m = fall_flying_movement([0.0, 0.0, 0.0], 0.0, 0.0, 0.08);
    v.push(("glide from rest", format!("{:.5} {:.5} {:.5}", m[0], m[1], m[2]), "0.00000 -0.01764 0.00178"));
    // Diving at 30 degrees from 1 block a tick south: speed taken from the fall.
    let m = fall_flying_movement([0.0, -0.5, 1.0], 30.0, 0.0, 0.08);
    v.push(("glide diving", format!("{:.4} {:.4} {:.4}", m[0], m[1], m[2]), "0.0000 -0.4850 1.0258"));
    // An enchantments component with riptide (id 30) III and loyalty (id 20) I.
    let mut val = crate::wire::Out::new();
    val.varint(2).varint(20).varint(1).varint(30).varint(3);
    let mut p = crate::wire::Out::new();
    p.varint(1).varint(0).varint(13).varint(val.b.len() as i32).bytes(&val.b);
    v.push(("riptide level", format!("{} {}", enchantment_level(&p.b, 30), enchantment_level(&p.b, 7)), "3 0"));
    v
}
