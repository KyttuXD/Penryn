//! Penryn's start as vanilla's: the title screen (TitleScreen with the turning panorama of
//! Panorama/CubeMap, LogoRenderer's logo and edition, SplashRenderer's yellow splash), the
//! options screens from it, the multiplayer screens (multiplayer.rs), ConnectScreen, the
//! disconnected screen and the sign-in through the relay. The pictures are in menu.bin
//! (optional: without it the screens are drawn on a dark background).

use crate::client::{Account, Game, Login};
use crate::gui::Gui;
use crate::lang;
use crate::multiplayer::{self, fl, Mp};
use crate::screens::{self, Button, WHITE};
use crate::sdl::{self, Gl, Video};
use crate::sys;
use crate::task::{self, Sendable, Task};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;

pub const MENU_MAGIC: &[u8; 4] = b"PNM1";
const GL_RGB: u32 = 0x1907;
const LINEAR: i32 = 0x2601;
const CLAMP_TO_EDGE: i32 = 0x812F;
const WRAP_S: u32 = 0x2802;
const WRAP_T: u32 = 0x2803;
const VERSION_NAME: &str = "26.3";

// ------------------------------------------------------------------ menu.bin

pub struct Image {
    pub tex: u32,
    pub w: f32,
    pub h: f32,
}

pub struct Assets {
    faces: [u32; 6],
    have_faces: bool,
    images: BTreeMap<String, Image>,
    /// Sprite: (texture, uv rectangle).
    sprites: BTreeMap<String, (u32, [f32; 4])>,
    pub splashes: Vec<String>,
}

struct State {
    assets: Option<Assets>,
    tried: bool,
    /// Panorama.spin (degrees) and when it was last turned.
    spin: f32,
    last: f64,
    /// LogoRenderer.showEasterEgg: "Minceraft", one start in 10000.
    easter: bool,
    /// The blurred background: its texture, the small picture read back and a scratch row.
    blur_tex: u32,
    blur_px: Vec<u8>,
    blur_tmp: Vec<u8>,
}
struct Main(UnsafeCell<State>);
unsafe impl Sync for Main {}
static STATE: Main = Main(UnsafeCell::new(State { assets: None, tried: false, spin: 0.0, last: 0.0, easter: false, blur_tex: 0, blur_px: Vec::new(), blur_tmp: Vec::new() }));

/// The main thread's menu state (only the main thread draws).
fn st() -> &'static mut State {
    unsafe { &mut *STATE.0.get() }
}

pub fn new_texture(gl: &Gl) -> u32 {
    let mut t = 0u32;
    unsafe { (gl.GenTextures)(1, &mut t) };
    t
}

/// Uploads pixels (3 or 4 channels) to texture `t`: smooth and clamped, or crisp and tiled.
pub fn upload(gl: &Gl, t: u32, w: usize, h: usize, px: &[u8], channels: u8, smooth: bool) {
    unsafe {
        (gl.BindTexture)(sdl::TEXTURE_2D, t);
        (gl.PixelStorei)(sdl::UNPACK_ALIGNMENT, 1);
        let f = if smooth { LINEAR } else { sdl::NEAREST };
        (gl.TexParameteri)(sdl::TEXTURE_2D, sdl::TEXTURE_MAG_FILTER, f);
        (gl.TexParameteri)(sdl::TEXTURE_2D, sdl::TEXTURE_MIN_FILTER, f);
        (gl.TexParameteri)(sdl::TEXTURE_2D, sdl::TEXTURE_MAX_LEVEL, 0);
        if smooth {
            (gl.TexParameteri)(sdl::TEXTURE_2D, WRAP_S, CLAMP_TO_EDGE);
            (gl.TexParameteri)(sdl::TEXTURE_2D, WRAP_T, CLAMP_TO_EDGE);
        }
        let fmt = if channels == 3 { GL_RGB } else { sdl::RGBA };
        (gl.TexImage2D)(sdl::TEXTURE_2D, 0, fmt as i32, w as i32, h as i32, 0, fmt, sdl::UNSIGNED_BYTE, px.as_ptr() as *const c_void);
    }
}

fn parse(gl: &Gl, d: &[u8]) -> Option<Assets> {
    if d.len() < 8 || &d[..4] != MENU_MAGIC {
        return None;
    }
    let raw_len = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
    let raw = crate::zlib::inflate(&d[8..], raw_len)?;
    let r16 = |o: usize| raw.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let n = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let text = String::from_utf8_lossy(raw.get(4..4 + n)?).into_owned();
    // SplashManager.prepare: trimmed lines, without the one whose hash is 125780783.
    let java_hash = |s: &str| s.encode_utf16().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32));
    let splashes = text.lines().map(|l| String::from(l.trim())).filter(|l| java_hash(l) != 125780783).collect();
    let mut p = 4 + n;
    let count = r16(p)?;
    p += 2;
    let mut images = BTreeMap::new();
    let mut by_index = Vec::new();
    for _ in 0..count {
        let l = *raw.get(p)? as usize;
        let name = String::from_utf8_lossy(raw.get(p + 1..p + 1 + l)?).into_owned();
        p += 1 + l;
        let (w, h, ch) = (r16(p)?, r16(p + 2)?, *raw.get(p + 4)?);
        p += 5;
        let size = w * h * ch as usize;
        let px = raw.get(p..p + size)?;
        p += size;
        let t = new_texture(gl);
        upload(gl, t, w, h, px, ch, name.starts_with("panorama"));
        by_index.push((t, w as f32, h as f32));
        images.insert(name, Image { tex: t, w: w as f32, h: h as f32 });
    }
    let count = r16(p)?;
    p += 2;
    let mut sprites = BTreeMap::new();
    for _ in 0..count {
        let l = *raw.get(p)? as usize;
        let name = String::from_utf8_lossy(raw.get(p + 1..p + 1 + l)?).into_owned();
        p += 1 + l;
        let img = *raw.get(p)? as usize;
        let (x, y, w, h) = (r16(p + 1)? as f32, r16(p + 3)? as f32, r16(p + 5)? as f32, r16(p + 7)? as f32);
        p += 9;
        let (t, aw, ah) = *by_index.get(img)?;
        sprites.insert(name, (t, [x / aw, y / ah, (x + w) / aw, (y + h) / ah]));
    }
    let face = |prefix: &str, i: usize| images.get(&format!("{}{}", prefix, i)).map(|im| im.tex);
    let have_faces = (0..6).all(|i| face("panorama_", i).is_some());
    let mut faces = [0u32; 6];
    if have_faces {
        for i in 0..6 {
            faces[i] = face("panorama_", i).unwrap_or(0);
        }
    }
    Some(Assets { faces, have_faces, images, sprites, splashes })
}

/// menu.bin's pictures (loaded once; None without the file).
pub fn assets(gl: &Gl) -> Option<&'static Assets> {
    let s = st();
    if !s.tried {
        s.tried = true;
        let path = format!("{}/menu.bin", crate::data_dir());
        s.assets = sys::read_file(&path).and_then(|d| parse(gl, &d));
        if s.assets.is_none() {
            crate::say!("no menu.bin (or of another version): the menus are drawn without the panorama and logo");
        }
    }
    s.assets.as_ref()
}

/// A picture of menu.bin, stretched to w x h.
pub fn image(gui: &mut Gui, a: Option<&Assets>, name: &str, x: f32, y: f32, w: f32, h: f32) {
    if let Some(im) = a.and_then(|a| a.images.get(name)) {
        gui.blit_tex(im.tex, x, y, w, h, [0.0, 0.0, 1.0, 1.0], WHITE);
    }
}

/// A small sprite of menu.bin's atlas (server list icons...).
pub fn sprite(gui: &mut Gui, a: Option<&Assets>, name: &str, x: f32, y: f32, w: f32, h: f32) {
    if let Some((t, uv)) = a.and_then(|a| a.sprites.get(name)) {
        gui.blit_tex(*t, x, y, w, h, *uv, WHITE);
    }
}

/// A texture tiled at 32 x 32 units (vanilla's menu backgrounds), its v starting at `v0`.
fn tiled(gui: &mut Gui, a: Option<&Assets>, name: &str, x0: f32, y0: f32, x1: f32, y1: f32, v0: f32, alpha: u32) {
    if let Some(im) = a.and_then(|a| a.images.get(name)) {
        let (tw, th) = (im.w.max(1.0), im.h.max(1.0));
        gui.blit_tex(im.tex, x0, y0, x1 - x0, y1 - y0, [x0 / tw, (y0 + v0) / th, x1 / tw, (y1 + v0) / th], alpha << 24 | 0xFFFFFF);
    }
}

/// AbstractSelectionList.extractListBackground: menu_list_background under the rows.
pub fn list_background(gui: &mut Gui, a: Option<&Assets>, top: f32, bottom: f32, scroll: f32) {
    let w = gui.width;
    if a.map_or(false, |a| a.images.contains_key("menu_list_background")) {
        tiled(gui, a, "menu_list_background", 0.0, top, w, bottom, fl(scroll), 0xFF);
    } else {
        gui.fill(0.0, top, w, bottom, 0x7000_0000);
    }
}

/// The list's header and footer separators (2 units, over its top and bottom edges).
pub fn list_frame(gui: &mut Gui, a: Option<&Assets>, top: f32, bottom: f32) {
    let w = gui.width;
    if a.map_or(false, |a| a.images.contains_key("header_separator")) {
        tiled(gui, a, "header_separator", 0.0, top - 2.0, w, top, -(top - 2.0), 0xFF);
        tiled(gui, a, "footer_separator", 0.0, bottom, w, bottom + 2.0, -bottom, 0xFF);
    } else {
        gui.fill(0.0, top - 2.0, w, top - 1.0, 0x33FF_FFFF);
        gui.fill(0.0, top - 1.0, w, top, 0xBF00_0000);
        gui.fill(0.0, bottom, w, bottom + 1.0, 0xBF00_0000);
        gui.fill(0.0, bottom + 1.0, w, bottom + 2.0, 0x33FF_FFFF);
    }
}

// ------------------------------------------------------------------ the panorama

fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut m = [0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            m[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    m
}

fn rot_x(deg: f64) -> [f32; 16] {
    let (s, c) = (crate::math::sin(crate::math::to_rad(deg)) as f32, crate::math::cos(crate::math::to_rad(deg)) as f32);
    [1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0]
}

fn rot_y(deg: f64) -> [f32; 16] {
    let (s, c) = (crate::math::sin(crate::math::to_rad(deg)) as f32, crate::math::cos(crate::math::to_rad(deg)) as f32);
    [c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// CubeMap's six faces: corners and their uv (panorama_0 is in front, then clockwise; 4 is
/// below, 5 above).
const FACES: [[[f32; 5]; 4]; 6] = [
    [[-1.0, -1.0, 1.0, 0.0, 0.0], [-1.0, 1.0, 1.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0, 1.0], [1.0, -1.0, 1.0, 1.0, 0.0]],
    [[1.0, -1.0, 1.0, 0.0, 0.0], [1.0, 1.0, 1.0, 0.0, 1.0], [1.0, 1.0, -1.0, 1.0, 1.0], [1.0, -1.0, -1.0, 1.0, 0.0]],
    [[1.0, -1.0, -1.0, 0.0, 0.0], [1.0, 1.0, -1.0, 0.0, 1.0], [-1.0, 1.0, -1.0, 1.0, 1.0], [-1.0, -1.0, -1.0, 1.0, 0.0]],
    [[-1.0, -1.0, -1.0, 0.0, 0.0], [-1.0, 1.0, -1.0, 0.0, 1.0], [-1.0, 1.0, 1.0, 1.0, 1.0], [-1.0, -1.0, 1.0, 1.0, 0.0]],
    [[-1.0, -1.0, -1.0, 0.0, 0.0], [-1.0, -1.0, 1.0, 0.0, 1.0], [1.0, -1.0, 1.0, 1.0, 1.0], [1.0, -1.0, -1.0, 1.0, 0.0]],
    [[-1.0, 1.0, 1.0, 0.0, 0.0], [-1.0, 1.0, -1.0, 0.0, 1.0], [1.0, 1.0, -1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0, 0.0]],
];

/// Panorama.extractRenderState: the spin goes on by 0.1 degree a tick times the Panorama
/// Scroll Speed, and the cube is drawn with an 85-degree field of view, 10 degrees down.
pub fn panorama(gl: &Gl, w: i32, h: i32, speed: f64) {
    panorama_in(gl, w, h, w, h, speed);
}

/// The panorama drawn into the lower left `vw` x `vh` pixels of a `w` x `h` window.
fn panorama_in(gl: &Gl, w: i32, h: i32, vw: i32, vh: i32, speed: f64) {
    let s = st();
    let now = sys::now();
    if s.last > 0.0 {
        let ticks = ((now - s.last).clamp(0.0, 1.0) * 20.0) as f32;
        s.spin = crate::math::wrap((s.spin + ticks * speed as f32 * 0.1) as f64, 360.0) as f32;
    }
    s.last = now;
    unsafe {
        (gl.Viewport)(0, 0, w, h);
        (gl.ClearColor)(0.0, 0.0, 0.0, 1.0);
        (gl.Clear)(sdl::COLOR_BUFFER_BIT | sdl::DEPTH_BUFFER_BIT);
        (gl.Viewport)(0, 0, vw, vh);
    }
    let Some(a) = s.assets.as_ref().filter(|a| a.have_faces) else { return };
    let proj = crate::render::perspective_pub(85.0, w as f32 / h.max(1) as f32, 0.05, 10.0);
    let view = mat_mul(&rot_x(190.0), &rot_y(-s.spin as f64));
    static WHITE4: [u8; 16] = [255; 16];
    unsafe {
        (gl.MatrixMode)(sdl::PROJECTION);
        (gl.LoadMatrixf)(proj.as_ptr());
        (gl.MatrixMode)(sdl::MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Disable)(sdl::DEPTH_TEST);
        (gl.Disable)(sdl::CULL_FACE);
        (gl.Disable)(sdl::FOG);
        (gl.Disable)(sdl::BLEND);
        (gl.Disable)(sdl::ALPHA_TEST);
        (gl.Enable)(sdl::TEXTURE_2D);
        (gl.BindBuffer)(sdl::ARRAY_BUFFER, 0);
        (gl.EnableClientState)(sdl::VERTEX_ARRAY);
        (gl.EnableClientState)(sdl::TEXTURE_COORD_ARRAY);
        (gl.EnableClientState)(sdl::COLOR_ARRAY);
        (gl.ColorPointer)(4, sdl::UNSIGNED_BYTE, 4, WHITE4.as_ptr() as *const c_void);
        for (i, f) in FACES.iter().enumerate() {
            (gl.BindTexture)(sdl::TEXTURE_2D, a.faces[i]);
            (gl.VertexPointer)(3, sdl::FLOAT, 20, f.as_ptr() as *const c_void);
            (gl.TexCoordPointer)(2, sdl::FLOAT, 20, (f.as_ptr() as *const f32).add(3) as *const c_void);
            (gl.DrawArrays)(sdl::QUADS, 0, 4);
        }
        (gl.DisableClientState)(sdl::TEXTURE_COORD_ARRAY);
        (gl.Disable)(sdl::TEXTURE_2D);
    }
}

/// One box blur of radius 1 along rows (`dx` 4) or columns (`dx` the row's bytes), edges
/// clamped.
fn box_blur(px: &mut [u8], tmp: &mut Vec<u8>, w: usize, h: usize, along_rows: bool) {
    tmp.clear();
    tmp.extend_from_slice(px);
    let (n, count, step, stride) = if along_rows { (w, h, 4, w * 4) } else { (h, w, w * 4, 4) };
    for line in 0..count {
        let base = line * stride;
        for i in 0..n {
            let a = base + i.saturating_sub(1) * step;
            let b = base + i * step;
            let c = base + (i + 1).min(n - 1) * step;
            for k in 0..3 {
                px[b + k] = ((tmp[a + k] as u16 + tmp[b + k] as u16 + tmp[c + k] as u16 + 1) / 3) as u8;
            }
        }
    }
}

/// GameRenderer.processBlurEffect: vanilla blurs the screen with three box blurs of the Menu
/// Background Blur radius in each direction. Here the panorama is drawn small, read back,
/// blurred three times (radius 1) and stretched smoothly over the window: the same spread
/// for a fraction of the work on an old GPU.
fn blurred_panorama(gl: &Gl, gui: &mut Gui, w: i32, h: i32, speed: f64, radius: i32) {
    let r = radius.max(1) as f64;
    let d = ((r * (r + 1.0)) / 2.0).max(1.0);
    let d = (crate::math::sqrt(d) + 0.5) as i32;
    let d = (d.max(1)).min(8);
    let (sw, sh) = ((w / d).max(1), (h / d).max(1));
    panorama_in(gl, w, h, sw, sh, speed);
    let s = st();
    if s.assets.as_ref().map_or(true, |a| !a.have_faces) {
        return;
    }
    let n = (sw * sh * 4) as usize;
    s.blur_px.resize(n, 0);
    unsafe {
        (gl.PixelStorei)(sdl::PACK_ALIGNMENT, 1);
        (gl.ReadPixels)(0, 0, sw, sh, sdl::RGBA, sdl::UNSIGNED_BYTE, s.blur_px.as_mut_ptr() as *mut c_void);
        (gl.Viewport)(0, 0, w, h);
        (gl.Clear)(sdl::COLOR_BUFFER_BIT);
    }
    for _ in 0..3 {
        box_blur(&mut s.blur_px, &mut s.blur_tmp, sw as usize, sh as usize, true);
        box_blur(&mut s.blur_px, &mut s.blur_tmp, sw as usize, sh as usize, false);
    }
    if s.blur_tex == 0 {
        s.blur_tex = new_texture(gl);
    }
    upload(gl, s.blur_tex, sw as usize, sh as usize, &s.blur_px, 4, true);
    // Read back bottom row first: drawn upside down.
    gui.blit_tex(s.blur_tex, 0.0, 0.0, gui.width, gui.height, [0.0, 1.0, 1.0, 0.0], WHITE);
}

/// Screen.extractBackground without a world: the panorama, blurred when the Menu Background
/// Blur option is on, then menu_background over it.
pub fn menu_background(gl: &Gl, gui: &mut Gui, w: i32, h: i32, speed: f64, blur: i32) {
    if blur > 0 {
        blurred_panorama(gl, gui, w, h, speed, blur);
    } else {
        panorama(gl, w, h, speed);
    }
    let a = st().assets.as_ref();
    let (gw, gh) = (gui.width, gui.height);
    if a.map_or(false, |a| a.images.contains_key("menu_background")) {
        tiled(gui, a, "menu_background", 0.0, 0.0, gw, gh, 0.0, 0xFF);
    } else if a.map_or(true, |a| !a.have_faces) {
        gui.fill(0.0, 0.0, gw, gh, 0xFF20_2020);
    }
}

/// LevelLoadingScreen while the first chunks come: "Loading terrain..." over the menu
/// background (play.rs calls this until the client is in its world).
pub fn draw_loading(v: &Video, gui: &mut Gui, speed: f64, blur: i32) {
    let (w, h) = v.size();
    assets(&v.gl);
    gui.begin(w, h);
    menu_background(&v.gl, gui, w, h, speed, blur);
    let (gw, gh) = (gui.width, gui.height);
    gui.text_centered(&lang::get("multiplayer.downloadingTerrain"), fl(gw / 2.0), fl(gh / 2.0) - 50.0, WHITE);
    gui.flush(&v.gl, w, h);
    v.swap();
}

// ------------------------------------------------------------------ the title screen

/// SplashManager.getSplash: the date's own splash, else one of splashes.txt (now and then
/// the player's name).
pub fn choose_splash(splashes: &[String], date: (u32, u32), name: &str, random: u32) -> Option<String> {
    match date {
        (12, 24) => return Some(String::from("Merry X-mas!")),
        (1, 1) => return Some(String::from("Happy new year!")),
        (10, 31) => return Some(String::from("OOoooOOOoooo! Spooky!")),
        _ => {}
    }
    if splashes.is_empty() {
        return None;
    }
    let n = splashes.len() as u32;
    if !name.is_empty() && (random >> 16) % n == 42 {
        return Some(format!("{} IS YOU", name.to_uppercase()));
    }
    Some(splashes[(random % n) as usize].clone())
}

fn random_u32() -> u32 {
    sys::read_urandom(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or((sys::now() * 1e6) as u32)
}

/// TitleScreen.init's widgets: Singleplayer, Multiplayer, Realms, then the icon row (friends,
/// language, accessibility) and Options... / Quit Game.
pub fn title_buttons(gw: f32, gh: f32) -> [Button; 8] {
    let x = fl(gw / 2.0) - 100.0;
    let y = fl(gh / 4.0) + 48.0;
    let icon_x = |i: f32| fl(gw / 2.0) - 34.0 + (i - 1.0) * 24.0;
    [
        Button::new(x, y, 200.0, lang::get("menu.singleplayer")).off(),
        Button::new(x, y + 24.0, 200.0, lang::get("menu.multiplayer")),
        Button::new(x, y + 48.0, 200.0, lang::get("menu.online")).off(),
        Button::new(icon_x(1.0), y + 72.0, 20.0, String::new()).off(),
        Button::new(icon_x(2.0), y + 72.0, 20.0, String::new()).off(),
        Button::new(icon_x(3.0), y + 72.0, 20.0, String::new()),
        Button::new(x, y + 96.0, 98.0, lang::get("menu.options")),
        Button::new(fl(gw / 2.0) + 2.0, y + 96.0, 98.0, lang::get("menu.quit")),
    ]
}

fn over(b: &Button, mx: f32, my: f32) -> bool {
    mx >= b.x && mx < b.x + b.w && my >= b.y && my < b.y + b.h
}

/// SplashRenderer: the splash at (width / 2 + 123, 69), turned 20 degrees up, pulsing.
fn draw_splash(gui: &mut Gui, splash: &str, alpha: u8) {
    let tw = gui.text_width(splash);
    let t = (sys::now_ms() % 1000) as f64 / 1000.0 * core::f64::consts::TAU;
    let s = 1.8 - (crate::math::sin(t) * 0.1).abs() as f32;
    let scale = s * 100.0 / (tw + 32.0);
    let (ox, oy) = (fl(gui.width / 2.0) + 123.0, 69.0);
    let (sn, cs) = (crate::math::sin(-0.349_065_84) as f32, crate::math::cos(-0.349_065_84) as f32);
    let place = |x: f32, y: f32| -> [f32; 2] {
        let (x, y) = (x * scale, y * scale);
        [ox + x * cs - y * sn, oy + x * sn + y * cs]
    };
    let x0 = -fl(tw / 2.0);
    let mut quads: Vec<([f32; 4], [f32; 4])> = Vec::new();
    gui.glyph_quads(splash, |a, b, c, d, uv| quads.push(([a, b, c, d], uv)));
    for (shadow, colour) in [(1.0f32, 0x3F3F_00u32), (0.0, 0xFFFF_00)] {
        for (r, uv) in quads.iter() {
            let (a0, b0, a1, b1) = (x0 + r[0] + shadow, -8.0 + r[1] + shadow, x0 + r[2] + shadow, -8.0 + r[3] + shadow);
            gui.quad4([place(a0, b0), place(a0, b1), place(a1, b1), place(a1, b0)], *uv, (alpha as u32) << 24 | colour);
        }
    }
}

/// The title screen's own drawing: logo, edition, splash, version, copyright, the widgets.
fn draw_title(gui: &mut Gui, a: Option<&Assets>, splash: Option<&str>, fade: f32, mx: f32, my: f32, account: &str, sign_in: Option<&Button>) {
    let (gw, gh) = (gui.width, gui.height);
    let alpha = (fade.clamp(0.0, 1.0) * 255.0) as u8;
    gui.fade = alpha;
    // LogoRenderer: 256 x 44 of a 256 x 64 picture at (width / 2 - 128, 30), the edition
    // 128 x 14 under it, overlapping by 7.
    let lx = fl(gw / 2.0) - 128.0;
    let easter = st().easter;
    if let Some(im) = a.and_then(|a| a.images.get(if easter { "minceraft" } else { "minecraft" })) {
        gui.blit_tex(im.tex, lx, 30.0, 256.0, 44.0, [0.0, 0.0, 1.0, 44.0 / 64.0], (alpha as u32) << 24 | 0xFFFFFF);
    } else {
        gui.text_centered("MINECRAFT", fl(gw / 2.0), 46.0, WHITE);
    }
    if let Some(im) = a.and_then(|a| a.images.get("edition")) {
        gui.blit_tex(im.tex, fl(gw / 2.0) - 64.0, 30.0 + 44.0 - 7.0, 128.0, 14.0, [0.0, 0.0, 1.0, 14.0 / 16.0], (alpha as u32) << 24 | 0xFFFFFF);
    }
    let buttons = title_buttons(gw, gh);
    for (i, b) in buttons.iter().enumerate() {
        b.draw(gui, mx, my);
        let icon = match i {
            3 => "friends/friends",
            4 => "icon/language",
            5 => "icon/accessibility",
            _ => continue,
        };
        sprite(gui, a, icon, b.x + 2.0, b.y + 2.0, 15.0, 15.0);
    }
    if let Some(b) = sign_in {
        b.draw(gui, mx, my);
    }
    if let Some(s) = splash {
        draw_splash(gui, s, alpha);
    }
    let version = format!("Minecraft {}", VERSION_NAME);
    gui.text(&version, 2.0, gh - 10.0, (alpha as u32) << 24 | 0xFFFFFF);
    let credits = lang::get("title.credits");
    let cw = gui.text_width(&credits);
    let cx = gw - cw - 2.0;
    gui.text(&credits, cx, gh - 10.0, (alpha as u32) << 24 | 0xFFFFFF);
    if mx >= cx && mx < cx + cw && my >= gh - 10.0 && my < gh {
        gui.fill(cx, gh - 1.0, cx + cw, gh, (alpha as u32) << 24 | 0xFFFFFF);
    }
    gui.text(account, 2.0, 2.0, (alpha as u32) << 24 | 0xA0A0A0);
    gui.fade = 255;
    // Tooltips (inactive buttons show theirs too).
    let tip: Option<Vec<String>> = if over(&buttons[0], mx, my) {
        Some(alloc::vec![String::from("Penryn has no integrated server:"), String::from("play on a server with Multiplayer.")])
    } else if over(&buttons[2], mx, my) {
        Some(alloc::vec![String::from("Realms are not available in Penryn.")])
    } else if over(&buttons[3], mx, my) {
        Some(alloc::vec![lang::get("gui.friends.open.tooltip")])
    } else if over(&buttons[4], mx, my) {
        Some(alloc::vec![lang::get("options.language.tooltip")])
    } else if over(&buttons[5], mx, my) {
        Some(alloc::vec![lang::get("options.accessibility.tooltip")])
    } else if mx < 2.0 + gui.text_width(&version) && my >= gh - 10.0 {
        Some(alloc::vec![String::from("Penryn: a no_std Rust client"), format!("protocol {}", crate::ids::PROTOCOL)])
    } else {
        None
    };
    if let Some(t) = tip {
        screens::tooltip(gui, &t, mx, my);
    }
}

// ------------------------------------------------------------------ the screens' state

/// What happens after the menus.
pub enum Next {
    Quit,
    /// The login went through: the window finishes the join (the packs' configuration, play.rs),
    /// to the address it was made at, and back to the server list when it fails.
    Join(Login, String, bool),
    /// Options > Resource Packs: the selection screen (packs.rs), then the menus again.
    Packs,
}

/// What the first sign-in step gave: the account (Horizon's launcher had one), or Microsoft's
/// device code to enter (session, code, where, poll seconds).
type Asked = Result<Result<Account, (String, String, String, u32)>, String>;

enum SignIn {
    Asking(Task<Asked>),
    /// The device code to enter, where, and the poll running now.
    Code { session: String, code: String, url: String, every: u32, next: u64, poll: Option<Task<Result<Option<Account>, String>>> },
    Failed(String),
}

enum Scr {
    Title,
    /// The options screens (options.rs), from the title screen.
    Options,
    Multiplayer,
    Connect { task: Task<Result<Sendable<Login>, String>>, address: String, back_to_list: bool },
    Disconnected { title: String, reason: String, to_list: bool },
    SignIn(SignIn),
}

/// The pieces of the window the menus use, lent by play.rs.
pub struct Ui<'a> {
    pub v: &'a Video,
    pub gui: &'a mut Gui,
    pub menu: &'a mut crate::options::Menu,
    pub audio: Option<&'a mut crate::audio::Audio>,
    pub shot: &'a Option<(String, u64)>,
}

pub struct Front {
    scr: Scr,
    pub account: Option<Account>,
    relay: Option<(String, u16)>,
    name: String,
    mp: Option<Mp>,
    splash: Option<String>,
    fade_from: Option<u64>,
    keys: [bool; 512],
    mouse: (f32, f32),
    music_delay: i32,
    /// PENRYN_TEST_INPUT's menu steps (mclick=X:Y, mtype=TEXT, mkey=NAME), one each half second.
    test: Vec<String>,
    test_next: u64,
    /// Whether SDL's text input is on (only switched when it changes).
    text_on: Option<bool>,
}

fn is_menu_step(t: &str) -> bool {
    t.starts_with("mclick=") || t.starts_with("mtype=") || t.starts_with("mkey=") || t.starts_with("mwait")
}

/// PENRYN_TEST_INPUT without the menus' steps (the game reads single letters from the rest).
pub fn game_test_keys(all: &str) -> String {
    let v: Vec<&str> = all.split(',').filter(|t| !is_menu_step(t)).collect();
    v.join(",")
}

const CONNECT_STAGES: [&str; 4] = ["connect.connecting", "connect.authorizing", "connect.encrypting", "connect.joining"];

impl Front {
    pub fn new(account: Option<Account>, relay: Option<(String, u16)>, name: &str, test_keys: &str) -> Front {
        Front {
            scr: Scr::Title,
            account,
            relay,
            name: String::from(name),
            mp: None,
            splash: None,
            fade_from: None,
            keys: [false; 512],
            mouse: (0.0, 0.0),
            music_delay: 20,
            test: test_keys.split(',').filter(|t| is_menu_step(t)).map(String::from).collect(),
            test_next: sys::now_ms() + 1500,
            text_on: None,
        }
    }

    fn player_name(&self) -> String {
        self.account.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| self.name.clone())
    }

    /// Starts joining `address` (ConnectScreen.startConnecting): the login runs on its own
    /// thread while the screen shows how far it got.
    pub fn connect(&mut self, address: &str, back_to_list: bool) {
        if crate::servers::address(address).is_none() {
            self.scr = Scr::Disconnected { title: lang::get("connect.failed"), reason: lang::get("disconnect.unknownHost"), to_list: back_to_list };
            return;
        }
        let acc = self.account.clone();
        let name = self.player_name();
        crate::client::JOIN_STAGE.store(0, core::sync::atomic::Ordering::Relaxed);
        // The SRV lookup blocks, so it runs on the join's thread with the login.
        let typed = String::from(address);
        let task = task::spawn(move || {
            let ((host, port), (dial_host, dial_port)) = crate::srv::dial(&typed).ok_or_else(|| String::from("unknown host"))?;
            Game::login_dialing(&host, port, &dial_host, dial_port, &name, acc.as_ref(), crate::serverui::Common::default(), false).map(Sendable)
        });
        self.scr = Scr::Connect { task, address: String::from(address), back_to_list };
    }

    /// Joining failed after the login (the configuration): the disconnected screen.
    pub fn join_failed(&mut self, back_to_list: bool, e: String) {
        let reason = String::from(
            e.strip_prefix("the server refused the login: ").or_else(|| e.strip_prefix("disconnected while joining: ")).unwrap_or(&e),
        );
        crate::warn!("could not join: {}", e);
        self.scr = Scr::Disconnected { title: lang::get("connect.failed"), reason, to_list: back_to_list };
    }

    /// The game ended: back to the menus (DisconnectedScreen when it was lost).
    pub fn after_game(&mut self, lost: Option<String>) {
        if let Some(mp) = self.mp.as_mut() {
            mp.reopen();
        }
        self.scr = match lost {
            Some(reason) => Scr::Disconnected { title: lang::get("disconnect.lost"), reason, to_list: true },
            None => Scr::Multiplayer,
        };
        if self.mp.is_none() {
            self.mp = Some(Mp::new(String::new()));
        }
    }

    fn open_multiplayer(&mut self, ui: &Ui) {
        let last = ui.menu.o.other("lastServer").map(String::from).unwrap_or_default();
        match self.mp.as_mut() {
            Some(mp) => mp.reopen(),
            None => self.mp = Some(Mp::new(last)),
        }
        self.scr = Scr::Multiplayer;
    }

    fn click_sound(ui: &mut Ui) {
        if let Some(a) = ui.audio.as_mut() {
            a.play_named("ui.button.click", None, 0.25, 1.0, 10);
        }
    }

    fn sign_in_button(&self, gw: f32) -> Option<Button> {
        let _ = gw;
        if self.account.is_some() || self.relay.is_none() {
            return None;
        }
        Some(Button::new(2.0, 12.0, 98.0, String::from("Sign in...")))
    }

    fn start_sign_in(&mut self) {
        let Some(relay) = self.relay.clone() else { return };
        // Through Horizon's launcher on the relay's computer when it has an account, else
        // Microsoft's device code.
        let task = task::spawn(move || match crate::account::login_launcher(&relay, None)? {
            Some(acc) => Ok(Ok(acc)),
            None => crate::account::start(&relay).map(Err),
        });
        self.scr = Scr::SignIn(SignIn::Asking(task));
    }

    /// Runs the menus until a game is joined or the window closes.
    pub fn run(&mut self, ui: &mut Ui) -> Next {
        let gl = &ui.v.gl;
        let a = assets(gl);
        if self.splash.is_none() {
            let splashes: &[String] = a.map_or(&[][..], |a| &a.splashes[..]);
            self.splash = choose_splash(splashes, sys::local_date(), &self.player_name(), random_u32());
            self.fade_from = Some(sys::now_ms());
            st().easter = random_u32() % 10000 == 0;
        }
        ui.v.grab_mouse(false);
        self.text_on = None;
        let mut next_tick = sys::now_ms();
        let mut frame_at = 0u64;
        loop {
            // ---- events
            while let Some(e) = ui.v.poll() {
                if let Some(n) = self.event(ui, &e) {
                    return n;
                }
            }
            // ---- options changed on the options screens
            self.apply_options(ui);
            // ---- ticks
            let now = sys::now_ms();
            if now > next_tick + 1000 {
                next_tick = now;
            }
            while now >= next_tick {
                next_tick += 50;
                if let Some(n) = self.tick(ui) {
                    return n;
                }
            }
            if let Some(a) = ui.audio.as_mut() {
                a.feed_music();
            }
            self.draw(ui);
            if let Some(shot) = ui.shot.as_ref() {
                if sys::now_ms() >= shot.1 {
                    let (w, h) = ui.v.size();
                    let png = sdl::screenshot_png(&ui.v.gl, w, h);
                    sys::write_file_private(&shot.0, &png);
                    crate::say!("screenshot: {}", shot.0);
                    return Next::Quit;
                }
            }
            ui.v.swap();
            // Minecraft.getFramerateLimit: 60 frames a second in the menus.
            let t = sys::now_ms();
            if t < frame_at + 16 {
                unsafe { (ui.v.sdl.SDL_Delay)((frame_at + 16 - t) as u32) };
            }
            frame_at = sys::now_ms();
        }
    }

    fn apply_options(&mut self, ui: &mut Ui) {
        use crate::options::{O, VOLUMES};
        if ui.menu.changed.is_empty() {
            return;
        }
        let mut keep: Vec<O> = Vec::new();
        for o in core::mem::take(&mut ui.menu.changed) {
            let opts = &ui.menu.o;
            match o {
                O::Fullscreen | O::ExclusiveFullscreen => {
                    let mode = if !opts.on(O::Fullscreen) {
                        0
                    } else if opts.on(O::ExclusiveFullscreen) {
                        sdl::WINDOW_FULLSCREEN
                    } else {
                        sdl::WINDOW_FULLSCREEN_DESKTOP
                    };
                    unsafe { (ui.v.sdl.SDL_SetWindowFullscreen)(ui.v.window, mode) };
                }
                O::Vsync => unsafe {
                    (ui.v.sdl.SDL_GL_SetSwapInterval)(opts.on(o) as i32);
                },
                O::GuiScale => ui.gui.fixed_scale = opts.gui_scale(),
                _ if VOLUMES.contains(&o) => {
                    let v = opts.volumes();
                    if let Some(a) = ui.audio.as_mut() {
                        a.set_volumes(v);
                    }
                }
                _ => {
                    // For the game to put into effect when it starts.
                    if !keep.contains(&o) {
                        keep.push(o);
                    }
                }
            }
        }
        ui.menu.changed = keep;
    }

    fn tick(&mut self, ui: &mut Ui) -> Option<Next> {
        // MusicManager with Musics.MENU: a song, then 1 to 30 seconds of quiet.
        if let Some(a) = ui.audio.as_mut() {
            if !a.music_playing() {
                self.music_delay -= 1;
                if self.music_delay <= 0 {
                    a.play_named("music.menu", None, 1.0, 1.0, crate::audio::MUSIC);
                    self.music_delay = 20 + (random_u32() % 580) as i32;
                }
            }
        }
        if let Some(mp) = self.mp.as_mut() {
            if matches!(self.scr, Scr::Multiplayer) {
                mp.tick(&ui.v.gl);
            }
        }
        self.test_step(ui);
        match &mut self.scr {
            Scr::Connect { task, address, back_to_list } => {
                if let Some(r) = task.take() {
                    let (address, back) = (address.clone(), *back_to_list);
                    match r {
                        Ok(l) => return Some(Next::Join(l.0, address, back)),
                        Err(e) => {
                            crate::warn!("could not join {}: {}", address, e);
                            self.join_failed(back, e);
                        }
                    }
                }
            }
            Scr::SignIn(s) => {
                let mut to = None;
                match s {
                    SignIn::Asking(t) => match t.take() {
                        Some(Ok(Ok(acc))) => self.account = Some(acc),
                        Some(Ok(Err((session, code, url, every)))) => {
                            to = Some(SignIn::Code { session, code, url, every, next: sys::now_ms() + every as u64 * 1000, poll: None });
                        }
                        Some(Err(e)) => to = Some(SignIn::Failed(e)),
                        None => {}
                    },
                    SignIn::Code { session, every, next, poll, .. } => {
                        if let Some(p) = poll.as_mut() {
                            match p.take() {
                                Some(Ok(Some(acc))) => self.account = Some(acc),
                                Some(Ok(None)) => *poll = None,
                                Some(Err(e)) => to = Some(SignIn::Failed(e)),
                                None => {}
                            }
                        } else if sys::now_ms() >= *next {
                            *next = sys::now_ms() + (*every).max(1) as u64 * 1000;
                            let (relay, session) = (self.relay.clone()?, session.clone());
                            *poll = Some(task::spawn(move || crate::account::poll(&relay, &session)));
                        }
                    }
                    SignIn::Failed(_) => {}
                }
                if let Some(t) = to {
                    *s = t;
                }
                if self.account.is_some() {
                    crate::skins::set_relay(self.relay.clone());
                    self.splash = choose_splash(assets(&ui.v.gl).map_or(&[][..], |a| &a.splashes[..]), sys::local_date(), &self.player_name(), random_u32());
                    self.scr = Scr::Title;
                }
            }
            _ => {}
        }
        None
    }

    /// PENRYN_TEST_INPUT's menu steps, for tests without a mouse.
    fn test_step(&mut self, ui: &mut Ui) {
        if self.test.is_empty() || sys::now_ms() < self.test_next {
            return;
        }
        self.test_next = sys::now_ms() + 500;
        let step = self.test.remove(0);
        crate::say!("test step {}", step);
        if let Some(xy) = step.strip_prefix("mclick=") {
            if let Some((x, y)) = xy.split_once(':') {
                self.mouse = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                let _ = self.press(ui, 1);
            }
        } else if let Some(t) = step.strip_prefix("mtype=") {
            self.text(t);
        } else if let Some(k) = step.strip_prefix("mkey=") {
            let sc = match k {
                "esc" => 41,
                "enter" => 40,
                "tab" => 43,
                "f5" => 62,
                "down" => 81,
                "up" => 82,
                "backspace" => 42,
                _ => 0,
            };
            let _ = self.key(ui, sc);
        }
    }

    fn text(&mut self, s: &str) {
        if let (Scr::Multiplayer, Some(mp)) = (&self.scr, self.mp.as_mut()) {
            mp.text(s);
        }
    }

    fn event(&mut self, ui: &mut Ui, e: &sdl::Event) -> Option<Next> {
        let t = e.u32_at(0);
        match t {
            sdl::EV_QUIT => return Some(Next::Quit),
            sdl::EV_KEYDOWN | sdl::EV_KEYUP => {
                let sc = e.i32_at(16).clamp(0, 511) as usize;
                self.keys[sc] = t == sdl::EV_KEYDOWN;
                if t == sdl::EV_KEYDOWN {
                    return self.key(ui, sc);
                }
            }
            sdl::EV_TEXTINPUT => {
                let n = e.0[12..44].iter().position(|b| *b == 0).unwrap_or(32);
                let s = String::from_utf8_lossy(&e.0[12..12 + n]).into_owned();
                self.text(&s);
            }
            sdl::EV_MOUSEMOTION => {
                self.mouse = (e.i32_at(20) as f32 / ui.gui.scale, e.i32_at(24) as f32 / ui.gui.scale);
                if let Scr::Options = self.scr {
                    ui.menu.drag_to(self.mouse.0);
                }
            }
            sdl::EV_MOUSEDOWN => return self.press(ui, e.0[16]),
            sdl::EV_MOUSEUP => {
                if let (Scr::Options, 1) = (&self.scr, e.0[16]) {
                    if let crate::options::Ev::Click = ui.menu.release() {
                        Self::click_sound(ui);
                    }
                }
            }
            sdl::EV_MOUSEWHEEL => {
                let dy = e.i32_at(20);
                match self.scr {
                    Scr::Options => ui.menu.wheel(ui.gui.width, ui.gui.height, dy),
                    Scr::Multiplayer => {
                        if let Some(mp) = self.mp.as_mut() {
                            mp.wheel(ui.gui, dy);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    fn ctrl(&self) -> bool {
        self.keys[224] || self.keys[228] || self.keys[227] || self.keys[231]
    }

    fn shift(&self) -> bool {
        self.keys[225] || self.keys[229]
    }

    fn key(&mut self, ui: &mut Ui, sc: usize) -> Option<Next> {
        if sc == 68 {
            // F11: full screen, as in the game.
            let f = !ui.menu.o.on(crate::options::O::Fullscreen);
            ui.menu.set(crate::options::O::Fullscreen, f as i32 as f64);
            ui.menu.o.save();
            return None;
        }
        match &mut self.scr {
            Scr::Title => {}
            Scr::Options => {
                if sc == 41 && ui.menu.back() {
                    self.scr = Scr::Title;
                }
            }
            Scr::Multiplayer => {
                let (ctrl, shift) = (self.ctrl(), self.shift());
                let v = ui.v;
                let act = self.mp.as_mut()?.key(sc, ctrl, shift, || v.clipboard());
                return self.mp_act(ui, act);
            }
            // ConnectScreen and DisconnectedScreen: shouldCloseOnEsc is false (only their
            // buttons leave them).
            Scr::Connect { .. } | Scr::Disconnected { .. } => {}
            Scr::SignIn(_) => {
                if sc == 41 {
                    self.scr = Scr::Title;
                }
            }
        }
        None
    }

    fn mp_act(&mut self, ui: &mut Ui, act: multiplayer::Act) -> Option<Next> {
        match act {
            multiplayer::Act::None => {}
            multiplayer::Act::Click => Self::click_sound(ui),
            multiplayer::Act::Back => {
                Self::click_sound(ui);
                self.scr = Scr::Title;
            }
            multiplayer::Act::Join(address) => {
                Self::click_sound(ui);
                if let Some(mp) = self.mp.as_ref() {
                    if !mp.last_direct.is_empty() {
                        ui.menu.o.set_other("lastServer", &mp.last_direct);
                        ui.menu.o.save();
                    }
                }
                self.connect(&address, true);
            }
        }
        None
    }

    fn connect_buttons(gw: f32, gh: f32) -> Button {
        Button::new(fl(gw / 2.0) - 100.0, fl(gh / 4.0) + 120.0 + 12.0, 200.0, lang::get("gui.cancel"))
    }

    /// DisconnectedScreen's column (cells padded 10, the button 2): title, reason lines,
    /// button, centred on the screen.
    fn disconnected_layout(gui: &Gui, reason: &str, to_list: bool) -> (f32, Vec<String>, Button) {
        let lines: Vec<String> = reason.split('\n').flat_map(|p| screens::wrap(gui, p, gui.width - 50.0)).collect();
        let total = 29.0 + lines.len() as f32 * 9.0 + 20.0 + 24.0;
        let top = fl((gui.height - total) / 2.0);
        let by = top + 29.0 + lines.len() as f32 * 9.0 + 20.0 + 2.0;
        let label = lang::get(if to_list { "gui.toMenu" } else { "gui.toTitle" });
        (top + 10.0, lines, Button::new(fl(gui.width / 2.0) - 100.0, by, 200.0, label))
    }

    fn press(&mut self, ui: &mut Ui, button: u8) -> Option<Next> {
        if button != 1 {
            return None;
        }
        let (mx, my) = self.mouse;
        let (gw, gh) = (ui.gui.width, ui.gui.height);
        match &mut self.scr {
            Scr::Title => {
                let b = title_buttons(gw, gh);
                if b[1].hit(mx, my) {
                    Self::click_sound(ui);
                    self.open_multiplayer(ui);
                } else if b[5].hit(mx, my) {
                    Self::click_sound(ui);
                    ui.menu.open_only(crate::options::Page::Accessibility);
                    self.scr = Scr::Options;
                } else if b[6].hit(mx, my) {
                    Self::click_sound(ui);
                    ui.menu.open(crate::options::Page::Main);
                    self.scr = Scr::Options;
                } else if b[7].hit(mx, my) {
                    Self::click_sound(ui);
                    return Some(Next::Quit);
                } else if self.sign_in_button(gw).map_or(false, |s| s.hit(mx, my)) {
                    Self::click_sound(ui);
                    self.start_sign_in();
                }
            }
            Scr::Options => {
                let shift = self.keys[225] || self.keys[229];
                match ui.menu.press(gw, gh, mx, my, shift) {
                    crate::options::Ev::Click => Self::click_sound(ui),
                    crate::options::Ev::Close => {
                        Self::click_sound(ui);
                        self.scr = Scr::Title;
                    }
                    crate::options::Ev::Packs => {
                        Self::click_sound(ui);
                        return Some(Next::Packs);
                    }
                    crate::options::Ev::None => {}
                }
            }
            Scr::Multiplayer => {
                let shift = self.shift();
                let act = self.mp.as_mut()?.click(ui.gui, mx, my, shift);
                return self.mp_act(ui, act);
            }
            Scr::Connect { back_to_list, .. } => {
                if Self::connect_buttons(gw, gh).hit(mx, my) {
                    Self::click_sound(ui);
                    let back = *back_to_list;
                    self.scr = if back { Scr::Multiplayer } else { Scr::Title };
                }
            }
            Scr::Disconnected { reason, to_list, .. } => {
                let (_, _, b) = Self::disconnected_layout(ui.gui, reason, *to_list);
                if b.hit(mx, my) {
                    Self::click_sound(ui);
                    let to = *to_list;
                    if to {
                        self.open_multiplayer(ui);
                    } else {
                        self.scr = Scr::Title;
                    }
                }
            }
            Scr::SignIn(_) => {
                if Self::connect_buttons(gw, gh).hit(mx, my) {
                    Self::click_sound(ui);
                    self.scr = Scr::Title;
                }
            }
        }
        None
    }

    fn draw(&mut self, ui: &mut Ui) {
        use crate::options::O;
        let (w, h) = ui.v.size();
        let gl = &ui.v.gl;
        let a = assets(gl);
        ui.menu.max_gui_scale = crate::options::auto_gui_scale(w, h);
        let gui = &mut *ui.gui;
        gui.begin(w, h);
        let speed = ui.menu.o.get(O::PanoramaSpeed);
        let blur = ui.menu.o.int(O::MenuBlur);
        let (mx, my) = self.mouse;
        let (gw, gh) = (gui.width, gui.height);
        let cx = fl(gw / 2.0);
        // Text fields want the text input on.
        let want = matches!(self.scr, Scr::Multiplayer) && self.mp.as_ref().map_or(false, |m| m.in_text());
        if self.text_on != Some(want) {
            self.text_on = Some(want);
            ui.v.text_input(want);
        }
        match &self.scr {
            Scr::Title => {
                panorama(gl, w, h, speed);
                // The widgets fade in over the first two seconds (TitleScreen.fading).
                let fade = match self.fade_from {
                    Some(t) => {
                        let f = (sys::now_ms().saturating_sub(t)) as f32 / 2000.0;
                        if f >= 1.0 {
                            self.fade_from = None;
                        }
                        ((f.clamp(0.0, 1.0) - 0.5) * 2.0).clamp(0.0, 1.0)
                    }
                    None => 1.0,
                };
                let account = match &self.account {
                    Some(acc) => format!("Signed in as {}", acc.name),
                    None => format!("Offline as {}", self.name),
                };
                let sign_in = self.sign_in_button(gw);
                let splash = if ui.menu.o.on(O::HideSplash) { None } else { self.splash.as_deref() };
                draw_title(gui, a, splash, fade, mx, my, &account, sign_in.as_ref());
            }
            Scr::Options => {
                menu_background(gl, gui, w, h, speed, blur);
                ui.menu.draw(gui, mx, my);
            }
            Scr::Multiplayer => {
                menu_background(gl, gui, w, h, speed, blur);
                if let Some(mp) = self.mp.as_mut() {
                    mp.draw(gui, a, mx, my);
                }
            }
            Scr::Connect { .. } => {
                menu_background(gl, gui, w, h, speed, blur);
                let stage = crate::client::JOIN_STAGE.load(core::sync::atomic::Ordering::Relaxed) as usize;
                gui.text_centered(&lang::get(CONNECT_STAGES[stage.min(3)]), cx, fl(gh / 2.0) - 50.0, WHITE);
                Self::connect_buttons(gw, gh).draw(gui, mx, my);
            }
            Scr::Disconnected { title, reason, to_list } => {
                menu_background(gl, gui, w, h, speed, blur);
                let (top, lines, b) = Self::disconnected_layout(gui, reason, *to_list);
                gui.text_centered(title, cx, top, WHITE);
                for (i, l) in lines.iter().enumerate() {
                    gui.text_centered(l, cx, top + 29.0 + i as f32 * 9.0, WHITE);
                }
                b.draw(gui, mx, my);
            }
            Scr::SignIn(s) => {
                menu_background(gl, gui, w, h, speed, blur);
                let lines: Vec<String> = match s {
                    SignIn::Asking(_) => alloc::vec![String::from("Signing in through the relay...")],
                    SignIn::Code { code, url, .. } => alloc::vec![
                        format!("To sign in, open {} on any device", url),
                        String::from("and enter the code:"),
                        String::new(),
                        format!("\u{a7}e{}", code),
                    ],
                    SignIn::Failed(e) => alloc::vec![String::from("Could not sign in:"), e.clone()],
                };
                gui.text_centered("Sign in with your Microsoft account", cx, 20.0, WHITE);
                for (i, l) in lines.iter().enumerate() {
                    gui.text_centered(l, cx, fl(gh / 2.0) - 50.0 + i as f32 * 12.0, WHITE);
                }
                let mut b = Self::connect_buttons(gw, gh);
                if matches!(s, SignIn::Failed(_)) {
                    b.label = lang::get("gui.back");
                }
                b.draw(gui, mx, my);
            }
        }
        gui.flush(gl, w, h);
    }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let b = title_buttons(427.0, 240.0);
    out.push((
        "title screen buttons (427 x 240)",
        format!("{} {} {} {} {} {} {}", b[0].x, b[0].y, b[2].y, b[3].x, b[5].x, b[6].y, b[7].x),
        "113 108 156 179 227 204 215",
    ));
    let list = [String::from("Hello!"), String::from("World!")];
    out.push((
        "splash dates",
        format!(
            "{:?} {:?} {:?} {:?}",
            choose_splash(&list, (12, 24), "a", 0),
            choose_splash(&list, (10, 31), "a", 0),
            choose_splash(&list, (6, 1), "a", 1),
            choose_splash(&[], (6, 1), "a", 1)
        ),
        "Some(\"Merry X-mas!\") Some(\"OOoooOOOoooo! Spooky!\") Some(\"World!\") None",
    ));
    let v = mat_mul(&rot_x(190.0), &rot_y(0.0));
    // Straight ahead (0, 0, 1) is in front of the camera, a little above the middle.
    let z = v[8] * 0.0 + v[9] * 0.0 + v[10] * 1.0;
    out.push(("panorama looks at panorama_0", format!("{}", z < 0.0), "true"));
    out
}
