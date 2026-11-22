//! The game modes' differences on the client: spectators fly through blocks
//! (Entity.noPhysics, Player.travel's flying), adventure players break only what their item's
//! can_break allows (tooltip.rs has the predicates), and which HUD parts each mode shows.

use crate::math;
use crate::physics::{Input, Mover};

pub const SPECTATOR: i32 = 3;

/// One tick of a spectator's movement: LocalPlayer.aiStep's vertical flight (3x the flying
/// speed), Player.travel's flying travel, Entity.move with noPhysics (no collisions, never on
/// the ground), then the flying drag.
pub fn spectator_tick(m: &mut Mover, inp: &Input, yaw: f32) {
    let v = (inp.fly_speed * 3.0) as f64;
    if inp.jump {
        m.vel[1] += v;
    }
    if inp.shift {
        m.vel[1] -= v;
    }
    let vy = m.vel[1];
    let speed = (if inp.sprint { inp.fly_speed * 2.0 } else { inp.fly_speed }) as f64;
    // Entity.moveRelative (getInputVector).
    let (mut s, mut f) = (inp.strafe, inp.forward);
    let l = s * s + f * f;
    if l >= 1e-7 {
        if l > 1.0 {
            let n = math::sqrt(l);
            s /= n;
            f /= n;
        }
        s *= speed;
        f *= speed;
        let y = math::to_rad(yaw as f64);
        let (sin, cos) = (math::sin(y), math::cos(y));
        m.vel[0] += s * cos - f * sin;
        m.vel[2] += f * cos + s * sin;
    }
    for a in 0..3 {
        m.pos[a] += m.vel[a];
    }
    m.vel = [m.vel[0] * 0.91f32 as f64, vy * 0.6, m.vel[2] * 0.91f32 as f64];
    m.on_ground = false;
    m.horizontal_collision = false;
    m.fall_distance = 0.0;
}

pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    let mut out = alloc::vec::Vec::new();
    let mut m = Mover::new([0.0, 64.0, 0.0]);
    let inp = Input { forward: 0.98, jump: true, ..Input::idle() };
    for _ in 0..20 {
        spectator_tick(&mut m, &inp, 0.0);
    }
    // Flying forward (south at yaw 0) and up, nothing in the way.
    out.push(("spectator flight", alloc::format!("{:.2} {:.2} {:.2} {}", m.pos[0], m.pos[1], m.pos[2], m.on_ground), "0.00 70.94 6.22 false"));
    out
}
