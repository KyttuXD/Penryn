//! Players' own skins and capes, as vanilla's SkinManager and CapeLayer: the player info
//! packet (ClientboundPlayerInfoUpdatePacket, ADD_PLAYER) carries each profile's `textures`
//! property (base64 JSON: the skin URL, its model slim or classic, the cape URL). Mac OS X 10.6
//! cannot reach textures.minecraft.net (HTTPS), so the PNGs come through the relay
//! (tools/relay.py, GET /skin?url=...), fetched and decoded on a worker thread so frames never
//! wait, then uploaded a few a frame. Legacy 64 x 32 skins are converted as
//! SkinTextureDownloader.processLegacySkin does. Offline-mode profiles carry no textures and
//! keep the default skins.
//!
//! `PENRYN_TEST_SKIN` (a textures value in base64, or `@file` holding one) gives every player
//! without textures that profile, to see a skin on an offline server.

use crate::math;
use crate::sdl::*;
use crate::wire::In;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

// ------------------------------------------------------------------ textures property

/// What a profile's `textures` property says.
#[derive(Clone, PartialEq, Debug)]
pub struct Profile {
    pub slim: bool,
    pub skin: Option<String>,
    pub cape: Option<String>,
}

pub fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// The text of the JSON object that follows `"key"` in `j` (braces matched, strings skipped).
fn object<'a>(j: &'a str, key: &str) -> Option<&'a str> {
    let at = j.find(&format!("\"{}\"", key))? + key.len() + 2;
    let start = at + j[at..].find('{')?;
    let b = j.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0, false, false);
    for i in start..b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&j[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The string value of `"key"` in `j`.
fn string_of(j: &str, key: &str) -> Option<String> {
    let at = j.find(&format!("\"{}\"", key))? + key.len() + 2;
    let rest = j[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(String::from(rest[..end].replace("\\/", "/")))
}

/// MinecraftProfileTextures from the property value (base64 JSON).
pub fn parse_textures(value: &str) -> Option<Profile> {
    let raw = base64(value)?;
    let j = core::str::from_utf8(&raw).ok()?;
    let t = object(j, "textures")?;
    let skin = object(t, "SKIN");
    let slim = skin.and_then(|s| object(s, "metadata")).and_then(|m| string_of(m, "model")).map_or(false, |m| m == "slim");
    Some(Profile { slim, skin: skin.and_then(|s| string_of(s, "url")), cape: object(t, "CAPE").and_then(|s| string_of(s, "url")) })
}

/// The texture's hash, the last part of a textures.minecraft.net URL (the relay refuses others).
fn texture_hash(url: &str) -> Option<String> {
    let h = url.strip_prefix("http://textures.minecraft.net/texture/").or_else(|| url.strip_prefix("https://textures.minecraft.net/texture/"))?;
    if h.len() < 16 || h.len() > 128 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(String::from(h))
}

// ------------------------------------------------------------------ PNG

/// Decodes a non-interlaced 8-bit PNG (grey, RGB, palette, grey + alpha, RGBA) into RGBA.
pub fn png(d: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if d.len() < 33 || &d[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let be = |o: usize| u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize;
    let (mut w, mut h, mut ct) = (0, 0, 0u8);
    let mut idat = Vec::new();
    let mut pal: Vec<[u8; 4]> = Vec::new();
    let mut p = 8;
    while p + 8 <= d.len() {
        let n = be(p);
        let kind = &d[p + 4..p + 8];
        let body = d.get(p + 8..p + 8 + n)?;
        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    return None;
                }
                w = be(p + 8);
                h = be(p + 12);
                ct = body[9];
                if body[8] != 8 || body[12] != 0 || w == 0 || h == 0 || w > 1024 || h > 1024 {
                    return None;
                }
            }
            b"PLTE" => pal = body.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect(),
            b"tRNS" => {
                for (i, a) in body.iter().enumerate() {
                    if let Some(e) = pal.get_mut(i) {
                        e[3] = *a;
                    }
                }
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        p += 12 + n;
    }
    if w == 0 {
        return None; // no IHDR
    }
    let bpp = match ct {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        6 => 4,
        _ => return None,
    };
    let stride = w * bpp;
    let raw = crate::zlib::inflate(&idat, h * (stride + 1))?;
    if raw.len() < h * (stride + 1) {
        return None;
    }
    let mut px = alloc::vec![0u8; h * stride];
    for y in 0..h {
        let f = raw[y * (stride + 1)];
        let src = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { px[y * stride + x - bpp] as i32 } else { 0 };
            let b = if y > 0 { px[(y - 1) * stride + x] as i32 } else { 0 };
            let c = if x >= bpp && y > 0 { px[(y - 1) * stride + x - bpp] as i32 } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let pp = a + b - c;
                    let (pa, pb, pc) = ((pp - a).abs(), (pp - b).abs(), (pp - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return None,
            };
            px[y * stride + x] = (src[x] as i32 + pred) as u8;
        }
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let s = &px[i * bpp..i * bpp + bpp];
        match ct {
            0 => out.extend_from_slice(&[s[0], s[0], s[0], 255]),
            4 => out.extend_from_slice(&[s[0], s[0], s[0], s[1]]),
            2 => out.extend_from_slice(&[s[0], s[1], s[2], 255]),
            3 => out.extend_from_slice(&pal.get(s[0] as usize).copied().unwrap_or([0, 0, 0, 255])),
            _ => out.extend_from_slice(s),
        }
    }
    Some((w, h, out))
}

/// SkinTextureDownloader.processLegacySkin: 64 x 32 skins get the 64 x 64 layout (the right
/// arm and leg mirrored into the left ones), and the inner layers lose their transparency.
/// `None` for any other size (vanilla refuses it too).
pub fn process_skin(w: usize, h: usize, px: Vec<u8>) -> Option<Vec<u8>> {
    if w != 64 || (h != 32 && h != 64) {
        return None;
    }
    let legacy = h == 32;
    let mut img = px;
    img.resize(64 * 64 * 4, 0);
    let get = |img: &Vec<u8>, x: usize, y: usize| -> [u8; 4] { let o = (y * 64 + x) * 4; [img[o], img[o + 1], img[o + 2], img[o + 3]] };
    if legacy {
        // copyRect(x, y, dx, dy, w, h, mirrorX, false)
        let copies: [(i32, i32, i32, i32, i32, i32); 12] = [
            (4, 16, 16, 32, 4, 4),
            (8, 16, 16, 32, 4, 4),
            (0, 20, 24, 32, 4, 12),
            (4, 20, 16, 32, 4, 12),
            (8, 20, 8, 32, 4, 12),
            (12, 20, 16, 32, 4, 12),
            (44, 16, -8, 32, 4, 4),
            (48, 16, -8, 32, 4, 4),
            (40, 20, 0, 32, 4, 12),
            (44, 20, -8, 32, 4, 12),
            (48, 20, -16, 32, 4, 12),
            (52, 20, -8, 32, 4, 12),
        ];
        for (x, y, dx, dy, cw, ch) in copies {
            for j in 0..ch {
                for i in 0..cw {
                    let c = get(&img, (x + i) as usize, (y + j) as usize);
                    let (tx, ty) = ((x + dx + cw - 1 - i) as usize, (y + dy + j) as usize);
                    img[(ty * 64 + tx) * 4..(ty * 64 + tx) * 4 + 4].copy_from_slice(&c);
                }
            }
        }
    }
    let no_alpha = |img: &mut Vec<u8>, x0: usize, y0: usize, x1: usize, y1: usize| {
        for y in y0..y1 {
            for x in x0..x1 {
                img[(y * 64 + x) * 4 + 3] = 255;
            }
        }
    };
    no_alpha(&mut img, 0, 0, 32, 16);
    if legacy {
        // doNotchTransparencyHack: an opaque hat layer was meant to be transparent.
        let opaque = (32..64).all(|x| (0..32).all(|y| get(&img, x, y)[3] >= 128));
        if opaque {
            for y in 0..32 {
                for x in 32..64 {
                    img[(y * 64 + x) * 4 + 3] = 0;
                }
            }
        }
    }
    no_alpha(&mut img, 0, 16, 64, 32);
    no_alpha(&mut img, 16, 48, 48, 64);
    Some(img)
}

// ------------------------------------------------------------------ the worker thread

struct Shared {
    /// (texture hash, is a skin, relay)
    jobs: Vec<(String, bool, (String, u16))>,
    /// (texture hash, width, height, RGBA) or None when it could not be had.
    done: Vec<(String, Option<(usize, usize, Vec<u8>)>)>,
    started: bool,
}

struct Locked(AtomicBool, UnsafeCell<Shared>);
unsafe impl Sync for Locked {}

static SHARED: Locked = Locked(AtomicBool::new(false), UnsafeCell::new(Shared { jobs: Vec::new(), done: Vec::new(), started: false }));

/// Runs `f` with the shared queues; `wait` false gives up at once when the other side has them.
fn shared<R>(wait: bool, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    while SHARED.0.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        if !wait {
            return None;
        }
        crate::sys::sleep_ms(1);
    }
    let r = f(unsafe { &mut *SHARED.1.get() });
    SHARED.0.store(false, Ordering::Release);
    Some(r)
}

fn fetch(hash: &str, skin: bool, relay: &(String, u16)) -> Option<(usize, usize, Vec<u8>)> {
    let path = format!("/skin?url=http%3A%2F%2Ftextures.minecraft.net%2Ftexture%2F{}", hash);
    let r = match crate::http::get(&relay.0, relay.1, &path, 20000) {
        Ok(r) => r,
        Err(e) => {
            crate::warn!("skin {}: {}", hash, e);
            return None;
        }
    };
    if r.status != 200 {
        crate::warn!("skin {}: the relay answered {} {}", hash, r.status, r.text().trim());
        return None;
    }
    let (w, h, px) = png(&r.body)?;
    if skin {
        Some((64, 64, process_skin(w, h, px)?))
    } else {
        Some((w, h, px))
    }
}

extern "C" fn worker(_: *mut c_void) -> *mut c_void {
    loop {
        match shared(true, |s| s.jobs.pop()).flatten() {
            Some((hash, skin, relay)) => {
                let img = fetch(&hash, skin, &relay);
                shared(true, |s| s.done.push((hash, img)));
            }
            None => crate::sys::sleep_ms(50),
        }
    }
}

// ------------------------------------------------------------------ the client's side

enum Tex {
    Loading,
    Ready(u32),
    Failed,
}

/// The cloak (ClientAvatarState): a point trailing the player, which the cape leans away
/// from, and the walk bob and distance that flap it.
#[derive(Clone, Copy, Default)]
struct Cloak {
    at: [f64; 3],
    prev: [f64; 3],
    bob: f32,
    prev_bob: f32,
    walk: f32,
    prev_walk: f32,
    started: bool,
}

struct State {
    relay: Option<(String, u16)>,
    local: [u8; 16],
    profiles: BTreeMap<[u8; 16], Profile>,
    textures: BTreeMap<String, Tex>,
    cloaks: BTreeMap<i32, Cloak>,
    test: Option<Option<Profile>>,
}

struct Main(UnsafeCell<State>);
unsafe impl Sync for Main {}

static STATE: Main = Main(UnsafeCell::new(State {
    relay: None,
    local: [0; 16],
    profiles: BTreeMap::new(),
    textures: BTreeMap::new(),
    cloaks: BTreeMap::new(),
    test: None,
}));

/// The main thread's state (the game loop is single-threaded; the worker never touches it).
fn st() -> &'static mut State {
    unsafe { &mut *STATE.0.get() }
}

/// Where to fetch textures from (the relay of `--relay`, PENRYN_RELAY or the account).
pub fn set_relay(relay: Option<(String, u16)>) {
    st().relay = relay;
}

fn test_profile() -> Option<Profile> {
    let s = st();
    if s.test.is_none() {
        let v = crate::sys::env("PENRYN_TEST_SKIN").and_then(|v| match v.strip_prefix('@') {
            Some(path) => crate::sys::read_file(path).map(|d| String::from_utf8_lossy(&d).trim().into()),
            None => Some(v),
        });
        s.test = Some(v.and_then(|v| parse_textures(&v)));
    }
    s.test.clone().flatten()
}

/// Asks the worker for a texture once.
fn want(url: &Option<String>, skin: bool) {
    let s = st();
    let (Some(url), Some(relay)) = (url, s.relay.clone()) else { return };
    let Some(hash) = texture_hash(url) else { return };
    if s.textures.contains_key(&hash) {
        return;
    }
    s.textures.insert(hash.clone(), Tex::Loading);
    let start = shared(true, |q| {
        q.jobs.insert(0, (hash, skin, relay));
        !core::mem::replace(&mut q.started, true)
    })
    .unwrap_or(false);
    if start {
        let mut t: usize = 0;
        if unsafe { crate::sys::pthread_create(&mut t, core::ptr::null(), worker, core::ptr::null_mut()) } == 0 {
            unsafe { crate::sys::pthread_detach(t) };
        } else {
            crate::warn!("skins: cannot start the download thread");
        }
    }
}

/// Sets a player's profile textures (and starts their downloads).
pub fn set_profile(uuid: [u8; 16], p: Profile) {
    want(&p.skin, true);
    want(&p.cape, false);
    st().profiles.insert(uuid, p);
}

/// ClientboundPlayerInfoUpdatePacket: ADD_PLAYER entries' textures. `local` is our own UUID.
pub fn player_info(r: &mut In, local: [u8; 16]) {
    st().local = local;
    let Some(actions) = r.u8() else { return };
    if actions & 1 == 0 {
        return; // only ADD_PLAYER carries the profile; the other actions come after it
    }
    let n = r.varint().unwrap_or(0);
    for _ in 0..n {
        let Some(uuid) = r.uuid() else { return };
        let Some(_name) = r.string() else { return };
        let props = r.varint().unwrap_or(0);
        let mut profile = None;
        for _ in 0..props {
            let (Some(key), Some(value)) = (r.string(), r.string()) else { return };
            if r.bool() == Some(true) {
                r.string();
            }
            if key == "textures" {
                profile = parse_textures(&value);
            }
        }
        if let Some(p) = profile.or_else(test_profile) {
            set_profile(uuid, p);
        }
        // The other actions of this entry: with more than ADD_PLAYER their layouts must be
        // read to reach the next entry.
        if actions & !1 != 0 && !skip_actions(r, actions) {
            return;
        }
    }
}

/// Reads past an entry's INITIALIZE_CHAT .. UPDATE_HAT fields.
fn skip_actions(r: &mut In, actions: u8) -> bool {
    let mut ok = || -> Option<()> {
        if actions & 2 != 0 && r.bool()? {
            r.take(16 + 8)?;
            let n = r.varint()?;
            r.take(n.max(0) as usize)?;
            let n = r.varint()?;
            r.take(n.max(0) as usize)?;
        }
        if actions & 4 != 0 {
            r.varint()?;
        }
        if actions & 8 != 0 {
            r.bool()?;
        }
        if actions & 16 != 0 {
            r.varint()?;
        }
        if actions & 32 != 0 && r.bool()? {
            crate::nbt::skip_root(r)?;
        }
        if actions & 64 != 0 {
            r.varint()?;
        }
        if actions & 128 != 0 {
            r.bool()?;
        }
        Some(())
    };
    ok().is_some()
}

/// ClientboundPlayerInfoRemovePacket.
pub fn player_remove(r: &mut In) {
    let n = r.varint().unwrap_or(0);
    for _ in 0..n {
        let Some(u) = r.uuid() else { return };
        st().profiles.remove(&u);
    }
}

/// Uploads textures the worker has finished: at most a few a frame.
pub fn frame(gl: &Gl) {
    let Some(done) = shared(false, |q| {
        let n = q.done.len().min(4);
        q.done.drain(..n).collect::<Vec<_>>()
    }) else {
        return;
    };
    for (hash, img) in done {
        let t = match img {
            Some((w, h, px)) => {
                let mut tex = 0u32;
                unsafe {
                    (gl.GenTextures)(1, &mut tex);
                    (gl.BindTexture)(TEXTURE_2D, tex);
                    (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
                    (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
                }
                if crate::sys::env("PENRYN_DEBUG").is_some() {
                    crate::say!("skin texture {} ({} x {})", hash, w, h);
                }
                Tex::Ready(tex)
            }
            None => Tex::Failed,
        };
        st().textures.insert(hash, t);
    }
}

fn ready(url: &Option<String>) -> Option<u32> {
    match st().textures.get(&texture_hash(url.as_ref()?)?)? {
        Tex::Ready(t) => Some(*t),
        _ => None,
    }
}

/// A player's downloaded skin: (texture, slim arms). None: the default skin.
pub fn skin_of(uuid: &[u8; 16]) -> Option<(u32, bool)> {
    let p = st().profiles.get(uuid)?;
    Some((ready(&p.skin)?, p.slim))
}

/// The texture of a skin PNG at `url` (a player head's profile): asked for once, None until it is downloaded.
pub fn texture_for(url: &str) -> Option<u32> {
    let u = Some(String::from(url));
    want(&u, true);
    ready(&u)
}

/// A player's downloaded cape texture.
pub fn cape_of(uuid: &[u8; 16]) -> Option<u32> {
    ready(&st().profiles.get(uuid)?.cape)
}

/// Our own player's skin (the first-person arm).
pub fn local_skin() -> Option<(u32, bool)> {
    skin_of(&st().local)
}

/// Whether skins.rs draws this player (EntityTable::draw leaves it out).
pub fn custom(uuid: &[u8; 16]) -> bool {
    skin_of(uuid).is_some() || cape_of(uuid).is_some()
}

/// The client tick, before the entities' own (their `prev` is still last tick's position):
/// ClientAvatarState.tick's cloak, bob and walk distance for each player with a cape.
pub fn tick(ents: &BTreeMap<i32, crate::entities::Entity>) {
    let s = st();
    if s.profiles.is_empty() {
        s.cloaks.clear();
        return;
    }
    s.cloaks.retain(|id, _| ents.contains_key(id));
    for (&id, e) in ents.iter() {
        if !s.profiles.get(&e.uuid).map_or(false, |p| p.cape.is_some()) {
            continue;
        }
        let c = s.cloaks.entry(id).or_default();
        if !c.started {
            *c = Cloak { at: e.pos, prev: e.pos, started: true, ..Cloak::default() };
        }
        // moveCloak: follows a quarter of the way each tick, jumps when 10 blocks behind.
        c.prev = c.at;
        for a in 0..3 {
            let d = e.pos[a] - c.at[a];
            if d > 10.0 || d < -10.0 {
                c.at[a] = e.pos[a];
                c.prev[a] = c.at[a];
            } else {
                c.at[a] += d * 0.25;
            }
        }
        let (dx, dz) = (e.pos[0] - e.prev[0], e.pos[2] - e.prev[2]);
        let dist = math::sqrt(dx * dx + dz * dz) as f32;
        c.prev_walk = c.walk;
        c.walk += dist * 0.6;
        // Player.aiStep: the bob follows the horizontal speed (at most 0.1) on the ground.
        c.prev_bob = c.bob;
        let target = if e.on_ground { dist.min(0.1) } else { 0.0 };
        c.bob += (target - c.bob) * 0.4;
    }
}

/// AvatarRenderer.extractCapeState: (capeFlap, capeLean, capeLean2) in degrees.
fn cape_angles(e: &crate::entities::Entity, c: &Cloak, partial: f32) -> (f32, f32, f32) {
    let pf = partial as f64;
    let lerp = |a: f64, b: f64| a + (b - a) * pf;
    let dx = lerp(c.prev[0], c.at[0]) - lerp(e.prev[0], e.pos[0]);
    let dy = lerp(c.prev[1], c.at[1]) - lerp(e.prev[1], e.pos[1]);
    let dz = lerp(c.prev[2], c.at[2]) - lerp(e.prev[2], e.pos[2]);
    let yaw = math::to_rad(e.body_rot(partial) as f64);
    let (s, co) = (math::sin(yaw), -math::cos(yaw));
    let mut flap = ((dy * 10.0) as f32).clamp(-6.0, 32.0);
    let lean = (((dx * s + dz * co) * 100.0) as f32).clamp(0.0, 150.0);
    let lean2 = (((dx * co - dz * s) * 100.0) as f32).clamp(-20.0, 20.0);
    let bob = c.prev_bob + (c.bob - c.prev_bob) * partial;
    let walk = c.prev_walk + (c.walk - c.prev_walk) * partial;
    flap += math::sin(walk as f64 * 6.0) as f32 * 32.0 * bob;
    (flap, lean, lean2)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// PlayerCapeModel's cape (texOffs 0 0, a 10 x 16 x 1 box) on a 64 x 32 texture: corners
/// (x, y, z, u, v) in the part's space and the face normal.
const CAPE: [([[f32; 5]; 4], [f32; 3]); 6] = [
    ([[5.0, 0.0, 0.0, 0.171875, 0.0], [-5.0, 0.0, 0.0, 0.015625, 0.0], [-5.0, 0.0, -1.0, 0.015625, 0.03125], [5.0, 0.0, -1.0, 0.171875, 0.03125]], [0.0, -1.0, 0.0]),
    ([[5.0, 16.0, -1.0, 0.328125, 0.03125], [-5.0, 16.0, -1.0, 0.171875, 0.03125], [-5.0, 16.0, 0.0, 0.171875, 0.0], [5.0, 16.0, 0.0, 0.328125, 0.0]], [0.0, 1.0, 0.0]),
    ([[-5.0, 0.0, -1.0, 0.015625, 0.03125], [-5.0, 0.0, 0.0, 0.0, 0.03125], [-5.0, 16.0, 0.0, 0.0, 0.53125], [-5.0, 16.0, -1.0, 0.015625, 0.53125]], [-1.0, 0.0, 0.0]),
    ([[5.0, 0.0, -1.0, 0.171875, 0.03125], [-5.0, 0.0, -1.0, 0.015625, 0.03125], [-5.0, 16.0, -1.0, 0.015625, 0.53125], [5.0, 16.0, -1.0, 0.171875, 0.53125]], [0.0, 0.0, -1.0]),
    ([[5.0, 0.0, 0.0, 0.1875, 0.03125], [5.0, 0.0, -1.0, 0.171875, 0.03125], [5.0, 16.0, -1.0, 0.171875, 0.53125], [5.0, 16.0, 0.0, 0.1875, 0.53125]], [1.0, 0.0, 0.0]),
    ([[-5.0, 0.0, 0.0, 0.34375, 0.03125], [5.0, 0.0, 0.0, 0.1875, 0.03125], [5.0, 16.0, 0.0, 0.1875, 0.53125], [-5.0, 16.0, 0.0, 0.34375, 0.53125]], [0.0, 0.0, 1.0]),
];

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

/// The cape's quads for a player at `at` (relative to the eye), as CapeLayer poses it: on the
/// body at (0, 0, 2), turned by rotateY(-180) rotateX(6 + lean / 2 + flap) rotateZ(lean2 / 2)
/// rotateY(180 - lean2 / 2) after the part's own yRot 180.
fn cape_quads(out: &mut Vec<V>, body: f32, at: [f64; 3], angles: (f32, f32, f32)) {
    let (flap, lean, lean2) = angles;
    let yaw = math::to_rad((180.0 - body) as f64);
    let base = mul(
        &mul(&[[1.0, 0.0, 0.0, at[0]], [0.0, 1.0, 0.0, at[1]], [0.0, 0.0, 1.0, at[2]]], &rot(1, yaw)),
        &[[-1.0 / 16.0, 0.0, 0.0, 0.0], [0.0, -1.0 / 16.0, 0.0, 1.501], [0.0, 0.0, 1.0 / 16.0, 0.0]],
    );
    let mut t = mul(&base, &[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 2.0]]);
    t = mul(&t, &rot(0, math::to_rad((6.0 + lean / 2.0 + flap) as f64)));
    t = mul(&t, &rot(2, math::to_rad((lean2 / 2.0) as f64)));
    t = mul(&t, &rot(1, math::to_rad((180.0 - lean2 / 2.0) as f64)));
    for (c, n) in CAPE.iter() {
        let ny = 16.0 * (t[1][0] * n[0] as f64 + t[1][1] * n[1] as f64 + t[1][2] * n[2] as f64);
        let l = (0.7 + 0.3 * ny.clamp(-1.0, 1.0)) as f32;
        // CapeLayer draws with OverlayTexture.NO_OVERLAY: no red flash when hurt.
        let col = [(255.0 * l) as u8, (255.0 * l) as u8, (255.0 * l) as u8, 255];
        for v in c.iter() {
            let x = t[0][0] * v[0] as f64 + t[0][1] * v[1] as f64 + t[0][2] * v[2] as f64 + t[0][3];
            let y = t[1][0] * v[0] as f64 + t[1][1] * v[1] as f64 + t[1][2] * v[2] as f64 + t[1][3];
            let z = t[2][0] * v[0] as f64 + t[2][1] * v[1] as f64 + t[2][2] * v[2] as f64 + t[2][3];
            out.push(V { p: [x as f32, y as f32, z as f32], uv: [v[3], v[4]], c: col });
        }
    }
}

fn submit(gl: &Gl, tex: u32, v: &[V], alpha: bool) {
    if v.is_empty() {
        return;
    }
    unsafe {
        (gl.BindTexture)(TEXTURE_2D, tex);
        if alpha {
            (gl.Enable)(ALPHA_TEST);
        } else {
            (gl.Disable)(ALPHA_TEST);
        }
        (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, v.len() as i32);
    }
}

/// Draws the players EntityTable::draw left out: their own skin, and their cape
/// (`Avatar.isModelPartShown(CAPE)`, entity data 16 bit 0). One draw call per texture.
pub fn draw(gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, crate::entities::Entity>, partial: f32, skip: i32, table: &crate::entities::EntityTable, items: &[String], lit: &crate::lightmap::Lit) {
    let s = st();
    if s.profiles.is_empty() {
        return;
    }
    let mut v: Vec<V> = Vec::new();
    let mut started = false;
    for (&id, e) in ents.iter() {
        if id == skip || e.kind != table.player_type || !custom(&e.uuid) {
            continue;
        }
        let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
        let at = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
        if at[0] * at[0] + at[2] * at[2] > 96.0 * 96.0 {
            continue;
        }
        if !started {
            started = true;
            unsafe {
                (gl.MatrixMode)(MODELVIEW);
                (gl.LoadMatrixf)(view.as_ptr());
                (gl.Enable)(TEXTURE_2D);
                (gl.AlphaFunc)(GREATER, 0.1);
                (gl.Disable)(CULL_FACE);
                (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
                (gl.BindBuffer)(ARRAY_BUFFER, 0);
            }
        }
        let lc = table.light(e, [at[0] + eye[0], at[1] + eye[1], at[2] + eye[2]], lit);
        // The body: its own skin, or the default one (with a cape over it).
        v.clear();
        match skin_of(&e.uuid) {
            Some((tex, slim)) => {
                for q in table.player_quads(e, at, partial, slim, items) {
                    v.push(V { p: q.0, uv: q.1, c: crate::lightmap::tint(q.2, lc) });
                }
                submit(gl, tex, &v, true);
            }
            None => {
                let i = crate::entities::EntityTable::skin_index(&e.uuid);
                let (aw, ah) = table.atlas_size();
                if let Some(r) = table.skins.get(i) {
                    for q in table.player_quads(e, at, partial, i < 9, items) {
                        v.push(V { p: q.0, uv: [(r[0] as f32 + q.1[0] * r[2] as f32) / aw, (r[1] as f32 + q.1[1] * r[3] as f32) / ah], c: crate::lightmap::tint(q.2, lc) });
                    }
                    submit(gl, table.texture(), &v, true);
                }
            }
        }
        let shown = e.data.get(&16).map_or(true, |b| b & 1 != 0);
        if let (Some(tex), true) = (cape_of(&e.uuid), shown) {
            let c = s.cloaks.get(&id).copied().unwrap_or(Cloak { at: e.pos, prev: e.prev, ..Cloak::default() });
            v.clear();
            cape_quads(&mut v, e.body_rot(partial), at, cape_angles(e, &c, partial));
            for q in v.iter_mut() {
                q.c = crate::lightmap::tint(q.c, lc);
            }
            submit(gl, tex, &v, false); // RenderTypes.entitySolid
        }
    }
    if started {
        unsafe {
            (gl.Enable)(CULL_FACE);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
    }
}

/// The cape as a GUI model (gui::Gui::entity), standing still: PlayerCapeModel's body and
/// cape with capeFlap and capeLean 0, so the cape hangs 6 degrees back.
pub fn gui_cape_model() -> crate::gui::Model {
    use crate::gui::{Model, Part};
    // rotateX(6) after yRot 180 is the Euler pose yRot 180, xRot -6.
    let cape = Part {
        name: String::from("cape"),
        parent: 0,
        pose: [0.0, 0.0, 2.0, -(math::to_rad(6.0) as f32), math::PI as f32, 0.0, 1.0, 1.0, 1.0],
        polys: CAPE.to_vec(),
    };
    let body = Part { name: String::from("body"), parent: -1, pose: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0], polys: Vec::new() };
    Model { name: String::from("penryn:cape"), parts: alloc::vec![body, cape] }
}

// ------------------------------------------------------------------ selftest

/// Checks for `penryn selftest`: (what, got, want).
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    // A textures value as the session server writes it (jeb_'s, with a slim model added).
    let json = r#"{"timestamp":1,"profileId":"853c80ef3c3749fdaa49938b674adae6","profileName":"jeb_","textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/7fd9ba42a7c81eeea22f1524271ae85a8e045ce0af5a6ae16c6406ae917e68b5","metadata":{"model":"slim"}},"CAPE":{"url":"http://textures.minecraft.net/texture/9e507afc56359978a3eb3e32367042b853cddd0995d17d0da995662913fb00f7"}}}"#;
    let b64 = encode64(json.as_bytes());
    let p = parse_textures(&b64);
    out.push(("skin textures property", format!("{:?}", p.as_ref().map(|p| (p.slim, p.skin.as_deref().and_then(texture_hash).map(|h| h.len()), p.cape.is_some()))), "Some((true, Some(64), true))"));
    let plain = encode64(br#"{"textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680"}}}"#);
    let p = parse_textures(&plain);
    out.push(("classic skin, no cape", format!("{:?}", p.map(|p| (p.slim, p.cape.is_none()))), "Some((false, true))"));
    out.push(("other hosts refused", format!("{:?}", texture_hash("http://example.com/texture/0123456789abcdef")), "None"));
    // A legacy 64 x 32 skin through the PNG decoder (RGBA, the Sub and Up filters).
    let mut raw = Vec::new();
    for y in 0..32u32 {
        raw.push(if y % 2 == 0 { 1 } else { 2 });
        for x in 0..64u32 {
            let px = [x as u8 * 4, y as u8 * 8, 77, if x >= 32 && y < 16 { 0 } else { 255 }];
            for k in 0..4 {
                // Filtered as the encoder would: Sub against the left pixel, Up against the row above.
                let left = if x > 0 { [(x - 1) as u8 * 4, y as u8 * 8, 77, if x - 1 >= 32 && y < 16 { 0 } else { 255 }][k] } else { 0 };
                let up = if y > 0 { [x as u8 * 4, (y - 1) as u8 * 8, 77, if x >= 32 && y - 1 < 16 { 0 } else { 255 }][k] } else { 0 };
                raw.push(px[k].wrapping_sub(if y % 2 == 0 { left } else { up }));
            }
        }
    }
    let file = png_file(64, 32, &raw);
    let img = png(&file);
    out.push(("PNG decode", format!("{:?}", img.as_ref().map(|i| (i.0, i.1, &i.2[(5 * 64 + 9) * 4..(5 * 64 + 9) * 4 + 4]))), "Some((64, 32, [36, 40, 77, 255]))"));
    // Broken files are refused, never a crash: a short IHDR, a PLTE of 4 bytes, no IHDR, cut short.
    let mut short = b"\x89PNG\r\n\x1a\n".to_vec();
    short.extend_from_slice(&[0, 0, 0, 2, b'I', b'H', b'D', b'R', 0, 0, 0, 0, 0, 0, 0, 4, b'P', b'L', b'T', b'E', 1, 2, 3, 4, 0, 0, 0, 0]);
    short.resize(40, 0);
    let mut no_ihdr = file.clone();
    no_ihdr[12..16].copy_from_slice(b"iHDR");
    let cut = &file[..file.len() / 2];
    out.push(("broken PNGs refused", format!("{:?}", (png(&short).is_none(), png(&no_ihdr).is_none(), png(cut).is_none())), "(true, true, true)"));
    let skin = img.and_then(|(w, h, px)| process_skin(w, h, px));
    // The right leg's front (4, 20) mirrored onto the left leg's front (20, 52): x 4 lands at 23.
    let at = |s: &Vec<u8>, x: usize, y: usize| [s[(y * 64 + x) * 4], s[(y * 64 + x) * 4 + 1], s[(y * 64 + x) * 4 + 3]];
    out.push(("legacy skin to 64 x 64", format!("{:?}", skin.as_ref().map(|s| (s.len(), at(s, 23, 52), at(s, 40, 5)))), "Some((16384, [16, 160, 255], [160, 40, 0]))"));
    out
}

fn encode64(d: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// A PNG file around filtered RGBA rows (a stored zlib stream).
fn png_file(w: u32, h: u32, raw: &[u8]) -> Vec<u8> {
    let mut f = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        f.extend_from_slice(&(body.len() as u32).to_be_bytes());
        f.extend_from_slice(kind);
        f.extend_from_slice(body);
        f.extend_from_slice(&[0, 0, 0, 0]); // the CRC is not checked
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &crate::zlib::store(raw));
    chunk(b"IEND", &[]);
    f
}

/// The inventory's player (renderEntityInInventoryFollowsMouse) with the player's own skin
/// and cape when downloaded; `model` and `sprite` the default skin otherwise.
pub fn gui_entity(gui: &mut crate::gui::Gui, uuid: &[u8; 16], model: &str, sprite: &str, x: f32, y: f32, scale: f32, body: f32, head: f32, pitch: f32) {
    // The cape first: it hangs behind the body (the GUI draws without a depth buffer).
    if let Some(tex) = cape_of(uuid) {
        if !gui.models.iter().any(|m| m.name == "penryn:cape") {
            gui.models.push(gui_cape_model());
        }
        gui.entity_tex("penryn:cape", tex, x, y, scale, body, 0.0, 0.0);
    }
    match skin_of(uuid) {
        Some((tex, slim)) => gui.entity_tex(if slim { "minecraft:player_slim#main" } else { "minecraft:player#main" }, tex, x, y, scale, body, head, pitch),
        None => gui.entity(model, sprite, x, y, scale, body, head, pitch),
    }
}
