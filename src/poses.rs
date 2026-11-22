//! How riding and special movement look on models: the boat's renderer transform and its
//! rowing paddles (AbstractBoatRenderer, AbstractBoatModel.animatePaddle), the minecart's
//! (AbstractMinecartRenderer), and the player poses vanilla draws from the synced pose and
//! flags: sitting on a vehicle (HumanoidModel's riding legs and arms), crouching (the leaning
//! body and the 2/16 lower render offset), swimming and crawling (AvatarRenderer turning the
//! body flat along the look), gliding with an elytra, the riptide spin and sleeping.

use crate::entities::Entity;
use crate::math;

type M = [[f64; 4]; 3];

fn mul(a: &M, b: &M) -> M {
    let mut r = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..4 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + if j == 3 { a[i][3] } else { 0.0 };
        }
    }
    r
}

fn rot(axis: usize, a: f64) -> M {
    let (s, c) = (math::sin(a), math::cos(a));
    match axis {
        0 => [[1.0, 0.0, 0.0, 0.0], [0.0, c, -s, 0.0], [0.0, s, c, 0.0]],
        1 => [[c, 0.0, s, 0.0], [0.0, 1.0, 0.0, 0.0], [-s, 0.0, c, 0.0]],
        _ => [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
    }
}

fn tr(x: f64, y: f64, z: f64) -> M {
    [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z]]
}

fn sc(x: f64, y: f64, z: f64) -> M {
    [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0]]
}

/// Pose ids (Entity.DATA_POSE, index 6).
pub const FALL_FLYING: i64 = 1;
pub const SLEEPING: i64 = 2;
pub const SWIMMING: i64 = 3;
pub const SPIN_ATTACK: i64 = 4;
pub const CROUCHING: i64 = 5;

fn is_boat(name: &str) -> bool {
    name.ends_with("_boat") || name.ends_with("_raft")
}

/// The whole-model transform of a boat or a minecart (their renderers', not a living
/// entity's), down to model pixels. `at` is the entity's drawn place relative to the eye.
pub fn vehicle_base(name: &str, e: &Entity, at: [f64; 3]) -> Option<M> {
    if is_boat(name) {
        // AbstractBoatRenderer.submit: up 0.375, turned 180 - yaw, flipped, a quarter turn,
        // then pixels.
        let b = mul(&tr(at[0], at[1] + 0.375, at[2]), &rot(1, math::to_rad((180.0 - e.yaw) as f64)));
        let b = mul(&mul(&b, &sc(-1.0, -1.0, 1.0)), &rot(1, math::PI / 2.0));
        return Some(mul(&b, &sc(1.0 / 16.0, 1.0 / 16.0, 1.0 / 16.0)));
    }
    if name.ends_with("minecart") {
        // AbstractMinecartRenderer: up 0.375, turned 180 - yaw, tilted by the pitch, flipped.
        let mut b = mul(&tr(at[0], at[1] + 0.375, at[2]), &rot(1, math::to_rad((180.0 - e.yaw) as f64)));
        b = mul(&b, &rot(2, math::to_rad(-e.pitch as f64)));
        b = mul(&b, &sc(-1.0, -1.0, 1.0));
        return Some(mul(&b, &sc(1.0 / 16.0, 1.0 / 16.0, 1.0 / 16.0)));
    }
    None
}

fn avatar(name: &str) -> bool {
    matches!(name, "player" | "mannequin")
}

/// AvatarRenderer.getRenderOffset (crouching: 2/16 down) and setupRotations (swimming,
/// gliding, the riptide spin, sleeping), applied after the body's turn. `base` is the model's
/// transform so far (its place and body yaw).
pub fn setup_rotations(name: &str, e: &Entity, partial: f32, mut base: M) -> M {
    if !avatar(name) || e.death_time > 0 {
        return base;
    }
    let pose = e.d(6);
    let riding = e.data.contains_key(&crate::riding::D_RIDING);
    match pose {
        CROUCHING if !riding => base[1][3] -= 0.125,
        SWIMMING => {
            // In water the body lies along the look; out of it (crawling) flat.
            let in_water = e.d(0) & 0x10 != 0;
            let x = if in_water { -90.0 - e.pitch as f64 } else { -90.0 };
            base = mul(&base, &rot(0, math::to_rad(x)));
            base = mul(&base, &tr(0.0, -1.0, 0.3));
        }
        FALL_FLYING => {
            // fallFlyTicks squared over 100, up to a full lean along the look.
            let ticks = e.data.get(&crate::riding::D_FLY_TICKS).copied().unwrap_or(0) as f64;
            let t = (ticks + partial as f64).min(10.0);
            let g = (t * t / 100.0).clamp(0.0, 1.0);
            base = mul(&base, &rot(0, math::to_rad(g * (-90.0 - e.pitch as f64))));
        }
        SPIN_ATTACK => {
            // The riptide spin: lie along the look and spin 72 degrees a tick.
            base = mul(&base, &rot(0, math::to_rad(-90.0 - e.pitch as f64)));
            let ticks = e.data.get(&crate::riding::D_FLY_TICKS).copied().unwrap_or(0) as f64;
            base = mul(&base, &rot(1, math::to_rad((ticks + partial as f64) * -72.0)));
        }
        SLEEPING => {
            // Lying in the bed: turned on the back along its body yaw.
            base = mul(&base, &rot(2, math::to_rad(90.0)));
            base = mul(&base, &rot(1, math::to_rad(270.0)));
            base = mul(&base, &tr(0.0, -0.1, 0.0));
        }
        _ => {}
    }
    base
}

/// A part's pose: the boat's paddles rowing; a humanoid sitting on a vehicle or crouching
/// (HumanoidModel.setupAnim's riding and crouching blocks). `pos` is the part's offset
/// (pixels), `rot` its x, y and z turns.
pub fn part(name: &str, e: &Entity, partial: f32, part: &str, pos: &mut [f64; 3], rot: &mut [f64; 3]) {
    if is_boat(name) {
        let side = match part {
            "left_paddle" => 0,
            "right_paddle" => 1,
            _ => return,
        };
        // getRowingTime: the phase partial ticks on from the last tick's (PI/8 behind).
        let key = if side == 0 { crate::riding::D_PADDLE_LEFT } else { crate::riding::D_PADDLE_RIGHT };
        let phase = f32::from_bits(e.data.get(&key).copied().unwrap_or(0) as u32);
        let t = if phase > 0.0 { phase - core::f32::consts::PI / 8.0 + partial.clamp(0.0, 1.0) * core::f32::consts::PI / 8.0 } else { 0.0 };
        let (x, y) = crate::boat::paddle_angles(t, side);
        rot[0] = x as f64;
        rot[1] = y as f64;
        return;
    }
    if !avatar(name) && !matches!(name, "zombie" | "husk" | "drowned" | "skeleton" | "stray" | "bogged" | "wither_skeleton" | "piglin" | "zombified_piglin" | "zombie_villager" | "piglin_brute" | "villager" | "pillager" | "vindicator" | "evoker") {
        return;
    }
    let riding = e.data.contains_key(&crate::riding::D_RIDING);
    if riding {
        match part {
            "right_arm" | "left_arm" => rot[0] += -math::PI / 5.0,
            "right_leg" => *rot = [-1.4137167, math::PI / 10.0, 0.07853982],
            "left_leg" => *rot = [-1.4137167, -math::PI / 10.0, -0.07853982],
            _ => {}
        }
        return;
    }
    if avatar(name) && e.d(6) == CROUCHING {
        match part {
            "body" => {
                rot[0] = 0.5;
                pos[1] += 3.2;
            }
            "right_arm" | "left_arm" => {
                rot[0] += 0.4;
                pos[1] += 3.2;
            }
            "right_leg" | "left_leg" => {
                pos[2] += 4.0;
                pos[1] += 0.2;
            }
            "head" => pos[1] += 4.2,
            _ => {}
        }
    }
}

/// Checks: a swimmer's and a glider's body lie along the look, a sleeper flat, a crouching
/// player sits 2/16 lower; a rider's legs reach forward; a rowing paddle turns.
pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    let mut v = alloc::vec::Vec::new();
    let id: M = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let up = |m: &M| [m[0][1], m[1][1], m[2][1]];
    let mut e = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    e.data.insert(6, SWIMMING);
    e.data.insert(0, 0x10);
    let m = setup_rotations("player", &e, 0.0, id);
    let u = up(&m);
    v.push(("swimming body", format!("{:.2} {:.2} {:.2}", u[0], u[1], u[2]), "0.00 0.00 -1.00"));
    e.data.insert(6, CROUCHING);
    let m = setup_rotations("player", &e, 0.0, id);
    v.push(("crouching offset", format!("{:.3}", m[1][3]), "-0.125"));
    e.data.insert(6, FALL_FLYING);
    e.data.insert(crate::riding::D_FLY_TICKS, 20);
    e.pitch = 0.0;
    let m = setup_rotations("player", &e, 0.0, id);
    let u = up(&m);
    v.push(("gliding body", format!("{:.2} {:.2} {:.2}", u[0], u[1], u[2]), "0.00 0.00 -1.00"));
    e.data.insert(6, 0);
    e.data.insert(crate::riding::D_RIDING, 1);
    let (mut pos, mut r) = ([0.0; 3], [0.0; 3]);
    part("player", &e, 0.0, "right_leg", &mut pos, &mut r);
    v.push(("riding legs", format!("{:.4} {:.4} {:.4}", r[0], r[1], r[2]), "-1.4137 0.3142 0.0785"));
    let mut b = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    b.data.insert(crate::riding::D_PADDLE_LEFT, (core::f32::consts::PI / 2.0).to_bits() as i64);
    let mut r = [0.0; 3];
    part("oak_boat", &b, 1.0, "left_paddle", &mut pos, &mut r);
    v.push(("rowing paddle", format!("{:.4} {:.4}", r[0], r[1]), "-1.0472 -0.4243"));
    v
}
