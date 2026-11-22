//! The container screens vanilla draws for its other menus, laid out as their screens lay them
//! out: anvil (the name field, the cost), beacon (the powers), brewing stand (bubbles, fuel),
//! enchanting table (the three offers in the alt font, lapis), grindstone, loom (the
//! patterns), merchant (the trade list, the level bar, out of stock), smithing table (the
//! templates), stonecutter (the recipes from the server's recipe data), cartography table,
//! crafter (slot toggles), horse, donkey and llama inventories, the lectern, and the
//! furnaces' progress arrows. Their sprites come from containers.bin (made by
//! tools/make_assets.py); the container data (ClientboundContainerSetData) drives the bars.

use crate::client::{Container, Game, Sound, SoundRef};
use crate::gui::{rd16, rd32, Gui};
use crate::lang;
use crate::math::FloorExt;
use crate::screens::{self, Window};
use crate::sdl::*;
use crate::wire::{In, Out};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

pub const MAGIC: &[u8; 4] = b"PNC1";

pub const C_CONTAINER_SET_DATA: i32 = 19;
pub const C_MOUNT_SCREEN_OPEN: i32 = 42;
pub const C_MERCHANT_OFFERS: i32 = 53;
pub const C_UPDATE_RECIPES: i32 = 136;
pub const S_CONTAINER_BUTTON_CLICK: i32 = 17;
pub const S_CONTAINER_SLOT_STATE_CHANGED: i32 = 20;
pub const S_RENAME_ITEM: i32 = 49;
pub const S_SELECT_TRADE: i32 = 52;
pub const S_SET_BEACON: i32 = 53;

/// The menu types' registry ids (26.3).
pub const CRAFTER: i32 = 7;
pub const ANVIL: i32 = 8;
pub const BEACON: i32 = 9;
pub const BLAST_FURNACE: i32 = 10;
pub const BREWING: i32 = 11;
pub const ENCHANTMENT: i32 = 13;
pub const FURNACE: i32 = 14;
pub const GRINDSTONE: i32 = 15;
pub const LECTERN: i32 = 17;
pub const LOOM: i32 = 18;
pub const MERCHANT: i32 = 19;
pub const SMITHING: i32 = 21;
pub const SMOKER: i32 = 22;
pub const CARTOGRAPHY: i32 = 23;
pub const STONECUTTER: i32 = 24;
/// Not a registry id: a mount's inventory (ClientboundMountScreenOpen), which has no menu type.
pub const MOUNT: i32 = 1000;

const LABEL: u32 = 0xFF40_4040;
const WHITE: u32 = 0xFFFF_FFFF;

// ------------------------------------------------------------------ containers.bin

pub struct Assets {
    pub tex: u32,
    w: f32,
    h: f32,
    sprites: BTreeMap<String, [u16; 4]>,
    /// The alt font: code point -> x, y, cell width, cell height, advance.
    sga: BTreeMap<u32, (u16, u16, u8, u8, u8)>,
    words: Vec<String>,
    /// MapColor's colours by id (RGB).
    pub map_colours: Vec<u32>,
    loom_free: Vec<String>,
    loom_items: Vec<(String, Vec<String>)>,
    /// Item frame models: item_frame, item_frame_map, glow_item_frame, glow_item_frame_map;
    /// quads of (x, y, z, u, v) corners and a shade.
    pub frames: Vec<Vec<([[f32; 5]; 4], f32)>>,
    /// Map decoration types by registry id: sprite (map/decorations/...), shown in frames.
    pub decorations: Vec<(String, bool)>,
    /// The status effect icons (effects.bin), for the beacon.
    effect_icons: Option<(u32, Vec<Option<[f32; 4]>>)>,
}

static ASSETS: AtomicPtr<Assets> = AtomicPtr::new(core::ptr::null_mut());

pub fn assets() -> Option<&'static Assets> {
    let p = ASSETS.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { &*p })
    }
}

/// Reads containers.bin (optional) into a texture and its tables.
pub fn load(gl: &Gl) {
    let Some(d) = crate::sys::read_file(&format!("{}/containers.bin", crate::data_dir())) else {
        crate::say!("no containers.bin: anvils, beacons and the other menus show their slots only");
        return;
    };
    match parse(&d) {
        Some((mut a, px_at)) => {
            let (w, h) = (a.w as i32, a.h as i32);
            unsafe {
                (gl.GenTextures)(1, &mut a.tex);
                (gl.BindTexture)(TEXTURE_2D, a.tex);
                (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
                (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
                (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w, h, 0, RGBA, UNSIGNED_BYTE, d[px_at..].as_ptr() as *const c_void);
            }
            a.effect_icons = crate::effects::load_icons(gl).map(|i| (i.tex, i.uv));
            ASSETS.store(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(a)), Ordering::Release);
        }
        None => crate::say!("containers.bin is of another version"),
    }
}

/// The tables of containers.bin, and where its pixels start.
pub fn parse(d: &[u8]) -> Option<(Assets, usize)> {
    if d.get(..4)? != MAGIC {
        return None;
    }
    let (w, h) = (rd16(d, 4) as usize, rd16(d, 6) as usize);
    let px_at = 8;
    let mut p = 8 + w * h * 4;
    let mut r = |n: usize| -> Option<usize> {
        let at = p;
        p += n;
        if p > d.len() {
            None
        } else {
            Some(at)
        }
    };
    let s = |o: usize, l: usize| String::from_utf8_lossy(&d[o..o + l]).into_owned();
    let mut sprites = BTreeMap::new();
    let n = rd16(d, r(2)?) as usize;
    for _ in 0..n {
        let l = d[r(1)?] as usize;
        let name = s(r(l)?, l);
        let o = r(8)?;
        sprites.insert(name, [rd16(d, o), rd16(d, o + 2), rd16(d, o + 4), rd16(d, o + 6)]);
    }
    let mut sga = BTreeMap::new();
    let n = rd16(d, r(2)?) as usize;
    for _ in 0..n {
        let o = r(9)?;
        sga.insert(rd16(d, o) as u32, (rd16(d, o + 2), rd16(d, o + 4), d[o + 6], d[o + 7], d[o + 8]));
    }
    let l = rd16(d, r(2)?) as usize;
    let words = s(r(l)?, l).split(' ').filter(|w| !w.is_empty()).map(String::from).collect();
    let n = d[r(1)?] as usize;
    let mut map_colours = Vec::with_capacity(n);
    for _ in 0..n {
        map_colours.push(rd32(d, r(4)?));
    }
    let strs = |r: &mut dyn FnMut(usize) -> Option<usize>| -> Option<Vec<String>> {
        let n = d[r(1)?] as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let l = d[r(1)?] as usize;
            v.push(s(r(l)?, l));
        }
        Some(v)
    };
    let loom_free = strs(&mut r)?;
    let n = d[r(1)?] as usize;
    let mut loom_items = Vec::with_capacity(n);
    for _ in 0..n {
        let l = d[r(1)?] as usize;
        let item = s(r(l)?, l);
        loom_items.push((item, strs(&mut r)?));
    }
    let n = d[r(1)?] as usize;
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let nq = rd16(d, r(2)?) as usize;
        let mut qs = Vec::with_capacity(nq);
        for _ in 0..nq {
            let o = r(84)?;
            let f = |i: usize| f32::from_bits(rd32(d, o + 4 * i));
            let mut c = [[0f32; 5]; 4];
            for (k, v) in c.iter_mut().enumerate() {
                for (j, x) in v.iter_mut().enumerate() {
                    *x = f(k * 5 + j);
                }
            }
            qs.push((c, f(20)));
        }
        frames.push(qs);
    }
    let n = d[r(1)?] as usize;
    let mut decorations = Vec::with_capacity(n);
    for _ in 0..n {
        let l = d[r(1)?] as usize;
        let name = s(r(l)?, l);
        decorations.push((format!("map/decorations/{}", name), d[r(1)?] != 0));
    }
    Some((Assets { tex: 0, w: w as f32, h: h as f32, sprites, sga, words, map_colours, loom_free, loom_items, frames, decorations, effect_icons: None }, px_at))
}

impl Assets {
    pub fn has(&self, name: &str) -> bool {
        self.sprites.contains_key(name)
    }
    /// A sprite's uv rectangle (u0, v0, u1, v1).
    pub fn uv(&self, name: &str) -> Option<[f32; 4]> {
        let s = self.sprites.get(name)?;
        Some([s[0] as f32 / self.w, s[1] as f32 / self.h, (s[0] + s[2]) as f32 / self.w, (s[1] + s[3]) as f32 / self.h])
    }
    fn size(&self, name: &str) -> (f32, f32) {
        self.sprites.get(name).map_or((0.0, 0.0), |s| (s[2] as f32, s[3] as f32))
    }
    /// Draws a sprite at its own size.
    pub fn blit(&self, gui: &mut Gui, name: &str, x: f32, y: f32) {
        let (w, h) = self.size(name);
        self.blit_part(gui, name, x, y, 0.0, 0.0, w, h);
    }
    /// Draws a sprite stretched to w x h.
    pub fn blit_size(&self, gui: &mut Gui, name: &str, x: f32, y: f32, w: f32, h: f32) {
        if let Some(uv) = self.uv(name) {
            gui.blit_tex(self.tex, x, y, w, h, uv, WHITE);
        }
    }
    /// Part (u, v, w, h in the sprite's pixels) of a sprite at (x, y), unscaled.
    pub fn blit_part(&self, gui: &mut Gui, name: &str, x: f32, y: f32, u: f32, v: f32, w: f32, h: f32) {
        let Some(s) = self.sprites.get(name) else { return };
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (u0, v0) = (s[0] as f32 + u, s[1] as f32 + v);
        gui.blit_tex(self.tex, x, y, w, h, [u0 / self.w, v0 / self.h, (u0 + w) / self.w, (v0 + h) / self.h], WHITE);
    }
    fn tinted(&self, gui: &mut Gui, name: &str, x: f32, y: f32, w: f32, h: f32, c: u32) {
        if let Some(uv) = self.uv(name) {
            gui.blit_tex(self.tex, x, y, w, h, uv, c);
        }
    }
    /// The alt font (the enchanting table's runes): its width.
    fn sga_width(&self, s: &str) -> f32 {
        s.chars().map(|c| self.sga.get(&(c as u32)).map_or(if c == ' ' { 4 } else { 6 }, |g| g.4) as f32).sum()
    }
    fn sga_text(&self, gui: &mut Gui, s: &str, x: f32, y: f32, c: u32) {
        let mut cx = x;
        for ch in s.chars() {
            match self.sga.get(&(ch as u32)) {
                Some(&(gx, gy, cw, chh, adv)) => {
                    let uv = [gx as f32 / self.w, gy as f32 / self.h, (gx as f32 + cw as f32) / self.w, (gy as f32 + chh as f32) / self.h];
                    gui.blit_tex(self.tex, cx, y, cw as f32 * 8.0 / chh as f32, 8.0, uv, c);
                    cx += adv as f32;
                }
                None => cx += if ch == ' ' { 4.0 } else { 6.0 },
            }
        }
    }
    pub fn effect_icon(&self, id: usize) -> Option<(u32, [f32; 4])> {
        let (t, uv) = self.effect_icons.as_ref()?;
        Some((*t, (*uv.get(id)?)?))
    }
}

// ------------------------------------------------------------------ components of a stack

/// One data component's value in a patch as creative::read_stack keeps it.
pub fn component(patch: &[u8], kind: i32) -> Option<&[u8]> {
    let mut r = In::new(patch);
    let add = r.varint()?;
    let _remove = r.varint()?;
    for _ in 0..add {
        let k = r.varint()?;
        let l = r.varint()? as usize;
        let v = r.take(l)?;
        if k == kind {
            return Some(v);
        }
    }
    None
}

/// A patch with one component's value set (added, or replaced).
pub fn set_component(patch: &[u8], kind: i32, value: &[u8]) -> Vec<u8> {
    let mut entries: Vec<(i32, &[u8])> = Vec::new();
    let mut removed: Vec<i32> = Vec::new();
    let mut r = In::new(patch);
    if let (Some(add), Some(remove)) = (r.varint(), r.varint()) {
        for _ in 0..add {
            let (Some(k), Some(l)) = (r.varint(), r.varint()) else { break };
            let Some(v) = r.take(l as usize) else { break };
            if k != kind {
                entries.push((k, v));
            }
        }
        for _ in 0..remove {
            if let Some(k) = r.varint() {
                if k != kind {
                    removed.push(k);
                }
            }
        }
    }
    entries.push((kind, value));
    let mut o = Out::new();
    o.varint(entries.len() as i32).varint(removed.len() as i32);
    for (k, v) in entries {
        o.varint(k).varint(v.len() as i32).bytes(v);
    }
    for k in removed {
        o.varint(k);
    }
    o.b
}

/// The name a stack shows: its custom_name, its item_name, or the item's own.
pub fn stack_name(g: &Game, item: i32, patch: &[u8]) -> String {
    for kind in [6, 9] {
        if let Some(v) = component(patch, kind) {
            let t = crate::nbt::text(&mut In::new(v));
            if !t.is_empty() {
                return t;
            }
        }
    }
    if let Some(v) = component(patch, 57) {
        // A written book's title.
        if let Some(t) = In::new(v).string() {
            return t;
        }
    }
    lang::item_name(&item_name(g, item))
}

pub fn item_name(g: &Game, item: i32) -> String {
    g.blocks.items.get(item as usize).cloned().unwrap_or_default()
}

// ------------------------------------------------------------------ state

pub struct Offer {
    pub base_a: (i32, i32),
    pub cost_b: Option<(i32, i32)>,
    pub result: (i32, i32),
    pub out_of_stock: bool,
    pub xp: i32,
    pub special_price: i32,
    pub multiplier: f32,
    pub demand: i32,
}

impl Offer {
    /// MerchantOffer.getCostA: the base cost moved by demand and the special price.
    pub fn cost_a(&self) -> (i32, i32) {
        let (item, n) = self.base_a;
        if n <= 0 {
            return (item, n);
        }
        let d = ((self.demand as f32 * n as f32 * self.multiplier) as i32).max(0);
        (item, (n + d + self.special_price).clamp(1, 64))
    }
}

pub struct State {
    /// The open menu's data slots (ContainerSetData), as vanilla's ContainerData.
    pub data: [i32; 16],
    pub offers: Vec<Offer>,
    pub trader_level: i32,
    pub trader_xp: i32,
    pub show_progress: bool,
    /// The selected trade (MerchantScreen.shopItem) and the first one shown.
    pub shop_item: i32,
    pub trade_scroll: i32,
    /// The anvil's name field, its cursor, and the slot-0 stack it was filled from.
    pub name: String,
    pub name_for: Option<(i32, Vec<u8>)>,
    /// The beacon's chosen powers (registry ids).
    pub beacon: (Option<i32>, Option<i32>),
    beacon_synced: (i32, i32),
    /// Stonecutter and loom scroll position (0..1) and the scroll bar being dragged.
    pub scroll: f32,
    dragging: bool,
    /// ClientboundUpdateRecipes' stonecutter recipes: the input items, the result.
    pub stonecutter: Vec<(Vec<i32>, (i32, i32))>,
    /// A mount's inventory: its chest columns and the entity.
    pub mount: (i32, i32),
    /// Item ids looked up once.
    ids: Option<Ids>,
    /// Ticks (for the smithing table's cycling icons and the name field's cursor).
    pub ticks: u32,
    opened_for: i32,
    /// ClientboundOpenBook's hand, until the window opens the book.
    pub open_book: Option<i32>,
}

#[derive(Clone, Copy)]
struct Ids {
    map: i32,
    paper: i32,
    glass_pane: i32,
    netherite_ingot: i32,
    emerald: i32,
    diamond: i32,
    gold_ingot: i32,
    iron_ingot: i32,
}

/// BeaconBlockEntity.BEACON_EFFECTS by tier.
const BEACON_EFFECTS: [&[&str]; 4] = [&["speed", "haste"], &["resistance", "jump_boost"], &["strength"], &["regeneration"]];

fn effect_id(name: &str) -> i32 {
    crate::effects::EFFECTS.iter().position(|e| e.0 == name).map_or(-1, |i| i as i32)
}

impl State {
    pub fn new() -> State {
        State {
            data: [0; 16],
            offers: Vec::new(),
            trader_level: 0,
            trader_xp: 0,
            show_progress: false,
            shop_item: 0,
            trade_scroll: 0,
            name: String::new(),
            name_for: None,
            beacon: (None, None),
            beacon_synced: (-1, -1),
            scroll: 0.0,
            dragging: false,
            stonecutter: Vec::new(),
            mount: (0, -1),
            ids: None,
            ticks: 0,
            opened_for: -1,
            open_book: None,
        }
    }

    /// A new menu opened: its data starts over.
    pub fn opened(&mut self) {
        self.data = [0; 16];
        self.offers.clear();
        self.shop_item = 0;
        self.trade_scroll = 0;
        self.name.clear();
        self.name_for = None;
        self.beacon = (None, None);
        self.beacon_synced = (-1, -1);
        self.scroll = 0.0;
        self.dragging = false;
    }

    fn ids(&mut self, g_items: &[String]) -> Ids {
        if let Some(i) = self.ids {
            return i;
        }
        let f = |n: &str| g_items.iter().position(|x| x == n).map_or(-1, |i| i as i32);
        let i = Ids {
            map: f("map"),
            paper: f("paper"),
            glass_pane: f("glass_pane"),
            netherite_ingot: f("netherite_ingot"),
            emerald: f("emerald"),
            diamond: f("diamond"),
            gold_ingot: f("gold_ingot"),
            iron_ingot: f("iron_ingot"),
        };
        self.ids = Some(i);
        i
    }
}

/// The packets this module reads; true when `id` was one of them.
pub fn packet(g: &mut Game, id: i32, r: &mut In) -> bool {
    match id {
        C_CONTAINER_SET_DATA => {
            let c = r.varint().unwrap_or(-1);
            let k = r.u16().unwrap_or(0) as i16 as i32;
            let v = r.u16().unwrap_or(0) as i16 as i32;
            if g.container.as_ref().map_or(false, |x| x.id == c) && (0..16).contains(&k) {
                g.menus.data[k as usize] = v;
            }
        }
        C_MOUNT_SCREEN_OPEN => {
            let c = r.varint().unwrap_or(0);
            let columns = r.varint().unwrap_or(0).clamp(0, 5);
            let entity = r.i32().unwrap_or(-1);
            let title = g.entities.get(&entity).and_then(|e| e.custom_name.clone()).unwrap_or_else(|| {
                mount_kind(g, entity).map(|k| lang::get(&format!("entity.minecraft.{}", k))).unwrap_or_default()
            });
            g.menus.opened();
            g.menus.mount = (columns, entity);
            g.container = Some(Container { id: c, menu: MOUNT, title, slots: Vec::new(), patches: Vec::new(), state: 0 });
        }
        C_MERCHANT_OFFERS => {
            let c = r.varint().unwrap_or(-1);
            if let Some(list) = read_offers(r) {
                let m = &mut g.menus;
                m.trader_level = r.varint().unwrap_or(0);
                m.trader_xp = r.varint().unwrap_or(0);
                m.show_progress = r.bool().unwrap_or(false);
                let _can_restock = r.bool();
                if g.container.as_ref().map_or(true, |x| x.id == c) {
                    m.offers = list;
                }
            }
        }
        C_UPDATE_RECIPES => {
            if let Some(v) = read_recipes(r) {
                g.menus.stonecutter = v;
            }
        }
        _ => return false,
    }
    true
}

/// ItemCost: item, count, then its exact component predicate (typed components).
fn read_cost(r: &mut In) -> Option<(i32, i32)> {
    let item = r.varint()?;
    let count = r.varint()?;
    for _ in 0..r.varint()? {
        let kind = r.varint()?;
        crate::client::skip_component(kind, r)?;
    }
    Some((item, count))
}

fn read_offers(r: &mut In) -> Option<Vec<Offer>> {
    let n = r.varint()?;
    let mut v = Vec::new();
    for _ in 0..n.clamp(0, 256) {
        let base_a = read_cost(r)?;
        let (result, ok, _) = crate::creative::read_stack(r)?;
        if !ok {
            return None;
        }
        let cost_b = if r.bool()? { Some(read_cost(r)?) } else { None };
        v.push(Offer {
            base_a,
            cost_b,
            result: result.unwrap_or((0, 0)),
            out_of_stock: r.bool()?,
            xp: {
                // uses, max uses (not shown), then the experience
                r.i32()?;
                r.i32()?;
                r.i32()?
            },
            special_price: r.i32()?,
            multiplier: r.f32()?,
            demand: r.i32()?,
        });
    }
    Some(v)
}

/// HolderSet of items: 0 and a tag name (not resolved here), or the size plus one and the ids.
fn holder_items(r: &mut In) -> Option<Vec<i32>> {
    match r.varint()? {
        0 => {
            r.string()?;
            Some(Vec::new())
        }
        n if n > 0 => (0..n - 1).map(|_| r.varint()).collect(),
        _ => None,
    }
}

/// A SlotDisplay: the first stack it shows (item, count), reading past the rest.
fn slot_display(r: &mut In, depth: u32) -> Option<Option<(i32, i32)>> {
    if depth > 16 {
        return None;
    }
    Some(match r.varint()? {
        0 | 1 => None,
        2 => slot_display(r, depth + 1)?,
        3 => {
            let s = slot_display(r, depth + 1)?;
            r.varint()?;
            s
        }
        4 => Some((r.varint()?, 1)),
        5 => {
            // ItemStackTemplate: item, count, patch.
            let item = r.varint()?;
            let count = r.varint()?;
            let add = r.varint()?;
            let remove = r.varint()?;
            for _ in 0..add {
                let k = r.varint()?;
                crate::client::skip_component(k, r)?;
            }
            for _ in 0..remove {
                r.varint()?;
            }
            Some((item, count))
        }
        6 => holder_items(r)?.first().map(|i| (*i, 1)),
        7 | 9 => {
            let a = slot_display(r, depth + 1)?;
            let b = slot_display(r, depth + 1)?;
            b.or(a)
        }
        8 => {
            let a = slot_display(r, depth + 1)?;
            slot_display(r, depth + 1)?;
            match r.varint()? {
                0 => {
                    r.string()?;
                    crate::nbt::skip_root(r)?;
                    r.bool()?;
                }
                _ => {}
            }
            a
        }
        10 => {
            let mut first = None;
            for _ in 0..r.varint()? {
                let s = slot_display(r, depth + 1)?;
                first = first.or(s);
            }
            first
        }
        _ => return None,
    })
}

/// ClientboundUpdateRecipesPacket: the property sets (skipped), then the stonecutter's
/// SingleInputSet in the server's order.
fn read_recipes(r: &mut In) -> Option<Vec<(Vec<i32>, (i32, i32))>> {
    for _ in 0..r.varint()? {
        r.string()?;
        for _ in 0..r.varint()? {
            r.varint()?;
        }
    }
    let n = r.varint()?;
    let mut v = Vec::new();
    for _ in 0..n.max(0) {
        let input = holder_items(r)?;
        let out = slot_display(r, 0)?;
        v.push((input, out.unwrap_or((0, 0))));
    }
    Some(v)
}

/// The entity type name of a mount ("horse", "llama"...).
fn mount_kind(g: &Game, entity: i32) -> Option<&'static str> {
    let kind = g.entities.get(&entity)?.kind;
    entity_name(kind)
}

/// The few entity types this module needs, by their 26.3 registry ids (containers.bin's
/// item frame models do not need them).
pub fn entity_name(kind: i32) -> Option<&'static str> {
    Some(match kind {
        67 => "horse",
        79 => "llama",
        138 => "trader_llama",
        37 => "donkey",
        88 => "mule",
        155 => "zombie_horse",
        119 => "skeleton_horse",
        19 => "camel",
        20 => "camel_husk",
        89 => "nautilus",
        156 => "zombie_nautilus",
        _ => return None,
    })
}

// ------------------------------------------------------------------ layouts

fn player_rows(slots: &mut Vec<(i16, f32, f32)>, first: i16, x: f32, y: f32) {
    for r in 0..3 {
        for c in 0..9 {
            slots.push((first + r * 9 + c, x + c as f32 * 18.0, y + r as f32 * 18.0));
        }
    }
    for c in 0..9 {
        slots.push((first + 27 + c, x + c as f32 * 18.0, y + 58.0));
    }
}

/// Where the player's own rows start among a menu's slots (the crafter keeps its result last).
pub fn player_start(menu: i32, n: usize) -> usize {
    if menu == CRAFTER {
        9
    } else {
        n.saturating_sub(36)
    }
}

pub fn handles(menu: i32) -> bool {
    matches!(menu, CRAFTER | ANVIL | BEACON | BREWING | ENCHANTMENT | GRINDSTONE | LECTERN | LOOM | MERCHANT | SMITHING | CARTOGRAPHY | STONECUTTER | MOUNT)
}

/// The window of a menu this module draws (None: screens.rs has it, or containers.bin is
/// missing and the slots show as chest rows).
pub fn window(g: &Game, c: &Container) -> Option<Window> {
    if !handles(c.menu) || assets().is_none() {
        return None;
    }
    let mut slots: Vec<(i16, f32, f32)> = Vec::new();
    let s = |slots: &mut Vec<(i16, f32, f32)>, i: i16, x: f32, y: f32| slots.push((i, x, y));
    let (mut w, mut h, mut title_at, mut label_y) = (176.0, 166.0, (8.0, 6.0), 72.0);
    let first: i16;
    match c.menu {
        CRAFTER => {
            for r in 0..3 {
                for col in 0..3 {
                    s(&mut slots, r * 3 + col, 26.0 + col as f32 * 18.0, 17.0 + r as f32 * 18.0);
                }
            }
            first = 9;
            s(&mut slots, 45, 134.0, 35.0);
            title_at.0 = -1.0; // centred
        }
        ANVIL => {
            s(&mut slots, 0, 27.0, 47.0);
            s(&mut slots, 1, 76.0, 47.0);
            s(&mut slots, 2, 134.0, 47.0);
            first = 3;
            title_at.0 = 60.0;
        }
        BEACON => {
            s(&mut slots, 0, 136.0, 110.0);
            first = 1;
            w = 230.0;
            h = 219.0;
            label_y = -1.0;
        }
        BREWING => {
            s(&mut slots, 0, 56.0, 51.0);
            s(&mut slots, 1, 79.0, 58.0);
            s(&mut slots, 2, 102.0, 51.0);
            s(&mut slots, 3, 79.0, 17.0);
            s(&mut slots, 4, 17.0, 17.0);
            first = 5;
            title_at.0 = -1.0;
        }
        ENCHANTMENT => {
            s(&mut slots, 0, 15.0, 47.0);
            s(&mut slots, 1, 35.0, 47.0);
            first = 2;
        }
        GRINDSTONE => {
            s(&mut slots, 0, 49.0, 19.0);
            s(&mut slots, 1, 49.0, 40.0);
            s(&mut slots, 2, 129.0, 34.0);
            first = 3;
        }
        LOOM => {
            s(&mut slots, 0, 13.0, 26.0);
            s(&mut slots, 1, 33.0, 26.0);
            s(&mut slots, 2, 23.0, 45.0);
            s(&mut slots, 3, 143.0, 57.0);
            first = 4;
            title_at.1 = 4.0;
        }
        MERCHANT => {
            s(&mut slots, 0, 136.0, 37.0);
            s(&mut slots, 1, 162.0, 37.0);
            s(&mut slots, 2, 220.0, 37.0);
            w = 276.0;
            let mut v = Vec::new();
            player_rows(&mut v, 3, 108.0, 84.0);
            slots.extend(v);
            return Some(Window { w, h, parts: Vec::new(), slots, title: String::new(), title_at: (0.0, 6.0), inventory_label_y: -1.0, container_id: c.id });
        }
        SMITHING => {
            s(&mut slots, 0, 8.0, 48.0);
            s(&mut slots, 1, 26.0, 48.0);
            s(&mut slots, 2, 44.0, 48.0);
            s(&mut slots, 3, 98.0, 48.0);
            first = 4;
            title_at = (44.0, 15.0);
        }
        CARTOGRAPHY => {
            s(&mut slots, 0, 15.0, 15.0);
            s(&mut slots, 1, 15.0, 52.0);
            s(&mut slots, 2, 145.0, 39.0);
            first = 3;
            title_at.1 = 4.0;
        }
        STONECUTTER => {
            s(&mut slots, 0, 20.0, 33.0);
            s(&mut slots, 1, 143.0, 33.0);
            first = 2;
            title_at.1 = 5.0;
        }
        MOUNT => {
            let kind = mount_kind(g, g.menus.mount.1).unwrap_or("horse");
            if saddled(kind) {
                s(&mut slots, 0, 8.0, 18.0);
            }
            if armoured(kind) {
                s(&mut slots, 1, 8.0, 36.0);
            }
            let cols = g.menus.mount.0;
            for r in 0..3 {
                for col in 0..cols {
                    s(&mut slots, (2 + r * cols + col) as i16, 80.0 + col as f32 * 18.0, 18.0 + r as f32 * 18.0);
                }
            }
            first = (2 + cols * 3) as i16;
        }
        LECTERN => {
            return Some(Window { w: 192.0, h: 192.0, parts: Vec::new(), slots, title: String::new(), title_at: (0.0, 0.0), inventory_label_y: -1.0, container_id: c.id });
        }
        _ => return None,
    }
    if c.menu == BEACON {
        player_rows(&mut slots, first, 36.0, 137.0);
    } else {
        player_rows(&mut slots, first, 8.0, 84.0);
    }
    let title = if c.menu == BEACON { String::new() } else { c.title.clone() };
    Some(Window { w, h, parts: Vec::new(), slots, title, title_at, inventory_label_y: label_y, container_id: c.id })
}

fn saddled(kind: &str) -> bool {
    matches!(kind, "horse" | "donkey" | "mule" | "zombie_horse" | "skeleton_horse" | "camel" | "camel_husk" | "nautilus" | "zombie_nautilus")
}

fn armoured(kind: &str) -> bool {
    matches!(kind, "horse" | "llama" | "trader_llama" | "zombie_horse" | "nautilus" | "zombie_nautilus")
}

fn slot(c: &Container, i: usize) -> Option<(i32, i32)> {
    c.slots.get(i).copied().flatten()
}

fn patch(c: &Container, i: usize) -> &[u8] {
    c.patches.get(i).map_or(&[][..], |p| &p[..])
}

/// A centred title (BrewingStandScreen, CrafterScreen) needs the font; screens.rs calls this.
pub fn title_x(gui: &Gui, win: &Window) -> f32 {
    if win.title_at.0 < 0.0 {
        ((win.w - gui.text_width(&win.title)) / 2.0).floor()
    } else {
        win.title_at.0
    }
}

// ------------------------------------------------------------------ drawing

fn hit(mx: f32, my: f32, x: f32, y: f32, w: f32, h: f32) -> bool {
    mx >= x && mx < x + w && my >= y && my < y + h
}

/// The background and everything under the slots' items.
pub fn draw_back(gui: &mut Gui, g: &Game, win: &Window, left: f32, top: f32, mx: f32, my: f32) {
    let Some(c) = g.container.as_ref() else { return };
    // The furnaces' flame and arrow (AbstractFurnaceScreen), with gui.bin's sprites.
    if matches!(c.menu, FURNACE | BLAST_FURNACE | SMOKER) {
        let pre = match c.menu {
            BLAST_FURNACE => "container/blast_furnace/",
            SMOKER => "container/smoker/",
            _ => "container/furnace/",
        };
        let d = &g.menus.data;
        if d[0] > 0 {
            let total = if d[1] == 0 { 200 } else { d[1] };
            let lit = (d[0] as f32 / total as f32).clamp(0.0, 1.0);
            let l = libm_ceil(lit * 12.0) + 1;
            gui.blit_part(&format!("{}lit_progress", pre), left + 56.0, top + 36.0 + 14.0 - l as f32, 0.0, 14.0 - l as f32, 14.0, l as f32);
        }
        let burn = if d[2] != 0 && d[3] != 0 { (d[2] as f32 / d[3] as f32).clamp(0.0, 1.0) } else { 0.0 };
        let l = libm_ceil(burn * 24.0);
        gui.blit_part(&format!("{}burn_progress", pre), left + 79.0, top + 34.0, 0.0, 0.0, l as f32, 16.0);
        return;
    }
    let Some(a) = assets() else { return };
    if !handles(c.menu) {
        return;
    }
    let m = &g.menus;
    let creative = g.game_mode == 1;
    match c.menu {
        CRAFTER => {
            a.blit(gui, "bg/crafter", left, top);
            for i in 0..9 {
                if m.data[i] == 1 {
                    let (_, x, y) = win.slots[i];
                    a.blit(gui, "container/crafter/disabled_slot", left + x - 1.0, top + y - 1.0);
                }
            }
            let rs = if m.data[9] == 1 { "container/crafter/powered_redstone" } else { "container/crafter/unpowered_redstone" };
            a.blit(gui, rs, left + 97.0, top + 35.0);
        }
        ANVIL => {
            a.blit(gui, "bg/anvil", left, top);
            let has0 = slot(c, 0).is_some();
            a.blit_size(gui, if has0 { "container/anvil/text_field" } else { "container/anvil/text_field_disabled" }, left + 59.0, top + 20.0, 110.0, 16.0);
            // The name field (an EditBox without its border), the cursor blinking.
            let mut t = m.name.clone();
            let max = 103.0;
            while gui.text_width(&t) > max && !t.is_empty() {
                t.remove(0);
            }
            let x = gui.text(&t, left + 62.0, top + 26.0, WHITE);
            if has0 && (m.ticks / 6) % 2 == 0 {
                gui.text("_", left + 62.0 + x, top + 26.0, WHITE);
            }
            if (has0 || slot(c, 1).is_some()) && slot(c, 2).is_none() {
                a.blit(gui, "container/anvil/error", left + 99.0, top + 45.0);
            }
            let cost = m.data[0];
            if cost > 0 {
                let (text, color) = if cost >= 40 && !creative {
                    (Some(lang::get("container.repair.expensive")), 0xFFFF_6060)
                } else if slot(c, 2).is_none() {
                    (None, 0)
                } else {
                    let ok = creative || g.xp_level >= cost;
                    (Some(lang::format("container.repair.cost", &[format!("{}", cost)])), if ok { 0xFF80_FF20 } else { 0xFFFF_6060 })
                };
                if let Some(t) = text {
                    let x = win.w - 8.0 - gui.text_width(&t) - 2.0;
                    gui.fill(left + x - 2.0, top + 67.0, left + win.w - 8.0, top + 79.0, 0x4F00_0000);
                    gui.text(&t, left + x, top + 69.0, color);
                }
            }
        }
        GRINDSTONE => {
            a.blit(gui, "bg/grindstone", left, top);
            if (slot(c, 0).is_some() || slot(c, 1).is_some()) && slot(c, 2).is_none() {
                a.blit(gui, "container/grindstone/error", left + 92.0, top + 31.0);
            }
        }
        BREWING => {
            a.blit(gui, "bg/brewing_stand", left, top);
            let fuel = ((18 * m.data[1] + 20 - 1) / 20).clamp(0, 18);
            if fuel > 0 {
                a.blit_part(gui, "container/brewing_stand/fuel_length", left + 60.0, top + 44.0, 0.0, 0.0, fuel as f32, 4.0);
            }
            let brew = m.data[0];
            if brew > 0 {
                let j = (28.0 * (1.0 - brew as f32 / 400.0)) as i32;
                if j > 0 {
                    a.blit_part(gui, "container/brewing_stand/brew_progress", left + 97.0, top + 16.0, 0.0, 0.0, 9.0, j as f32);
                }
                const BUBBLES: [i32; 7] = [29, 24, 20, 16, 11, 6, 0];
                let j = BUBBLES[(brew / 2 % 7) as usize];
                if j > 0 {
                    a.blit_part(gui, "container/brewing_stand/bubbles", left + 63.0, top + 14.0 + 29.0 - j as f32, 0.0, 29.0 - j as f32, 12.0, j as f32);
                }
            }
            if slot(c, 4).is_none() {
                gui.blit("container/slot/brewing_fuel", left + 17.0, top + 17.0);
            }
            for (i, x, y) in [(0, 56.0, 51.0), (1, 79.0, 58.0), (2, 102.0, 51.0)] {
                if slot(c, i).is_none() {
                    gui.blit("container/slot/potion", left + x, top + y);
                }
            }
        }
        ENCHANTMENT => draw_enchanting(gui, g, a, c, left, top, mx, my),
        BEACON => draw_beacon(gui, g, a, c, left, top, mx, my),
        LOOM => draw_loom(gui, g, a, c, left, top, mx, my),
        STONECUTTER => draw_stonecutter(gui, g, a, c, left, top, mx, my),
        MERCHANT => draw_merchant(gui, g, a, left, top, mx, my),
        SMITHING => {
            a.blit(gui, "bg/smithing", left, top);
            let i = (m.ticks / 30) as usize;
            let template = slot(c, 0);
            let cycle = |gui: &mut Gui, icons: &[&str], x: f32, y: f32| {
                if !icons.is_empty() {
                    gui.blit(&format!("container/slot/{}", icons[i % icons.len()]), left + x, top + y);
                }
            };
            if template.is_none() {
                cycle(gui, &["smithing_template_armor_trim", "smithing_template_netherite_upgrade"], 8.0, 48.0);
            }
            if let Some((t, _)) = template {
                let upgrade = item_name(g, t) == "netherite_upgrade_smithing_template";
                if slot(c, 1).is_none() {
                    let tools: &[&str] = &["helmet", "sword", "chestplate", "pickaxe", "leggings", "axe", "boots", "hoe", "shovel"];
                    let armour: &[&str] = &["helmet", "chestplate", "leggings", "boots"];
                    cycle(gui, if upgrade { tools } else { armour }, 26.0, 48.0);
                }
                if slot(c, 2).is_none() {
                    let mats: &[&str] = &["ingot", "redstone_dust", "lapis_lazuli", "quartz", "diamond", "emerald", "amethyst_shard"];
                    cycle(gui, if upgrade { &["ingot"] } else { mats }, 44.0, 48.0);
                }
            }
            if slot(c, 0).is_some() && slot(c, 1).is_some() && slot(c, 2).is_some() && slot(c, 3).is_none() {
                a.blit(gui, "container/smithing/error", left + 65.0, top + 46.0);
            }
        }
        CARTOGRAPHY => draw_cartography(gui, g, a, c, left, top),
        MOUNT => {
            a.blit(gui, "bg/horse", left, top);
            let cols = m.mount.0;
            if cols > 0 {
                a.blit_part(gui, "container/horse/chest_slots", left + 79.0, top + 17.0, 0.0, 0.0, cols as f32 * 18.0, 54.0);
            }
            let kind = mount_kind(g, m.mount.1).unwrap_or("horse");
            if saddled(kind) {
                a.blit(gui, "container/slot", left + 7.0, top + 17.0);
                if slot(c, 0).is_none() {
                    gui.blit("container/slot/saddle", left + 8.0, top + 18.0);
                }
            }
            if armoured(kind) {
                a.blit(gui, "container/slot", left + 7.0, top + 35.0);
                if slot(c, 1).is_none() {
                    gui.blit(if kind.contains("llama") {
                        "container/slot/llama_armor"
                    } else if kind.contains("nautilus") {
                        "container/slot/nautilus_armor_inventory"
                    } else {
                        "container/slot/horse_armor"
                    }, left + 8.0, top + 36.0);
                }
            }
        }
        LECTERN => crate::books::draw_lectern(gui, g, c, mx, my),
        _ => {}
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

/// The java.util.Random that LegacyRandomSource is (EnchantmentNames' random).
struct JavaRandom(u64);

impl JavaRandom {
    fn new(seed: i64) -> JavaRandom {
        JavaRandom((seed as u64 ^ 0x5DEE_CE66D) & ((1 << 48) - 1))
    }
    fn next(&mut self, bits: u32) -> i32 {
        self.0 = (self.0.wrapping_mul(0x5DEE_CE66D).wrapping_add(0xB)) & ((1 << 48) - 1);
        (self.0 >> (48 - bits)) as i64 as i32
    }
    fn next_int(&mut self, bound: i32) -> i32 {
        if bound & (bound - 1) == 0 {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let v = bits % bound;
            if bits.wrapping_sub(v).wrapping_add(bound - 1) >= 0 {
                return v;
            }
        }
    }
}

/// EnchantmentNames.getRandomName: three or four words, cut to `width` in the alt font.
fn random_name(a: &Assets, rnd: &mut JavaRandom, width: f32) -> String {
    let n = rnd.next_int(2) + 3;
    let mut s = String::new();
    for i in 0..n {
        if i != 0 {
            s.push(' ');
        }
        if !a.words.is_empty() {
            s.push_str(&a.words[rnd.next_int(a.words.len() as i32) as usize]);
        }
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if a.sga_width(&out) > width {
            out.pop();
            break;
        }
    }
    out
}

fn draw_enchanting(gui: &mut Gui, g: &Game, a: &Assets, c: &Container, left: f32, top: f32, mx: f32, my: f32) {
    a.blit(gui, "bg/enchanting_table", left, top);
    if slot(c, 1).is_none() {
        gui.blit("container/slot/lapis_lazuli", left + 35.0, top + 47.0);
    }
    let m = &g.menus;
    let mut rnd = JavaRandom::new(m.data[3] as i64);
    let gold = slot(c, 1).map_or(0, |s| s.1);
    let creative = g.game_mode == 1;
    for i in 0..3 {
        let x = left + 60.0;
        let tx = x + 20.0;
        let y = top + 14.0 + 19.0 * i as f32;
        let cost = m.data[i];
        if cost == 0 {
            a.blit_size(gui, "container/enchanting_table/enchantment_slot_disabled", x, y, 108.0, 19.0);
            continue;
        }
        let cs = format!("{}", cost);
        let width = 86.0 - gui.text_width(&cs);
        let name = random_name(a, &mut rnd, width);
        let mut color = 0xFF68_5E4Au32;
        let cost_color;
        if (gold < i as i32 + 1 || g.xp_level < cost) && !creative {
            a.blit_size(gui, "container/enchanting_table/enchantment_slot_disabled", x, y, 108.0, 19.0);
            a.blit(gui, &format!("container/enchanting_table/level_{}_disabled", i + 1), x + 1.0, y + 1.0);
            a.sga_text(gui, &name, tx, top + 16.0 + 19.0 * i as f32, 0xFF00_0000 | ((color & 0xFEFEFE) >> 1));
            cost_color = 0xFF40_7F10;
        } else {
            if hit(mx, my, x, y, 108.0, 19.0) {
                a.blit_size(gui, "container/enchanting_table/enchantment_slot_highlighted", x, y, 108.0, 19.0);
                color = 0xFFFF_FF80;
            } else {
                a.blit_size(gui, "container/enchanting_table/enchantment_slot", x, y, 108.0, 19.0);
            }
            a.blit(gui, &format!("container/enchanting_table/level_{}", i + 1), x + 1.0, y + 1.0);
            a.sga_text(gui, &name, tx, top + 16.0 + 19.0 * i as f32, color);
            cost_color = 0xFF80_FF20;
        }
        gui.text(&cs, tx + 86.0 - gui.text_width(&cs), top + 16.0 + 19.0 * i as f32 + 7.0, cost_color);
    }
}

/// The enchanting table's tooltip for the offer under the mouse (EnchantmentScreen.render).
fn enchant_tooltip(g: &Game, c: &Container, left: f32, top: f32, mx: f32, my: f32) -> Option<Vec<String>> {
    let m = &g.menus;
    let gold = slot(c, 1).map_or(0, |s| s.1);
    for i in 0..3 {
        let cost = m.data[i];
        let (clue, level) = (m.data[4 + i], m.data[7 + i]);
        if !hit(mx, my, left + 60.0, top + 14.0 + 19.0 * i as f32, 108.0, 17.0) || cost <= 0 || level < 0 || clue < 0 {
            continue;
        }
        let names = g.creative.registries.iter().find(|r| r.0 == "minecraft:enchantment").map(|r| &r.1);
        let name = names.and_then(|n| n.get(clue as usize)).map(|n| {
            let short = n.split(':').last().unwrap_or(n);
            let mut s = lang::get(&format!("enchantment.minecraft.{}", short));
            // Enchantment.getFullname: the level, unless the enchantment has only one.
            const SINGLE: [&str; 9] = ["aqua_affinity", "channeling", "flame", "infinity", "mending", "multishot", "silk_touch", "binding_curse", "vanishing_curse"];
            if level != 1 || !SINGLE.contains(&short) {
                s.push(' ');
                s.push_str(&lang::get(&format!("enchantment.level.{}", level)));
            }
            s
        });
        let mut lines = alloc::vec![format!("\u{a7}f{}", lang::format("container.enchant.clue", &[name.unwrap_or_default()]))];
        if g.game_mode != 1 {
            lines.push(String::new());
            if g.xp_level < cost {
                lines.push(format!("\u{a7}c{}", lang::format("container.enchant.level.requirement", &[format!("{}", cost)])));
            } else {
                let k = i + 1;
                let lapis = if k == 1 { lang::get("container.enchant.lapis.one") } else { lang::format("container.enchant.lapis.many", &[format!("{}", k)]) };
                lines.push(format!("{}{}", if gold >= k as i32 { "\u{a7}7" } else { "\u{a7}c" }, lapis));
                let lv = if k == 1 { lang::get("container.enchant.level.one") } else { lang::format("container.enchant.level.many", &[format!("{}", k)]) };
                lines.push(format!("\u{a7}7{}", lv));
            }
        }
        return Some(lines);
    }
    None
}

/// The beacon's power buttons: (x, y, effect id, primary, tier), then the upgrade button.
fn beacon_buttons(left: f32, top: f32) -> Vec<(f32, f32, i32, bool, i32)> {
    let mut v = Vec::new();
    for (tier, effects) in BEACON_EFFECTS.iter().enumerate().take(3) {
        let l = effects.len() as f32;
        let w = l * 22.0 + (l - 1.0) * 2.0;
        for (j, e) in effects.iter().enumerate() {
            v.push(((left + 76.0 + j as f32 * 24.0 - w / 2.0).floor(), top + 22.0 + tier as f32 * 25.0, effect_id(e), true, tier as i32));
        }
    }
    let effects = BEACON_EFFECTS[3];
    let l = effects.len() as f32 + 1.0;
    let w = l * 22.0 + (l - 1.0) * 2.0;
    for (j, e) in effects.iter().enumerate() {
        v.push(((left + 167.0 + j as f32 * 24.0 - w / 2.0).floor(), top + 47.0, effect_id(e), false, 3));
    }
    // The upgrade: the primary power at level II (effect -1 here).
    v.push(((left + 167.0 + (l - 1.0) * 24.0 - w / 2.0).floor(), top + 47.0, -1, false, 3));
    v
}

fn beacon_sync(m: &State) -> (Option<i32>, Option<i32>) {
    // BeaconMenu's data: 0 levels, 1 primary, 2 secondary (registry id + 1, 0 none).
    let p = if m.data[1] > 0 { Some(m.data[1] - 1) } else { None };
    let s = if m.data[2] > 0 { Some(m.data[2] - 1) } else { None };
    (p, s)
}

fn draw_beacon(gui: &mut Gui, g: &Game, a: &Assets, c: &Container, left: f32, top: f32, mx: f32, my: f32) {
    a.blit(gui, "bg/beacon", left, top);
    let m = &g.menus;
    let levels = m.data[0];
    let (primary, secondary) = m.beacon;
    for (x, y, e, is_primary, tier) in beacon_buttons(left, top) {
        let (effect, selected, active) = if e < 0 {
            match primary {
                Some(p) => (p, primary == secondary, tier < levels),
                None => continue,
            }
        } else {
            (e, if is_primary { primary == Some(e) } else { secondary == Some(e) }, tier < levels)
        };
        let sprite = if !active {
            "container/beacon/button_disabled"
        } else if selected {
            "container/beacon/button_selected"
        } else if hit(mx, my, x, y, 22.0, 22.0) {
            "container/beacon/button_highlighted"
        } else {
            "container/beacon/button"
        };
        a.blit(gui, sprite, x, y);
        if let Some((tex, uv)) = a.effect_icon(effect as usize) {
            gui.blit_tex(tex, x + 2.0, y + 2.0, 18.0, 18.0, uv, WHITE);
        }
    }
    let can_confirm = slot(c, 0).is_some() && primary.is_some();
    for (x, icon, active) in [(left + 164.0, "container/beacon/confirm", can_confirm), (left + 190.0, "container/beacon/cancel", true)] {
        let y = top + 107.0;
        let sprite = if !active {
            "container/beacon/button_disabled"
        } else if hit(mx, my, x, y, 22.0, 22.0) {
            "container/beacon/button_highlighted"
        } else {
            "container/beacon/button"
        };
        a.blit(gui, sprite, x, y);
        a.blit(gui, icon, x + 2.0, y + 2.0);
    }
    let ids = m.ids.unwrap_or(Ids { map: -1, paper: -1, glass_pane: -1, netherite_ingot: -1, emerald: -1, diamond: -1, gold_ingot: -1, iron_ingot: -1 });
    for (item, dx) in [(ids.netherite_ingot, 20.0), (ids.emerald, 41.0), (ids.diamond, 63.0), (ids.gold_ingot, 86.0), (ids.iron_ingot, 108.0)] {
        if item >= 0 {
            gui.item(&g.blocks, item, 1, left + dx, top + 109.0);
        }
    }
    gui.text_centered(&lang::get("block.minecraft.beacon.primary"), left + 62.0, top + 10.0, 0xFFE0_E0E0);
    gui.text_centered(&lang::get("block.minecraft.beacon.secondary"), left + 169.0, top + 10.0, 0xFFE0_E0E0);
}

/// The patterns the loom offers for what is in its pattern slot (LoomMenu.getSelectablePatterns).
fn loom_patterns<'a>(g: &Game, a: &'a Assets, c: &Container) -> &'a [String] {
    match slot(c, 2) {
        None => &a.loom_free,
        Some((item, _)) => {
            let n = item_name(g, item);
            a.loom_items.iter().find(|x| x.0 == n).map_or(&[][..], |x| &x.1[..])
        }
    }
}

fn loom_shows(g: &Game, a: &Assets, c: &Container) -> bool {
    slot(c, 0).is_some() && slot(c, 1).is_some() && !loom_patterns(g, a, c).is_empty()
}

fn rows_scroll(total: usize, visible_rows: usize, scroll: f32) -> usize {
    let rows = (total + 3) / 4;
    let off = rows.saturating_sub(visible_rows);
    ((scroll * off as f32 + 0.5) as usize) * 4
}

fn draw_loom(gui: &mut Gui, g: &Game, a: &Assets, c: &Container, left: f32, top: f32, mx: f32, my: f32) {
    a.blit(gui, "bg/loom", left, top);
    for (i, icon, x, y) in [(0, "container/slot/banner", 13.0, 26.0), (1, "container/slot/dye", 33.0, 26.0), (2, "container/slot/banner_pattern", 23.0, 45.0)] {
        if slot(c, i).is_none() {
            gui.blit(icon, left + x, top + y);
        }
    }
    let pats = loom_patterns(g, a, c);
    let shows = loom_shows(g, a, c);
    let active = shows && pats.len() > 16;
    let sy = (41.0 * g.menus.scroll) as i32 as f32;
    a.blit(gui, if active { "container/loom/scroller" } else { "container/loom/scroller_disabled" }, left + 119.0, top + 13.0 + sy);
    if !shows {
        return;
    }
    let start = rows_scroll(pats.len(), 4, g.menus.scroll);
    let selected = g.menus.data[0];
    for (k, p) in pats.iter().enumerate().skip(start).take(16) {
        let j = k - start;
        let (x, y) = (left + 60.0 + (j % 4) as f32 * 14.0, top + 13.0 + (j / 4) as f32 * 14.0);
        let sprite = if k as i32 == selected {
            "container/loom/pattern_selected"
        } else if hit(mx, my, x, y, 14.0, 14.0) {
            "container/loom/pattern_highlighted"
        } else {
            "container/loom/pattern"
        };
        a.blit(gui, sprite, x, y);
        // extractBannerOnButton: a grey flag with the pattern in white, 5 x 10.
        gui.fill(x + 4.0, y + 2.0, x + 9.0, y + 12.0, 0xFF47_4F52);
        a.tinted(gui, &format!("banner/{}", p), x + 4.0, y + 2.0, 5.0, 10.0, WHITE);
    }
}

/// The stonecutter's recipes for the item in its input slot.
fn stonecutter_recipes<'a>(g: &'a Game, c: &Container) -> Vec<&'a (Vec<i32>, (i32, i32))> {
    match slot(c, 0) {
        Some((item, _)) => g.menus.stonecutter.iter().filter(|r| r.0.contains(&item)).collect(),
        None => Vec::new(),
    }
}

fn draw_stonecutter(gui: &mut Gui, g: &Game, a: &Assets, c: &Container, left: f32, top: f32, mx: f32, my: f32) {
    a.blit(gui, "bg/stonecutter", left, top);
    let recipes = stonecutter_recipes(g, c);
    let active = recipes.len() > 12;
    let sy = (41.0 * g.menus.scroll) as i32 as f32;
    a.blit(gui, if active { "container/stonecutter/scroller" } else { "container/stonecutter/scroller_disabled" }, left + 119.0, top + 15.0 + sy);
    let start = rows_scroll(recipes.len(), 3, g.menus.scroll);
    let selected = g.menus.data[0];
    for (k, r) in recipes.iter().enumerate().skip(start).take(12) {
        let j = k - start;
        let (x, y) = (left + 52.0 + (j % 4) as f32 * 16.0, top + 14.0 + (j / 4) as f32 * 18.0 + 2.0);
        let sprite = if k as i32 == selected {
            "container/stonecutter/recipe_selected"
        } else if hit(mx, my, x, y, 16.0, 18.0) {
            "container/stonecutter/recipe_highlighted"
        } else {
            "container/stonecutter/recipe"
        };
        a.blit(gui, sprite, x, y - 1.0);
        gui.item(&g.blocks, r.1 .0, r.1 .1, x, y);
    }
}

/// VillagerData's experience per level.
const LEVEL_XP: [i32; 5] = [0, 10, 70, 150, 250];

fn draw_merchant(gui: &mut Gui, g: &Game, a: &Assets, left: f32, top: f32, mx: f32, my: f32) {
    a.blit(gui, "bg/villager", left, top);
    let m = &g.menus;
    let title = g.container.as_ref().map(|c| c.title.clone()).unwrap_or_default();
    let title = if m.trader_level > 0 && m.trader_level <= 5 && m.show_progress {
        lang::format("merchant.title", &[title, lang::get(&format!("merchant.level.{}", m.trader_level))])
    } else {
        title
    };
    let tw = gui.text_width(&title);
    gui.text_plain(&title, left + (49.0 + 276.0 / 2.0 - tw / 2.0).floor(), top + 6.0, LABEL);
    gui.text_plain(&lang::get("container.inventory"), left + 107.0, top + 72.0, LABEL);
    let trades = lang::get("merchant.trades");
    gui.text_plain(&trades, left + (5.0 - gui.text_width(&trades) / 2.0 + 48.0).floor(), top + 6.0, LABEL);
    if let Some(o) = m.offers.get(m.shop_item as usize) {
        if o.out_of_stock {
            a.blit(gui, "container/villager/out_of_stock", left + 83.0 + 99.0, top + 35.0);
        }
        if m.show_progress {
            let level = m.trader_level;
            if level < 5 {
                a.blit(gui, "container/villager/experience_bar_background", left + 136.0, top + 16.0);
                let min = if (1..5).contains(&level) { LEVEL_XP[level as usize - 1] } else { 0 };
                if m.trader_xp >= min && (1..5).contains(&level) {
                    let f = 102.0 / (LEVEL_XP[level as usize] - min) as f32;
                    let cur = ((f * (m.trader_xp - min) as f32) as i32).min(102);
                    a.blit_part(gui, "container/villager/experience_bar_current", left + 136.0, top + 16.0, 0.0, 0.0, cur as f32, 5.0);
                    if o.xp > 0 {
                        let q = ((o.xp as f32 * f) as i32).min(102 - cur);
                        a.blit_part(gui, "container/villager/experience_bar_result", left + 136.0 + cur as f32, top + 16.0, cur as f32, 0.0, q as f32, 5.0);
                    }
                }
            }
        }
    }
    // The trade buttons, their items and arrows, and the scroller.
    let n = m.offers.len();
    if n > 7 {
        let i = n as i32 + 1 - 7;
        let j = 139 - (27 + (i - 1) * 139 / i);
        let k = 1 + j / i + 139 / i;
        let mut off = (m.trade_scroll * k).min(113);
        if m.trade_scroll == i - 1 {
            off = 113;
        }
        a.blit(gui, "container/villager/scroller", left + 94.0, top + 18.0 + off as f32);
    } else {
        a.blit(gui, "container/villager/scroller_disabled", left + 94.0, top + 18.0);
    }
    let start = if n > 7 { m.trade_scroll as usize } else { 0 };
    for (row, o) in m.offers.iter().enumerate().skip(start).take(7) {
        let k = row - start;
        let by = top + 18.0 + k as f32 * 20.0;
        let sprite = if hit(mx, my, left + 5.0, by, 88.0, 20.0) { "widget/button_highlighted" } else { "widget/button" };
        gui.blit_nine(sprite, left + 5.0, by, 88.0, 20.0);
        let y = top + 17.0 + k as f32 * 20.0 + 2.0;
        let x = left + 10.0;
        let real = o.cost_a();
        if real.1 == o.base_a.1 {
            gui.item(&g.blocks, real.0, real.1, x, y);
        } else {
            // A discount: the old count struck through, the new one beside it.
            gui.item(&g.blocks, real.0, 0, x, y);
            count_text(gui, o.base_a.1, x, y);
            count_text(gui, real.1, x + 14.0, y);
            a.blit(gui, "container/villager/discount_strikethrough", x + 7.0, y + 12.0);
        }
        if let Some((b, bn)) = o.cost_b {
            gui.item(&g.blocks, b, bn, left + 5.0 + 35.0, y);
        }
        a.blit(gui, if o.out_of_stock { "container/villager/trade_arrow_out_of_stock" } else { "container/villager/trade_arrow" }, left + 5.0 + 35.0 + 20.0, y + 3.0);
        gui.item(&g.blocks, o.result.0, o.result.1, left + 5.0 + 68.0, y);
    }
}

fn count_text(gui: &mut Gui, n: i32, x: f32, y: f32) {
    let t = format!("{}", n);
    let w = gui.text_width(&t);
    gui.text(&t, x + 19.0 - 2.0 - w, y + 6.0 + 3.0, WHITE);
}

fn draw_cartography(gui: &mut Gui, g: &Game, a: &Assets, c: &Container, left: f32, top: f32) {
    a.blit(gui, "bg/cartography_table", left, top);
    let ids = g.menus.ids.unwrap_or(Ids { map: -1, paper: -1, glass_pane: -1, netherite_ingot: -1, emerald: -1, diamond: -1, gold_ingot: -1, iron_ingot: -1 });
    let extra = slot(c, 1).map(|s| s.0);
    let (dup, paper, glass) = (extra == Some(ids.map), extra == Some(ids.paper), extra == Some(ids.glass_pane));
    let map_id = crate::maps::map_id(patch(c, 0));
    let data = map_id.and_then(|id| g.maps.get(id));
    let mut error = false;
    if let Some(d) = data {
        if d.locked {
            error = true;
            if paper || glass {
                a.blit(gui, "container/cartography_table/error", left + 35.0, top + 31.0);
            }
        }
        if paper && d.scale >= 4 {
            error = true;
            a.blit(gui, "container/cartography_table/error", left + 35.0, top + 31.0);
        }
    }
    let draw_map = |gui: &mut Gui, x: f32, y: f32, s: f32| {
        if let Some(t) = data.map(|d| d.tex).filter(|t| *t != 0) {
            gui.blit_tex(t, x, y, 128.0 * s, 128.0 * s, [0.0, 0.0, 1.0, 1.0], WHITE);
        }
    };
    if paper && !error {
        a.blit(gui, "container/cartography_table/scaled_map", left + 67.0, top + 13.0);
        draw_map(gui, left + 85.0, top + 31.0, 0.226);
    } else if dup {
        a.blit(gui, "container/cartography_table/duplicated_map", left + 83.0, top + 13.0);
        draw_map(gui, left + 86.0, top + 16.0, 0.34);
        a.blit(gui, "container/cartography_table/duplicated_map", left + 67.0, top + 29.0);
        draw_map(gui, left + 70.0, top + 32.0, 0.34);
    } else if glass {
        a.blit(gui, "container/cartography_table/map", left + 67.0, top + 13.0);
        draw_map(gui, left + 71.0, top + 17.0, 0.45);
        a.blit(gui, "container/cartography_table/locked", left + 118.0, top + 60.0);
    } else {
        a.blit(gui, "container/cartography_table/map", left + 67.0, top + 13.0);
        draw_map(gui, left + 71.0, top + 17.0, 0.45);
    }
}

/// Over the items: the tooltips of the menu's own widgets (shown when no slot's item is).
pub fn draw_front(gui: &mut Gui, g: &Game, win: &Window, left: f32, top: f32, mx: f32, my: f32) -> Option<Vec<String>> {
    let c = g.container.as_ref()?;
    if !handles(c.menu) || assets().is_none() {
        return None;
    }
    let a = assets()?;
    match c.menu {
        ENCHANTMENT => enchant_tooltip(g, c, left, top, mx, my),
        CRAFTER => {
            let i = screens::slot_at(gui, win, mx, my)?;
            if (0..9).contains(&i) && slot(c, i as usize).is_none() && g.menus.data[i as usize] != 1 && g.carried.is_none() {
                Some(alloc::vec![lang::get("gui.togglable_slot")])
            } else {
                None
            }
        }
        BEACON => {
            let m = &g.menus;
            for (x, y, e, _, _) in beacon_buttons(left, top) {
                if hit(mx, my, x, y, 22.0, 22.0) {
                    let (id, upgrade) = if e < 0 { (m.beacon.0?, true) } else { (e, false) };
                    let name = crate::effects::EFFECTS.get(id as usize).map_or("", |x| x.0);
                    let mut s = lang::get(&format!("effect.minecraft.{}", name));
                    if upgrade {
                        s.push_str(" II");
                    }
                    return Some(alloc::vec![s]);
                }
            }
            if hit(mx, my, left + 164.0, top + 107.0, 22.0, 22.0) {
                return Some(alloc::vec![lang::get("gui.done")]);
            }
            if hit(mx, my, left + 190.0, top + 107.0, 22.0, 22.0) {
                return Some(alloc::vec![lang::get("gui.cancel")]);
            }
            None
        }
        STONECUTTER => {
            let recipes = stonecutter_recipes(g, c);
            let start = rows_scroll(recipes.len(), 3, g.menus.scroll);
            for (k, r) in recipes.iter().enumerate().skip(start).take(12) {
                let j = k - start;
                let (x, y) = (left + 52.0 + (j % 4) as f32 * 16.0, top + 14.0 + (j / 4) as f32 * 18.0 + 2.0);
                if hit(mx, my, x, y, 16.0, 18.0) {
                    return Some(alloc::vec![lang::item_name(&item_name(g, r.1 .0))]);
                }
            }
            None
        }
        LOOM => {
            if !loom_shows(g, a, c) {
                return None;
            }
            let pats = loom_patterns(g, a, c);
            let start = rows_scroll(pats.len(), 4, g.menus.scroll);
            let dye = slot(c, 1).map(|s| String::from(item_name(g, s.0).trim_end_matches("_dye"))).unwrap_or_default();
            for (k, p) in pats.iter().enumerate().skip(start).take(16) {
                let j = k - start;
                if hit(mx, my, left + 60.0 + (j % 4) as f32 * 14.0, top + 13.0 + (j / 4) as f32 * 14.0, 14.0, 14.0) {
                    return Some(alloc::vec![lang::get(&format!("block.minecraft.banner.{}.{}", p, dye))]);
                }
            }
            None
        }
        MERCHANT => {
            let m = &g.menus;
            if m.offers.get(m.shop_item as usize).map_or(false, |o| o.out_of_stock) && hit(mx, my, left + 186.0, top + 35.0, 22.0, 21.0) {
                return Some(alloc::vec![lang::get("merchant.deprecated")]);
            }
            let start = if m.offers.len() > 7 { m.trade_scroll as usize } else { 0 };
            for (row, o) in m.offers.iter().enumerate().skip(start).take(7) {
                let k = row - start;
                let y = top + 17.0 + k as f32 * 20.0 + 2.0;
                for (x, it) in [(left + 10.0, Some(o.cost_a().0)), (left + 40.0, o.cost_b.map(|b| b.0)), (left + 73.0, Some(o.result.0))] {
                    if let Some(it) = it {
                        if hit(mx, my, x, y, 16.0, 16.0) {
                            return Some(alloc::vec![lang::item_name(&item_name(g, it))]);
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ input

fn ui_click(g: &mut Game) {
    g.sounds.push(Sound { sound: SoundRef::Name(String::from("ui.button.click")), pos: None, volume: 0.25, pitch: 1.0, source: 10 });
}

pub fn button(g: &mut Game, id: i32) -> Result<(), String> {
    let Some(c) = g.container.as_ref() else { return Ok(()) };
    let mut o = Out::new();
    o.varint(c.id).varint(id);
    g.conn.send(S_CONTAINER_BUTTON_CLICK, &o)
}

/// Each tick while a container is open: the anvil's name follows slot 0, the beacon's choice
/// the server's, the item ids get looked up.
pub fn tick(g: &mut Game) -> Result<(), String> {
    g.menus.ticks = g.menus.ticks.wrapping_add(1);
    let Some(c) = g.container.as_ref() else {
        g.menus.opened_for = -1;
        return Ok(());
    };
    if g.menus.opened_for != c.id {
        g.menus.opened_for = c.id;
    }
    let items = core::mem::take(&mut g.blocks.items);
    g.menus.ids(&items);
    g.blocks.items = items;
    let c = g.container.as_ref().unwrap();
    if g.menus.ticks % 40 == 0 && crate::sys::env("PENRYN_DEBUG_MENU").is_some() {
        crate::say!("menu {} id {} data {:?} beacon {:?} slots {:?}", c.menu, c.id, &g.menus.data[..10], g.menus.beacon, c.slots);
    }
    match c.menu {
        ANVIL => {
            // ItemCombinerScreen.slotChanged: slot 0's name fills the field.
            let now = slot(c, 0).map(|s| (s.0, patch(c, 0).to_vec()));
            if now != g.menus.name_for {
                let name = match &now {
                    Some((item, p)) => stack_name(g, *item, p),
                    None => String::new(),
                };
                g.menus.name_for = now;
                g.menus.name = name;
                if g.menus.name_for.is_some() {
                    send_name(g)?;
                }
            }
        }
        BEACON => {
            let s = beacon_sync(&g.menus);
            let raw = (g.menus.data[1], g.menus.data[2]);
            if raw != g.menus.beacon_synced {
                g.menus.beacon_synced = raw;
                g.menus.beacon = s;
            }
        }
        _ => {}
    }
    Ok(())
}

/// AnvilScreen.onNameChanged: an unchanged default name goes as "" (no rename).
fn send_name(g: &mut Game) -> Result<(), String> {
    let Some(c) = g.container.as_ref() else { return Ok(()) };
    let Some((item, _)) = slot(c, 0) else { return Ok(()) };
    let p = patch(c, 0).to_vec();
    let mut name = g.menus.name.clone();
    if component(&p, 6).is_none() && name == stack_name(g, item, &p) {
        name.clear();
    }
    let mut o = Out::new();
    o.string(&name);
    g.conn.send(S_RENAME_ITEM, &o)
}

/// Whether the open menu takes typed text (the anvil's field).
pub fn wants_text(g: &Game) -> bool {
    g.container.as_ref().map_or(false, |c| c.menu == ANVIL && slot(c, 0).is_some() && assets().is_some())
}

/// Typed text for the anvil's name field (50 characters at most).
pub fn text(g: &mut Game, s: &str) -> Result<(), String> {
    if !wants_text(g) {
        return Ok(());
    }
    let before = g.menus.name.clone();
    for ch in s.chars() {
        if g.menus.name.chars().count() < 50 && ch != '\u{a7}' && ch >= ' ' {
            g.menus.name.push(ch);
        }
    }
    if g.menus.name != before {
        send_name(g)?;
    }
    Ok(())
}

/// A key while a menu is open: true when this module used it (the name field takes every
/// key but Escape).
pub fn key(g: &mut Game, sc: usize) -> Result<bool, String> {
    const K_ESC: usize = 41;
    const K_BACKSPACE: usize = 42;
    if g.container.as_ref().map_or(false, |c| c.menu == LECTERN) {
        return crate::books::lectern_key(g, sc);
    }
    if !wants_text(g) || sc == K_ESC {
        return Ok(false);
    }
    if sc == K_BACKSPACE && g.menus.name.pop().is_some() {
        send_name(g)?;
    }
    // Letters, digits and the rest arrive as text input; E, Q and the number keys stay there.
    Ok(sc != 40 && sc < 224)
}

pub fn wheel(gui: &Gui, g: &mut Game, y: i32) {
    let Some(c) = g.container.as_ref() else { return };
    let Some(a) = assets() else { return };
    match c.menu {
        MERCHANT => {
            let n = g.menus.offers.len() as i32;
            if n > 7 {
                g.menus.trade_scroll = (g.menus.trade_scroll - y.signum()).clamp(0, n - 7);
            }
        }
        STONECUTTER | LOOM => {
            let (total, rows) = if c.menu == LOOM { (loom_patterns(g, a, c).len(), 4) } else { (stonecutter_recipes(g, c).len(), 3) };
            let off = ((total + 3) / 4).saturating_sub(rows);
            if off > 0 {
                g.menus.scroll = (g.menus.scroll - y.signum() as f32 / off as f32).clamp(0.0, 1.0);
            }
        }
        _ => {}
    }
    let _ = gui;
}

/// The mouse moved with a button held: drags the scroll bar.
pub fn drag(gui: &Gui, g: &mut Game, mx: f32, my: f32) {
    if !g.menus.dragging {
        return;
    }
    let Some(c) = g.container.as_ref() else { return };
    let Some(win) = window(g, c) else { return };
    let (_left, top) = screens::window_origin(gui, &win);
    match c.menu {
        MERCHANT => {
            let n = g.menus.offers.len() as i32;
            if n > 7 {
                let f = ((my - (top + 18.0) - 13.5) / (139.0 - 27.0)).clamp(0.0, 1.0);
                g.menus.trade_scroll = ((f * (n - 7) as f32 + 0.5) as i32).clamp(0, n - 7);
            }
        }
        STONECUTTER | LOOM => {
            let y0 = top + if c.menu == LOOM { 13.0 } else { 14.0 };
            g.menus.scroll = ((my - y0 - 7.5) / (56.0 - 15.0)).clamp(0.0, 1.0);
        }
        _ => {}
    }
    let _ = mx;
}

pub fn release(g: &mut Game) {
    g.menus.dragging = false;
}

/// A click on the open menu's own widgets, before its slots: true when it took the click.
pub fn click(gui: &Gui, g: &mut Game, win: &Window, mx: f32, my: f32, button_no: u8) -> Result<bool, String> {
    let Some(c) = g.container.as_ref() else { return Ok(false) };
    let Some(a) = assets() else { return Ok(false) };
    if !handles(c.menu) {
        return Ok(false);
    }
    let (left, top) = screens::window_origin(gui, win);
    let menu = c.menu;
    let cid = c.id;
    match menu {
        LECTERN => return crate::books::lectern_click(gui, g, mx, my, button_no),
        CRAFTER => {
            // CrafterScreen.slotClicked: an empty grid slot with nothing on the cursor toggles.
            if let Some(i) = screens::slot_at(gui, win, mx, my) {
                if (0..9).contains(&i) && slot(c, i as usize).is_none() && g.carried.is_none() && (button_no == 1 || button_no == 3) {
                    let disabled = g.menus.data[i as usize] == 1;
                    g.menus.data[i as usize] = if disabled { 0 } else { 1 };
                    let mut o = Out::new();
                    o.varint(i as i32).varint(cid).bool(disabled);
                    g.conn.send(S_CONTAINER_SLOT_STATE_CHANGED, &o)?;
                    g.sounds.push(Sound { sound: SoundRef::Name(String::from("ui.button.click")), pos: None, volume: 0.4, pitch: if disabled { 1.0 } else { 0.75 }, source: 10 });
                    return Ok(true);
                }
            }
        }
        _ => {}
    }
    if button_no != 1 {
        return Ok(false);
    }
    match menu {
        ENCHANTMENT => {
            for i in 0..3 {
                if hit(mx, my, left + 60.0, top + 14.0 + 19.0 * i as f32, 108.0, 19.0) {
                    // EnchantmentMenu.clickMenuButton's own checks, then the server decides.
                    let lapis = slot(c, 1).map_or(0, |s| s.1);
                    let cost = g.menus.data[i];
                    if cost > 0 && (g.game_mode == 1 || (lapis >= i as i32 + 1 && g.xp_level >= cost)) {
                        button(g, i as i32)?;
                    }
                    return Ok(true);
                }
            }
        }
        BEACON => {
            let levels = g.menus.data[0];
            for (x, y, e, is_primary, tier) in beacon_buttons(left, top) {
                if !hit(mx, my, x, y, 22.0, 22.0) || tier >= levels {
                    continue;
                }
                let m = &mut g.menus;
                if e < 0 {
                    match m.beacon.0 {
                        Some(p) if m.beacon.1 != Some(p) => m.beacon.1 = Some(p),
                        _ => return Ok(true),
                    }
                } else if is_primary {
                    if m.beacon.0 == Some(e) {
                        return Ok(true);
                    }
                    m.beacon.0 = Some(e);
                } else {
                    if m.beacon.1 == Some(e) {
                        return Ok(true);
                    }
                    m.beacon.1 = Some(e);
                }
                ui_click(g);
                return Ok(true);
            }
            let can_confirm = slot(c, 0).is_some() && g.menus.beacon.0.is_some();
            if hit(mx, my, left + 164.0, top + 107.0, 22.0, 22.0) && can_confirm {
                ui_click(g);
                let mut o = Out::new();
                let (p, s) = g.menus.beacon;
                for e in [p, s] {
                    match e {
                        Some(e) => o.bool(true).varint(e),
                        None => o.bool(false),
                    };
                }
                g.conn.send(S_SET_BEACON, &o)?;
                g.close_container(cid)?;
                return Ok(true);
            }
            if hit(mx, my, left + 190.0, top + 107.0, 22.0, 22.0) {
                ui_click(g);
                g.close_container(cid)?;
                return Ok(true);
            }
        }
        STONECUTTER | LOOM => {
            let (total, rows, x0, y0, cw, ch, cells) = if menu == LOOM {
                (if loom_shows(g, a, c) { loom_patterns(g, a, c).len() } else { 0 }, 4, 60.0, 13.0, 14.0, 14.0, 16)
            } else {
                (stonecutter_recipes(g, c).len(), 3, 52.0, 16.0, 16.0, 18.0, 12)
            };
            let start = rows_scroll(total, rows, g.menus.scroll);
            for j in 0..cells {
                let k = start + j;
                if k >= total {
                    break;
                }
                if hit(mx, my, left + x0 + (j % 4) as f32 * cw, top + y0 + (j / 4) as f32 * ch, cw, ch) {
                    g.sounds.push(Sound { sound: SoundRef::Name(String::from(if menu == LOOM { "ui.loom.select_pattern" } else { "ui.stonecutter.select_recipe" })), pos: None, volume: 1.0, pitch: 1.0, source: 10 });
                    g.menus.data[0] = k as i32;
                    button(g, k as i32)?;
                    return Ok(true);
                }
            }
            let (sx, sy) = (left + 119.0, top + if menu == LOOM { 13.0 } else { 9.0 });
            if hit(mx, my, sx, sy, 12.0, if menu == LOOM { 56.0 } else { 54.0 }) && ((total + 3) / 4) > rows {
                g.menus.dragging = true;
                drag(gui, g, mx, my);
                return Ok(true);
            }
        }
        MERCHANT => {
            let n = g.menus.offers.len();
            let start = if n > 7 { g.menus.trade_scroll as usize } else { 0 };
            for k in 0..7 {
                if start + k >= n {
                    break;
                }
                if hit(mx, my, left + 5.0, top + 18.0 + k as f32 * 20.0, 88.0, 20.0) {
                    ui_click(g);
                    let idx = (start + k) as i32;
                    g.menus.shop_item = idx;
                    let mut o = Out::new();
                    o.varint(idx);
                    g.conn.send(S_SELECT_TRADE, &o)?;
                    return Ok(true);
                }
            }
            if n > 7 && hit(mx, my, left + 94.0, top + 18.0, 6.0, 139.0 + 1.0) {
                g.menus.dragging = true;
                drag(gui, g, mx, my);
                return Ok(true);
            }
        }
        _ => {}
    }
    Ok(false)
}

// ------------------------------------------------------------------ tests

pub fn selftest(data: Option<&[u8]>) -> Vec<(&'static str, String, &'static str)> {
    let mut v = Vec::new();
    // java.util.Random's sequences (as LegacyRandomSource gives them).
    let mut r = JavaRandom::new(42);
    v.push(("java random (42): nextInt(10) x 2", format!("{} {}", r.next_int(10), r.next_int(10)), "0 3"));
    let mut r = JavaRandom::new(-7);
    v.push(("java random (-7): nextInt(2), nextInt(16)", format!("{} {}", r.next_int(2), r.next_int(16)), "0 14"));
    // A merchant offer's cost: base 10, demand 4, multiplier 0.05, special price -3.
    let o = Offer { base_a: (1, 10), cost_b: None, result: (2, 1), out_of_stock: false, xp: 1, special_price: -3, multiplier: 0.05, demand: 4 };
    v.push(("merchant offer cost with demand and discount", format!("{}", o.cost_a().1), "9"));
    // ContainerSetData into the furnace's arrow: 100 of 200 ticks is 12 of 24 pixels.
    v.push(("furnace arrow at half", format!("{}", libm_ceil(0.5 * 24.0)), "12"));
    let mut p = Out::new();
    p.varint(2).varint(0).varint(48).varint(1).varint(7).varint(6).varint(3).bytes(&[10, 0, 0]);
    let q = set_component(&p.b, 48, &[9]);
    v.push(("component replaced in a patch", format!("{:?} {:?}", component(&q, 48), component(&q, 6)), "Some([9]) Some([10, 0, 0])"));
    v.push(("component in a patch (map_id)", format!("{:?} {:?}", component(&p.b, 48), crate::maps::map_id(&p.b)), "Some([7]) Some(7)"));
    // A stonecutter recipe list: no property sets, one recipe (stone -> 2 stone slabs).
    let mut rec = Out::new();
    rec.varint(0).varint(1).varint(2).varint(1).varint(5).varint(10).varint(2).varint(0).varint(0);
    let got = read_recipes(&mut In::new(&rec.b)).map(|v| format!("{:?} {:?}", v[0].0, v[0].1));
    v.push(("stonecutter recipe from UpdateRecipes", got.unwrap_or_default(), "[1] (10, 2)"));
    match data.and_then(parse) {
        Some((a, _)) => {
            let need = ["bg/anvil", "bg/beacon", "bg/villager", "bg/book", "container/villager/trade_arrow", "container/beacon/button", "map/background", "banner/stripe_top", "block/item_frame"];
            let missing: Vec<&str> = need.iter().copied().filter(|n| !a.has(n)).collect();
            v.push(("containers.bin sprites", format!("missing {:?}", missing), "missing []"));
            v.push(("containers.bin tables", format!("{} words, {} colours, {} free patterns, {} frames, glyph a {}", a.words.len(), a.map_colours.len(), a.loom_free.len(), a.frames.len(), a.sga.contains_key(&('a' as u32))), "62 words, 62 colours, 32 free patterns, 4 frames, glyph a true"));
            v.push(("map colour GRASS (id 1)", format!("{:06x}", a.map_colours.get(1).copied().unwrap_or(0)), "7fb238"));
            v.push(("map pixel 4 x 1 + 1 (GRASS, normal)", format!("{:08x}", crate::maps::pixel(&a.map_colours, 5)), "ff6d9930"));
        }
        None => v.push(("containers.bin", String::from("missing"), "present (skipped)")),
    }
    v
}
