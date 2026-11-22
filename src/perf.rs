//! The per-stage frame timer behind PENRYN_PERF: where each frame's time goes.
//!
//! `PENRYN_PERF=1` times each stage on the CPU (what the driver takes to queue the work);
//! the GPU's own work then shows up in "swap", where the driver waits for it. `PENRYN_PERF=2`
//! also finishes the GL queue after every stage (glFinish), so each stage is charged its
//! GPU time too, at the price of a slower frame overall: use it to find what to fix, and 1 to
//! measure the frame rate. `PENRYN_PERF=gpu` instead measures each stage's GPU time with
//! timer queries (GL_EXT_timer_query, on Mac OS X 10.6 too), read back a few frames later so
//! nothing waits: the honest answer to "what does the card spend its time on".
//!
//! The timer is a few statics: Penryn draws from one thread, and the timer must be reachable
//! from every drawing module without threading a handle through them.

use crate::sdl::Video;
use alloc::string::String;
use core::ffi::{c_char, c_void};

#[derive(Clone, Copy)]
pub enum S {
    /// Window events (input).
    Events,
    /// Network packets.
    Net,
    /// Sound: starting sounds, feeding the music.
    Sound,
    /// Game ticks (20 a second) and everything else between frames.
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

const N: usize = 17;
const NAMES: [&str; N] = ["events", "net", "sound", "logic", "mesh", "sky", "world", "entities", "blockent", "clouds", "particles", "water", "weather", "hand", "gui", "swap", "sleep"];

static mut MODE: u8 = 0;
static mut LAST: f64 = 0.0;
static mut T: [f64; N] = [0.0; N];
/// This frame's time per stage, and the longest single frame's per stage since the report.
static mut CUR: [f64; N] = [0.0; N];
static mut WORST: [f64; N] = [0.0; N];
static mut FINISH: Option<unsafe extern "system" fn()> = None;

/// GPU timing: timer queries in a ring, each charged to its stage once the card has done it.
struct Gpu {
    begin: unsafe extern "system" fn(u32, u32),
    end: unsafe extern "system" fn(u32),
    get_iv: unsafe extern "system" fn(u32, u32, *mut i32),
    get_u64: unsafe extern "system" fn(u32, u32, *mut u64),
    ids: [u32; RING],
    stage: [u8; RING],
    /// Queries made and read so far (indices into the ring, modulo RING).
    made: usize,
    read: usize,
    t: [f64; N],
}

const RING: usize = 256;
const TIME_ELAPSED: u32 = 0x88BF;
const QUERY_RESULT: u32 = 0x8866;
const QUERY_RESULT_AVAILABLE: u32 = 0x8867;
static mut GPU: Option<Gpu> = None;

/// Reads PENRYN_PERF (0 off, 1 CPU times, 2 with glFinish after each stage, gpu: GPU times).
pub fn init(v: &Video) {
    let env = crate::sys::env("PENRYN_PERF");
    let m = match env.as_deref().map(|s| s.trim()) {
        Some("2") => 2,
        Some("gpu") => 3,
        Some(_) => 1,
        None => 0,
    };
    unsafe {
        MODE = m;
        FINISH = Some(v.gl.Finish);
        LAST = crate::sys::now();
        if m == 3 {
            let get = |n: &str| -> *mut c_void {
                for suffix in ["", "EXT", "ARB"] {
                    let p = (v.sdl.SDL_GL_GetProcAddress)(crate::sys::cstr(&alloc::format!("gl{}{}", n, suffix)).as_ptr() as *const c_char);
                    if !p.is_null() {
                        return p;
                    }
                }
                core::ptr::null_mut()
            };
            let f = [get("GenQueries"), get("BeginQuery"), get("EndQuery"), get("GetQueryObjectiv"), get("GetQueryObjectui64v")];
            if f.iter().any(|p| p.is_null()) {
                crate::say!("perf: no timer queries in this OpenGL; CPU times only");
                MODE = 1;
                return;
            }
            let gen: unsafe extern "system" fn(i32, *mut u32) = core::mem::transmute(f[0]);
            let mut g = Gpu {
                begin: core::mem::transmute(f[1]),
                end: core::mem::transmute(f[2]),
                get_iv: core::mem::transmute(f[3]),
                get_u64: core::mem::transmute(f[4]),
                ids: [0; RING],
                stage: [0; RING],
                made: 0,
                read: 0,
                t: [0.0; N],
            };
            gen(RING as i32, g.ids.as_mut_ptr());
            (g.begin)(TIME_ELAPSED, g.ids[0]);
            g.made = 0;
            GPU = Some(g);
        }
    }
}

/// Ends the running query (charged to `s`), starts the next, and collects finished ones.
unsafe fn gpu_lap(g: &mut Gpu, s: S) {
    (g.end)(TIME_ELAPSED);
    g.stage[g.made % RING] = s as u8;
    g.made += 1;
    // Collect what the card has finished; wait only if the ring is about to wrap.
    while g.read < g.made {
        let id = g.ids[g.read % RING];
        if g.made - g.read < RING - 1 {
            let mut ready = 0i32;
            (g.get_iv)(id, QUERY_RESULT_AVAILABLE, &mut ready);
            if ready == 0 {
                break;
            }
        }
        let mut ns = 0u64;
        (g.get_u64)(id, QUERY_RESULT, &mut ns);
        g.t[g.stage[g.read % RING] as usize] += ns as f64 / 1e9;
        g.read += 1;
    }
    (g.begin)(TIME_ELAPSED, g.ids[g.made % RING]);
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
        if let Some(g) = (*core::ptr::addr_of_mut!(GPU)).as_mut() {
            gpu_lap(g, s);
        }
        let t = crate::sys::now();
        T[s as usize] += t - LAST;
        CUR[s as usize] += t - LAST;
        LAST = t;
        // The frame's last stage: keep the worst of each.
        if s as usize == S::Sleep as usize {
            for i in 0..N {
                if CUR[i] > WORST[i] {
                    WORST[i] = CUR[i];
                }
                CUR[i] = 0.0;
            }
        }
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
        // The stage behind the slowest frame, when one stalled.
        let mut w = 0;
        for i in 0..N {
            if WORST[i] > WORST[w] {
                w = i;
            }
        }
        if WORST[w] > 0.008 {
            out.push_str(&alloc::format!("; stall: {} {:.0} ms", NAMES[w], WORST[w] * 1000.0));
        }
        for i in 0..N {
            WORST[i] = 0.0;
        }
        if let Some(g) = (*core::ptr::addr_of_mut!(GPU)).as_mut() {
            let total: f64 = g.t.iter().sum();
            out.push_str(&alloc::format!("; GPU {:.2} ms:", total * 1000.0 / f));
            for i in 0..N {
                let ms = g.t[i] * 1000.0 / f;
                if ms >= 0.005 {
                    out.push_str(&alloc::format!(" {} {:.2}", NAMES[i], ms));
                }
                g.t[i] = 0.0;
            }
        }
    }
    out
}
