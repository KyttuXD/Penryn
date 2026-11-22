//! Drawing what serverui.rs keeps, as vanilla's Hud draws it: boss bars (BossHealthOverlay),
//! the scoreboard sidebar (Hud.displayScoreboardSidebar), the action bar and titles with their
//! fades, the tab list (PlayerTabOverlay: faces, names, scores or hearts, ping bars, header and
//! footer), the world border's wall (WorldBorderRenderer) and its red vignette, the death
//! screen's cause and score, the server links screen (ServerLinksDialogScreen) and the link
//! confirmation (ConfirmLinkScreen). Pictures come from serverui.bin (optional).

use crate::client::Game;
use crate::gui::Gui;
use crate::lang;
use crate::math::FloorExt;
use crate::screens::Button;
use crate::sdl::*;
use crate::serverui::{NumFmt, ServerUi};
use crate::tooltip::Defaults;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;

pub const SERVERUI_MAGIC: &[u8; 4] = b"PNU1";
const WHITE: u32 = 0xFFFF_FFFF;

struct Pics {
    atlas: u32,
    aw: f32,
    ah: f32,
    rects: BTreeMap<String, [f32; 4]>,
    forcefield: Option<u32>,
    vignette: Option<u32>,
}

#[derive(Default)]
struct State {
    loaded: bool,
    /// The pictures until they are uploaded: name, width, height, RGBA.
    raw: Vec<(String, u16, u16, Vec<u8>)>,
    pics: Option<Pics>,
    uploaded: bool,
    defaults: Option<Defaults>,
    /// The links screen: its scroll, and the link being confirmed.
    scroll: f32,
    confirm: Option<usize>,
    /// The sidebar's rows for (objective, ServerUi.version), made again only when they change.
    sidebar: Option<(String, u32, Vec<(String, String)>)>,
}

struct Main(UnsafeCell<Option<State>>);
unsafe impl Sync for Main {}
static STATE: Main = Main(UnsafeCell::new(None));

/// The main thread's state (drawing and the game loop are single-threaded).
fn st() -> &'static mut State {
    let s = unsafe { &mut *STATE.0.get() };
    if s.is_none() {
        *s = Some(State::default());
    }
    let s = s.as_mut().unwrap();
    if !s.loaded {
        s.loaded = true;
        if let Some(d) = crate::sys::read_file(&format!("{}/serverui.bin", crate::data_dir())) {
            if parse(&d, s).is_none() {
                crate::say!("serverui.bin is of another version: plain boss bars, no world border");
            }
        }
    }
    s
}

fn parse(d: &[u8], s: &mut State) -> Option<()> {
    if d.get(..4)? != SERVERUI_MAGIC {
        return None;
    }
    let rd16 = |p: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?])) };
    let n = rd16(4)?;
    let mut p = 6;
    for _ in 0..n {
        let l = *d.get(p)? as usize;
        let name = String::from_utf8_lossy(d.get(p + 1..p + 1 + l)?).into_owned();
        p += 1 + l;
        let (w, h) = (rd16(p)?, rd16(p + 2)?);
        p += 4;
        let len = w as usize * h as usize * 4;
        s.raw.push((name, w, h, d.get(p..p + len)?.to_vec()));
        p += len;
    }
    s.defaults = crate::tooltip::parse_defaults(d, p);
    Some(())
}

/// The items' default attribute modifiers (serverui.bin), for tooltips.
pub fn with_defaults<R>(f: impl FnOnce(Option<&Defaults>) -> R) -> R {
    f(st().defaults.as_ref())
}

fn texture(gl: &Gl, w: u16, h: u16, px: &[u8]) -> u32 {
    let mut t = 0u32;
    unsafe {
        (gl.GenTextures)(1, &mut t);
        (gl.BindTexture)(TEXTURE_2D, t);
        (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
        (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
    }
    t
}

/// The pictures on the GPU: the small ones packed in rows of one atlas, the wall and the
/// vignette as textures of their own (the wall repeats).
fn pics(gl: &Gl) -> Option<&'static Pics> {
    let s = st();
    if !s.uploaded {
        s.uploaded = true;
        let raw = core::mem::take(&mut s.raw);
        if raw.is_empty() {
            return None;
        }
        const AW: usize = 256;
        let (mut x, mut y, mut row) = (0usize, 0usize, 0usize);
        let mut places = Vec::new();
        for (name, w, h, _) in raw.iter().filter(|r| !r.0.starts_with("misc/")) {
            let (w, h) = (*w as usize, *h as usize);
            if x + w > AW {
                x = 0;
                y += row;
                row = 0;
            }
            places.push((name.clone(), x, y, w, h));
            x += w;
            row = row.max(h);
        }
        let ah = (y + row).max(1).next_power_of_two();
        let mut px = alloc::vec![0u8; AW * ah * 4];
        let mut rects = BTreeMap::new();
        for ((_, w, h, data), (name, x, y, _, _)) in raw.iter().filter(|r| !r.0.starts_with("misc/")).zip(places.iter()) {
            let (w, h) = (*w as usize, *h as usize);
            for j in 0..h {
                px[((y + j) * AW + x) * 4..((y + j) * AW + x + w) * 4].copy_from_slice(&data[j * w * 4..(j + 1) * w * 4]);
            }
            rects.insert(name.clone(), [*x as f32, *y as f32, w as f32, h as f32]);
        }
        let atlas = texture(gl, AW as u16, ah as u16, &px);
        let own = |n: &str| raw.iter().find(|r| r.0 == n).map(|(_, w, h, d)| texture(gl, *w, *h, d));
        s.pics = Some(Pics { atlas, aw: AW as f32, ah: ah as f32, rects, forcefield: own("misc/forcefield"), vignette: own("misc/vignette") });
    }
    s.pics.as_ref()
}

/// Part of a picture (u, v, w, h in its pixels) at (x, y), unscaled.
fn blit(gui: &mut Gui, p: &Pics, name: &str, x: f32, y: f32, u: f32, v: f32, w: f32, h: f32) -> bool {
    let Some(r) = p.rects.get(name) else { return false };
    let uv = [(r[0] + u) / p.aw, (r[1] + v) / p.ah, (r[0] + u + w) / p.aw, (r[1] + v + h) / p.ah];
    gui.blit_tex(p.atlas, x, y, w, h, uv, WHITE);
    true
}

/// Text at `scale` times the font's size (titles): formatting codes as Gui.text has them,
/// with vanilla's shadow one scaled unit down-right.
pub fn text_scaled(gui: &mut Gui, s: &str, x: f32, y: f32, scale: f32, color: u32, shadow: bool) {
    if shadow {
        glyphs_scaled(gui, s, x + scale, y + scale, scale, color, true);
    }
    glyphs_scaled(gui, s, x, y, scale, color, false);
}

fn glyphs_scaled(gui: &mut Gui, s: &str, x: f32, y: f32, scale: f32, color: u32, shadow: bool) {
    const CODES: [u32; 16] = [
        0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55, 0x55FFFF, 0xFF5555,
        0xFF55FF, 0xFFFF55, 0xFFFFFF,
    ];
    let alpha = color & 0xFF00_0000;
    let tint = |rgb: u32| -> u32 {
        if shadow {
            alpha | ((rgb >> 2) & 0x3F3F3F)
        } else {
            alpha | (rgb & 0xFFFFFF)
        }
    };
    let base = tint(color & 0xFFFFFF);
    let mut c = base;
    let tex = gui.texture();
    let mut pen = 0.0;
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        if ch == '\u{a7}' {
            match it.next() {
                Some('#') => {
                    let hex: String = (0..6).filter_map(|_| it.next()).collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        c = tint(v);
                    }
                }
                Some(d) if d.is_ascii_hexdigit() => c = tint(CODES[d.to_digit(16).unwrap() as usize]),
                Some('r') => c = base,
                _ => {}
            }
            continue;
        }
        let Some((uv, q, adv)) = gui.glyph_quad(ch) else { continue };
        if q[2] > 0.0 {
            gui.blit_tex(tex, x + (pen + q[0]) * scale, y + q[1] * scale, q[2] * scale, q[3] * scale, uv, c);
        }
        pen += adv;
    }
}

// ------------------------------------------------------------------ the HUD

/// Boss bars, the sidebar, the action bar and titles, in Hud.extractRenderState's order
/// (before the chat).
pub fn hud(gui: &mut Gui, gl: &Gl, g: &Game, partial: f32) {
    bosses(gui, gl, &g.ui);
    if let Some((name, o)) = g.ui.sidebar(&g.name) {
        sidebar(gui, &g.ui, name, &o.display);
    }
    action_bar(gui, &g.ui, partial);
    titles(gui, &g.ui, partial);
}

/// BossHealthOverlay: each bar 182 wide from 12 down, 19 apart, its name above it, until a
/// third of the screen.
fn bosses(gui: &mut Gui, gl: &Gl, ui: &ServerUi) {
    if ui.bosses.is_empty() {
        return;
    }
    const COLOURS: [&str; 7] = ["pink", "blue", "red", "green", "yellow", "purple", "white"];
    const PLAIN: [u32; 7] = [0xFFEC_00B8, 0xFF00_B7EC, 0xFFEC_3500, 0xFF1D_EC00, 0xFFE9_EC00, 0xFF7B_00EC, 0xFFEC_ECEC];
    let p = pics(gl);
    let now = crate::sys::now_ms();
    let (w, h) = (gui.width, gui.height);
    let mut y = 12.0;
    for b in ui.bosses.iter() {
        let x = (w / 2.0).floor() - 91.0;
        let colour = COLOURS[b.color as usize];
        let fill = (b.shown_progress(now).clamp(0.0, 1.0) * 182.0) as i32 as f32;
        let notch = if b.overlay > 0 { Some(format!("boss_bar/notched_{}", [6, 10, 12, 20][b.overlay as usize - 1])) } else { None };
        match p {
            Some(p) => {
                blit(gui, p, &format!("boss_bar/{}_background", colour), x, y, 0.0, 0.0, 182.0, 5.0);
                if let Some(n) = notch.as_ref() {
                    blit(gui, p, &format!("{}_background", n), x, y, 0.0, 0.0, 182.0, 5.0);
                }
                if fill > 0.0 {
                    blit(gui, p, &format!("boss_bar/{}_progress", colour), x, y, 0.0, 0.0, fill, 5.0);
                    if let Some(n) = notch.as_ref() {
                        blit(gui, p, &format!("{}_progress", n), x, y, 0.0, 0.0, fill, 5.0);
                    }
                }
            }
            None => {
                gui.fill(x, y, x + 182.0, y + 5.0, 0xFF20_2020);
                gui.fill(x, y, x + fill, y + 5.0, PLAIN[b.color as usize]);
            }
        }
        let tw = gui.text_width(&b.name);
        gui.text(&b.name, (w / 2.0).floor() - (tw / 2.0).floor(), y - 9.0, WHITE);
        y += 19.0;
        if y >= h / 3.0 {
            break;
        }
    }
}

/// The sidebar's rows (name, score) as Hud.displayScoreboardSidebar makes them: not the
/// holders starting with '#', by score down then name, at most 15, names in their team's
/// format, scores in the score's or objective's number format (red by default).
pub fn sidebar_rows(ui: &ServerUi, objective: &str) -> Vec<(String, String)> {
    let o = ui.objectives.get(objective);
    let mut v: Vec<(&String, &crate::serverui::Score)> =
        ui.scores.iter().filter(|(h, _)| !h.starts_with('#')).filter_map(|(h, m)| m.get(objective).map(|s| (h, s))).collect();
    v.sort_by(|a, b| b.1.value.cmp(&a.1.value).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    v.truncate(15);
    let default = NumFmt::Styled(String::from("\u{a7}c"));
    v.iter()
        .map(|(h, s)| {
            let name = s.display.clone().unwrap_or_else(|| (*h).clone());
            let f = s.format.as_ref().or(o.and_then(|o| o.format.as_ref())).unwrap_or(&default);
            (ui.format_name(h, &name), f.format(s.value))
        })
        .collect()
}

fn sidebar(gui: &mut Gui, ui: &ServerUi, objective: &str, title: &str) {
    let s = st();
    if s.sidebar.as_ref().map_or(true, |c| c.0 != objective || c.1 != ui.version) {
        s.sidebar = Some((String::from(objective), ui.version, sidebar_rows(ui, objective)));
    }
    let rows = &s.sidebar.as_ref().unwrap().2;
    let tw = gui.text_width(title);
    let colon = gui.text_width(": ");
    let mut width = tw;
    let widths: Vec<f32> = rows.iter().map(|r| gui.text_width(&r.1)).collect();
    for (r, sw) in rows.iter().zip(widths.iter()) {
        width = width.max(gui.text_width(&r.0) + if *sw > 0.0 { colon + sw } else { 0.0 });
    }
    let l = rows.len() as f32;
    let bottom = (gui.height / 2.0).floor() + (l * 9.0 / 3.0).floor();
    let left = gui.width - width - 3.0;
    let right = gui.width - 3.0 + 2.0;
    let top = bottom - l * 9.0;
    // Options.getBackgroundColor(0.3) and (0.4).
    gui.fill(left - 2.0, top - 9.0 - 1.0, right, top - 1.0, 0x6600_0000);
    gui.fill(left - 2.0, top - 1.0, right, bottom, 0x4C00_0000);
    gui.text_plain(title, left + (width / 2.0).floor() - (tw / 2.0).floor(), top - 9.0, WHITE);
    for (i, (r, sw)) in rows.iter().zip(widths.iter()).enumerate() {
        let y = bottom - (l - i as f32) * 9.0;
        gui.text_plain(&r.0, left, y, WHITE);
        gui.text_plain(&r.1, right - sw, y, WHITE);
    }
}

/// Mth.hsvToArgb.
fn hsv(h: f32, s: f32, v: f32, a: u32) -> u32 {
    let h6 = (h * 6.0) as i32;
    let f = h * 6.0 - h6 as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match h6.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    a << 24 | ((r * 255.0) as u32).min(255) << 16 | ((g * 255.0) as u32).min(255) << 8 | ((b * 255.0) as u32).min(255)
}

/// Hud.extractOverlayMessage: 68 up from the bottom, fading over its last second.
fn action_bar(gui: &mut Gui, ui: &ServerUi, partial: f32) {
    let Some(msg) = ui.action.as_ref().filter(|_| ui.action_time > 0) else { return };
    let f = ui.action_time as f32 - partial;
    let a = ((f * 255.0 / 20.0) as i32).min(255);
    if a <= 8 {
        return;
    }
    let c = if ui.action_animate { hsv(f / 50.0, 0.7, 0.6, a as u32) } else { (a as u32) << 24 | 0xFFFFFF };
    let w = gui.text_width(msg);
    gui.text(msg, (gui.width / 2.0).floor() - (w / 2.0).floor(), gui.height - 68.0 - 4.0, c);
}

/// Hud.extractTitle: the title at 4x and the subtitle at 2x around the middle, fading in and
/// out over their times.
fn titles(gui: &mut Gui, ui: &ServerUi, partial: f32) {
    let Some(title) = ui.title.as_ref().filter(|_| ui.title_time > 0) else { return };
    let f = ui.title_time as f32 - partial;
    let mut a = 255;
    if ui.title_time > ui.fade_out + ui.stay {
        let g = (ui.fade_in + ui.stay + ui.fade_out) as f32 - f;
        a = (g * 255.0 / ui.fade_in.max(1) as f32) as i32;
    }
    if ui.title_time <= ui.fade_out {
        a = (f * 255.0 / ui.fade_out.max(1) as f32) as i32;
    }
    let a = a.clamp(0, 255);
    if a <= 8 {
        return;
    }
    let c = (a as u32) << 24 | 0xFFFFFF;
    let (cx, cy) = ((gui.width / 2.0).floor(), (gui.height / 2.0).floor());
    let w = gui.text_width(title);
    text_scaled(gui, title, cx + (-(w / 2.0).floor()) * 4.0, cy - 10.0 * 4.0, 4.0, c, true);
    if let Some(sub) = ui.subtitle.as_ref() {
        let w = gui.text_width(sub);
        text_scaled(gui, sub, cx + (-(w / 2.0).floor()) * 2.0, cy + 5.0 * 2.0, 2.0, c, true);
    }
}

// ------------------------------------------------------------------ the tab list

/// PlayerTabOverlay.PLAYER_COMPARATOR: list order (high first), spectators last, team name,
/// then name (ignoring case).
fn listed(ui: &ServerUi) -> Vec<([u8; 16], &crate::serverui::TabPlayer)> {
    let mut v: Vec<([u8; 16], &crate::serverui::TabPlayer)> = ui.players.iter().filter(|(_, p)| p.listed).map(|(u, p)| (*u, p)).collect();
    v.sort_by(|a, b| {
        let team = |p: &crate::serverui::TabPlayer| ui.team_of.get(&p.name).cloned().unwrap_or_default();
        b.1.order
            .cmp(&a.1.order)
            .then((a.1.game_mode == 3).cmp(&(b.1.game_mode == 3)))
            .then_with(|| team(a.1).cmp(&team(b.1)))
            .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
    });
    v.truncate(80);
    v
}

/// Whether the tab list shows (the key held; vanilla also hides it on a local server alone).
pub fn tab_list(gui: &mut Gui, gl: &Gl, g: &mut Game) {
    let mut states = core::mem::take(&mut g.ui.health_states);
    draw_tab_list(gui, gl, &g.ui, &mut states);
    g.ui.health_states = states;
}

fn draw_tab_list(gui: &mut Gui, gl: &Gl, ui: &ServerUi, states: &mut BTreeMap<[u8; 16], (i32, i32, u32)>) {
    let players = listed(ui);
    let objective = ui.shown(0);
    let space = gui.text_width(" ");
    let mut name_w: f32 = 0.0;
    let mut score_w: f32 = 0.0;
    struct Row {
        name: String,
        score: i32,
        formatted: Option<String>,
        width: f32,
    }
    let mut rows = Vec::with_capacity(players.len());
    for (_, p) in players.iter() {
        let base = p.display.clone().unwrap_or_else(|| ui.format_name(&p.name, &p.name));
        name_w = name_w.max(gui.text_width(&base));
        let mut row = Row { name: base, score: 0, formatted: None, width: 0.0 };
        if let Some((oname, o)) = objective {
            let s = ui.score_of(&p.name, oname);
            row.score = s.map_or(0, |s| s.value);
            if !o.hearts {
                let f = s.and_then(|s| s.format.clone()).or_else(|| o.format.clone()).unwrap_or(NumFmt::Styled(String::from("\u{a7}e")));
                let t = f.format(row.score);
                row.width = gui.text_width(&t);
                score_w = score_w.max(if row.width > 0.0 { space + row.width } else { 0.0 });
                row.formatted = Some(t);
            }
        }
        rows.push(row);
    }
    let n = players.len() as i32;
    let mut per_col = n;
    let mut cols = 1;
    while per_col > 20 {
        cols += 1;
        per_col = (n + cols - 1) / cols;
    }
    // Faces only on encrypted (online-mode) connections, as vanilla.
    let faces = ui.online || crate::sys::env("PENRYN_TEST_FACES").is_some();
    let hearts = objective.map_or(false, |o| o.1.hearts);
    let score_space = match objective {
        Some(_) if hearts => 90.0,
        Some(_) => score_w,
        None => 0.0,
    };
    let sw = gui.width;
    let col_w = ((cols as f32 * ((if faces { 9.0 } else { 0.0 }) + name_w + score_space + 13.0)).min(sw - 50.0) / cols as f32).floor();
    let left = (sw / 2.0).floor() - ((col_w * cols as f32 + (cols - 1) as f32 * 5.0) / 2.0).floor();
    let mut y = 10.0;
    let mut width = col_w * cols as f32 + (cols - 1) as f32 * 5.0;
    let header = ui.header.as_ref().map(|h| crate::screens::wrap(gui, h, sw - 50.0));
    let footer = ui.footer.as_ref().map(|f| crate::screens::wrap(gui, f, sw - 50.0));
    for l in header.iter().chain(footer.iter()).flatten() {
        width = width.max(gui.text_width(l));
    }
    let half = (width / 2.0).floor();
    let cx = (sw / 2.0).floor();
    if let Some(h) = header.as_ref() {
        gui.fill(cx - half - 1.0, y - 1.0, cx + half + 1.0, y + h.len() as f32 * 9.0, 0x8000_0000);
        for l in h {
            let w = gui.text_width(l);
            gui.text(l, cx - (w / 2.0).floor(), y, WHITE);
            y += 9.0;
        }
        y += 1.0;
    }
    gui.fill(cx - half - 1.0, y - 1.0, cx + half + 1.0, y + per_col as f32 * 9.0, 0x8000_0000);
    let ticks = ui.ticks;
    for i in 0..n {
        let (col, rowi) = (i / per_col.max(1), i % per_col.max(1));
        let mut x = left + col as f32 * col_w + col as f32 * 5.0;
        let ry = y + rowi as f32 * 9.0;
        gui.fill(x, ry, x + col_w, ry + 8.0, 0x20FF_FFFF);
        let (uuid, p) = players[i as usize];
        let r = &rows[i as usize];
        let x0 = x;
        if faces {
            face(gui, &uuid, x, ry, p.hat);
            x += 9.0;
        }
        gui.text(&r.name, x, ry, if p.game_mode == 3 { 0x90FF_FFFF } else { WHITE });
        if let (Some((_, o)), false) = (objective, p.game_mode == 3) {
            let (l, rr) = (x + name_w + 1.0, x + name_w + 1.0 + score_space);
            if rr - l > 5.0 {
                if o.hearts {
                    tab_hearts(gui, gl, states, uuid, ry, l, rr, r.score, ticks);
                } else if let Some(t) = r.formatted.as_ref() {
                    gui.text(t, rr - r.width, ry, WHITE);
                }
            }
        }
        // PlayerTabOverlay.extractPingIcon: bars by latency at the column's right end.
        let ping = match p.latency {
            l if l < 0 => "icon/ping_unknown",
            l if l < 150 => "icon/ping_5",
            l if l < 300 => "icon/ping_4",
            l if l < 600 => "icon/ping_3",
            l if l < 1000 => "icon/ping_2",
            _ => "icon/ping_1",
        };
        if let Some(pc) = pics(gl) {
            blit(gui, pc, ping, x0 + col_w - 11.0, ry, 0.0, 0.0, 10.0, 8.0);
        }
    }
    if let Some(f) = footer.as_ref() {
        let mut fy = y + per_col as f32 * 9.0 + 1.0;
        gui.fill(cx - half - 1.0, fy - 1.0, cx + half + 1.0, fy + f.len() as f32 * 9.0, 0x8000_0000);
        for l in f {
            let w = gui.text_width(l);
            gui.text(l, cx - (w / 2.0).floor(), fy, WHITE);
            fy += 9.0;
        }
    }
}

/// PlayerFaceExtractor: the skin's face and, when shown, its hat layer, 8 x 8.
fn face(gui: &mut Gui, uuid: &[u8; 16], x: f32, y: f32, hat: bool) {
    let parts = |u0: f32, v0: f32, u1: f32, v1: f32| -> ([f32; 4], [f32; 4]) {
        let (du, dv) = ((u1 - u0) / 64.0, (v1 - v0) / 64.0);
        ([u0 + 8.0 * du, v0 + 8.0 * dv, u0 + 16.0 * du, v0 + 16.0 * dv], [u0 + 40.0 * du, v0 + 8.0 * dv, u0 + 48.0 * du, v0 + 16.0 * dv])
    };
    let (tex, uv) = match crate::skins::skin_of(uuid) {
        Some((t, _)) => (t, [0.0, 0.0, 1.0, 1.0]),
        None => {
            let (_, sprite) = crate::screens::skin_of(uuid);
            match gui.sprite_uv(&sprite) {
                Some(uv) => (gui.texture(), uv),
                None => return,
            }
        }
    };
    let (f, h) = parts(uv[0], uv[1], uv[2], uv[3]);
    gui.blit_tex(tex, x, y, 8.0, 8.0, f, WHITE);
    if hat {
        gui.blit_tex(tex, x, y, 8.0, 8.0, h, WHITE);
    }
}

/// PlayerTabOverlay.extractTablistHearts: hearts (or the health as a number when they do not
/// fit), blinking for a moment when the score changes (HealthState).
fn tab_hearts(gui: &mut Gui, _gl: &Gl, states: &mut BTreeMap<[u8; 16], (i32, i32, u32)>, uuid: [u8; 16], y: f32, left: f32, right: f32, score: i32, ticks: u32) {
    let st = states.entry(uuid).or_insert((score, score, 0));
    // HealthState.update: a change blinks 20 ticks (10 when it rose), showing the old value.
    if score != st.0 {
        st.2 = ticks + if score < st.0 { 20 } else { 10 };
        st.1 = st.0;
        st.0 = score;
    }
    if ticks >= st.2 {
        st.1 = score;
    }
    let displayed = st.1;
    let blink = ticks < st.2 && (st.2 - ticks) / 3 % 2 == 1;
    let full = (score.max(displayed) + 1) / 2;
    let slots = score.max(displayed.max(20)) / 2;
    if full <= 0 {
        return;
    }
    let step = ((right - left - 4.0) / slots.max(1) as f32).min(9.0).floor();
    if step <= 3.0 {
        let f = (score as f32 / 20.0).clamp(0.0, 1.0);
        let c = 0xFF00_0000 | (((1.0 - f) * 255.0) as u32) << 16 | ((f * 255.0) as u32) << 8;
        let hp = score as f32 / 2.0;
        let t = lang::format("multiplayer.player.list.hp", &[format!("{:.1}", hp)]);
        let t = if right - gui.text_width(&t) >= left { t } else { format!("{:.1}", hp) };
        let w = gui.text_width(&t);
        gui.text(&t, ((right + left - w) / 2.0).floor(), y, c);
        return;
    }
    let container = if blink { "hud/heart/container_blinking" } else { "hud/heart/container" };
    for m in full..slots {
        gui.blit(container, left + m as f32 * step, y);
    }
    for m in 0..full {
        let x = left + m as f32 * step;
        gui.blit(container, x, y);
        if blink {
            if m * 2 + 1 < displayed {
                gui.blit("hud/heart/full_blinking", x, y);
            }
            if m * 2 + 1 == displayed {
                gui.blit("hud/heart/half_blinking", x, y);
            }
        }
        if m * 2 + 1 < score {
            gui.blit(if m >= 10 { "hud/heart/absorbing_full_blinking" } else { "hud/heart/full" }, x, y);
        }
        if m * 2 + 1 == score {
            gui.blit(if m >= 10 { "hud/heart/absorbing_half_blinking" } else { "hud/heart/half" }, x, y);
        }
    }
}

// ------------------------------------------------------------------ the world border

/// Entity.collide's world border: a player inside it stops at it (its collision shape is the
/// outside of the border's floored and ceiled bounds).
pub fn collide(m: &mut crate::physics::Mover, prev: [f64; 3], ui: &ServerUi) {
    let b = ui.border.bounds(1.0);
    let (lo_x, lo_z, hi_x, hi_z) = (crate::math::floor(b[0]), crate::math::floor(b[1]), -crate::math::floor(-b[2]), -crate::math::floor(-b[3]));
    const R: f64 = 0.3;
    let inside = |x: f64, z: f64| x - R >= lo_x - 1e-7 && x + R <= hi_x + 1e-7 && z - R >= lo_z - 1e-7 && z + R <= hi_z + 1e-7;
    if !inside(prev[0], prev[2]) {
        return;
    }
    let p = &mut m.pos;
    if p[0] - R < lo_x {
        p[0] = lo_x + R;
        m.vel[0] = 0.0;
        m.horizontal_collision = true;
    } else if p[0] + R > hi_x {
        p[0] = hi_x - R;
        m.vel[0] = 0.0;
        m.horizontal_collision = true;
    }
    if p[2] - R < lo_z {
        p[2] = lo_z + R;
        m.vel[2] = 0.0;
        m.horizontal_collision = true;
    } else if p[2] + R > hi_z {
        p[2] = hi_z - R;
        m.vel[2] = 0.0;
        m.horizontal_collision = true;
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// WorldBorderRenderer: the forcefield on the sides within the render distance, fading in as
/// the camera nears them, scrolling over 3 s, in the border's colour (still blue, growing
/// green, shrinking red), added onto what is behind it.
pub fn draw_wall(gl: &Gl, view: &[f32; 16], eye: [f64; 3], ui: &ServerUi, partial: f64, view_chunks: i32, depth_far: f64) {
    let Some(tex) = pics(gl).and_then(|p| p.forcefield) else { return };
    let b = ui.border.bounds(partial);
    let d = view_chunks as f64 * 16.0;
    let (cx, cz) = (eye[0], eye[2]);
    if cx < b[2] - d && cx > b[0] + d && cz < b[3] - d && cz > b[1] + d {
        return;
    }
    let dist = {
        let dz = (cz - b[1]).min(b[3] - cz);
        let dx = (cx - b[0]).min(b[2] - cx);
        dz.min(dx)
    };
    let e = (1.0 - dist / d).max(0.0);
    let e = (e * e * e * e).clamp(0.0, 1.0);
    let colour = match ui.border.moving() {
        -1 => 0xFF3030u32,
        1 => 0x40FF80,
        _ => 0x20A0FF,
    };
    let c = [(colour >> 16) as u8, (colour >> 8) as u8, colour as u8, (e * 255.0) as u8];
    let m = (crate::sys::now_ms() % 3000) as f32 / 3000.0;
    let n = -((eye[1] * 0.5) - crate::math::floor(eye[1] * 0.5)) as f32;
    let h = depth_far;
    let o = n + h as f32;
    let mut vs: Vec<V> = Vec::new();
    let mut quad = |a: [f64; 3], bb: [f64; 3], cc: [f64; 3], dd: [f64; 3], uv: [[f32; 2]; 4]| {
        for (p, t) in [a, bb, cc, dd].iter().zip(uv.iter()) {
            vs.push(V { p: [(p[0]) as f32, (p[1]) as f32, (p[2]) as f32], uv: *t, c });
        }
    };
    // Along z on the x sides, along x on the z sides, one block a quad.
    let (zlo, zhi) = ((crate::math::floor(cz - d)).max(b[1]), (-crate::math::floor(-(cz + d))).min(b[3]));
    let r = (crate::math::floor(zlo) as i64 & 1) as f32 * 0.5;
    for (side, x) in [(cx > b[2] - d, b[2]), (cx < b[0] + d, b[0])] {
        if !side {
            continue;
        }
        let mut s = r;
        let mut t = zlo;
        while t < zhi {
            let u = (zhi - t).min(1.0);
            let v = u as f32 * 0.5;
            let px = x - cx;
            quad(
                [px, -h, t - cz],
                [px, -h, t + u - cz],
                [px, h, t + u - cz],
                [px, h, t - cz],
                [[m + s, m + o], [m + v + s, m + o], [m + v + s, m + n], [m + s, m + n]],
            );
            t += 1.0;
            s += 0.5;
        }
    }
    let (xlo, xhi) = ((crate::math::floor(cx - d)).max(b[0]), (-crate::math::floor(-(cx + d))).min(b[2]));
    let r = (crate::math::floor(xlo) as i64 & 1) as f32 * 0.5;
    for (side, z) in [(cz > b[3] - d, b[3]), (cz < b[1] + d, b[1])] {
        if !side {
            continue;
        }
        let mut s = r;
        let mut t = xlo;
        while t < xhi {
            let u = (xhi - t).min(1.0);
            let v = u as f32 * 0.5;
            let pz = z - cz;
            quad(
                [t - cx, -h, pz],
                [t + u - cx, -h, pz],
                [t + u - cx, h, pz],
                [t - cx, h, pz],
                [[m + s, m + o], [m + v + s, m + o], [m + v + s, m + n], [m + s, m + n]],
            );
            t += 1.0;
            s += 0.5;
        }
    }
    if vs.is_empty() {
        return;
    }
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, tex);
        (gl.Enable)(BLEND);
        (gl.BlendFunc)(SRC_ALPHA, ONE);
        (gl.DepthMask)(0);
        (gl.Disable)(CULL_FACE);
        (gl.Disable)(FOG);
        (gl.Disable)(ALPHA_TEST);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.EnableClientState)(VERTEX_ARRAY);
        (gl.EnableClientState)(COLOR_ARRAY);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.VertexPointer)(3, FLOAT, 24, vs.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (vs.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (vs.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, vs.len() as i32);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        (gl.DepthMask)(1);
        (gl.Enable)(CULL_FACE);
        (gl.Enable)(FOG);
        (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
        (gl.Disable)(BLEND);
        (gl.Disable)(TEXTURE_2D);
    }
}

/// Hud.extractVignette's world border part: the screen's edges tinted red (green and blue
/// taken away) as the camera nears a border, by the warning distance or warning time.
pub fn vignette(gl: &Gl, w: i32, h: i32, ui: &ServerUi, x: f64, z: f64) {
    let bd = &ui.border;
    let dist = bd.distance(x, z);
    let lerp = (bd.speed() * bd.warning_time as f64 * 20.0).min((bd.to - bd.size).abs());
    let e = (bd.warning_blocks as f64).max(lerp);
    if dist >= e || e <= 0.0 {
        return;
    }
    let f = (1.0 - dist / e).clamp(0.0, 1.0) as f32;
    let Some(tex) = pics(gl).and_then(|p| p.vignette) else { return };
    let c = [0u8, (f * 255.0) as u8, (f * 255.0) as u8, 255];
    let vs = [
        V { p: [-1.0, 1.0, 0.0], uv: [0.0, 0.0], c },
        V { p: [-1.0, -1.0, 0.0], uv: [0.0, 1.0], c },
        V { p: [1.0, -1.0, 0.0], uv: [1.0, 1.0], c },
        V { p: [1.0, 1.0, 0.0], uv: [1.0, 0.0], c },
    ];
    unsafe {
        (gl.Viewport)(0, 0, w, h);
        (gl.MatrixMode)(PROJECTION);
        (gl.LoadIdentity)();
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadIdentity)();
        (gl.Disable)(DEPTH_TEST);
        (gl.Disable)(FOG);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(CULL_FACE);
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, tex);
        (gl.Enable)(BLEND);
        // RenderPipelines.VIGNETTE: the screen times one minus the picture's colour.
        (gl.BlendFunc)(0, ONE_MINUS_SRC_COLOR);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.EnableClientState)(VERTEX_ARRAY);
        (gl.EnableClientState)(COLOR_ARRAY);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.VertexPointer)(3, FLOAT, 24, vs.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (vs.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (vs.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, 4);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
        (gl.Disable)(BLEND);
        (gl.Disable)(TEXTURE_2D);
    }
}

// ------------------------------------------------------------------ the death screen

/// DeathScreen's buttons: respawn (spectate in hardcore) and back to the title screen.
pub fn death_buttons(gui: &Gui, ui: &ServerUi) -> [Button; 2] {
    let mut b = crate::screens::death_buttons(gui);
    if ui.hardcore {
        b[0].label = lang::get("deathScreen.spectate");
    }
    b
}

/// DeathScreen: the red gradient, the title at twice the size, the cause of death and the
/// score.
pub fn draw_death(gui: &mut Gui, g: &Game, fallback: &str, mx: f32, my: f32) {
    let (w, h) = (gui.width, gui.height);
    gui.fill(0.0, 0.0, w, h / 2.0, 0x6050_0000);
    gui.fill(0.0, h / 2.0, w, h, 0xA080_3030);
    let cx = (w / 2.0).floor();
    let title = lang::get(if g.ui.hardcore { "deathScreen.title.hardcore" } else { "deathScreen.title" });
    let tw = gui.text_width(&title);
    text_scaled(gui, &title, (cx / 2.0 - (tw / 2.0).floor()) * 2.0, 30.0 * 2.0, 2.0, WHITE, true);
    let msg = g.ui.death_message.clone().unwrap_or_else(|| String::from(fallback));
    if !msg.is_empty() {
        gui.text_centered(&msg, cx, 85.0, WHITE);
    }
    let score = lang::format("deathScreen.score.value", &[format!("\u{a7}e{}", g.ui.score)]);
    gui.text_centered(&score, cx, 100.0, WHITE);
    for b in death_buttons(gui, &g.ui).iter() {
        b.draw(gui, mx, my);
    }
}

// ------------------------------------------------------------------ server links

pub enum LinkAct {
    None,
    Click,
    /// Back to the pause menu.
    Close,
}

/// The pause menu's "Server Links..." button (PauseScreen's custom additions row: 204 wide
/// under the icon row, pushing what follows down a row).
pub fn links_button(gui: &Gui) -> Button {
    let gx = ((gui.width - 212.0) / 2.0).floor() + 4.0;
    let top = ((gui.height - 190.0) * 0.25).floor();
    Button::new(gx, top + 122.0, 204.0, lang::get("menu.server_links"))
}

fn link_buttons(gui: &Gui, ui: &ServerUi) -> (Vec<Button>, Button) {
    let s = st();
    let n = ui.common.links.len();
    let body_top = 33.0;
    let body_bottom = gui.height - 33.0;
    let total = n as f32 * 24.0;
    let max_scroll = (total - (body_bottom - body_top - 8.0)).max(0.0);
    s.scroll = s.scroll.clamp(0.0, max_scroll);
    let w = 310.0f32.min(gui.width - 20.0);
    let x = ((gui.width - w) / 2.0).floor();
    let v = ui
        .common
        .links
        .iter()
        .enumerate()
        .map(|(i, (label, _))| Button::new(x, body_top + 4.0 + i as f32 * 24.0 - s.scroll, w, label.clone()))
        .collect();
    let back = Button::new(((gui.width - 200.0) / 2.0).floor(), gui.height - 33.0 + 6.0, 200.0, lang::get("gui.back"));
    (v, back)
}

fn confirm_buttons(gui: &Gui) -> [Button; 3] {
    let cx = (gui.width / 2.0).floor();
    let y = ((gui.height / 6.0).floor() + 96.0).max(140.0).min(gui.height - 24.0);
    [
        Button::new(cx - 50.0 - 105.0, y, 100.0, lang::get("chat.link.open")),
        Button::new(cx - 50.0, y, 100.0, lang::get("chat.copy")),
        Button::new(cx - 50.0 + 105.0, y, 100.0, lang::get("gui.cancel")),
    ]
}

pub fn open_links() {
    let s = st();
    s.scroll = 0.0;
    s.confirm = None;
}

pub fn draw_links(gui: &mut Gui, g: &Game, mx: f32, my: f32) {
    crate::screens::dim(gui);
    let cx = (gui.width / 2.0).floor();
    if let Some(i) = st().confirm {
        let url = g.ui.common.links.get(i).map(|l| l.1.clone()).unwrap_or_default();
        gui.text_centered(&lang::get("chat.link.confirm"), cx, 70.0, WHITE);
        gui.text_centered(&url, cx, 90.0, WHITE);
        gui.text_centered(&lang::get("chat.link.warning"), cx, 110.0, 0xFFFF_CCCC);
        for b in confirm_buttons(gui).iter() {
            b.draw(gui, mx, my);
        }
        return;
    }
    gui.text_centered(&lang::get("menu.server_links.title"), cx, 12.0, WHITE);
    let (links, back) = link_buttons(gui, &g.ui);
    for b in links.iter() {
        if b.y + b.h > 33.0 && b.y < gui.height - 33.0 {
            b.draw(gui, mx, my);
        }
    }
    back.draw(gui, mx, my);
}

pub fn links_wheel(dy: i32) {
    st().scroll -= dy as f32 * 12.0;
}

pub fn links_escape() -> LinkAct {
    let s = st();
    if s.confirm.take().is_some() {
        return LinkAct::Click;
    }
    LinkAct::Close
}

pub fn links_click(gui: &Gui, g: &Game, mx: f32, my: f32) -> LinkAct {
    let s = st();
    if let Some(i) = s.confirm {
        let url = g.ui.common.links.get(i).map(|l| l.1.clone()).unwrap_or_default();
        let b = confirm_buttons(gui);
        if b[0].hit(mx, my) {
            open_url(&url);
            s.confirm = None;
            return LinkAct::Click;
        }
        if b[1].hit(mx, my) {
            set_clipboard(&url);
            s.confirm = None;
            return LinkAct::Click;
        }
        if b[2].hit(mx, my) {
            s.confirm = None;
            return LinkAct::Click;
        }
        return LinkAct::None;
    }
    let (links, back) = link_buttons(gui, &g.ui);
    if back.hit(mx, my) {
        return LinkAct::Close;
    }
    for (i, b) in links.iter().enumerate() {
        if b.hit(mx, my) && my > 33.0 && my < gui.height - 33.0 {
            st().confirm = Some(i);
            return LinkAct::Click;
        }
    }
    LinkAct::None
}

/// dlsym with RTLD_DEFAULT: SDL is loaded with RTLD_GLOBAL, the C library is always there.
fn symbol(name: &str) -> *mut c_void {
    let default: *mut c_void = if cfg!(target_os = "macos") { -2isize as *mut c_void } else { core::ptr::null_mut() };
    unsafe { crate::sys::dlsym(default, crate::sys::cstr(name).as_ptr() as *const core::ffi::c_char) }
}

/// SDL_SetClipboardText (SDL 2.0.0 has it), looked up when used.
pub fn set_clipboard(s: &str) {
    let f = symbol("SDL_SetClipboardText");
    if f.is_null() {
        return;
    }
    let set: unsafe extern "C" fn(*const core::ffi::c_char) -> i32 = unsafe { core::mem::transmute(f) };
    let c = crate::sys::cstr(s);
    unsafe { set(c.as_ptr() as *const core::ffi::c_char) };
}

/// Util.getPlatform().openUri: `open` on the Mac, `xdg-open` elsewhere, started with
/// posix_spawnp (in 10.6's libSystem and glibc), found at run time.
pub fn open_url(url: &str) {
    let l = url.to_ascii_lowercase();
    if !(l.starts_with("http://") || l.starts_with("https://")) {
        return;
    }
    let spawn = symbol("posix_spawnp");
    if spawn.is_null() {
        return;
    }
    type Spawn = unsafe extern "C" fn(*mut i32, *const core::ffi::c_char, *const c_void, *const c_void, *const *const core::ffi::c_char, *const *const core::ffi::c_char) -> i32;
    let spawn: Spawn = unsafe { core::mem::transmute(spawn) };
    let env: *const *const core::ffi::c_char = if cfg!(target_os = "macos") {
        let f = symbol("_NSGetEnviron");
        if f.is_null() {
            core::ptr::null()
        } else {
            let g: unsafe extern "C" fn() -> *const *const *const core::ffi::c_char = unsafe { core::mem::transmute(f) };
            unsafe { *g() }
        }
    } else {
        let p = symbol("environ") as *const *const *const core::ffi::c_char;
        if p.is_null() {
            core::ptr::null()
        } else {
            unsafe { *p }
        }
    };
    let prog = crate::sys::cstr(if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    let arg = crate::sys::cstr(url);
    let argv = [prog.as_ptr() as *const core::ffi::c_char, arg.as_ptr() as *const core::ffi::c_char, core::ptr::null()];
    let mut pid = 0i32;
    unsafe { spawn(&mut pid, argv[0], core::ptr::null(), core::ptr::null(), argv.as_ptr(), env) };
}

// ------------------------------------------------------------------ self test

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    out.push(("hsv to argb", format!("{:08x}", hsv(0.0, 0.7, 0.6, 255)), "ff992d2d"));
    let s = st();
    let n = s.raw.len() + s.pics.as_ref().map_or(0, |p| p.rects.len());
    if n > 0 {
        let have = |name: &str| s.raw.iter().any(|r| r.0 == name);
        out.push(("serverui.bin pictures", format!("{} {} {}", have("boss_bar/red_progress"), have("icon/ping_5"), have("misc/forcefield")), "true true true"));
        let sword = s.defaults.as_ref().and_then(|d| d.modifiers.values().find(|m| m.iter().any(|x| x.id == "minecraft:base_attack_damage" && x.amount == 6.0)));
        out.push(("item default attributes", format!("{}", sword.map_or(0, |m| m.len())), "2"));
    } else {
        crate::say!("skip serverui.bin checks: no serverui.bin");
    }
    out
}
