//! Vanilla's creative inventory, `CreativeModeInventoryScreen`: the tabs of `CreativeModeTabs`
//! (from Horizon's export, in gui.bin) with their sprites, the 9 x 5 item grid and its scroll
//! bar, the search box (names, tooltips, item ids, `#tags`), the survival inventory tab with
//! the destroy slot, saved hotbars, and the clicks as `slotClicked` handles them. In creative
//! the inventory is the client's: every slot it changes goes to the server as a
//! `ServerboundSetCreativeModeSlotPacket` (slot -1 drops the stack), with the stack's data
//! components (`DataComponentPatch.DELIMITED_STREAM_CODEC`).

use crate::client::Game;
use crate::gui::Gui;
use crate::ids::play;
use crate::lang;
use crate::math::FloorExt;
use crate::wire::{In, Out};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The synchronised registries the creative stacks' components refer to (component specs
/// number them 0-3), numbered as the server sends them.
pub const REGISTRIES: [&str; 4] = ["minecraft:enchantment", "minecraft:painting_variant", "minecraft:instrument", "minecraft:banner_pattern"];

const W: f32 = 195.0;
const H: f32 = 136.0;
const LABEL: u32 = 0xFF40_4040;
const GRAY: u32 = 0xFFAA_AAAA;
const BLUE: u32 = 0xFF55_55FF;
const DARK_PURPLE: u32 = 0xFFAA_00AA;
const RARITY: [u32; 4] = [0xFFFF_FFFF, 0xFFFF_FF55, 0xFF55_FFFF, 0xFFFF_55FF];

pub const CATEGORY: u8 = 0;
pub const HOTBAR: u8 = 1;
pub const SEARCH: u8 = 2;
pub const INVENTORY: u8 = 3;

/// What the client keeps for creative mode beside `Game`'s inventory.
pub struct Sync {
    /// Per inventory-menu slot: the stack's components in the delimited form the creative
    /// packet sends (both counts, then each value behind its length). Empty: none known.
    pub patches: Vec<Vec<u8>>,
    /// `REGISTRIES`' entries in the server's order (RegistryData in the configuration).
    pub registries: Vec<(String, Vec<String>)>,
    /// The player's permission level (entity events 24-28); 2 and up see the operator tab.
    pub op_level: i32,
}

impl Sync {
    pub fn new() -> Sync {
        Sync { patches: vec![Vec::new(); 46], registries: Vec::new(), op_level: 0 }
    }
}

/// An item stack as the creative screen moves it: item, count and its component patch.
#[derive(Clone, PartialEq, Debug)]
pub struct Stack {
    pub item: i32,
    pub count: i32,
    pub patch: Vec<u8>,
}

impl Stack {
    fn with(&self, count: i32) -> Stack {
        Stack { item: self.item, count, patch: self.patch.clone() }
    }
    fn same(&self, o: &Stack) -> bool {
        self.item == o.item && norm(&self.patch) == norm(&o.patch)
    }
}

fn norm(p: &[u8]) -> &[u8] {
    if p.len() < 2 {
        &[0, 0]
    } else {
        p
    }
}

/// ItemStack.OPTIONAL_STREAM_CODEC, keeping the component patch in the delimited form:
/// the stack, whether what follows can still be read, and the patch (empty when unknown).
pub fn read_stack(r: &mut In) -> Option<(Option<(i32, i32)>, bool, Vec<u8>)> {
    let count = r.varint()?;
    if count <= 0 {
        return Some((None, true, Vec::new()));
    }
    let item = r.varint()?;
    let add = r.varint()?;
    let remove = r.varint()?;
    let mut o = Out::new();
    o.varint(add).varint(remove);
    for _ in 0..add {
        let kind = r.varint()?;
        let start = r.pos;
        if crate::client::skip_component(kind, r).is_none() {
            return Some((Some((item, count)), false, Vec::new()));
        }
        let v = &r.b[start..r.pos];
        o.varint(kind).varint(v.len() as i32).bytes(v);
    }
    for _ in 0..remove {
        let k = r.varint()?;
        o.varint(k);
    }
    Some((Some((item, count)), true, o.b))
}

// ------------------------------------------------------------------ the data in gui.bin

pub struct ItemInfo {
    pub max: u8,
    pub rarity: u8,
    /// 0 none, 1 head, 2 chest, 3 legs, 4 feet, 5 offhand.
    pub equip: u8,
}

pub struct Entry {
    pub item: u16,
    pub op: bool,
    /// Bit per tab that shows it (not the search tab).
    pub tabs: u16,
    pub lines: Vec<String>,
    lower: Vec<String>,
    pub tags: Vec<u16>,
    pub spec: Vec<u8>,
}

pub struct Tab {
    pub kind: u8,
    pub row: u8,
    pub col: u8,
    pub flags: u8,
    pub key: String,
    pub title: String,
    pub icon: u16,
    pub bg: String,
    pub items: Vec<u32>,
}

impl Tab {
    pub fn name(&self) -> String {
        if lang::has(&self.key) {
            lang::get(&self.key)
        } else {
            self.title.clone()
        }
    }
    fn can_scroll(&self) -> bool {
        self.flags & 4 == 0
    }
    /// getTabX, relative to the window.
    fn x(&self) -> f32 {
        if self.flags & 1 != 0 {
            W - 27.0 * (7 - self.col as i32) as f32 + 1.0
        } else {
            27.0 * self.col as f32
        }
    }
    /// getTabY (where clicks land), relative to the window.
    fn y(&self) -> f32 {
        if self.row == 0 {
            -32.0
        } else {
            H
        }
    }
}

pub struct Data {
    pub items: Vec<ItemInfo>,
    pub tags: Vec<String>,
    pub entries: Vec<Entry>,
    pub tabs: Vec<Tab>,
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str8(&mut self) -> Option<String> {
        let n = self.u8()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
    fn str16(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
}

impl Data {
    /// The creative section of gui.bin (its offset is in the file's last 4 bytes).
    pub fn from_gui_bin(d: &[u8]) -> Option<Data> {
        if d.len() < 8 {
            return None;
        }
        let at = crate::gui::rd32(d, d.len() - 4) as usize;
        if at == 0 || at + 4 > d.len() - 4 {
            return None;
        }
        Data::parse(&d[at..d.len() - 4])
    }

    fn parse(d: &[u8]) -> Option<Data> {
        let mut r = R { d, p: 0 };
        if r.bytes(4)? != b"CRT1" {
            return None;
        }
        let n = r.u16()? as usize;
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(ItemInfo { max: r.u8()?.max(1), rarity: r.u8()?.min(3), equip: r.u8()? });
        }
        let n = r.u16()? as usize;
        let mut tags = Vec::with_capacity(n);
        for _ in 0..n {
            tags.push(r.str8()?);
        }
        let n = r.u32()? as usize;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let item = r.u16()?;
            let op = r.u8()? & 1 != 0;
            let tabs = r.u16()?;
            let nl = r.u8()? as usize;
            let mut lines = Vec::with_capacity(nl);
            for _ in 0..nl {
                lines.push(r.str16()?);
            }
            let lower = lines.iter().map(|l| l.to_lowercase()).collect();
            let nt = r.u8()? as usize;
            let mut tg = Vec::with_capacity(nt);
            for _ in 0..nt {
                tg.push(r.u16()?);
            }
            let sl = r.u16()? as usize;
            let spec = r.bytes(sl)?.to_vec();
            entries.push(Entry { item, op, tabs, lines, lower, tags: tg, spec });
        }
        let n = r.u8()? as usize;
        let mut tabs = Vec::with_capacity(n);
        for _ in 0..n {
            let (kind, row, col, flags) = (r.u8()?, r.u8()?, r.u8()?.min(6), r.u8()?);
            let key = r.str8()?;
            let title = r.str8()?;
            let icon = r.u16()?;
            let bg = r.str8()?;
            let ni = r.u32()? as usize;
            let mut its = Vec::with_capacity(ni);
            for _ in 0..ni {
                let e = r.u32()?;
                if (e as usize) < entries.len() {
                    its.push(e);
                }
            }
            tabs.push(Tab { kind, row, col, flags, key, title, icon, bg, items: its });
        }
        if tabs.is_empty() {
            return None;
        }
        Some(Data { items, tags, entries, tabs })
    }

    fn max_stack(&self, item: i32) -> i32 {
        self.items.get(item as usize).map_or(64, |i| i.max as i32)
    }
    fn equip(&self, item: i32) -> u8 {
        self.items.get(item as usize).map_or(0, |i| i.equip)
    }
    fn visible(&self, t: usize, op: bool) -> bool {
        self.tabs.get(t).map_or(false, |t| t.flags & 8 == 0 || op)
    }
    /// CreativeModeTabs.getDefaultTab: the first category.
    fn default_tab(&self) -> usize {
        self.tabs.iter().position(|t| t.kind == CATEGORY).unwrap_or(0)
    }

    /// An entry's component patch (delimited), its holders numbered by `regs`; a component
    /// naming an entry the server does not have is left out.
    pub fn patch(&self, e: usize, regs: &[(String, Vec<String>)]) -> Vec<u8> {
        let spec = &self.entries[e].spec;
        let mut comps: Vec<(u16, Vec<u8>)> = Vec::new();
        let mut r = R { d: spec, p: 0 };
        'comp: while r.p < spec.len() {
            let Some(kind) = r.u16() else { break };
            let mut v = Out::new();
            let mut ok = true;
            loop {
                match r.u8() {
                    Some(1) => {
                        let Some(n) = r.u16() else { break 'comp };
                        let Some(b) = r.bytes(n as usize) else { break 'comp };
                        v.bytes(b);
                    }
                    Some(2) => {
                        let (Some(reg), Some(n)) = (r.u8(), r.u8()) else { break 'comp };
                        let Some(name) = r.bytes(n as usize) else { break 'comp };
                        let Some(add) = r.u8() else { break 'comp };
                        let name = core::str::from_utf8(name).unwrap_or("");
                        let id = REGISTRIES
                            .get(reg as usize)
                            .and_then(|rn| regs.iter().find(|(k, _)| k == rn))
                            .and_then(|(_, names)| names.iter().position(|x| x == name));
                        match id {
                            Some(i) => {
                                v.varint(i as i32 + add as i8 as i32);
                            }
                            None => ok = false,
                        }
                    }
                    _ => break,
                }
            }
            if ok {
                comps.push((kind, v.b));
            }
        }
        let mut o = Out::new();
        o.varint(comps.len() as i32).varint(0);
        for (k, v) in comps {
            o.varint(k as i32).varint(v.len() as i32).bytes(&v);
        }
        o.b
    }

    /// The search tab's results for `query` (refreshSearchResults): every stack when empty,
    /// `#tag` by tag id, else by the tooltip's lines and the item id; and the tags shown.
    pub fn search(&self, query: &str, op: bool, names: &[String]) -> (Vec<u32>, Vec<u16>) {
        let Some(st) = self.tabs.iter().find(|t| t.kind == SEARCH) else { return (Vec::new(), Vec::new()) };
        let q = query.to_lowercase();
        let all = st.items.iter().copied().filter(|&e| op || !self.entries[e as usize].op);
        if q.is_empty() {
            return (all.collect(), Vec::new());
        }
        if let Some(t) = q.strip_prefix('#') {
            let tags: Vec<u16> = (0..self.tags.len() as u16).filter(|&i| id_matches(&self.tags[i as usize], t)).collect();
            let out = all.filter(|&e| self.entries[e as usize].tags.iter().any(|x| tags.contains(x))).collect();
            return (out, tags);
        }
        let out = all
            .filter(|&e| {
                let en = &self.entries[e as usize];
                let id = names.get(en.item as usize).map_or("", |s| s.as_str());
                let by_id = if id.contains(':') { id_matches(id, &q) } else { id_matches(&format!("minecraft:{}", id), &q) };
                by_id || (!q.contains(':') && en.lower.iter().any(|l| l.contains(q.as_str())))
            })
            .collect();
        (out, Vec::new())
    }
}

/// IdSearchTree: "ns:path" by the parts around the colon, else by the path.
fn id_matches(id: &str, q: &str) -> bool {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    match q.split_once(':') {
        Some((qn, qp)) => ns.contains(qn.trim()) && path.contains(qp.trim()),
        None => path.contains(q),
    }
}

// ------------------------------------------------------------------ clicks

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Input {
    /// Left (0) or right (1) click.
    Pickup(i32),
    /// Shift-click.
    QuickMove(i32),
    /// A number key: hotbar slot 0-8.
    Swap(i32),
    /// The middle button.
    Clone,
    /// Q (true with Ctrl: the whole stack).
    Throw(bool),
}

/// Where a click lands.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    /// The item grid's slot 0-44.
    Grid(usize),
    /// An inventory-menu slot (5-8 armour, 9-35 main, 36-44 hotbar, 45 offhand).
    Menu(usize),
    Destroy,
    /// Outside the window (and not on a tab).
    Outside,
    None,
}

/// The player's inventory during a click: the slots (inventory-menu order), the cursor, and
/// the stacks thrown out.
pub struct Inv {
    pub slots: Vec<Option<Stack>>,
    pub carried: Option<Stack>,
    pub drops: Vec<Stack>,
}

impl Inv {
    fn may_place(&self, d: &Data, s: usize, c: &Stack) -> bool {
        match s {
            5..=8 => d.equip(c.item) as usize == s - 4,
            0..=4 => false,
            _ => true,
        }
    }
    fn slot_max(&self, d: &Data, s: usize, c: &Stack) -> i32 {
        if (5..=8).contains(&s) {
            1
        } else {
            d.max_stack(c.item)
        }
    }
    fn take_carried(&mut self, n: i32) {
        if let Some(c) = self.carried.as_mut() {
            c.count -= n;
            if c.count <= 0 {
                self.carried = None;
            }
        }
    }

    /// AbstractContainerMenu.doClick's PICKUP on an inventory slot.
    pub fn pickup(&mut self, d: &Data, s: usize, button: i32) {
        let cur = self.slots[s].take();
        let car = self.carried.take();
        match (cur, car) {
            (None, None) => {}
            (Some(x), None) => {
                if button == 0 {
                    self.carried = Some(x);
                } else {
                    let take = (x.count + 1) / 2;
                    self.carried = Some(x.with(take));
                    if x.count > take {
                        self.slots[s] = Some(x.with(x.count - take));
                    }
                }
            }
            (None, Some(c)) => {
                if self.may_place(d, s, &c) {
                    let n = if button == 0 { c.count } else { 1 }.min(self.slot_max(d, s, &c));
                    self.slots[s] = Some(c.with(n));
                    if c.count > n {
                        self.carried = Some(c.with(c.count - n));
                    }
                } else {
                    self.carried = Some(c);
                }
            }
            (Some(mut x), Some(mut c)) => {
                if self.may_place(d, s, &c) {
                    if x.same(&c) {
                        let n = if button == 0 { c.count } else { 1 }.min(self.slot_max(d, s, &c) - x.count).max(0);
                        x.count += n;
                        c.count -= n;
                        self.slots[s] = Some(x);
                        self.carried = if c.count > 0 { Some(c) } else { None };
                    } else if c.count <= self.slot_max(d, s, &c) {
                        self.slots[s] = Some(c);
                        self.carried = Some(x);
                    } else {
                        self.slots[s] = Some(x);
                        self.carried = Some(c);
                    }
                } else if x.same(&c) {
                    // Pick what fits from the slot onto the cursor.
                    let n = x.count.min(d.max_stack(c.item) - c.count).max(0);
                    c.count += n;
                    x.count -= n;
                    self.slots[s] = if x.count > 0 { Some(x) } else { None };
                    self.carried = Some(c);
                } else {
                    self.slots[s] = Some(x);
                    self.carried = Some(c);
                }
            }
        }
    }

    /// moveItemStackTo: into stacks of the same item first, then empty slots.
    fn move_to(&mut self, d: &Data, from: usize, range: core::ops::Range<usize>) -> bool {
        let Some(mut st) = self.slots[from].take() else { return false };
        let before = st.count;
        if d.max_stack(st.item) > 1 {
            for s in range.clone() {
                if st.count == 0 {
                    break;
                }
                if let Some(x) = self.slots[s].as_mut() {
                    if x.same(&st) {
                        let lim = if (5..=8).contains(&s) { 1 } else { d.max_stack(st.item) };
                        let n = st.count.min(lim - x.count).max(0);
                        x.count += n;
                        st.count -= n;
                    }
                }
            }
        }
        for s in range {
            if st.count == 0 {
                break;
            }
            if self.slots[s].is_none() && self.may_place(d, s, &st) {
                let n = st.count.min(self.slot_max(d, s, &st));
                self.slots[s] = Some(st.with(n));
                st.count -= n;
            }
        }
        let moved = st.count != before;
        if st.count > 0 {
            self.slots[from] = Some(st);
        }
        moved
    }

    /// InventoryMenu.quickMoveStack, repeated while it moves something (doClick's QUICK_MOVE).
    pub fn quick_move(&mut self, d: &Data, s: usize) {
        for _ in 0..8 {
            let Some(st) = self.slots[s].clone() else { return };
            let eq = d.equip(st.item) as usize;
            let moved = match s {
                5..=8 => self.move_to(d, s, 9..45),
                _ if (1..=4).contains(&eq) && self.slots[4 + eq].is_none() => self.move_to(d, s, 4 + eq..5 + eq),
                _ if eq == 5 && s != 45 && self.slots[45].is_none() => self.move_to(d, s, 45..46),
                9..=35 => self.move_to(d, s, 36..45),
                36..=44 => self.move_to(d, s, 9..36),
                _ => self.move_to(d, s, 9..45),
            };
            if !moved || self.slots[s].as_ref().map_or(true, |x| !x.same(&st)) {
                return;
            }
        }
    }

    /// doClick's SWAP with hotbar slot `k` (40: the offhand).
    pub fn swap(&mut self, d: &Data, s: usize, k: usize) {
        let h = menu_of_swap(k as i32);
        if h == s {
            return;
        }
        let (a, b) = (self.slots[s].clone(), self.slots[h].clone());
        if let Some(x) = b.as_ref() {
            if !self.may_place(d, s, x) {
                return;
            }
        }
        self.slots[s] = b;
        self.slots[h] = a;
    }

    /// THROW on an inventory slot: one, or the stack, thrown out. The creative screen does it
    /// whether or not the cursor holds a stack (slotClicked's inventory-tab THROW, and its
    /// hotbar-row THROW after ItemPickerMenu.clicked).
    pub fn throw(&mut self, s: usize, all: bool) {
        if let Some(mut x) = self.slots[s].take() {
            let n = if all { x.count } else { 1 };
            self.drops.push(x.with(n));
            x.count -= n;
            if x.count > 0 {
                self.slots[s] = Some(x);
            }
        }
    }

    /// Clicking outside: the cursor's stack (right: one of it) is thrown.
    fn throw_carried(&mut self, button: i32) {
        if let Some(c) = self.carried.clone() {
            if button == 0 {
                self.drops.push(c);
                self.carried = None;
            } else {
                self.drops.push(c.with(1));
                self.take_carried(1);
            }
        }
    }

    /// slotClicked on a creative grid slot showing `item` (CustomCreativeSlot).
    pub fn grid(&mut self, d: &Data, item: Option<Stack>, input: Input) {
        let max = |s: &Stack| d.max_stack(s.item);
        match input {
            Input::Swap(k) => {
                if let Some(it) = item {
                    let n = max(&it);
                    self.slots[menu_of_swap(k)] = Some(it.with(n));
                }
            }
            Input::Clone => {
                if self.carried.is_none() {
                    if let Some(it) = item {
                        let n = max(&it);
                        self.carried = Some(it.with(n));
                    }
                }
            }
            Input::Throw(all) => {
                if let Some(it) = item {
                    let n = if all { max(&it) } else { 1 };
                    self.drops.push(it.with(n));
                }
            }
            Input::Pickup(button) | Input::QuickMove(button) => {
                let quick = matches!(input, Input::QuickMove(_));
                match (item, self.carried.as_mut()) {
                    (Some(it), Some(c)) if c.same(&it) => {
                        if button == 0 {
                            let m = max(c);
                            if quick {
                                c.count = m;
                            } else if c.count < m {
                                c.count += 1;
                            }
                        } else {
                            self.take_carried(1);
                        }
                    }
                    (None, _) | (_, Some(_)) => {
                        if button == 0 {
                            self.carried = None;
                        } else {
                            self.take_carried(1);
                        }
                    }
                    (Some(it), None) => {
                        let n = if quick { max(&it) } else { it.count };
                        self.carried = Some(it.with(n));
                    }
                }
            }
        }
    }

    /// A click as `CreativeModeInventoryScreen.slotClicked` handles it (`inventory_tab`: the
    /// survival inventory tab is open).
    pub fn click(&mut self, d: &Data, inventory_tab: bool, hit: Hit, item: Option<Stack>, input: Input) {
        match hit {
            Hit::Destroy => {
                if matches!(input, Input::QuickMove(_)) {
                    for s in self.slots.iter_mut() {
                        *s = None;
                    }
                } else {
                    self.carried = None;
                }
            }
            Hit::Outside => match input {
                // slotClicked turns PICKUP at -999 into THROW: the inventory tab's THROW path
                // drops the whole cursor stack whatever the button; the other tabs drop the
                // stack (left) or one of it (right).
                Input::Pickup(b) | Input::QuickMove(b) => self.throw_carried(if inventory_tab { 0 } else { b }),
                _ => {}
            },
            Hit::Grid(_) => self.grid(d, item, input),
            Hit::Menu(s) => match input {
                Input::Pickup(b) => self.pickup(d, s, b),
                Input::QuickMove(_) => {
                    if inventory_tab {
                        self.quick_move(d, s);
                    } else {
                        // ItemPickerMenu.quickMoveStack: a hotbar slot is emptied.
                        self.slots[s] = None;
                    }
                }
                Input::Swap(k) => self.swap(d, s, k as usize),
                Input::Clone => {
                    if self.carried.is_none() {
                        if let Some(x) = self.slots[s].as_ref() {
                            self.carried = Some(x.with(d.max_stack(x.item)));
                        }
                    }
                }
                Input::Throw(all) => self.throw(s, all),
            },
            Hit::None => {}
        }
    }
}

/// The inventory-menu slot of a SWAP's button: hotbar 0-8, or 40 (the offhand key).
fn menu_of_swap(k: i32) -> usize {
    if k == 40 {
        45
    } else {
        36 + k.clamp(0, 8) as usize
    }
}

// ------------------------------------------------------------------ the screen

/// What a grid slot shows.
#[derive(Clone)]
enum Pick {
    Entry(u32),
    Stack(Stack),
    /// An empty saved hotbar's paper (its number).
    Info(usize),
    Empty,
}

pub enum Action {
    None,
    Close,
    /// Not in creative any more: the survival inventory instead.
    Survival,
}

pub struct Screen {
    pub tab: usize,
    scroll: f32,
    scrolling: bool,
    search: crate::textedit::Edit,
    /// A drag that started in the search box is selecting; when and where (byte offset) the last click there was.
    search_drag: bool,
    search_click: (u64, usize),
    shown: Vec<Pick>,
    carried: Option<Stack>,
    visible_tags: Vec<u16>,
    patches: Vec<Vec<u8>>,
    by_patch: BTreeMap<(i32, Vec<u8>), u32>,
    op_cached: bool,
    ignore_text: Option<(char, u64)>,
    hotbars: Option<Vec<Vec<Option<Stack>>>>,
    opened: bool,
    test: Vec<String>,
}

impl Screen {
    pub fn new() -> Screen {
        Screen {
            tab: usize::MAX,
            scroll: 0.0,
            scrolling: false,
            search: crate::textedit::Edit::new(50, crate::textedit::Filter::Line),
            search_drag: false,
            search_click: (0, usize::MAX),
            shown: Vec::new(),
            carried: None,
            visible_tags: Vec::new(),
            patches: Vec::new(),
            by_patch: BTreeMap::new(),
            op_cached: false,
            ignore_text: None,
            hotbars: None,
            opened: false,
            test: Vec::new(),
        }
    }

    fn op(g: &Game) -> bool {
        g.creative.op_level >= 2
    }

    /// init: the last tab again (the default the first time, or when it is gone).
    pub fn open(&mut self, d: &Data, g: &Game, key: char) {
        if self.patches.len() != d.entries.len() {
            self.patches = (0..d.entries.len()).map(|e| d.patch(e, &g.creative.registries)).collect();
            self.by_patch.clear();
            for (e, p) in self.patches.iter().enumerate() {
                self.by_patch.entry((d.entries[e].item as i32, p.clone())).or_insert(e as u32);
            }
        }
        if !self.opened {
            self.opened = true;
            if let Some(t) = crate::sys::env("PENRYN_TEST_CREATIVE") {
                self.test = t.split(';').rev().map(String::from).collect();
            }
        }
        self.carried = None;
        self.scrolling = false;
        self.search_drag = false;
        self.ignore_text = Some((key, crate::sys::now_ms()));
        let prev = self.tab;
        self.tab = usize::MAX;
        let t = if d.visible(prev, Self::op(g)) { prev } else { d.default_tab() };
        self.select(d, g, t);
    }

    fn tab_kind(&self, d: &Data) -> u8 {
        d.tabs.get(self.tab).map_or(CATEGORY, |t| t.kind)
    }

    /// selectTab: its stacks, the search box cleared on a new tab, scrolled to the top.
    pub fn select(&mut self, d: &Data, g: &Game, t: usize) {
        if t >= d.tabs.len() {
            return;
        }
        let changed = self.tab != t;
        self.tab = t;
        let tab = &d.tabs[t];
        let op = Self::op(g);
        match tab.kind {
            HOTBAR => self.fill_hotbars(),
            SEARCH => {
                if changed {
                    self.search.set("");
                }
                self.refresh_search(d, g);
            }
            INVENTORY => self.shown.clear(),
            _ => {
                self.shown = tab.items.iter().filter(|&&e| op || !d.entries[e as usize].op).map(|&e| Pick::Entry(e)).collect();
            }
        }
        if tab.kind != SEARCH {
            self.search.set("");
            self.visible_tags.clear();
        }
        self.scroll = 0.0;
    }

    fn refresh_search(&mut self, d: &Data, g: &Game) {
        let (items, tags) = d.search(&self.search.text, Self::op(g), &g.blocks.items);
        self.shown = items.into_iter().map(Pick::Entry).collect();
        self.visible_tags = tags;
        self.scroll = 0.0;
    }

    fn rows(&self) -> i32 {
        (self.shown.len() as i32 + 8) / 9 - 5
    }
    fn can_scroll(&self, d: &Data) -> bool {
        d.tabs.get(self.tab).map_or(false, |t| t.can_scroll()) && self.shown.len() > 45
    }
    /// getRowIndexForScroll.
    fn first_row(&self) -> i32 {
        crate::math::floor((self.scroll * self.rows() as f32) as f64 + 0.5).max(0.0) as i32
    }

    fn stack_of(&self, d: &Data, p: &Pick) -> Option<Stack> {
        match p {
            Pick::Entry(e) => Some(Stack {
                item: d.entries[*e as usize].item as i32,
                count: 1,
                patch: self.patches.get(*e as usize).cloned().unwrap_or_else(|| vec![0, 0]),
            }),
            Pick::Stack(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The grid slot `i`'s pick (scrollTo).
    fn grid_pick(&self, i: usize) -> &Pick {
        let k = (self.first_row() * 9) as usize + i;
        self.shown.get(k).unwrap_or(&Pick::Empty)
    }

    fn origin(gui: &Gui) -> (f32, f32) {
        (((gui.width - W) / 2.0).floor(), ((gui.height - H) / 2.0).floor())
    }

    /// The inventory tab's slot positions (selectTab's SlotWrappers).
    fn menu_pos(s: usize) -> (f32, f32) {
        match s {
            5..=8 => {
                let i = s - 5;
                (54.0 + (i / 2) as f32 * 54.0, 6.0 + (i % 2) as f32 * 27.0)
            }
            45 => (35.0, 20.0),
            9..=35 => (9.0 + ((s - 9) % 9) as f32 * 18.0, 54.0 + ((s - 9) / 9) as f32 * 18.0),
            _ => (9.0 + (s - 36) as f32 * 18.0, 112.0),
        }
    }

    fn slots(&self, d: &Data) -> Vec<(Hit, f32, f32)> {
        let mut v = Vec::with_capacity(54);
        if self.tab_kind(d) == INVENTORY {
            for s in 5..46 {
                let (x, y) = Self::menu_pos(s);
                v.push((Hit::Menu(s), x, y));
            }
            v.push((Hit::Destroy, 173.0, 112.0));
        } else {
            for i in 0..45 {
                v.push((Hit::Grid(i), 9.0 + (i % 9) as f32 * 18.0, 18.0 + (i / 9) as f32 * 18.0));
            }
            for j in 0..9 {
                v.push((Hit::Menu(36 + j), 9.0 + j as f32 * 18.0, 112.0));
            }
        }
        v
    }

    fn hit(&self, d: &Data, gui: &Gui, mx: f32, my: f32) -> (Hit, Option<(f32, f32)>) {
        let (left, top) = Self::origin(gui);
        for (h, x, y) in self.slots(d) {
            if mx >= left + x - 1.0 && mx < left + x + 17.0 && my >= top + y - 1.0 && my < top + y + 17.0 {
                return (h, Some((x, y)));
            }
        }
        // hasClickedOutside: outside the window. Its exception for the selected tab compares the
        // absolute mouse position with window-relative tab bounds, so it never applies: a
        // right click on a tab throws (a left click is the tab's).
        let outside = mx < left || my < top || mx >= left + W || my >= top + H;
        if outside {
            (Hit::Outside, None)
        } else {
            (Hit::None, None)
        }
    }

    /// checkTabClicked (or, with `hover`, checkTabHovering) for the visible tabs.
    fn tab_at(&self, d: &Data, gui: &Gui, mx: f32, my: f32, hover: bool) -> Option<usize> {
        let (left, top) = Self::origin(gui);
        let (rx, ry) = (mx - left, my - top);
        (0..d.tabs.len()).find(|&i| {
            let t = &d.tabs[i];
            if !self.tab_shown(d, i) {
                return false;
            }
            let (x, y) = (t.x(), t.y());
            if hover {
                rx >= x + 2.0 && rx < x + 25.0 && ry >= y + 2.0 && ry < y + 31.0
            } else {
                rx >= x && rx <= x + 26.0 && ry >= y && ry <= y + 32.0
            }
        })
    }

    fn tab_shown(&self, d: &Data, i: usize) -> bool {
        d.tabs[i].flags & 8 == 0 || self.op_cached
    }

    fn inside_scrollbar(&self, d: &Data, gui: &Gui, mx: f32, my: f32) -> bool {
        let (left, top) = Self::origin(gui);
        mx >= left + 175.0 && my >= top + 18.0 && mx < left + 189.0 && my < top + 130.0 && d.tabs.get(self.tab).map_or(false, |t| t.can_scroll())
    }

    // ---- the inventory with the server

    fn inv(&self, g: &Game) -> Inv {
        let slots = (0..46)
            .map(|i| {
                g.inventory[i].map(|(item, count)| Stack {
                    item,
                    count,
                    patch: g.creative.patches.get(i).map_or_else(|| vec![0, 0], |p| norm(p).to_vec()),
                })
            })
            .collect();
        Inv { slots, carried: self.carried.clone(), drops: Vec::new() }
    }

    /// The click's result: the thrown stacks and every changed slot go to the server.
    fn apply(&mut self, g: &mut Game, inv: Inv) -> Result<(), String> {
        let before = self.inv(g);
        self.carried = inv.carried;
        if !inv.drops.is_empty() {
            g.punch()?;
        }
        for s in inv.drops.iter() {
            send_slot(g, -1, Some(s))?;
        }
        for (i, (a, b)) in before.slots.iter().zip(inv.slots.iter()).enumerate() {
            if a != b {
                g.inventory[i] = b.as_ref().map(|s| (s.item, s.count));
                if let Some(p) = g.creative.patches.get_mut(i) {
                    *p = b.as_ref().map_or(Vec::new(), |s| s.patch.clone());
                }
                send_slot(g, i as i16, b.as_ref())?;
            }
        }
        Ok(())
    }

    fn click(&mut self, d: &Data, g: &mut Game, hit: Hit, input: Input) -> Result<(), String> {
        let item = match hit {
            Hit::Grid(i) => {
                let p = self.grid_pick(i).clone();
                if matches!(p, Pick::Info(_)) {
                    return Ok(()); // a locked paper (CREATIVE_SLOT_LOCK): mayPickup is false
                }
                self.stack_of(d, &p)
            }
            _ => None,
        };
        let mut inv = self.inv(g);
        inv.click(d, self.tab_kind(d) == INVENTORY, hit, item, input);
        self.apply(g, inv)
    }

    // ---- saved hotbars (HotbarManager): hotbar.bin in the data folder

    fn hotbar_path() -> String {
        format!("{}/hotbar.bin", crate::data_dir())
    }

    fn load_hotbars(&mut self) -> &mut Vec<Vec<Option<Stack>>> {
        if self.hotbars.is_none() {
            let mut hb = vec![vec![None; 9]; 9];
            if let Some(d) = crate::sys::read_file(&Self::hotbar_path()) {
                let mut r = R { d: &d, p: 0 };
                if r.bytes(4) == Some(&b"PNH1"[..]) {
                    'all: for row in hb.iter_mut() {
                        for s in row.iter_mut() {
                            match r.u8() {
                                Some(1) => {
                                    let (Some(item), Some(count), Some(n)) = (r.u32(), r.u32(), r.u16()) else { break 'all };
                                    let Some(p) = r.bytes(n as usize) else { break 'all };
                                    *s = Some(Stack { item: item as i32, count: count as i32, patch: p.to_vec() });
                                }
                                Some(_) => {}
                                None => break 'all,
                            }
                        }
                    }
                }
            }
            self.hotbars = Some(hb);
        }
        self.hotbars.as_mut().unwrap()
    }

    fn fill_hotbars(&mut self) {
        let hb = self.load_hotbars().clone();
        self.shown.clear();
        for (i, row) in hb.iter().enumerate() {
            if row.iter().all(|s| s.is_none()) {
                for j in 0..9 {
                    self.shown.push(if j == i { Pick::Info(i) } else { Pick::Empty });
                }
            } else {
                for s in row {
                    self.shown.push(s.clone().map_or(Pick::Empty, Pick::Stack));
                }
            }
        }
    }

    /// handleHotbarLoadOrSave: X+number puts saved hotbar `i` in the hotbar, C+number saves it.
    pub fn hotbar_load_or_save(&mut self, d: &Data, g: &mut Game, i: usize, load: bool) -> Result<(), String> {
        let i = i.min(8);
        if load {
            let row = self.load_hotbars()[i].clone();
            let mut inv = self.inv(g);
            for (j, s) in row.into_iter().enumerate() {
                inv.slots[36 + j] = s;
            }
            self.apply(g, inv)?;
        } else {
            let inv = self.inv(g);
            let row: Vec<Option<Stack>> = (36..45).map(|s| inv.slots[s].clone()).collect();
            self.load_hotbars()[i] = row;
            let mut out = Vec::from(&b"PNH1"[..]);
            for row in self.hotbars.as_ref().unwrap() {
                for s in row {
                    match s {
                        Some(s) => {
                            out.push(1);
                            out.extend_from_slice(&(s.item as u32).to_le_bytes());
                            out.extend_from_slice(&(s.count as u32).to_le_bytes());
                            out.extend_from_slice(&(s.patch.len() as u16).to_le_bytes());
                            out.extend_from_slice(&s.patch);
                        }
                        None => out.push(0),
                    }
                }
            }
            crate::sys::write_file_private(&Self::hotbar_path(), &out);
            if self.tab_kind(d) == HOTBAR {
                self.fill_hotbars();
            }
        }
        Ok(())
    }

    // ---- input

    /// A key (repeats included): what the screen does with it.
    pub fn key(&mut self, d: &Data, gui: &Gui, g: &mut Game, sc: usize, repeat: bool, keys: &[bool; 512], mouse: (f32, f32), v: &crate::sdl::Video) -> Result<Action, String> {
        const K_E: usize = 8;
        const K_F: usize = 9;
        const K_Q: usize = 20;
        const K_T: usize = 23;
        const K_1: usize = 30;
        const K_ESC: usize = 41;
        const K_BACKSPACE: usize = 42;
        let ctrl = keys[224] || keys[228];
        let search = self.tab_kind(d) == SEARCH;
        if sc == K_ESC && !repeat {
            return Ok(Action::Close);
        }
        if search {
            // The box's editing keys: arrows, Home, End, Backspace, Delete, and Ctrl (Command on macOS) with A, C, X, V.
            let before = self.search.text.clone();
            let (cmd, sh) = (crate::textedit::command(keys), crate::textedit::shift(keys));
            if self.search.key(sc, cmd, sh, &mut || v.clipboard(), &mut |s: &str| v.set_clipboard(s)) {
                if self.search.text != before {
                    self.refresh_search(d, g);
                }
                return Ok(Action::None);
            }
        }
        if repeat {
            return Ok(Action::None);
        }
        let (hit, _) = self.hit(d, gui, mouse.0, mouse.1);
        // checkHotbarKeyPressed: a number key (or F, the offhand) swaps with the slot under the
        // mouse when the cursor is empty. In the search tab only over a stack or an inventory
        // slot; else the key is typed into the box.
        let swap = if (K_1..K_1 + 9).contains(&sc) {
            Some((sc - K_1) as i32)
        } else if sc == K_F && !search {
            Some(40)
        } else {
            None
        };
        if let Some(k) = swap {
            let over = match hit {
                Hit::Grid(i) => !search || !matches!(self.grid_pick(i), Pick::Empty),
                Hit::Menu(_) => true,
                _ => false,
            };
            if over && self.carried.is_none() {
                self.click(d, g, hit, Input::Swap(k))?;
                if search {
                    self.ignore_text = Some(((b'1' + k as u8) as char, crate::sys::now_ms()));
                }
            }
            return Ok(Action::None);
        }
        if search {
            return Ok(Action::None); // typed into the box (text input)
        }
        match sc {
            K_E => return Ok(Action::Close),
            K_T => {
                let s = d.tabs.iter().position(|t| t.kind == SEARCH);
                if let Some(s) = s {
                    self.ignore_text = Some(('t', crate::sys::now_ms()));
                    self.select(d, g, s);
                }
            }
            K_Q => {
                if matches!(hit, Hit::Grid(_) | Hit::Menu(_)) {
                    self.click(d, g, hit, Input::Throw(ctrl))?;
                }
            }
            _ => {}
        }
        Ok(Action::None)
    }

    /// Typed text (charTyped): only the search tab takes it.
    pub fn text(&mut self, d: &Data, g: &Game, s: &str) {
        if let Some((c, at)) = self.ignore_text.take() {
            if crate::sys::now_ms() < at + 150 && s.chars().next().map(|x| x.to_ascii_lowercase()) == Some(c) {
                return;
            }
        }
        if self.tab_kind(d) != SEARCH {
            return;
        }
        let before = self.search.text.clone();
        self.search.insert(s);
        if self.search.text != before {
            self.refresh_search(d, g);
        }
    }

    /// The part of the search text that is shown (byte offsets): scrolled to keep the cursor in view.
    fn search_window(&self, gui: &Gui) -> (usize, usize) {
        let room = 80.0 - gui.text_width("_");
        let t = &self.search.text;
        let cur = self.search.cursor;
        let mut start = 0;
        while start < cur && gui.text_width(&t[start..cur]) > room {
            start += t[start..].chars().next().map_or(1, |c| c.len_utf8());
        }
        let mut end = start;
        while end < t.len() {
            let n = end + t[end..].chars().next().map_or(1, |c| c.len_utf8());
            if gui.text_width(&t[start..n]) > room {
                break;
            }
            end = n;
        }
        (start, end)
    }

    /// The byte offset in the search text nearest to the mouse.
    fn search_at(&self, gui: &Gui, left: f32, mx: f32) -> usize {
        let (sb, _) = self.search_window(gui);
        sb + crate::textedit::index_at(&self.search.text[sb..], mx - (left + 82.0), &|s: &str| gui.text_width(s))
    }

    /// A mouse button (SDL: 1 left, 2 middle, 3 right) going down or up.
    pub fn mouse(&mut self, d: &Data, gui: &Gui, g: &mut Game, button: u8, down: bool, shift: bool, mx: f32, my: f32) -> Result<(), String> {
        self.op_cached = Self::op(g);
        if down && button == 1 && self.tab_kind(d) == SEARCH {
            let (left, top) = Self::origin(gui);
            if mx >= left + 80.0 && mx < left + 170.0 && my >= top + 4.0 && my < top + 16.0 {
                // A click in the search box: the caret goes there (Shift extends), a double click selects the word.
                let at = self.search_at(gui, left, mx);
                let now = crate::sys::now_ms();
                if self.search_click.1 == at && now.saturating_sub(self.search_click.0) < 250 {
                    self.search.select_word(at);
                    self.search_click = (0, usize::MAX);
                } else {
                    self.search_click = (now, at);
                    self.search.click(at, shift);
                    self.search_drag = true;
                }
                return Ok(());
            }
        }
        if !down {
            if button == 1 {
                self.search_drag = false;
                self.scrolling = false;
                // mouseReleased: the tab under the mouse is selected.
                if let Some(t) = self.tab_at(d, gui, mx, my, false) {
                    self.select(d, g, t);
                }
            }
            return Ok(());
        }
        if button == 1 {
            if self.tab_at(d, gui, mx, my, false).is_some() {
                return Ok(()); // selected when the button comes up
            }
            if self.tab_kind(d) != INVENTORY && self.inside_scrollbar(d, gui, mx, my) {
                self.scrolling = self.can_scroll(d);
                return Ok(());
            }
        }
        let (hit, _) = self.hit(d, gui, mx, my);
        let input = match button {
            2 => Input::Clone,
            1 | 3 => {
                let b = if button == 1 { 0 } else { 1 };
                if shift {
                    Input::QuickMove(b)
                } else {
                    Input::Pickup(b)
                }
            }
            _ => return Ok(()),
        };
        if hit == Hit::Outside && input == Input::Clone {
            return Ok(());
        }
        self.click(d, g, hit, input)
    }

    /// The wheel: mouseScrolled (`y` up is positive).
    pub fn wheel(&mut self, d: &Data, y: i32) {
        if self.can_scroll(d) {
            self.scroll = (self.scroll - y as f32 / self.rows() as f32).clamp(0.0, 1.0);
        }
    }

    /// Every tick: leaves when the player is no longer in creative, and runs test steps
    /// (PENRYN_TEST_CREATIVE, ';'-separated: tab=N, search=TEXT, scroll=F, click=AREA[:b[:shift]]
    /// with AREA g<grid> s<slot> d (destroy) o (outside), key=AREA:<1-9>, mouse=AREA, close).
    pub fn tick(&mut self, d: &Data, gui: &Gui, g: &mut Game, mouse: &mut (f32, f32)) -> Result<Action, String> {
        self.op_cached = Self::op(g);
        if g.game_mode != 1 {
            return Ok(Action::Survival);
        }
        let Some(cmd) = self.test.pop() else { return Ok(Action::None) };
        crate::say!("creative test: {}", cmd);
        let (k, v) = cmd.split_once('=').unwrap_or((cmd.as_str(), ""));
        let area = |a: &str| -> (Hit, f32, f32) {
            let (left, top) = Self::origin(gui);
            let num = a.get(1..).and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
            match a.chars().next() {
                Some('g') => (Hit::Grid(num.min(44)), left + 9.0 + (num % 9) as f32 * 18.0 + 8.0, top + 18.0 + (num / 9 % 5) as f32 * 18.0 + 8.0),
                Some('s') => {
                    let (x, y) = if self.tab_kind(d) == INVENTORY { Self::menu_pos(num.clamp(5, 45)) } else { (9.0 + (num.clamp(36, 44) - 36) as f32 * 18.0, 112.0) };
                    (Hit::Menu(num.clamp(5, 45)), left + x + 8.0, top + y + 8.0)
                }
                Some('d') => (Hit::Destroy, left + 181.0, top + 120.0),
                Some('o') => (Hit::Outside, left - 20.0, top + 20.0),
                _ => (Hit::None, mouse.0, mouse.1),
            }
        };
        match k {
            "tab" => {
                let t = v.parse().unwrap_or(0);
                self.select(d, g, t);
            }
            "search" => {
                self.search.set(v);
                self.refresh_search(d, g);
            }
            "scroll" => self.scroll = v.parse::<f32>().unwrap_or(0.0).clamp(0.0, 1.0),
            "click" => {
                let mut it = v.split(':');
                let (h, x, y) = area(it.next().unwrap_or(""));
                let b = it.next().and_then(|b| b.parse::<i32>().ok()).unwrap_or(0);
                let shift = it.next() == Some("shift");
                *mouse = (x, y);
                let input = if b == 2 {
                    Input::Clone
                } else if shift {
                    Input::QuickMove(b)
                } else {
                    Input::Pickup(b)
                };
                self.click(d, g, h, input)?;
            }
            "key" => {
                let mut it = v.split(':');
                let (h, x, y) = area(it.next().unwrap_or(""));
                *mouse = (x, y);
                let n = it.next().and_then(|b| b.parse::<i32>().ok()).unwrap_or(1).clamp(1, 9);
                self.click(d, g, h, Input::Swap(n - 1))?;
            }
            "mouse" => {
                let (_, x, y) = area(v);
                *mouse = (x, y);
            }
            "close" => return Ok(Action::Close),
            _ => {}
        }
        Ok(Action::None)
    }

    // ---- drawing

    fn draw_tab(&self, d: &Data, gui: &mut Gui, g: &Game, i: usize, left: f32, top: f32) {
        let t = &d.tabs[i];
        let selected = i == self.tab;
        let x = left + t.x();
        let y = if t.row == 0 { top - 28.0 } else { top + H - 4.0 };
        let name = format!(
            "container/creative_inventory/tab_{}_{}_{}",
            if t.row == 0 { "top" } else { "bottom" },
            if selected { "selected" } else { "unselected" },
            t.col as usize + 1
        );
        gui.blit(&name, x, y);
        gui.item(&g.blocks, t.icon as i32, 1, x + 5.0, y + 8.0 + if t.row == 0 { 1.0 } else { -1.0 });
    }

    /// The screen as vanilla draws it: unselected tabs, the background, search box, scroll bar,
    /// the selected tab, the player (inventory tab), slots, labels, the cursor's stack and the
    /// tooltip.
    /// The left button's state each frame: a search drag needs it held.
    pub fn hold(&mut self, held: bool) {
        self.search_drag &= held;
    }

    pub fn draw(&mut self, d: &Data, gui: &mut Gui, g: &Game, mx: f32, my: f32) {
        self.op_cached = Self::op(g);
        if self.scrolling {
            let top = Self::origin(gui).1 + 18.0;
            self.scroll = ((my - top - 7.5) / (112.0 - 15.0)).clamp(0.0, 1.0);
        }
        crate::screens::dim(gui);
        let (left, top) = Self::origin(gui);
        for i in 0..d.tabs.len() {
            if i != self.tab && self.tab_shown(d, i) {
                self.draw_tab(d, gui, g, i, left, top);
            }
        }
        let kind = self.tab_kind(d);
        let bg = d.tabs.get(self.tab).map_or("container/creative_inventory/tab_items", |t| t.bg.as_str());
        gui.blit(bg, left, top);
        if kind == SEARCH {
            // EditBox without a border: scrolled to keep the cursor in view, the selection highlighted, the blinking cursor.
            if self.search_drag {
                let at = self.search_at(gui, left, mx);
                self.search.click(at, true);
            }
            let (sb, eb) = self.search_window(gui);
            let cur = self.search.cursor.clamp(sb, eb);
            if self.search.has_selection() {
                let (a, b) = self.search.selection();
                let (a, b) = (a.clamp(sb, eb), b.clamp(sb, eb));
                if a < b {
                    let (x0, x1) = (left + 82.0 + gui.text_width(&self.search.text[sb..a]), left + 82.0 + gui.text_width(&self.search.text[sb..b]));
                    gui.fill(x0, top + 5.0, x1, top + 15.0, 0xFF33_99FF);
                }
            }
            let x = left + 82.0 + gui.text(&self.search.text[sb..eb], left + 82.0, top + 6.0, 0xFFFF_FFFF); // text() gives the width
            if (crate::sys::now_ms() / 300) % 2 == 0 {
                if self.search.cursor >= self.search.text.len() {
                    gui.text("_", x, top + 6.0, 0xFFFF_FFFF);
                } else {
                    let cx = left + 82.0 + gui.text_width(&self.search.text[sb..cur]);
                    gui.fill(cx, top + 5.0, cx + 1.0, top + 15.0, 0xFFD0_D0D0);
                }
            }
        }
        if d.tabs.get(self.tab).map_or(false, |t| t.can_scroll()) {
            let sprite = if self.can_scroll(d) { "container/creative_inventory/scroller" } else { "container/creative_inventory/scroller_disabled" };
            gui.blit(sprite, left + 175.0, top + 18.0 + ((112.0 - 17.0) * self.scroll).floor());
        }
        if self.tab < d.tabs.len() {
            self.draw_tab(d, gui, g, self.tab, left, top);
        }
        if kind == INVENTORY {
            let (x1, y1, x2, y2) = (left + 73.0, top + 6.0, left + 105.0, top + 49.0);
            let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
            let f = crate::math::atan(((cx - mx) / 40.0) as f64) as f32;
            let gg = crate::math::atan(((cy - my) / 40.0) as f64) as f32;
            let (model, skin) = crate::screens::skin_of(&g.uuid);
            gui.entity(model, &skin, cx, y2 - 1.5, 20.0, -f * 20.0, -f * 20.0, -gg * 20.0);
            for (s, icon) in [(5, "container/slot/helmet"), (6, "container/slot/chestplate"), (7, "container/slot/leggings"), (8, "container/slot/boots"), (45, "container/slot/shield")] {
                if g.inventory[s].is_none() {
                    let (x, y) = Self::menu_pos(s);
                    gui.blit(icon, left + x, top + y);
                }
            }
        }
        let (hover, hover_at) = self.hit(d, gui, mx, my);
        let first = (self.first_row() * 9) as usize;
        for (h, x, y) in self.slots(d) {
            match h {
                Hit::Grid(i) => match self.shown.get(first + i) {
                    Some(Pick::Entry(e)) => gui.item(&g.blocks, d.entries[*e as usize].item as i32, 1, left + x, top + y),
                    Some(Pick::Stack(s)) => gui.item(&g.blocks, s.item, s.count, left + x, top + y),
                    Some(Pick::Info(_)) => {
                        if let Some(p) = g.blocks.items.iter().position(|n| n == "paper") {
                            gui.item(&g.blocks, p as i32, 1, left + x, top + y);
                        }
                    }
                    _ => {}
                },
                Hit::Menu(s) => {
                    if let Some((item, count)) = g.inventory[s] {
                        gui.item(&g.blocks, item, count, left + x, top + y);
                    }
                }
                _ => {}
            }
            if hover == h {
                gui.fill(left + x, top + y, left + x + 16.0, top + y + 16.0, 0x80FF_FFFF);
            }
        }
        if let Some(t) = d.tabs.get(self.tab) {
            if t.flags & 2 == 0 {
                gui.text_plain(&t.name(), left + 8.0, top + 6.0, LABEL);
            }
        }
        if let Some(c) = self.carried.as_ref() {
            gui.item(&g.blocks, c.item, c.count, mx - 8.0, my - 8.0);
            return;
        }
        if let Some(t) = self.tab_at(d, gui, mx, my, true) {
            tooltip(gui, &[(d.tabs[t].name(), 0xFFFF_FFFF)], mx, my);
            return;
        }
        if hover_at.is_none() {
            return;
        }
        let lines = match hover {
            Hit::Destroy => vec![(lang::get("inventory.binSlot"), 0xFFFF_FFFF)],
            Hit::Grid(i) => match self.grid_pick(i).clone() {
                Pick::Entry(e) => self.lines(d, g, Some(e), d.entries[e as usize].item as i32, true),
                Pick::Stack(s) => {
                    let e = self.by_patch.get(&(s.item, norm(&s.patch).to_vec())).copied();
                    self.lines(d, g, e, s.item, true)
                }
                Pick::Info(i) => vec![(lang::format("inventory.hotbarInfo", &[String::from("C"), format!("{}", i + 1)]), 0xFFFF_FFFF)],
                Pick::Empty => Vec::new(),
            },
            Hit::Menu(s) => match g.inventory[s] {
                Some((item, _)) => {
                    let p = g.creative.patches.get(s).map_or(&[0u8, 0][..], |p| norm(p)).to_vec();
                    let e = self.by_patch.get(&(item, p)).copied();
                    self.lines(d, g, e, item, false)
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        if !lines.is_empty() {
            tooltip(gui, &lines, mx, my);
        }
    }

    /// getTooltipFromContainerItem: the stack's lines, then (but for a category tab's own
    /// stacks) the tabs it is in, in blue, and in a tag search the tags shown.
    fn lines(&self, d: &Data, g: &Game, e: Option<u32>, item: i32, creative_slot: bool) -> Vec<(String, u32)> {
        let rarity = d.items.get(item as usize).map_or(0, |i| i.rarity as usize);
        let mut out: Vec<(String, u32)> = Vec::new();
        match e.map(|e| &d.entries[e as usize]).filter(|en| !en.lines.is_empty()) {
            Some(en) => {
                for (k, l) in en.lines.iter().enumerate() {
                    out.push((l.clone(), if k == 0 { RARITY[rarity] } else { GRAY }));
                }
            }
            None => {
                let id = g.blocks.items.get(item as usize).cloned().unwrap_or_default();
                out.push((lang::item_name(&id), RARITY[rarity]));
            }
        }
        let kind = self.tab_kind(d);
        if kind == CATEGORY && creative_slot {
            return out;
        }
        if kind == SEARCH && creative_slot {
            if let Some(en) = e.map(|e| &d.entries[e as usize]) {
                for t in self.visible_tags.iter() {
                    if en.tags.contains(t) {
                        out.insert(1, (format!("#{}", d.tags[*t as usize]), DARK_PURPLE));
                    }
                }
            }
        }
        // The tabs from index 1, ahead of the tags.
        let mut at = 1;
        let mask = match e {
            Some(e) => d.entries[e as usize].tabs,
            None => 0,
        };
        for (i, t) in d.tabs.iter().enumerate() {
            if t.kind != SEARCH && mask & (1 << i) != 0 {
                out.insert(at.min(out.len()), (t.name(), BLUE));
                at += 1;
            }
        }
        out
    }
}

/// ServerboundSetCreativeModeSlotPacket: slot (short), then the stack as
/// ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC.
pub fn send_slot(g: &mut Game, slot: i16, s: Option<&Stack>) -> Result<(), String> {
    let mut o = Out::new();
    o.b.extend_from_slice(&slot.to_be_bytes());
    match s {
        None => {
            o.varint(0);
        }
        Some(s) => {
            o.varint(s.count).varint(s.item).bytes(norm(&s.patch));
        }
    }
    g.conn.send(play::S_SET_CREATIVE_MODE_SLOT, &o)
}

/// Vanilla's tooltip with a colour per line.
fn tooltip(gui: &mut Gui, lines: &[(String, u32)], mx: f32, my: f32) {
    let w = lines.iter().map(|l| gui.text_width(&l.0)).fold(0.0, f32::max);
    let h = if lines.len() == 1 { 8.0 } else { 10.0 + (lines.len() - 1) as f32 * 10.0 };
    let mut x = mx + 12.0;
    let mut y = my - 12.0;
    if x + w + 4.0 > gui.width {
        x = (mx - 16.0 - w).max(4.0);
    }
    if y + h + 4.0 > gui.height {
        y = gui.height - h - 4.0;
    }
    y = y.max(4.0);
    gui.blit_nine("tooltip/background", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    gui.blit_nine("tooltip/frame", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    for (i, (l, c)) in lines.iter().enumerate() {
        gui.text(l, x, y + if i == 0 { 0.0 } else { 2.0 + i as f32 * 10.0 }, *c);
    }
}

// ------------------------------------------------------------------ self test

/// Checks of the data and the click rules (penryn selftest; needs gui.bin).
pub fn checks(gui_bin: &[u8], item_names: &[String]) -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let Some(d) = Data::from_gui_bin(gui_bin) else {
        out.push(("creative data in gui.bin", String::from("missing"), "present"));
        return out;
    };
    let name = |i: i32| item_names.get(i as usize).cloned().unwrap_or_default();
    let tabs: Vec<String> = d.tabs.iter().map(|t| format!("{}{}{}", t.row, t.col, t.kind)).collect();
    out.push(("creative tabs (row, column, type)", tabs.join(" "), "000 010 020 030 040 051 062 100 110 120 130 140 150 163"));
    let bb = &d.tabs[0];
    out.push(("building blocks: first stacks", format!("{} {} {}", bb.items.len() > 400, name(d.entries[bb.items[0] as usize].item as i32), name(bb.icon as i32)), "true oak_log bricks"));
    let op_tab = d.tabs.iter().position(|t| t.flags & 8 != 0).unwrap_or(0);
    out.push(("operator tab only for operators", format!("{} {}", d.visible(op_tab, false), d.visible(op_tab, true)), "false true"));
    // Search: by name, by id, by tag; operator stacks only for operators.
    let (r, _) = d.search("diamond pick", false, item_names);
    out.push(("search \"diamond pick\"", r.iter().map(|&e| name(d.entries[e as usize].item as i32)).collect::<Vec<_>>().join(","), "diamond_pickaxe"));
    let (r, _) = d.search("minecraft:netherite_ing", false, item_names);
    out.push(("search an id", r.iter().map(|&e| name(d.entries[e as usize].item as i32)).collect::<Vec<_>>().join(","), "netherite_ingot"));
    let (r, tags) = d.search("#minecraft:wool_carpets", false, item_names);
    out.push(("search #wool_carpets", format!("{} {}", r.len(), tags.len()), "16 1"));
    let (all, _) = d.search("", false, item_names);
    let (all_op, _) = d.search("", true, item_names);
    out.push(("search: operator stacks hidden", format!("{}", all_op.len() > all.len()), "true"));
    // Component patches: a potion by its static id, an enchanted book by the server's order.
    let regs = vec![(String::from("minecraft:enchantment"), vec![String::from("minecraft:aqua_affinity"), String::from("minecraft:sharpness")])];
    let potion = d.entries.iter().position(|e| name(e.item as i32) == "potion" && e.lines.first().map_or(false, |l| l == "Potion of Swiftness"));
    let book = d.entries.iter().position(|e| name(e.item as i32) == "enchanted_book" && e.lines.get(1).map_or(false, |l| l == "Sharpness II"));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join("");
    if let (Some(p), Some(b)) = (potion, book) {
        out.push(("creative potion patch", hex(&d.patch(p, &regs)), "01003505010d000000"));
        out.push(("creative enchanted book patch", hex(&d.patch(b, &regs)), "01002d03010102"));
    }
    // Clicks: pick a grid stack, put it in the hotbar, shift-click picks a full stack, the
    // hotbar's shift-click deletes, right click splits, destroy slot.
    let stone = item_names.iter().position(|n| n == "stone").unwrap_or(1) as i32;
    let pick = item_names.iter().position(|n| n == "diamond_pickaxe").unwrap_or(2) as i32;
    let st = |item: i32, count: i32| Stack { item, count, patch: vec![0, 0] };
    let mut inv = Inv { slots: vec![None; 46], carried: None, drops: Vec::new() };
    inv.click(&d, false, Hit::Grid(0), Some(st(stone, 1)), Input::Pickup(0));
    inv.click(&d, false, Hit::Grid(0), Some(st(stone, 1)), Input::Pickup(0));
    inv.click(&d, false, Hit::Menu(36), None, Input::Pickup(0));
    let a = format!("{:?}", inv.slots[36].as_ref().map(|s| s.count));
    inv.click(&d, false, Hit::Grid(1), Some(st(stone, 1)), Input::QuickMove(0));
    let b = format!("{:?}", inv.carried.as_ref().map(|s| s.count));
    inv.click(&d, false, Hit::Menu(37), None, Input::Pickup(1));
    inv.click(&d, false, Hit::Grid(5), Some(st(pick, 1)), Input::Pickup(0));
    let c = format!("{:?} {:?}", inv.slots[37].as_ref().map(|s| s.count), inv.carried.as_ref().map(|s| s.count));
    inv.click(&d, false, Hit::Menu(36), None, Input::QuickMove(0));
    inv.click(&d, false, Hit::Grid(3), Some(st(pick, 1)), Input::Swap(4));
    let e = format!("{} {:?}", inv.slots[36].is_none(), inv.slots[40].as_ref().map(|s| s.count));
    out.push(("creative clicks", format!("{} {} {} {}", a, b, c, e), "Some(2) Some(64) Some(1) None true Some(1)"));
    // The inventory tab: a stack shift-clicked from the hotbar goes to the main rows, a
    // chestplate to its slot; Q throws one; a click outside throws the cursor's stack.
    let chest = item_names.iter().position(|n| n == "diamond_chestplate").unwrap_or(3) as i32;
    inv.slots[38] = Some(st(chest, 1));
    inv.click(&d, true, Hit::Menu(37), None, Input::QuickMove(0));
    inv.click(&d, true, Hit::Menu(38), None, Input::QuickMove(0));
    let f = format!("{:?} {:?}", inv.slots[9].as_ref().map(|s| s.count), inv.slots[6].as_ref().map(|s| s.item == chest));
    inv.click(&d, true, Hit::Menu(9), None, Input::Throw(false));
    inv.click(&d, true, Hit::Menu(6), None, Input::Pickup(0));
    inv.click(&d, true, Hit::Outside, None, Input::Pickup(0));
    let g = format!("{} {:?} {}", inv.drops.len(), inv.slots[9].as_ref().map(|s| s.count), inv.carried.is_none());
    inv.click(&d, true, Hit::Destroy, None, Input::QuickMove(0));
    let h = format!("{}", inv.slots.iter().all(|s| s.is_none()));
    out.push(("creative inventory tab", format!("{} {} {}", f, g, h), "Some(1) Some(true) 2 None true true"));
    // A right click outside: the item tabs throw one, the inventory tab the whole stack; a
    // number key swaps the hotbar with the offhand (F is button 40).
    let mut inv = Inv { slots: vec![None; 46], carried: Some(st(stone, 5)), drops: Vec::new() };
    inv.click(&d, false, Hit::Outside, None, Input::Pickup(1));
    inv.click(&d, true, Hit::Outside, None, Input::Pickup(1));
    inv.slots[36] = Some(st(stone, 3));
    inv.click(&d, true, Hit::Menu(36), None, Input::Swap(40));
    let drops: Vec<i32> = inv.drops.iter().map(|s| s.count).collect();
    out.push(("creative right click outside, offhand swap", format!("{:?} {} {:?}", drops, inv.carried.is_none(), inv.slots[45].as_ref().map(|s| s.count)), "[1, 4] true Some(3)"));
    out
}
