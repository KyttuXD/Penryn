//! Vanilla's HUD and screens, laid out as `Gui`, `InventoryScreen`, `ContainerScreen`,
//! `PauseScreen` and `DeathScreen` lay them out (positions in GUI units).

use crate::client::Game;
use crate::gui::Gui;
use crate::lang;
use crate::math::FloorExt;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const WHITE: u32 = 0xFFFF_FFFF;
const LABEL: u32 = 0xFF40_4040;

/// A container window: its background pieces, slots and labels.
pub struct Window {
    pub w: f32,
    pub h: f32,
    /// (sprite, source y, height, destination y): vertical strips of the background.
    pub parts: Vec<(&'static str, f32, f32, f32)>,
    /// (menu slot index, x, y) relative to the window.
    pub slots: Vec<(i16, f32, f32)>,
    pub title: String,
    pub title_at: (f32, f32),
    pub inventory_label_y: f32,
    pub container_id: i32,
}

fn player_rows(slots: &mut Vec<(i16, f32, f32)>, first: i16, y: f32, hotbar_y: f32) {
    for r in 0..3 {
        for c in 0..9 {
            slots.push((first + r * 9 + c, 8.0 + c as f32 * 18.0, y + r as f32 * 18.0));
        }
    }
    for c in 0..9 {
        slots.push((first + 27 + c, 8.0 + c as f32 * 18.0, hotbar_y));
    }
}

/// The window of the open container, or of the player's inventory.
pub fn window(g: &Game) -> Window {
    let Some(c) = g.container.as_ref() else {
        // InventoryMenu: result, 2x2 crafting, armour, main, hotbar, offhand.
        let mut slots = alloc::vec![(0, 154.0, 28.0)];
        for i in 0..2 {
            for j in 0..2 {
                slots.push((1 + i * 2 + j, 98.0 + j as f32 * 18.0, 18.0 + i as f32 * 18.0));
            }
        }
        for i in 0..4 {
            slots.push((5 + i, 8.0, 8.0 + i as f32 * 18.0));
        }
        player_rows(&mut slots, 9, 84.0, 142.0);
        slots.push((45, 77.0, 62.0));
        return Window {
            w: 176.0,
            h: 166.0,
            parts: alloc::vec![("container/inventory", 0.0, 166.0, 0.0)],
            slots,
            title: lang::get("container.crafting"),
            title_at: (97.0, 6.0),
            inventory_label_y: -1.0,
            container_id: 0,
        };
    };
    let mut slots = Vec::new();
    let (w, h, parts, label_y, title_x);
    match c.menu {
        0..=5 => {
            // generic_9xN: the top rows of generic_54, then its player part.
            let rows = c.menu + 1;
            for r in 0..rows {
                for col in 0..9 {
                    slots.push(((r * 9 + col) as i16, 8.0 + col as f32 * 18.0, 18.0 + r as f32 * 18.0));
                }
            }
            let top = (rows * 18 + 17) as f32;
            let extra = ((rows - 4) * 18) as f32;
            player_rows(&mut slots, (rows * 9) as i16, 103.0 + extra, 161.0 + extra);
            w = 176.0;
            h = 114.0 + rows as f32 * 18.0;
            parts = alloc::vec![("container/generic_54", 0.0, top, 0.0), ("container/generic_54", 126.0, 96.0, top)];
            label_y = h - 94.0;
            title_x = 8.0;
        }
        20 => {
            for r in 0..3 {
                for col in 0..9 {
                    slots.push(((r * 9 + col) as i16, 8.0 + col as f32 * 18.0, 18.0 + r as f32 * 18.0));
                }
            }
            player_rows(&mut slots, 27, 84.0, 142.0);
            w = 176.0;
            h = 166.0;
            parts = alloc::vec![("container/shulker_box", 0.0, 166.0, 0.0)];
            label_y = 72.0;
            title_x = 8.0;
        }
        6 | 7 => {
            for r in 0..3 {
                for col in 0..3 {
                    slots.push(((r * 3 + col) as i16, 62.0 + col as f32 * 18.0, 17.0 + r as f32 * 18.0));
                }
            }
            player_rows(&mut slots, 9, 84.0, 142.0);
            w = 176.0;
            h = 166.0;
            parts = alloc::vec![("container/dispenser", 0.0, 166.0, 0.0)];
            label_y = 72.0;
            title_x = 61.0;
        }
        12 => {
            slots.push((0, 124.0, 35.0));
            for r in 0..3 {
                for col in 0..3 {
                    slots.push((1 + (r * 3 + col) as i16, 30.0 + col as f32 * 18.0, 17.0 + r as f32 * 18.0));
                }
            }
            player_rows(&mut slots, 10, 84.0, 142.0);
            w = 176.0;
            h = 166.0;
            parts = alloc::vec![("container/crafting_table", 0.0, 166.0, 0.0)];
            label_y = 72.0;
            title_x = 29.0;
        }
        10 | 14 | 22 => {
            slots.push((0, 56.0, 17.0));
            slots.push((1, 56.0, 53.0));
            slots.push((2, 116.0, 35.0));
            player_rows(&mut slots, 3, 84.0, 142.0);
            w = 176.0;
            h = 166.0;
            parts = alloc::vec![(
                match c.menu {
                    10 => "container/blast_furnace",
                    22 => "container/smoker",
                    _ => "container/furnace",
                },
                0.0,
                166.0,
                0.0
            )];
            label_y = 72.0;
            title_x = 8.0;
        }
        16 => {
            for col in 0..5 {
                slots.push((col as i16, 44.0 + col as f32 * 18.0, 20.0));
            }
            player_rows(&mut slots, 5, 51.0, 109.0);
            w = 176.0;
            h = 133.0;
            parts = alloc::vec![("container/hopper", 0.0, 133.0, 0.0)];
            label_y = 39.0;
            title_x = 8.0;
        }
        _ => {
            // Menus Penryn does not draw yet: their slots as chest rows, so items can move.
            let own = c.slots.len().saturating_sub(36).max(1);
            let rows = ((own + 8) / 9).clamp(1, 6) as i32;
            for i in 0..own.min(54) {
                slots.push((i as i16, 8.0 + (i % 9) as f32 * 18.0, 18.0 + (i / 9) as f32 * 18.0));
            }
            let top = (rows * 18 + 17) as f32;
            let extra = ((rows - 4) * 18) as f32;
            player_rows(&mut slots, own as i16, 103.0 + extra, 161.0 + extra);
            w = 176.0;
            h = 114.0 + rows as f32 * 18.0;
            parts = alloc::vec![("container/generic_54", 0.0, top, 0.0), ("container/generic_54", 126.0, 96.0, top)];
            label_y = h - 94.0;
            title_x = 8.0;
        }
    }
    Window { w, h, parts, slots, title: c.title.clone(), title_at: (title_x, 6.0), inventory_label_y: label_y, container_id: c.id }
}

fn slot_item(g: &Game, win: &Window, idx: i16) -> Option<(i32, i32)> {
    if win.container_id == 0 {
        g.inventory.get(idx as usize).copied().flatten()
    } else {
        g.container.as_ref().and_then(|c| c.slots.get(idx as usize).copied().flatten())
    }
}

pub fn window_origin(gui: &Gui, win: &Window) -> (f32, f32) {
    (((gui.width - win.w) / 2.0).floor(), ((gui.height - win.h) / 2.0).floor())
}

/// The slot under the mouse.
pub fn slot_at(gui: &Gui, win: &Window, mx: f32, my: f32) -> Option<i16> {
    let (left, top) = window_origin(gui, win);
    win.slots.iter().find(|(_, x, y)| mx >= left + x - 1.0 && mx < left + x + 17.0 && my >= top + y - 1.0 && my < top + y + 17.0).map(|s| s.0)
}

pub fn inside(gui: &Gui, win: &Window, mx: f32, my: f32) -> bool {
    let (left, top) = window_origin(gui, win);
    mx >= left && mx < left + win.w && my >= top && my < top + win.h
}

/// The darkened background behind a screen (Screen.renderTransparentBackground).
pub fn dim(gui: &mut Gui) {
    let (w, h) = (gui.width, gui.height);
    gui.fill(0.0, 0.0, w, h, 0xC010_1010);
}

pub fn draw_window(gui: &mut Gui, g: &Game, win: &Window, mx: f32, my: f32) {
    dim(gui);
    let (left, top) = window_origin(gui, win);
    for (sprite, sy, sh, dy) in win.parts.iter() {
        gui.blit_part(sprite, left, top + dy, 0.0, *sy, win.w, *sh);
    }
    gui.text_plain(&win.title, left + win.title_at.0, top + win.title_at.1, LABEL);
    if win.inventory_label_y >= 0.0 {
        gui.text_plain(&lang::get("container.inventory"), left + 8.0, top + win.inventory_label_y, LABEL);
    }
    // Empty armour and offhand slots show their outline icons; the player stands in the
    // dark box, turned towards the mouse (renderEntityInInventoryFollowsMouse).
    if win.container_id == 0 {
        let (x1, y1, x2, y2) = (left + 26.0, top + 8.0, left + 75.0, top + 78.0);
        let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        let f = crate::math::atan(((cx - mx) / 40.0) as f64) as f32;
        let gg = crate::math::atan(((cy - my) / 40.0) as f64) as f32;
        // Vanilla: body 180 + f*20, head 180 + f*40, pitch -g*20 (degrees from radians x 20).
        let (model, skin) = skin_of(&g.uuid);
        crate::skins::gui_entity(gui, &g.uuid, model, &skin, cx, y2 - 5.0, 30.0, -f * 20.0, -f * 20.0, -gg * 20.0);
        for (i, icon) in ["container/slot/helmet", "container/slot/chestplate", "container/slot/leggings", "container/slot/boots"].iter().enumerate() {
            if g.inventory[5 + i].is_none() {
                gui.blit(icon, left + 8.0, top + 8.0 + i as f32 * 18.0);
            }
        }
        if g.inventory[45].is_none() {
            gui.blit("container/slot/shield", left + 77.0, top + 62.0);
        }
    }
    let hover = slot_at(gui, win, mx, my);
    for &(idx, x, y) in win.slots.iter() {
        if let Some((item, count)) = slot_item(g, win, idx) {
            gui.item(&g.blocks, item, count, left + x, top + y);
        }
        if hover == Some(idx) {
            gui.fill(left + x, top + y, left + x + 16.0, top + y + 16.0, 0x80FF_FFFF);
        }
    }
    // The item on the cursor, then the hovered item's name.
    if let Some((item, count)) = g.carried {
        gui.item(&g.blocks, item, count, mx - 8.0, my - 8.0);
    } else if let Some(idx) = hover {
        if let Some((item, _)) = slot_item(g, win, idx) {
            // The stack's full tooltip from its components (tooltip.rs).
            let patch = if win.container_id == 0 {
                g.creative.patches.get(idx as usize)
            } else {
                g.container.as_ref().and_then(|c| c.patches.get(idx as usize))
            };
            let lines = crate::tooltip::stack_lines(g, gui, item, patch.map_or(&[][..], |p| &p[..]));
            crate::tooltip::draw(gui, &lines, mx, my);
        }
    }
}

/// Vanilla's tooltip: background and frame sprites around the lines, at the mouse.
pub fn tooltip(gui: &mut Gui, lines: &[String], mx: f32, my: f32) {
    let w = lines.iter().map(|l| gui.text_width(l)).fold(0.0, f32::max);
    let h = if lines.len() == 1 { 8.0 } else { 10.0 + (lines.len() - 1) as f32 * 10.0 };
    let mut x = mx + 12.0;
    let mut y = my - 12.0;
    if x + w + 4.0 > gui.width {
        x = (mx - 16.0 - w).max(4.0);
    }
    if y + h + 4.0 > gui.height {
        y = gui.height - h - 4.0;
    }
    gui.blit_nine("tooltip/background", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    gui.blit_nine("tooltip/frame", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    for (i, l) in lines.iter().enumerate() {
        gui.text(l, x, y + if i == 0 { 0.0 } else { 2.0 + i as f32 * 10.0 }, WHITE);
    }
}

pub struct Button {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    /// Inactive buttons are greyed out and do nothing (vanilla's `active`).
    pub active: bool,
}

impl Button {
    pub fn new(x: f32, y: f32, w: f32, label: String) -> Button {
        Button { x, y, w, h: 20.0, label, active: true }
    }
    pub fn off(mut self) -> Button {
        self.active = false;
        self
    }
    pub fn hit(&self, mx: f32, my: f32) -> bool {
        self.active && mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + self.h
    }
    pub fn draw(&self, gui: &mut Gui, mx: f32, my: f32) {
        let sprite = if !self.active {
            "widget/button_disabled"
        } else if self.hit(mx, my) {
            "widget/button_highlighted"
        } else {
            "widget/button"
        };
        gui.blit_nine(sprite, self.x, self.y, self.w, self.h);
        let color = if self.active { WHITE } else { 0xFFA0_A0A0 };
        gui.text_centered(&self.label, self.x + self.w / 2.0, self.y + (self.h - 8.0) / 2.0, color);
    }
}

/// PauseScreen.createPauseMenu's grid (cells padded 4 left, top and right; the first row 50
/// down), placed a quarter of the way down: 0 back to the game, 1 leave, 2 Options..., then
/// what Penryn has no screen for yet (advancements, statistics, world options). The row of
/// icon buttons (feedback, bugs, friends, reporting) keeps its place but is not drawn.
/// With the server's links, their button takes a row of its own above Options...
/// (serverhud::links_button), and the grid is a row taller.
pub fn pause_buttons(gui: &Gui, links: bool) -> [Button; 6] {
    let extra = if links { 24.0 } else { 0.0 };
    let gx = ((gui.width - 212.0) / 2.0).floor() + 4.0;
    let top = ((gui.height - 166.0 - extra) * 0.25).floor();
    [
        Button::new(gx, top + 50.0, 204.0, lang::get("menu.returnToGame")),
        Button::new(gx, top + 146.0 + extra, 204.0, lang::get("menu.disconnect")),
        Button::new(gx, top + 122.0 + extra, 98.0, lang::get("menu.options")),
        Button::new(gx, top + 74.0, 98.0, lang::get("gui.advancements")).off(),
        Button::new(gx + 106.0, top + 74.0, 98.0, lang::get("gui.stats")).off(),
        Button::new(gx + 106.0, top + 122.0 + extra, 98.0, lang::get("options.worldOptions.button")).off(),
    ]
}

pub fn draw_pause(gui: &mut Gui, mx: f32, my: f32, links: Option<&Button>) {
    dim(gui);
    let cx = gui.width / 2.0;
    gui.text_centered(&lang::get("menu.game"), cx, 40.0, WHITE);
    for b in pause_buttons(gui, links.is_some()).iter() {
        b.draw(gui, mx, my);
    }
    if let Some(b) = links {
        b.draw(gui, mx, my);
    }
}

pub fn death_buttons(gui: &Gui) -> [Button; 2] {
    let x = gui.width / 2.0 - 100.0;
    let y = gui.height / 4.0 + 72.0;
    [Button::new(x, y, 200.0, lang::get("deathScreen.respawn")), Button::new(x, y + 24.0, 200.0, lang::get("menu.disconnect"))]
}

/// The in-game HUD (Gui.render): crosshair, hotbar, hearts, food, experience, held item name.
pub fn hud(gui: &mut Gui, g: &Game, crosshair: bool, held_name: Option<(String, u8)>) {
    let (w, h) = (gui.width, gui.height);
    let cx = (w / 2.0).floor();
    if crosshair {
        gui.blit_invert("hud/crosshair", ((w - 15.0) / 2.0).floor(), ((h - 15.0) / 2.0).floor());
    }
    // Hotbar
    let x = cx - 91.0;
    let y = h - 22.0;
    if let Some((item, count)) = g.inventory[45] {
        gui.blit("hud/hotbar_offhand_left", x - 29.0, h - 23.0);
        gui.item(&g.blocks, item, count, x - 26.0, h - 19.0);
    }
    gui.blit("hud/hotbar", x, y);
    gui.blit("hud/hotbar_selection", x - 1.0 + g.selected as f32 * 20.0, y - 1.0);
    for i in 0..9 {
        if let Some((item, count)) = g.inventory[36 + i] {
            gui.item(&g.blocks, item, count, cx - 90.0 + i as f32 * 20.0 + 2.0, h - 16.0 - 3.0);
        }
    }
    let survival = g.game_mode == 0 || g.game_mode == 2;
    if survival {
        // Experience
        let by = h - 32.0 + 3.0;
        gui.blit("hud/experience_bar_background", x, by);
        let fill = (g.xp_progress.clamp(0.0, 1.0) * 183.0) as i32 as f32;
        if fill > 0.0 {
            gui.blit_part("hud/experience_bar_progress", x, by, 0.0, 0.0, fill, 5.0);
        }
        if g.xp_level > 0 {
            let t = format!("{}", g.xp_level);
            let tw = gui.text_width(&t);
            gui.text_outlined(&t, ((w - tw) / 2.0).floor(), h - 31.0 - 4.0, 0xFF80_FF20);
        }
        // Hearts and food
        let top = h - 39.0;
        let health = libm_ceil(g.health);
        for i in 0..10 {
            let hx = x + i as f32 * 8.0;
            gui.blit("hud/heart/container", hx, top);
            if health >= i * 2 + 2 {
                gui.blit("hud/heart/full", hx, top);
            } else if health == i * 2 + 1 {
                gui.blit("hud/heart/half", hx, top);
            }
        }
        let right = cx + 91.0;
        for i in 0..10 {
            let fx = right - i as f32 * 8.0 - 9.0;
            gui.blit("hud/food_empty", fx, top);
            if i * 2 + 1 < g.food {
                gui.blit("hud/food_full", fx, top);
            } else if i * 2 + 1 == g.food {
                gui.blit("hud/food_half", fx, top);
            }
        }
    }
    // The held item's name, fading out.
    if let Some((name, alpha)) = held_name {
        let ny = h - if survival { 59.0 } else { 45.0 };
        let tw = gui.text_width(&name);
        gui.text(&name, ((w - tw) / 2.0).floor(), ny, (alpha as u32) << 24 | 0xFFFFFF);
    }
}

fn libm_ceil(v: f32) -> i32 {
    let t = v as i32;
    if (t as f32) < v {
        t + 1
    } else {
        t
    }
}

/// The F3 screen's left column: grey text over dark bands.
pub fn debug(gui: &mut Gui, lines: &[String]) {
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            continue;
        }
        let y = 2.0 + 9.0 * i as f32;
        let w = gui.text_width(l);
        gui.fill(1.0, y - 1.0, 2.0 + w + 1.0, y + 8.0, 0x9050_5050);
        gui.text_plain(l, 2.0, y, 0xFFE0_E0E0);
    }
}

/// DefaultPlayerSkin: one of the 18 default skins (9 slim, then 9 wide), by the UUID's hash.
pub fn skin_of(uuid: &[u8; 16]) -> (&'static str, String) {
    const NAMES: [&str; 9] = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"];
    let msb = u64::from_be_bytes(uuid[..8].try_into().unwrap());
    let lsb = u64::from_be_bytes(uuid[8..].try_into().unwrap());
    let hilo = msb ^ lsb;
    let hash = ((hilo >> 32) as i32) ^ (hilo as i32);
    let i = hash.rem_euclid(18) as usize;
    if i < 9 {
        ("minecraft:player_slim#main", format!("skin/slim/{}", NAMES[i]))
    } else {
        ("minecraft:player#main", format!("skin/wide/{}", NAMES[i - 9]))
    }
}

/// Splits `s` into lines no wider than `width` (at spaces when it can).
pub fn wrap(gui: &Gui, s: &str, width: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let candidate = if line.is_empty() { String::from(word) } else { format!("{} {}", line, word) };
        if gui.text_width(&candidate) <= width || line.is_empty() {
            if gui.text_width(&candidate) > width {
                // One word longer than the line: cut it.
                let mut part = String::new();
                for ch in candidate.chars() {
                    let mut t = part.clone();
                    t.push(ch);
                    if gui.text_width(&t) > width && !part.is_empty() {
                        out.push(core::mem::take(&mut part));
                    }
                    part.push(ch);
                }
                line = part;
            } else {
                line = candidate;
            }
        } else {
            out.push(core::mem::take(&mut line));
            line = String::from(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    // A colour set on one line goes on into the next (vanilla keeps the style when it wraps).
    let mut last = String::new();
    for l in out.iter_mut() {
        if !last.is_empty() {
            l.insert_str(0, &last);
        }
        let b: Vec<char> = l.chars().collect();
        for i in 0..b.len() {
            if b[i] == '\u{a7}' && i + 1 < b.len() {
                last = if b[i + 1] == 'r' {
                    String::new()
                } else if b[i + 1] == '#' {
                    b[i..(i + 8).min(b.len())].iter().collect()
                } else {
                    b[i..i + 2].iter().collect()
                };
            }
        }
    }
    out
}
