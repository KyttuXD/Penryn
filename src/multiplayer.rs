//! Vanilla's multiplayer screens: JoinMultiplayerScreen with its ServerSelectionList (servers
//! with their status pings, favicons, ping bars and player tooltips, LAN worlds), the Add and
//! Edit Server screen (ManageServerScreen), Direct Connection (DirectJoinServerScreen) and the
//! delete question (ConfirmScreen), laid out as 26.3 lays them out.

use crate::gui::Gui;
use crate::lang;
use crate::screens::{self, Button, WHITE};
use crate::servers::{self, Pack, Ping, Server};
use crate::task::{self, Task};
use crate::title::{self, Assets};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const GREY: u32 = 0xFF80_8080;
const LABEL: u32 = 0xFFA0_A0A0;
/// HeaderAndFooterLayout(33, 60), 36-unit rows 305 wide.
const HEADER: f32 = 33.0;
const FOOTER: f32 = 60.0;
const ROW: f32 = 36.0;
const ROW_W: f32 = 305.0;

pub fn fl(x: f32) -> f32 {
    crate::math::floor(x as f64) as f32
}

// ------------------------------------------------------------------ EditBox

/// EditBox: a bordered text field with a caret, scrolled to keep the caret in view.
pub struct EditBox {
    pub text: String,
    /// The caret, in characters.
    pub caret: usize,
    pub max: usize,
    pub focused: bool,
    focused_at: u64,
    pub hint: Option<String>,
}

impl EditBox {
    pub fn new(text: &str, max: usize) -> EditBox {
        let text: String = text.chars().take(max).collect();
        EditBox { caret: text.chars().count(), text, max, focused: false, focused_at: 0, hint: None }
    }

    pub fn focus(&mut self, on: bool) {
        if on && !self.focused {
            self.focused_at = crate::sys::now_ms();
        }
        self.focused = on;
    }

    fn byte_at(&self, ch: usize) -> usize {
        self.text.char_indices().nth(ch).map_or(self.text.len(), |(i, _)| i)
    }

    /// Typed text (SharedConstants.isAllowedChatCharacter: no control characters, no §).
    pub fn insert(&mut self, s: &str) {
        for c in s.chars() {
            if c < ' ' || c == '\u{7f}' || c == '\u{a7}' || self.text.chars().count() >= self.max {
                continue;
            }
            let at = self.byte_at(self.caret);
            self.text.insert(at, c);
            self.caret += 1;
        }
        self.focused_at = crate::sys::now_ms();
    }

    /// A key: true when it changed the text.
    pub fn key(&mut self, sc: usize, ctrl: bool, clipboard: impl FnOnce() -> String) -> bool {
        let n = self.text.chars().count();
        self.focused_at = crate::sys::now_ms();
        match sc {
            42 if self.caret > 0 => {
                // Backspace; with Ctrl the word before the caret.
                let mut from = self.caret - 1;
                if ctrl {
                    let cs: Vec<char> = self.text.chars().collect();
                    while from > 0 && cs[from - 1] == ' ' {
                        from -= 1;
                    }
                    while from > 0 && cs[from - 1] != ' ' {
                        from -= 1;
                    }
                }
                let (a, b) = (self.byte_at(from), self.byte_at(self.caret));
                self.text.replace_range(a..b, "");
                self.caret = from;
                true
            }
            76 if self.caret < n => {
                let (a, b) = (self.byte_at(self.caret), self.byte_at(self.caret + 1));
                self.text.replace_range(a..b, "");
                true
            }
            80 => {
                self.caret = self.caret.saturating_sub(1);
                false
            }
            79 => {
                self.caret = (self.caret + 1).min(n);
                false
            }
            74 => {
                self.caret = 0;
                false
            }
            77 => {
                self.caret = n;
                false
            }
            25 if ctrl => {
                let t = clipboard();
                self.insert(&t);
                true
            }
            _ => false,
        }
    }

    /// The caret goes where the field was clicked.
    pub fn click(&mut self, gui: &Gui, x: f32, mx: f32) {
        let (start, _) = self.window(gui, 192.0);
        let mut pos = x + 4.0;
        let cs: Vec<char> = self.text.chars().collect();
        let mut i = start;
        let mut buf = [0u8; 4];
        while i < cs.len() {
            let cw = gui.text_width(cs[i].encode_utf8(&mut buf));
            if mx < pos + cw / 2.0 {
                break;
            }
            pos += cw;
            i += 1;
        }
        self.caret = i;
    }

    /// The characters shown: from where, and how many fit in `room`.
    fn window(&self, gui: &Gui, room: f32) -> (usize, usize) {
        let cs: Vec<char> = self.text.chars().collect();
        let mut buf = [0u8; 4];
        let w = |c: char, b: &mut [u8; 4]| gui.text_width(c.encode_utf8(b));
        // Scroll so the caret is in view.
        let mut start = 0;
        loop {
            let mut used = 0.0;
            let mut end = start;
            while end < cs.len() && used + w(cs[end], &mut buf) <= room {
                used += w(cs[end], &mut buf);
                end += 1;
            }
            if self.caret <= end || start >= cs.len() {
                return (start, end - start);
            }
            start += 1;
        }
    }

    pub fn draw(&self, gui: &mut Gui, x: f32, y: f32, w: f32, h: f32) {
        gui.blit_nine(if self.focused { "widget/text_field_highlighted" } else { "widget/text_field" }, x, y, w, h);
        let (tx, ty) = (x + 4.0, y + fl((h - 8.0) / 2.0));
        if self.text.is_empty() && !self.focused {
            if let Some(hint) = &self.hint {
                gui.text(hint, tx, ty, GREY);
            }
            return;
        }
        let (start, count) = self.window(gui, w - 8.0);
        let shown: String = self.text.chars().skip(start).take(count).collect();
        let before: String = self.text.chars().skip(start).take(self.caret.saturating_sub(start)).collect();
        gui.text(&shown, tx, ty, 0xFFE0_E0E0);
        let blink = (crate::sys::now_ms().saturating_sub(self.focused_at) / 300) % 2 == 0;
        if self.focused && blink {
            let cx = tx + gui.text_width(&before);
            if self.caret >= self.text.chars().count() {
                gui.text("_", cx, ty, 0xFFE0_E0E0);
            } else {
                gui.fill(cx, ty - 1.0, cx + 1.0, ty + 10.0, 0xFFD0_D0D0);
            }
        }
    }
}

// ------------------------------------------------------------------ the server list

#[derive(Clone, Copy, PartialEq)]
enum St {
    Initial,
    Pinging,
    Ok,
    Incompatible,
    Unreachable,
}

struct Entry {
    st: St,
    task: Option<Task<Ping>>,
    motd: String,
    status: String,
    players: Vec<String>,
    ping: u64,
    /// The favicon's texture (0: the unknown server picture) and what it shows.
    icon: u32,
    icon_of: Option<Vec<u8>>,
}

impl Entry {
    fn new() -> Entry {
        Entry { st: St::Initial, task: None, motd: String::new(), status: String::new(), players: Vec::new(), ping: 0, icon: 0, icon_of: None }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sel {
    Server(usize),
    Lan(usize),
}

enum Sub {
    List,
    /// ManageServerScreen: which server (None: a new one), the fields, which field has focus.
    Manage { edit: Option<usize>, name: EditBox, ip: EditBox, pack: Pack },
    Direct { ip: EditBox },
    Confirm { index: usize },
}

pub enum Act {
    None,
    /// A button was pressed (the click sound).
    Click,
    /// Back to the title screen.
    Back,
    /// Join this address (Direct Connection, a server, a LAN world), with its server's pack setting.
    Join(String, Pack),
}

pub struct Mp {
    pub list: servers::List,
    entries: Vec<Entry>,
    lan: Vec<(String, String)>,
    sel: Option<Sel>,
    scroll: f32,
    last_click: (u64, Option<Sel>),
    sub: Sub,
    /// Textures of favicons no longer shown, for the next ones.
    free_tex: Vec<u32>,
    /// options.txt's lastServer: what Direct Connection last joined.
    pub last_direct: String,
}

/// The footer's buttons (JoinMultiplayerScreen.init: a row of three 100 wide, a row of four 74
/// wide, 4 apart, the rows 4 apart and centred in the 60-unit footer).
fn footer(gw: f32, gh: f32, have_sel: bool, sel_server: bool) -> [Button; 7] {
    let top = gh - FOOTER + 8.0;
    let x1 = fl((gw - 308.0) / 2.0);
    let x2 = fl((gw - 308.0) / 2.0);
    let mut b = [
        Button::new(x1, top, 100.0, lang::get("selectServer.select")),
        Button::new(x1 + 104.0, top, 100.0, lang::get("selectServer.direct")),
        Button::new(x1 + 208.0, top, 100.0, lang::get("selectServer.add")),
        Button::new(x2, top + 24.0, 74.0, lang::get("selectServer.edit")),
        Button::new(x2 + 78.0, top + 24.0, 74.0, lang::get("selectServer.delete")),
        Button::new(x2 + 156.0, top + 24.0, 74.0, lang::get("selectServer.refresh")),
        Button::new(x2 + 234.0, top + 24.0, 74.0, lang::get("gui.back")),
    ];
    // onSelectedChange: Join for any entry, Edit and Delete for saved servers only.
    b[0].active = have_sel;
    b[3].active = sel_server;
    b[4].active = sel_server;
    b
}

/// ManageServerScreen's widgets: name field, address field (y), the pack button, Done, Cancel.
fn manage_layout(gw: f32, gh: f32) -> (f32, f32, f32, Button, Button, Button) {
    let x = fl(gw / 2.0) - 100.0;
    let q = fl(gh / 4.0);
    (
        x,
        66.0,
        106.0,
        Button::new(x, q + 72.0, 200.0, String::new()),
        Button::new(x, q + 96.0 + 18.0, 200.0, lang::get("gui.done")),
        Button::new(x, q + 120.0 + 18.0, 200.0, lang::get("gui.cancel")),
    )
}

fn direct_layout(gw: f32, gh: f32) -> (f32, f32, Button, Button) {
    let x = fl(gw / 2.0) - 100.0;
    let q = fl(gh / 4.0);
    (x, 116.0, Button::new(x, q + 96.0 + 12.0, 200.0, lang::get("selectServer.select")), Button::new(x, q + 120.0 + 12.0, 200.0, lang::get("gui.cancel")))
}

/// ConfirmScreen: title, message lines (8 apart), then the buttons 16 lower, centred on the
/// screen as one column.
fn confirm_layout(gui: &Gui, msg: &str) -> (f32, Vec<String>, Button, Button) {
    let lines = screens::wrap(gui, msg, gui.width - 50.0);
    let total = 9.0 + 8.0 + lines.len() as f32 * 9.0 + 8.0 + 16.0 + 20.0;
    let top = fl((gui.height - total) / 2.0);
    let by = top + 9.0 + 8.0 + lines.len() as f32 * 9.0 + 8.0 + 16.0;
    let x = fl(gui.width / 2.0) - 152.0;
    (top, lines, Button::new(x, by, 150.0, lang::get("selectServer.deleteButton")), Button::new(x + 154.0, by, 150.0, lang::get("gui.cancel")))
}

impl Mp {
    pub fn new(last_direct: String) -> Mp {
        let list = servers::List::load();
        let entries = list.servers.iter().map(|_| Entry::new()).collect();
        Mp { list, entries, lan: Vec::new(), sel: None, scroll: 0.0, last_click: (0, None), sub: Sub::List, free_tex: Vec::new(), last_direct }
    }

    /// Back on the list (after a game, or Refresh): every server is pinged again.
    pub fn reopen(&mut self) {
        self.sub = Sub::List;
        let list = servers::List::load();
        self.list = list;
        for e in self.entries.drain(..) {
            if e.icon != 0 {
                self.free_tex.push(e.icon);
            }
        }
        self.entries = self.list.servers.iter().map(|_| Entry::new()).collect();
        if let Some(Sel::Server(i)) = self.sel {
            if i >= self.entries.len() {
                self.sel = None;
            }
        }
    }

    pub fn in_text(&self) -> bool {
        !matches!(self.sub, Sub::List | Sub::Confirm { .. })
    }

    fn list_area(gui: &Gui) -> (f32, f32) {
        (HEADER, gui.height - FOOTER)
    }

    fn content_height(&self) -> f32 {
        (self.entries.len() + 1 + self.lan.len()) as f32 * ROW + 4.0
    }

    fn max_scroll(&self, gui: &Gui) -> f32 {
        let (top, bottom) = Self::list_area(gui);
        (self.content_height() - (bottom - top)).max(0.0)
    }

    /// Rows top to bottom: (y of the row, what it is). LAN header is None.
    fn rows(&self, gui: &Gui) -> Vec<(f32, Option<Sel>)> {
        let (top, _) = Self::list_area(gui);
        let mut y = top + 2.0 - fl(self.scroll);
        let mut out = Vec::new();
        for i in 0..self.entries.len() {
            out.push((y, Some(Sel::Server(i))));
            y += ROW;
        }
        out.push((y, None));
        y += ROW;
        for i in 0..self.lan.len() {
            out.push((y, Some(Sel::Lan(i))));
            y += ROW;
        }
        out
    }

    fn row_left(gui: &Gui) -> f32 {
        fl(gui.width / 2.0) - fl(ROW_W / 2.0)
    }

    fn row_at(&self, gui: &Gui, mx: f32, my: f32) -> Option<(f32, Option<Sel>)> {
        let (top, bottom) = Self::list_area(gui);
        let left = Self::row_left(gui);
        if my < top || my >= bottom || mx < left || mx >= left + ROW_W {
            return None;
        }
        self.rows(gui).into_iter().find(|(y, _)| my >= *y && my < *y + ROW)
    }

    /// The pings (ServerStatusPinger.tick): results in, new pings out (five at once, as
    /// vanilla's pinger pool), LAN worlds heard. Favicons go to textures; a changed favicon is
    /// saved in servers.dat as vanilla saves it.
    pub fn tick(&mut self, gl: &crate::sdl::Gl) {
        let mut save = false;
        let mut running = self.entries.iter().filter(|e| e.st == St::Pinging).count();
        for i in 0..self.entries.len() {
            if self.entries[i].st == St::Initial && running < 5 {
                let ip = self.list.servers[i].ip.clone();
                self.entries[i].st = St::Pinging;
                self.entries[i].motd.clear();
                self.entries[i].status.clear();
                self.entries[i].task = Some(task::spawn(move || servers::ping(&ip)));
                running += 1;
            }
            let Some(r) = self.entries[i].task.as_mut().and_then(|t| t.take()) else { continue };
            let e = &mut self.entries[i];
            e.task = None;
            match r {
                Ping::CantResolve | Ping::CantConnect => {
                    e.st = St::Unreachable;
                    let key = if matches!(r, Ping::CantResolve) { "multiplayer.status.cannot_resolve" } else { "multiplayer.status.cannot_connect" };
                    e.motd = format!("\u{a7}#FF0000{}", lang::get(key));
                    e.status.clear();
                    e.players.clear();
                }
                Ping::Ok(s) => {
                    e.st = if s.protocol == crate::ids::PROTOCOL { St::Ok } else { St::Incompatible };
                    e.motd = s.motd;
                    e.ping = s.ping;
                    match s.players {
                        Some((online, max)) => {
                            e.status = format!("\u{a7}8{}", lang::format("multiplayer.status.player_count", &[format!("\u{a7}7{}\u{a7}8", online), format!("\u{a7}7{}\u{a7}8", max)]));
                            e.players = s.sample.clone();
                            if (s.sample.len() as i32) < online && !s.sample.is_empty() {
                                e.players.push(lang::format("multiplayer.status.and_more", &[format!("{}", online - s.sample.len() as i32)]));
                            }
                        }
                        None => {
                            e.status = format!("\u{a7}8{}", lang::get("multiplayer.status.unknown"));
                            e.players.clear();
                        }
                    }
                    if e.st == St::Incompatible {
                        e.status = format!("\u{a7}c{}", s.version);
                    }
                    if s.icon != self.list.servers[i].icon {
                        self.list.servers[i].icon = s.icon;
                        save = true;
                    }
                }
            }
        }
        // Favicons to textures (and back to the unknown server picture when one goes away).
        for i in 0..self.entries.len() {
            let want = self.list.servers[i].icon.clone();
            if self.entries[i].icon_of != want {
                let img = want.as_deref().and_then(crate::skins::png).filter(|(w, h, _)| *w == 64 && *h == 64);
                match img {
                    Some((w, h, px)) => {
                        let t = match self.entries[i].icon {
                            0 => self.free_tex.pop().unwrap_or_else(|| title::new_texture(gl)),
                            t => t,
                        };
                        title::upload(gl, t, w, h, &px, 4, false);
                        self.entries[i].icon = t;
                    }
                    None => {
                        if self.entries[i].icon != 0 {
                            self.free_tex.push(self.entries[i].icon);
                        }
                        self.entries[i].icon = 0;
                        if want.is_some() {
                            // Not a 64 x 64 PNG: vanilla forgets it.
                            self.list.servers[i].icon = None;
                            save = true;
                        }
                    }
                }
                self.entries[i].icon_of = self.list.servers[i].icon.clone();
            }
        }
        if save {
            self.list.save();
        }
        self.lan = servers::lan_worlds();
    }

    fn select_join(&self) -> Act {
        match self.sel {
            Some(Sel::Server(i)) => match self.list.servers.get(i) {
                Some(s) => Act::Join(s.ip.clone(), s.pack),
                None => Act::None,
            },
            Some(Sel::Lan(i)) => match self.lan.get(i) {
                Some((_, a)) => Act::Join(a.clone(), Pack::Prompt),
                None => Act::None,
            },
            None => Act::None,
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.list.servers.swap(a, b);
        self.entries.swap(a, b);
        self.list.save();
        self.sel = Some(Sel::Server(b));
    }

    pub fn wheel(&mut self, gui: &Gui, dy: i32) {
        if let Sub::List = self.sub {
            let max = self.max_scroll(gui);
            self.scroll = (self.scroll - dy as f32 * ROW / 2.0).clamp(0.0, max);
        }
    }

    pub fn click(&mut self, gui: &Gui, mx: f32, my: f32, shift: bool) -> Act {
        let (gw, gh) = (gui.width, gui.height);
        match &mut self.sub {
            Sub::List => {
                let sel_server = matches!(self.sel, Some(Sel::Server(_)));
                let b = footer(gw, gh, self.sel.is_some(), sel_server);
                if b[0].hit(mx, my) {
                    return self.select_join();
                }
                if b[1].hit(mx, my) {
                    let mut ip = EditBox::new(&self.last_direct, 128);
                    ip.focus(true);
                    self.sub = Sub::Direct { ip };
                    return Act::Click;
                }
                if b[2].hit(mx, my) {
                    let mut name = EditBox::new(&lang::get("selectServer.defaultName"), 32);
                    name.hint = Some(lang::get("selectServer.defaultName"));
                    name.focus(true);
                    self.sub = Sub::Manage { edit: None, name, ip: EditBox::new("", 128), pack: Pack::Prompt };
                    return Act::Click;
                }
                if b[3].hit(mx, my) {
                    if let Some(Sel::Server(i)) = self.sel {
                        let s = &self.list.servers[i];
                        let mut name = EditBox::new(&s.name, 32);
                        name.hint = Some(lang::get("selectServer.defaultName"));
                        name.focus(true);
                        self.sub = Sub::Manage { edit: Some(i), name, ip: EditBox::new(&s.ip, 128), pack: s.pack };
                    }
                    return Act::Click;
                }
                if b[4].hit(mx, my) {
                    if let Some(Sel::Server(i)) = self.sel {
                        self.sub = Sub::Confirm { index: i };
                    }
                    return Act::Click;
                }
                if b[5].hit(mx, my) {
                    self.reopen();
                    return Act::Click;
                }
                if b[6].hit(mx, my) {
                    return Act::Back;
                }
                // The list: select, double-click (or the icon's right half) joins, the left
                // quarters move a server up or down.
                let Some((y, Some(what))) = self.row_at(gui, mx, my) else { return Act::None };
                let now = crate::sys::now_ms();
                let double = self.last_click.1 == Some(what) && now - self.last_click.0 < 250;
                self.last_click = (now, Some(what));
                self.sel = Some(what);
                let (cx, cy) = (Self::row_left(gui) + 2.0, y + 2.0);
                let (rx, ry) = (mx - cx, my - cy);
                if let Sel::Server(i) = what {
                    if rx < 32.0 && rx > 16.0 && ry >= 0.0 && ry < 32.0 {
                        return self.select_join();
                    }
                    if rx < 16.0 && rx >= 0.0 && ry < 16.0 && ry >= 0.0 && i > 0 {
                        let j = if shift { 0 } else { i - 1 };
                        self.swap(i, j);
                        return Act::None;
                    }
                    if rx < 16.0 && rx >= 0.0 && ry >= 16.0 && ry < 32.0 && i + 1 < self.list.servers.len() {
                        let j = if shift { self.list.servers.len() - 1 } else { i + 1 };
                        self.swap(i, j);
                        return Act::None;
                    }
                }
                if double {
                    return self.select_join();
                }
                Act::None
            }
            Sub::Manage { edit, name, ip, pack } => {
                let (x, ny, iy, pb, done, cancel) = manage_layout(gw, gh);
                let inside = |y: f32| mx >= x && mx < x + 200.0 && my >= y && my < y + 20.0;
                if inside(ny) {
                    name.focus(true);
                    ip.focus(false);
                    name.click(gui, x, mx);
                } else if inside(iy) {
                    ip.focus(true);
                    name.focus(false);
                    ip.click(gui, x, mx);
                }
                if pb.hit(mx, my) {
                    *pack = pack.next(shift);
                    return Act::Click;
                }
                if done.hit(mx, my) && servers::valid(&ip.text) {
                    let shown = if name.text.is_empty() { lang::get("selectServer.defaultName") } else { name.text.clone() };
                        let mut s = Server::new(&shown, &ip.text);
                    s.pack = *pack;
                    let edit = *edit;
                    self.finish_manage(edit, s);
                    return Act::Click;
                }
                if cancel.hit(mx, my) {
                    self.sub = Sub::List;
                    return Act::Click;
                }
                Act::None
            }
            Sub::Direct { ip } => {
                let (x, iy, join, cancel) = direct_layout(gw, gh);
                if mx >= x && mx < x + 200.0 && my >= iy && my < iy + 20.0 {
                    ip.focus(true);
                    ip.click(gui, x, mx);
                }
                if join.hit(mx, my) && servers::valid(&ip.text) {
                    return self.direct_join();
                }
                if cancel.hit(mx, my) {
                    self.sub = Sub::List;
                    return Act::Click;
                }
                Act::None
            }
            Sub::Confirm { index } => {
                let index = *index;
                let msg = format!("{}\n{}", lang::get("selectServer.deleteQuestion"), self.delete_warning(index));
                let (_, _, yes, no) = confirm_layout(gui, &msg);
                if yes.hit(mx, my) {
                    // deleteCallback: remove, save, select nothing.
                    if index < self.list.servers.len() {
                        self.list.servers.remove(index);
                        let e = self.entries.remove(index);
                        if e.icon != 0 {
                            self.free_tex.push(e.icon);
                        }
                        self.list.save();
                    }
                    self.sel = None;
                    self.sub = Sub::List;
                    return Act::Click;
                }
                if no.hit(mx, my) {
                    self.sub = Sub::List;
                    return Act::Click;
                }
                Act::None
            }
        }
    }

    fn delete_warning(&self, i: usize) -> String {
        lang::format("selectServer.deleteWarning", &[self.list.servers.get(i).map(|s| s.name.clone()).unwrap_or_default()])
    }

    /// addServerCallback / editServerCallback.
    fn finish_manage(&mut self, edit: Option<usize>, s: Server) {
        match edit {
            Some(i) if i < self.list.servers.len() => {
                let old = &mut self.list.servers[i];
                old.name = s.name;
                if old.ip != s.ip {
                    old.ip = s.ip;
                }
                old.pack = s.pack;
                self.entries[i].st = St::Initial;
            }
            _ => {
                // A Direct Connection address added for good keeps its remembered icon.
                let mut s = s;
                if let Some(h) = self.list.unhide(&s.ip) {
                    self.list.servers.pop();
                    s.icon = h.icon;
                }
                self.list.servers.push(s);
                self.entries.push(Entry::new());
                self.sel = Some(Sel::Server(self.list.servers.len() - 1));
            }
        }
        self.list.save();
        self.sub = Sub::List;
    }

    /// directJoinCallback: remember the address (hidden) unless it is listed, then join.
    fn direct_join(&mut self) -> Act {
        let Sub::Direct { ip } = &self.sub else { return Act::None };
        let addr = String::from(ip.text.trim());
        self.last_direct = addr.clone();
        if !self.list.find(&addr) {
            self.list.add_hidden(Server::new(&lang::get("selectServer.defaultName"), &addr));
            self.list.save();
        }
        self.sub = Sub::List;
        Act::Join(addr, Pack::Prompt)
    }

    /// Escape, Enter, Tab and the text keys.
    pub fn key(&mut self, sc: usize, ctrl: bool, shift: bool, clipboard: impl FnOnce() -> String) -> Act {
        match &mut self.sub {
            Sub::List => match sc {
                41 => Act::Back,
                62 => {
                    self.reopen();
                    Act::None
                }
                40 | 88 => self.select_join(),
                81 | 82 => {
                    // Up and down move the selection; with Shift, move the server.
                    let n = self.entries.len() + self.lan.len();
                    if n == 0 {
                        return Act::None;
                    }
                    let flat = |s: Option<Sel>, servers: usize| match s {
                        Some(Sel::Server(i)) => Some(i),
                        Some(Sel::Lan(i)) => Some(servers + i),
                        None => None,
                    };
                    let servers = self.entries.len();
                    let cur = flat(self.sel, servers);
                    if shift {
                        if let Some(Sel::Server(i)) = self.sel {
                            if sc == 82 && i > 0 {
                                self.swap(i, i - 1);
                            } else if sc == 81 && i + 1 < servers {
                                self.swap(i, i + 1);
                            }
                        }
                        return Act::None;
                    }
                    let next = match cur {
                        None => 0,
                        Some(c) if sc == 81 => (c + 1).min(n - 1),
                        Some(c) => c.saturating_sub(1),
                    };
                    self.sel = Some(if next < servers { Sel::Server(next) } else { Sel::Lan(next - servers) });
                    Act::None
                }
                _ => Act::None,
            },
            Sub::Manage { edit, name, ip, pack } => match sc {
                41 => {
                    self.sub = Sub::List;
                    Act::None
                }
                43 => {
                    let n = name.focused;
                    name.focus(!n);
                    ip.focus(n);
                    Act::None
                }
                40 | 88 => {
                    if servers::valid(&ip.text) {
                        let shown = if name.text.is_empty() { lang::get("selectServer.defaultName") } else { name.text.clone() };
                        let mut s = Server::new(&shown, &ip.text);
                        s.pack = *pack;
                        let edit = *edit;
                        self.finish_manage(edit, s);
                    }
                    Act::None
                }
                _ => {
                    if name.focused {
                        name.key(sc, ctrl, clipboard);
                    } else if ip.focused {
                        ip.key(sc, ctrl, clipboard);
                    }
                    Act::None
                }
            },
            Sub::Direct { ip } => match sc {
                41 => {
                    self.sub = Sub::List;
                    Act::None
                }
                40 | 88 if servers::valid(&ip.text) => self.direct_join(),
                _ => {
                    ip.key(sc, ctrl, clipboard);
                    Act::None
                }
            },
            Sub::Confirm { .. } => {
                if sc == 41 {
                    self.sub = Sub::List;
                }
                Act::None
            }
        }
    }

    pub fn text(&mut self, s: &str) {
        match &mut self.sub {
            Sub::Manage { name, ip, .. } => {
                if name.focused {
                    name.insert(s);
                } else if ip.focused {
                    ip.insert(s);
                }
            }
            Sub::Direct { ip } => ip.insert(s),
            _ => {}
        }
    }

    pub fn draw(&mut self, gui: &mut Gui, a: Option<&Assets>, mx: f32, my: f32) {
        let (gw, gh) = (gui.width, gui.height);
        let cx = fl(gw / 2.0);
        match &self.sub {
            Sub::List => {}
            Sub::Manage { edit, name, ip, pack } => {
                let (x, ny, iy, mut pb, mut done, cancel) = manage_layout(gw, gh);
                let title = lang::get(if edit.is_some() { "manageServer.edit.title" } else { "manageServer.add.title" });
                gui.text_centered(&title, cx, 17.0, WHITE);
                gui.text(&lang::get("manageServer.enterName"), x + 1.0, 53.0, LABEL);
                gui.text(&lang::get("manageServer.enterIp"), x + 1.0, 94.0, LABEL);
                name.draw(gui, x, ny, 200.0, 20.0);
                ip.draw(gui, x, iy, 200.0, 20.0);
                pb.label = lang::format("options.generic_value", &[lang::get("manageServer.resourcePack"), lang::get(pack.key())]);
                done.active = servers::valid(&ip.text);
                pb.draw(gui, mx, my);
                done.draw(gui, mx, my);
                cancel.draw(gui, mx, my);
                return;
            }
            Sub::Direct { ip } => {
                let (x, iy, mut join, cancel) = direct_layout(gw, gh);
                gui.text_centered(&lang::get("selectServer.direct"), cx, 20.0, WHITE);
                gui.text(&lang::get("manageServer.enterIp"), x + 1.0, 100.0, LABEL);
                ip.draw(gui, x, iy, 200.0, 20.0);
                join.active = servers::valid(&ip.text);
                join.draw(gui, mx, my);
                cancel.draw(gui, mx, my);
                return;
            }
            Sub::Confirm { index } => {
                let msg = self.delete_warning(*index);
                let (top, lines, yes, no) = confirm_layout(gui, &msg);
                gui.text_centered(&lang::get("selectServer.deleteQuestion"), cx, top, WHITE);
                for (i, l) in lines.iter().enumerate() {
                    gui.text_centered(l, cx, top + 17.0 + i as f32 * 9.0, WHITE);
                }
                yes.draw(gui, mx, my);
                no.draw(gui, mx, my);
                return;
            }
        }
        // ---- JoinMultiplayerScreen
        let (top, bottom) = Self::list_area(gui);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(gui));
        title::list_background(gui, a, top, bottom, self.scroll);
        gui.text_centered(&lang::get("multiplayer.title"), cx, 12.0, WHITE);
        let left = Self::row_left(gui);
        let hovered = self.row_at(gui, mx, my);
        let mut tip: Option<Vec<String>> = None;
        let now = crate::sys::now_ms();
        for (y, what) in self.rows(gui) {
            if y + ROW <= top || y >= bottom {
                continue;
            }
            let (cx0, cy) = (left + 2.0, y + 2.0);
            let right = cx0 + ROW_W - 4.0;
            let is_hover = hovered.map_or(false, |h| h.1 == what && h.0 == y);
            if what.is_some() && self.sel == what {
                // AbstractSelectionList.extractSelection: a white frame, black inside.
                gui.fill(left, y, left + ROW_W, y + ROW, WHITE);
                gui.fill(left + 1.0, y + 1.0, left + ROW_W - 1.0, y + ROW - 1.0, 0xFF00_0000);
            }
            match what {
                None => {
                    // LANHeader: "Scanning..." and LoadingDotsText under it.
                    let mid = left + ROW_W / 2.0;
                    gui.text_centered(&lang::get("lanServer.scanning"), mid, cy, WHITE);
                    const DOTS: [&str; 4] = ["O o o", "o O o", "o o O", "o O o"];
                    gui.text_centered(DOTS[(now / 300 % 4) as usize], mid, cy + 9.0 + 3.0, GREY);
                }
                Some(Sel::Lan(i)) => {
                    let (motd, addr) = &self.lan[i];
                    gui.text(&lang::get("lanServer.title"), cx0 + 35.0, cy + 1.0, WHITE);
                    gui.text(motd, cx0 + 35.0, cy + 12.0, GREY);
                    gui.text(addr, cx0 + 35.0, cy + 23.0, GREY);
                }
                Some(Sel::Server(i)) => {
                    let s = &self.list.servers[i];
                    let e = &self.entries[i];
                    gui.text(&s.name, cx0 + 35.0, cy + 1.0, WHITE);
                    let lines: Vec<String> = e.motd.split('\n').flat_map(|part| screens::wrap(gui, part, ROW_W - 4.0 - 34.0)).collect();
                    for (k, l) in lines.iter().take(2).enumerate() {
                        gui.text(l, cx0 + 35.0, cy + 12.0 + 9.0 * k as f32, GREY);
                    }
                    // The favicon (or the unknown server picture).
                    if e.icon != 0 {
                        gui.blit_tex(e.icon, cx0, cy, 32.0, 32.0, [0.0, 0.0, 1.0, 1.0], WHITE);
                    } else {
                        title::image(gui, a, "unknown_server", cx0, cy, 32.0, 32.0);
                    }
                    let (icon, icon_tip) = match e.st {
                        St::Initial | St::Pinging => {
                            let mut f = ((now / 100 + i as u64 * 2) & 7) as usize;
                            if f > 4 {
                                f = 8 - f;
                            }
                            (format!("server_list/pinging_{}", f + 1), lang::get("multiplayer.status.pinging"))
                        }
                        St::Incompatible => (String::from("server_list/incompatible"), lang::get("multiplayer.status.incompatible")),
                        St::Unreachable => (String::from("server_list/unreachable"), lang::get("multiplayer.status.no_connection")),
                        St::Ok => {
                            let bars = match e.ping {
                                p if p < 150 => 5,
                                p if p < 300 => 4,
                                p if p < 600 => 3,
                                p if p < 1000 => 2,
                                _ => 1,
                            };
                            (format!("server_list/ping_{}", bars), lang::format("multiplayer.status.ping", &[format!("{}", e.ping)]))
                        }
                    };
                    let ix = right - 10.0 - 5.0;
                    title::sprite(gui, a, &icon, ix, cy, 10.0, 8.0);
                    let sw = gui.text_width(&e.status);
                    let sx = ix - sw - 5.0;
                    gui.text(&e.status, sx, cy + 1.0, GREY);
                    if mx >= ix && mx <= ix + 10.0 && my >= cy && my <= cy + 8.0 {
                        tip = Some(alloc::vec![icon_tip]);
                    } else if !e.players.is_empty() && mx >= sx && mx <= sx + sw && my >= cy - 1.0 && my <= cy + 9.0 && matches!(e.st, St::Ok | St::Incompatible) {
                        tip = Some(e.players.clone());
                    }
                    if is_hover {
                        // The join arrow and the move arrows over the icon.
                        gui.fill(cx0, cy, cx0 + 32.0, cy + 32.0, 0xA090_9090);
                        let (rx, ry) = (mx - cx0, my - cy);
                        let over_right = rx < 32.0 && rx > 16.0;
                        title::sprite(gui, a, if over_right { "server_list/join_highlighted" } else { "server_list/join" }, cx0, cy, 32.0, 32.0);
                        if i > 0 {
                            let hl = rx < 16.0 && ry < 16.0;
                            title::sprite(gui, a, if hl { "server_list/move_up_highlighted" } else { "server_list/move_up" }, cx0, cy, 32.0, 32.0);
                        }
                        if i + 1 < self.list.servers.len() {
                            let hl = rx < 16.0 && ry > 16.0 && ry < 32.0;
                            title::sprite(gui, a, if hl { "server_list/move_down_highlighted" } else { "server_list/move_down" }, cx0, cy, 32.0, 32.0);
                        }
                    }
                }
            }
        }
        title::list_frame(gui, a, top, bottom);
        // The scroll bar, beside the rows.
        let view = bottom - top;
        let content = self.content_height();
        if content > view {
            let sx = left + ROW_W + 6.0 + 2.0;
            let max = content - view;
            let sh = fl((view * view / content).clamp(32.0, view - 8.0));
            let sy = top + fl(self.scroll * (view - sh) / max);
            gui.blit_nine("widget/scroller_background", sx, top, 6.0, view);
            gui.blit_nine("widget/scroller", sx, sy, 6.0, sh);
        }
        let sel_server = matches!(self.sel, Some(Sel::Server(_)));
        for b in footer(gw, gh, self.sel.is_some(), sel_server).iter() {
            b.draw(gui, mx, my);
        }
        if let Some(t) = tip {
            screens::tooltip(gui, &t, mx, my);
        }
    }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let b = footer(427.0, 240.0, true, false);
    out.push((
        "server list buttons (427 x 240)",
        format!("{} {} {} {} {} {}", b[0].x, b[0].y, b[2].x, b[6].x, b[6].y, b[3].active),
        "59 188 267 293 212 false",
    ));
    let mut e = EditBox::new("abc", 5);
    e.insert("de\u{a7}fg");
    e.key(80, false, String::new);
    e.key(42, false, String::new);
    out.push(("edit box typing", format!("{} {}", e.text, e.caret), "abce 3"));
    out
}
