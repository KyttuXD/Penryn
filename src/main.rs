//! penryn: a small Minecraft Java Edition 26.3 client for Mac OS X 10.6 and later
//! (and Linux, where it is developed and tested). See README.md.
#![no_std]
#![no_main]

extern crate alloc;

mod account;
mod audio;
mod blocks;
mod books;
mod boat;
mod font;
mod gui;
mod hand;
mod chatui;
mod client;
mod commands;
mod conn;
mod creative;
mod crypto;
mod effects;
mod entities;
mod http;
mod ids;
mod json;
mod lang;
mod maps;
mod math;
mod md5;
mod menus;
mod moving;
mod mobs;
mod multiplayer;
mod nbt;
mod physics;
mod play;
mod poses;
mod render;
mod riding;
mod screens;
mod sdl;
mod servers;
mod serverhud;
mod serverui;
mod gamemode;
mod tooltip;
mod skins;
mod signs;
mod sky;
mod special;
mod net;
mod options;
mod packs;
mod particles;
mod sys;
mod task;
mod title;
mod talk;
mod text;
mod weather;
mod wire;
mod world;
mod zlib;

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int};

#[global_allocator]
static ALLOC: sys::Malloc = sys::Malloc;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    sys::eprint(&alloc::format!("penryn crashed: {}\n", info));
    unsafe { sys::abort() }
}

// Mac OS X 10.6 starts a program at a raw entry point (LC_UNIXTHREAD) with argc and argv on
// the stack; LC_MAIN only arrived in 10.8. scripts/build-mac-10.6.sh links with this as the
// entry point and then rewrites the load command (scripts/macho_legacy.py, from VoxelRust).
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
core::arch::global_asm!(
    ".globl _penryn_start",
    ".p2align 4",
    "_penryn_start:",
    "push 0",
    "mov rbp, rsp",
    "and rsp, -16",
    "mov rdi, [rbp + 8]",
    "lea rsi, [rbp + 16]",
    "call _main",
    "mov edi, eax",
    "call _exit",
    "hlt",
);

const USAGE: &str = "Penryn: a Minecraft Java Edition 26.3 client for old Macs

  penryn [--relay R[:PORT]] [--name NAME]
                                   the title screen: Multiplayer has the server list
  penryn [--relay R[:PORT]] [--name NAME] HOST[:PORT]
                                   play: signed in if `penryn login` was done, else
                                   offline as NAME (offline-mode servers only)
  penryn login RELAY[:PORT] [NAME] sign in with your Microsoft account, through penryn-relay
                                   (Horizon's launcher's account there, else a device code)
  penryn logout                    forget the saved sign-in
  penryn status HOST[:PORT]        the server-list ping
  penryn add-server HOST[:PORT] [NAME]
                                   put a server in the Multiplayer list (servers.dat)
  penryn join HOST[:PORT] [NAME]   join without a window, printing what happens (tests)
  penryn selftest                  check the built-in crypto and compression

penryn-relay (tools/relay.py) runs on a newer computer on your network: it does the HTTPS
sign-in Mac OS X 10.6 cannot, and makes the game files from your own copy of Minecraft.
See README.md.";

pub fn data_dir() -> String {
    sys::env("PENRYN_DATA").unwrap_or_else(|| String::from("."))
}

pub fn load_blocks() -> Result<blocks::Blocks, String> {
    let path = packs::path("blocks.bin");
    let d = sys::read_file(&path).ok_or_else(|| alloc::format!("cannot read {} (see README.md)", path))?;
    blocks::Blocks::parse(&d).ok_or_else(|| alloc::format!("{} is damaged", path))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.get(1).map(|s| s.as_str()) {
        Some("status") => {
            let (h, p) = http::host_port(args.get(2).ok_or(USAGE)?, 25565);
            say!("{}", client::status(&h, p)?);
            Ok(())
        }
        Some("join") => {
            let (h, p) = http::host_port(args.get(2).ok_or(USAGE)?, 25565);
            let name = args.get(3).cloned().unwrap_or_else(|| String::from("LitePlayer"));
            let secs: u64 = sys::env("PENRYN_SECONDS").and_then(|s| s.parse().ok()).unwrap_or(20);
            packs::set_relay(sys::env("PENRYN_RELAY").map(|r| http::host_port(&r, 25580)));
            let mut g = client::Game::join(&h, p, &name, None, load_blocks()?, 6, Vec::new(), &mut packs::NoWindow)?;
            let end = sys::now_ms() + secs * 1000;
            let mut next_tick = sys::now_ms();
            let mut last_report = 0;
            let mut said = false;
            while sys::now_ms() < end {
                g.wait(10)?;
                if g.reconfigure {
                    g.reconfigure_now(&mut packs::NoWindow)?;
                }
                g.pack_step(&mut packs::NoWindow)?;
                if sys::now_ms() >= next_tick {
                    g.tick()?;
                    next_tick += 50;
                }
                if !said && g.loaded_sent && sys::now_ms() + secs * 1000 > end + 3000 {
                    said = true;
                    if let Some(text) = sys::env("PENRYN_SAY") {
                        g.say(&text)?;
                    }
                }
                if sys::now_ms() / 2000 != last_report {
                    last_report = sys::now_ms() / 2000;
                    let pl = &g.player;
                    let below = g.world.get(math::ifloor(pl.x), math::ifloor(pl.y - 0.5), math::ifloor(pl.z));
                    say!(
                        "at {:.1} {:.1} {:.1}, {} chunks, health {}, standing on {}",
                        pl.x, pl.y, pl.z, g.chunks_received, g.health, g.blocks.of_state(below).name
                    );
                }
            }
            say!("left after {} s", secs);
            Ok(())
        }
        Some("selftest") => selftest(),
        Some("add-server") => {
            let ip = args.get(2).ok_or(USAGE)?;
            servers::address(ip).ok_or(USAGE)?;
            let mut list = servers::List::load();
            if list.servers.iter().any(|s| s.ip == *ip) {
                return Ok(());
            }
            let name = args.get(3).cloned().unwrap_or_else(|| String::from("Minecraft Server"));
            list.servers.push(servers::Server::new(&name, ip));
            list.save();
            say!("added {} ({}) to {}", name, ip, servers::path());
            Ok(())
        }
        Some("login") => {
            let relay = http::host_port(args.get(2).ok_or(USAGE)?, 25580);
            account::login_terminal(relay, args.get(3).map(|s| s.as_str()))
        }
        Some("logout") => {
            account::logout();
            Ok(())
        }
        Some("help") | Some("--help") => {
            say!("{}", USAGE);
            Ok(())
        }
        _ => {
            // penryn [play] [--relay R] [--name N] [HOST[:PORT]] [NAME]: without a server,
            // the title screen.
            let mut relay = sys::env("PENRYN_RELAY").map(|r| http::host_port(&r, 25580));
            let mut name = None;
            let mut host = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "play" | "connect" => {}
                    "--relay" => {
                        i += 1;
                        relay = Some(http::host_port(args.get(i).ok_or(USAGE)?, 25580));
                    }
                    "--name" => {
                        i += 1;
                        name = args.get(i).cloned();
                    }
                    x if host.is_none() => host = Some(String::from(x)),
                    x => name = Some(String::from(x)),
                }
                i += 1;
            }
            let server = match host {
                Some(h) => Some(servers::address(&h).ok_or(USAGE)?),
                None => None,
            };
            let mut acc = match (&name, &server) {
                (Some(_), _) => None,
                (None, Some(_)) => account::load_refreshed()?,
                // The title screen starts even when the relay cannot renew the sign-in.
                (None, None) => account::load_refreshed().unwrap_or_else(|e| {
                    warn!("{}", e);
                    None
                }),
            };
            // --relay names the relay for this run's session calls too (join, chat keys).
            if let (Some(r), Some(a)) = (relay.as_ref(), acc.as_mut()) {
                a.relay = r.clone();
            }
            if let Some(a) = &acc {
                say!("signed in as {}", a.name);
            }
            let fetch_from = relay.clone().or_else(|| acc.as_ref().map(|a| a.relay.clone()));
            account::ensure_game_files(fetch_from.as_ref())?;
            skins::set_relay(fetch_from.clone());
            packs::set_relay(fetch_from.clone());
            let name = name.unwrap_or_else(|| String::from("Player"));
            play::run(server, &name, acc, fetch_from)
        }
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::new();
    for x in b {
        s.push_str(&alloc::format!("{:02x}", x));
    }
    s
}

fn selftest() -> Result<(), String> {
    let mut ok = true;
    let mut check = |what: &str, got: String, want: &str| {
        let pass = got == want;
        ok &= pass;
        say!("{} {}: {}", if pass { "ok  " } else { "FAIL" }, what, got);
    };
    // FIPS-197 appendix C.1
    let key: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    let mut b: [u8; 16] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    crypto::Aes128::new(&key).encrypt_block(&mut b);
    check("AES-128", hex(&b), "69c4e0d86a7b0430d8cdb78070b4c55a");
    // CFB8 round trip
    let mut p = crypto::Cfb8Pair::new(&key);
    let mut msg: Vec<u8> = b"the quick brown fox".to_vec();
    p.encrypt(&mut msg);
    let mut q = crypto::Cfb8Pair::new(&key);
    q.decrypt(&mut msg);
    check("CFB8 round trip", String::from_utf8_lossy(&msg).into_owned(), "the quick brown fox");
    let mut s = crypto::Sha1::new();
    s.update(b"abc");
    check("SHA-1", hex(&s.finish()), "a9993e364706816aba3e25717850c26c9cd0d89d");
    // wiki.vg's examples
    check("server hash Notch", crypto::server_hash(b"Notch", b"", b""), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
    check("server hash jeb_", crypto::server_hash(b"jeb_", b"", b""), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
    check("MD5", hex(&md5::md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    check("offline UUID", wire::uuid_text(&md5::offline_uuid("Notch")), "b50ad385-829d-3141-a216-7e7d7539ba7f");
    let data: Vec<u8> = (0..70000u32).map(|i| (i * 7 % 251) as u8).collect();
    let z = zlib::store(&data);
    check("zlib stored round trip", alloc::format!("{}", zlib::inflate(&z, data.len()) == Some(data.clone())), "true");
    for (what, got, want) in talk::selftest().into_iter().chain(text::selftest()).chain(commands::selftest()).chain(chatui::selftest()) {
        check(what, got, want);
    }
    for (what, got, want) in skins::selftest() {
        check(what, got, want);
    }
    for (what, got, want) in json::selftest().into_iter().chain(servers::selftest()).chain(title::selftest()).chain(multiplayer::selftest()) {
        check(what, got, want);
    }
    // Movement against vanilla's collision, in a made-up world (needs blocks.bin).
    match load_blocks() {
        Ok(b) => {
            for (what, got, want) in physics_checks(&b) {
                check(what, got, want);
            }
            let w = test_world(&b, &[]);
            for (what, got, want) in riding::selftest(&b, &w).into_iter().chain(special::selftest()).chain(poses::selftest()) {
                check(what, got, want);
            }
            for (what, got, want) in signs::selftest(Some(&b)).into_iter().chain(effects::selftest(&b)).chain(moving::selftest(&b)).chain(particles::selftest(&b, &w)) {
                check(what, got, want);
            }
        }
        Err(e) => {
            say!("skip physics checks: {}", e);
            for (what, got, want) in signs::selftest(None) {
                check(what, got, want);
            }
        }
    }
    // Weather: vanilla's randomness, and with weather.bin which biome rains or snows.
    let wdata = sys::read_file(&alloc::format!("{}/weather.bin", data_dir()));
    let wblocks = load_blocks().ok();
    for (what, got, want) in weather::checks(wdata.as_deref(), wblocks.as_ref()) {
        check(what, got, want);
    }
    for (what, got, want) in packs::selftest() {
        check(what, got, want);
    }
    for (what, got, want) in audio::overlay_selftest() {
        check(what, got, want);
    }
    for (what, got, want) in options::selftest() {
        check(what, got, want);
    }
    // The server-driven HUD, tooltips and game modes (serverui.bin when present).
    for (what, got, want) in serverui::selftest().into_iter().chain(tooltip::selftest()).chain(serverhud::selftest()).chain(gamemode::selftest()) {
        check(what, got, want);
    }
    // The creative inventory's tabs, search and clicks (needs gui.bin and blocks.bin).
    if let (Some(gui), Ok(b)) = (sys::read_file(&alloc::format!("{}/gui.bin", data_dir())), load_blocks()) {
        for (what, got, want) in creative::checks(&gui, &b.items) {
            check(what, got, want);
        }
    }
    // Mob looks (baby models, variants, layers, armour), from entities.bin.
    match sys::read_file(&alloc::format!("{}/entities.bin", data_dir())).ok_or(String::from("no entities.bin")).and_then(|d| entities::EntityTable::parse(&d)) {
        Ok(t) => {
            for (what, got, want) in mobs::selftest_checks(&t) {
                check(what, got, want);
            }
        }
        Err(e) => say!("skip mob checks: {}", e),
    }
    // The container screens, maps and books (containers.bin is optional).
    let cdata = sys::read_file(&alloc::format!("{}/containers.bin", data_dir()));
    for (what, got, want) in menus::selftest(cdata.as_deref()).into_iter().chain(maps::selftest()).chain(books::selftest()) {
        check(what, got, want);
    }
    if ok {
        Ok(())
    } else {
        Err(String::from("self test failed"))
    }
}

/// A flat stone floor at y 63 in one loaded chunk, plus `extra` blocks.
fn test_world(b: &blocks::Blocks, extra: &[(i32, i32, i32, u16)]) -> world::World {
    let mut w = world::World::new(b.global_bits(), 6);
    let n = (w.height / 16) as usize;
    let mut sections = Vec::new();
    for _ in 0..n {
        sections.push(None);
    }
    w.columns.insert((0, 0), world::Column { sections, biomes: alloc::vec![[0u16; 64]; n], version: 0 });
    let stone = first_state(b, "stone", |_| true).unwrap_or(1);
    let air = |s: u16| s == 0;
    for x in 0..16 {
        for z in 0..16 {
            w.set(x, 63, z, stone, &air);
        }
    }
    for &(x, y, z, s) in extra {
        w.set(x, y, z, s, &air);
    }
    w
}

fn first_state(b: &blocks::Blocks, name: &str, pick: impl Fn(u16) -> bool) -> Option<u16> {
    (0..b.states.len() as u16).find(|&s| b.of_state(s).name == name && pick(s))
}

/// Walks forward (+z) for `ticks` from `pos`; the end position.
fn walk(b: &blocks::Blocks, w: &world::World, pos: [f64; 3], ticks: u32, sneak: bool) -> [f64; 3] {
    let mut m = physics::Mover::new(pos);
    m.on_ground = true;
    let inp = physics::Input { forward: if sneak { 0.98 * 0.3 } else { 0.98 }, sneak, shift: sneak, ..physics::Input::idle() };
    for _ in 0..ticks {
        m.tick(w, b, &inp, 0.0, false, 0);
    }
    m.pos
}

fn physics_checks(b: &blocks::Blocks) -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let stone = first_state(b, "stone", |_| true).unwrap_or(1);
    let yes = |v: bool| String::from(if v { "yes" } else { "no" });
    // A fence gate across the way at z 5: closed it stops the player, open it lets them by.
    let spans_x = |s: u16| b.shape(s).iter().any(|bx| bx[0] == 0 && bx[3] == 16);
    let closed = first_state(b, "oak_fence_gate", |s| b.collides(s) && spans_x(s));
    let open = first_state(b, "oak_fence_gate", |s| !b.collides(s));
    if let (Some(c), Some(o)) = (closed, open) {
        let w = test_world(b, &[(8, 64, 5, c)]);
        // The panel's near side is at z 5 + 6/16; the player is 0.3 wide each way.
        out.push(("closed gate stops", yes(walk(b, &w, [8.5, 64.0, 3.5], 40, false)[2] < 5.1), "yes"));
        let w = test_world(b, &[(8, 64, 5, o)]);
        out.push(("open gate lets through", yes(walk(b, &w, [8.5, 64.0, 3.5], 40, false)[2] > 7.0), "yes"));
    }
    // A ladder on a stone pillar: walking into it climbs.
    let ladder = first_state(b, "ladder", |s| b.shape(s).iter().any(|bx| bx[5] == 16 && bx[2] > 8));
    if let Some(l) = ladder {
        let mut extra = Vec::new();
        for y in 64..72 {
            extra.push((8, y, 10, stone));
            extra.push((8, y, 9, l));
        }
        let w = test_world(b, &extra);
        out.push(("ladder climbs", yes(walk(b, &w, [8.5, 64.0, 8.5], 40, false)[1] > 66.0), "yes"));
    }
    // A wall's low side (outline 14/16) still stops a jump at 1.5.
    let low_side = first_state(b, "cobblestone_wall", |s| b.shape(s).iter().all(|bx| bx[4] == 14) && !b.shape(s).is_empty());
    if let Some(ws) = low_side {
        let w = test_world(b, &[(8, 64, 5, ws)]);
        let boxes = physics::boxes(&w, b, &physics::Aabb { lo: [8.0, 64.0, 5.0], hi: [9.0, 65.0, 6.0] });
        out.push(("low wall side is 1.5 high", yes(boxes.iter().any(|o| (o.hi[1] - 65.5).abs() < 1e-9)), "yes"));
    }
    // Under a 1.5-high gap (a bottom slab overhead) only crouching fits.
    let slab = first_state(b, "stone_slab", |s| b.shape(s).iter().all(|bx| bx[1] == 0 && bx[4] == 8));
    if let Some(sl) = slab {
        let w = test_world(b, &[(8, 65, 8, sl)]);
        // Feet at 64, slab bottom at 65: a 1.0 gap, nothing fits.
        let p = [8.5, 64.0, 8.5];
        let fits = (physics::fits(&w, b, p, physics::HEIGHT), physics::fits(&w, b, p, physics::SNEAK_HEIGHT));
        // Feet at 64.5, slab bottom at 66: a 1.5 gap, only crouching fits.
        let w2 = test_world(b, &[(8, 66, 8, sl)]);
        let fits2 = (physics::fits(&w2, b, [8.5, 64.5, 8.5], physics::HEIGHT), physics::fits(&w2, b, [8.5, 64.5, 8.5], physics::SNEAK_HEIGHT));
        out.push(("low gap: stand/crouch fit", alloc::format!("{:?} {:?}", fits, fits2), "(false, false) (false, true)"));
    }
    // Ground that changes movement: the distance walked in 30 ticks on stone, ice, soul sand
    // and through cobwebs; on ice it is longer, the others shorter.
    let floor_of = |name: &str| -> Option<world::World> {
        let st = first_state(b, name, |_| true)?;
        let mut extra = Vec::new();
        for z in 0..16 {
            extra.push((8, 63, z, st));
        }
        Some(test_world(b, &extra))
    };
    let dist = |w: &world::World| walk(b, w, [8.5, 64.0, 1.5], 30, false)[2] - 1.5;
    // How far the player slides after letting go of W at full speed.
    let slide = |w: &world::World| -> f64 {
        let mut m = physics::Mover::new([8.5, 64.0, 1.5]);
        m.on_ground = true;
        let mut inp = physics::Input { forward: 0.98, ..physics::Input::idle() };
        for _ in 0..20 {
            m.tick(w, b, &inp, 0.0, false, 0);
        }
        inp.forward = 0.0;
        let z0 = m.pos[2];
        for _ in 0..40 {
            m.tick(w, b, &inp, 0.0, false, 0);
        }
        m.pos[2] - z0
    };
    if let (Some(stone_w), Some(ice_w), Some(soul_w)) = (floor_of("stone"), floor_of("ice"), floor_of("soul_sand")) {
        let (s0, s1) = (slide(&stone_w), slide(&ice_w));
        let (d0, d2) = (dist(&stone_w), dist(&soul_w));
        out.push(("ice slides, soul sand slow", alloc::format!("{} {}", s1 > s0 * 4.0, d2 < d0 * 0.6), "true true"));
    }
    if let Some(web) = first_state(b, "cobweb", |_| true) {
        let mut extra = Vec::new();
        for z in 3..12 {
            extra.push((8, 64, z, web));
            extra.push((8, 65, z, web));
        }
        let w = test_world(b, &extra);
        let stone_w = test_world(b, &[]);
        out.push(("cobwebs hold", yes(dist(&w) < dist(&stone_w) * 0.4), "yes"));
    }
    // Jumping from honey reaches about half as high; landing on slime bounces.
    let jump_top = |w: &world::World, y0: f64| -> f64 {
        let mut m = physics::Mover::new([8.5, y0, 8.5]);
        m.on_ground = true;
        let mut inp = physics::Input { jump: true, ..physics::Input::idle() };
        let mut top = y0;
        for t in 0..20 {
            if t == 1 {
                inp.jump = false;
            }
            m.tick(w, b, &inp, 0.0, false, 0);
            top = top.max(m.pos[1]);
        }
        top - y0
    };
    if let Some(honey) = first_state(b, "honey_block", |_| true) {
        let w = test_world(b, &[(8, 63, 8, honey)]);
        let stone_w = test_world(b, &[]);
        let (h, s0) = (jump_top(&w, 63.9375), jump_top(&stone_w, 64.0));
        out.push(("honey jump is lower", yes(h < s0 * 0.4 && s0 > 1.2), "yes"));
    }
    if let Some(slime) = first_state(b, "slime_block", |_| true) {
        let w = test_world(b, &[(8, 63, 8, slime)]);
        let mut m = physics::Mover::new([8.5, 70.0, 8.5]);
        let inp = physics::Input::idle();
        let mut landed = false;
        let mut back_up = 64.0f64;
        for _ in 0..60 {
            m.tick(&w, b, &inp, 0.0, false, 0);
            if m.pos[1] < 64.01 {
                landed = true;
            }
            if landed {
                back_up = back_up.max(m.pos[1]);
            }
        }
        out.push(("slime bounces", yes(landed && back_up > 67.0), "yes"));
    }
    // Bamboo stands at its offset: vanilla BlockBehaviour.getOffset.
    if let Some(bam) = first_state(b, "bamboo", |_| true) {
        let w = test_world(b, &[(3, 64, 5, bam)]);
        let bx = physics::boxes(&w, b, &physics::Aabb { lo: [3.0, 64.0, 5.0], hi: [4.0, 65.0, 6.0] });
        let o = blocks::offset(3, 5, 0.25, 0.0);
        let ok = bx.iter().any(|a| (a.lo[0] - (3.0 + 6.5 / 16.0 + o[0])).abs() < 1e-9 && (a.lo[2] - (5.0 + 6.5 / 16.0 + o[2])).abs() < 1e-9);
        out.push(("bamboo at its offset", yes(ok), "yes"));
    }
    // Mth.getSeed against values computed with Java's arithmetic.
    out.push(("Mth.getSeed", alloc::format!("{} {}", blocks::seed(0, 0, 0), blocks::seed(1, 0, 1)), "0 60165349410229"));
    // A bottom slab collides half a block high, and walking into one steps onto it.
    if let Some(sl) = first_state(b, "stone_slab", |s| b.shape(s).iter().all(|bx| bx[1] == 0 && bx[4] == 8)) {
        let w = test_world(b, &[(8, 64, 5, sl)]);
        let top = physics::boxes(&w, b, &physics::Aabb { lo: [8.0, 64.0, 5.0], hi: [9.0, 65.0, 6.0] })
            .iter()
            .filter(|a| a.lo[0] >= 8.0 && a.lo[2] >= 5.0 && a.lo[1] >= 64.0)
            .fold(0.0f64, |m, a| m.max(a.hi[1]));
        let end = walk(b, &w, [8.5, 64.0, 4.0], 10, false);
        out.push(("slab: half high, stepped onto", alloc::format!("{} {}", top, end[1] == 64.5), "64.5 true"));
    }
    // Falling fast beside a one-block ledge lands beside it, not on it (vanilla steps up at
    // most 0.6 from where the fall stops).
    {
        let stone = first_state(b, "stone", |_| true).unwrap_or(1);
        let w = test_world(b, &[(8, 64, 6, stone)]);
        let mut m = physics::Mover::new([8.5, 65.5, 5.65]);
        m.vel = [0.0, -0.6, 0.3];
        let inp = physics::Input { forward: 0.98, ..physics::Input::idle() };
        for _ in 0..3 {
            m.tick(&w, b, &inp, 0.0, false, 0);
        }
        out.push(("no step onto a ledge mid-fall", yes(m.pos[1] < 64.5), "yes"));
    }
    // Azalea and lily pads are solid to the server: the player cannot walk into them.
    if let Some(a) = first_state(b, "azalea", |_| true) {
        let w = test_world(b, &[(8, 64, 5, a)]);
        out.push(("azalea is solid", yes(walk(b, &w, [8.5, 64.0, 3.5], 40, false)[2] < 5.5), "yes"));
    }
    out
}

#[no_mangle]
pub extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    let args: Vec<String> = (0..argc as usize).map(|i| unsafe { sys::from_cstr(*argv.add(i)) }).collect();
    match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            warn!("penryn: {}", e);
            1
        }
    }
}
