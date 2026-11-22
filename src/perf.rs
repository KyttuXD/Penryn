//! The per-stage frame timer behind PENRYN_PERF: where each frame's time goes.
//!
//! `PENRYN_PERF=1` times each stage on the CPU (what the driver takes to queue the work);
//! the GPU's own work then shows up in "swap", where the driver waits for it. `PENRYN_PERF=2`
//! also finishes the GL queue after every stage (glFinish), so each stage is charged its
//! GPU time too, at the price of a slower frame overall: use it to find what to fix, and 1 to
//! measure the frame rate.
//!
//! The timer is a few statics: Penryn draws from one thread, and the timer must be reachable
//! from every drawing module without threading a handle through them.

use crate::sdl::Gl;
use alloc::string::String;

#[derive(Clone, Copy)]
pub enum S {
    /// Events, network packets, sound, game ticks.
    Logic,
    /// Meshing dirty sections and uploading them.
    Mesh,
    /// Clearing, the sky, the sun, moon and stars.
    Sky,
    /// The opaque and cut-out terrain.
    World,
    /// Mobs, players and their skins, dropped items.
    Entities,
    /// Signs, moving blocks and other block entities.
    BlockEntities,
    Clouds,
    Particles,
    /// The translucent terrain (water, stained glass, ice).
    Water,
    /// Rain, snow and lightning.
    Weather,
    /// The block outline, cracks and the hand.
    Hand,
    /// The HUD, chat, screens and text.
    Gui,
    /// Swapping buffers: with PENRYN_PERF=1 mostly the wait for the GPU.
    Swap,
    /// The frame rate limiter's sleep.
    Sleep,
}

const N: usize = 14;
const NAMES: [&str; N] = ["logic", "mesh", "sky", "world", "entities", "blockent", "clouds", "particles", "water", "weather", "hand", "gui", "swap", "sleep"];

static mut MODE: u8 = 0;
static mut LAST: f64 = 0.0;
static mut T: [f64; N] = [0.0; N];
static mut FINISH: Option<unsafe extern "system" fn()> = None;

/// Reads PENRYN_PERF (0 off, 1 CPU times, 2 with glFinish after each stage).
pub fn init(gl: &Gl) {
    let m = match crate::sys::env("PENRYN_PERF") {
        Some(s) if s.trim() == "2" => 2,
        Some(_) => 1,
        None => 0,
    };
    unsafe {
        MODE = m;
        FINISH = Some(gl.Finish);
        LAST = crate::sys::now();
    }
}

/// Charges the time since the previous lap to `s`.
#[inline]
pub fn lap(s: S) {
    unsafe {
        if MODE == 0 {
            return;
        }
        if MODE == 2 {
            if let Some(f) = FINISH {
                f();
            }
        }
        let t = crate::sys::now();
        T[s as usize] += t - LAST;
        LAST = t;
    }
}

/// The milliseconds per frame of each stage over the last `frames` frames, then starts over.
pub fn report(frames: u32) -> String {
    let mut out = String::new();
    let f = frames.max(1) as f64;
    unsafe {
        let mut total = 0.0;
        for i in 0..N {
            total += T[i];
        }
        for i in 0..N {
            let ms = T[i] * 1000.0 / f;
            if ms >= 0.005 {
                out.push_str(&alloc::format!(" {} {:.2}", NAMES[i], ms));
            }
            T[i] = 0.0;
        }
        out.insert_str(0, &alloc::format!("frame {:.2} ms:", total * 1000.0 / f));
    }
    out
}
