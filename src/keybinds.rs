//! Vanilla's key binds (KeyMapping and Options.keyMappings in 26.3): every binding with its
//! category, the names options.txt keeps them under (key_key.forward:key.keyboard.w), and the
//! Controls > Key Binds screen (KeyBindsScreen and KeyBindsList): a category header per group,
//! a name, the key's button (red when another bind uses the key, yellow while it waits for a
//! key) and a Reset button; Reset Keys and Done along the footer.

use crate::gui::Gui;
use crate::lang;
use crate::options::{Layout, Menu, Placed, Settings, W};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// KeyMapping.Category, in the order the Key Binds screen lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    Movement,
    Gameplay,
    Inventory,
    Creative,
    Multiplayer,
    Ui,
    Misc,
    Spectator,
}

impl Cat {
    const ALL: [Cat; 8] = [Cat::Movement, Cat::Gameplay, Cat::Inventory, Cat::Creative, Cat::Multiplayer, Cat::Ui, Cat::Misc, Cat::Spectator];

    fn key(self) -> &'static str {
        match self {
            Cat::Movement => "key.categories.movement",
            Cat::Gameplay => "key.categories.gameplay",
            Cat::Inventory => "key.categories.inventory",
            Cat::Creative => "key.categories.creative",
            Cat::Multiplayer => "key.categories.multiplayer",
            Cat::Ui => "key.categories.ui",
            Cat::Misc => "key.categories.misc",
            Cat::Spectator => "key.categories.spectator",
        }
    }
}

/// What a binding is bound to: nothing, a keyboard key (its SDL scancode) or a mouse button
/// (GLFW's numbering: 0 left, 1 right, 2 middle, 3 and up the extra buttons).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Unbound,
    Kb(u16),
    Mouse(u8),
}

pub struct Def {
    pub id: B,
    pub name: &'static str,
    pub cat: Cat,
    pub def: Key,
}

macro_rules! binds {
    ($($id:ident: $name:literal, $cat:ident, $def:expr;)*) => {
        /// Every key binding of 26.3 that Penryn has a use for, in the order of `TABLE`.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum B { $($id),* }
        pub const TABLE: &[Def] = &[$(Def { id: B::$id, name: $name, cat: Cat::$cat, def: $def }),*];
    };
}

binds! {
    Forward: "key.forward", Movement, Key::Kb(26);
    Left: "key.left", Movement, Key::Kb(4);
    Back: "key.back", Movement, Key::Kb(22);
    Right: "key.right", Movement, Key::Kb(7);
    Jump: "key.jump", Movement, Key::Kb(44);
    Sneak: "key.sneak", Movement, Key::Kb(225);
    Sprint: "key.sprint", Movement, Key::Kb(224);
    Attack: "key.attack", Gameplay, Key::Mouse(0);
    Use: "key.use", Gameplay, Key::Mouse(1);
    PickItem: "key.pickItem", Gameplay, Key::Mouse(2);
    Inventory: "key.inventory", Inventory, Key::Kb(8);
    Drop: "key.drop", Inventory, Key::Kb(20);
    SwapOffhand: "key.swapOffhand", Inventory, Key::Kb(9);
    Hotbar1: "key.hotbar.1", Inventory, Key::Kb(30);
    Hotbar2: "key.hotbar.2", Inventory, Key::Kb(31);
    Hotbar3: "key.hotbar.3", Inventory, Key::Kb(32);
    Hotbar4: "key.hotbar.4", Inventory, Key::Kb(33);
    Hotbar5: "key.hotbar.5", Inventory, Key::Kb(34);
    Hotbar6: "key.hotbar.6", Inventory, Key::Kb(35);
    Hotbar7: "key.hotbar.7", Inventory, Key::Kb(36);
    Hotbar8: "key.hotbar.8", Inventory, Key::Kb(37);
    Hotbar9: "key.hotbar.9", Inventory, Key::Kb(38);
    SaveToolbar: "key.saveToolbarActivator", Creative, Key::Kb(6);
    LoadToolbar: "key.loadToolbarActivator", Creative, Key::Kb(27);
    Chat: "key.chat", Multiplayer, Key::Kb(23);
    Command: "key.command", Multiplayer, Key::Kb(56);
    PlayerList: "key.playerlist", Multiplayer, Key::Kb(43);
    Social: "key.socialInteractions", Multiplayer, Key::Kb(19);
    Advancements: "key.advancements", Multiplayer, Key::Kb(15);
    Friends: "key.friends", Multiplayer, Key::Unbound;
    ToggleGui: "key.toggleGui", Ui, Key::Kb(58);
    QuickActions: "key.quickActions", Ui, Key::Unbound;
    Screenshot: "key.screenshot", Misc, Key::Kb(59);
    VoiceTalk: "Push to Talk (Voice Chat)", Misc, Key::Kb(25);
    Perspective: "key.togglePerspective", Misc, Key::Kb(62);
    Fullscreen: "key.fullscreen", Misc, Key::Kb(68);
    SmoothCamera: "key.smoothCamera", Misc, Key::Unbound;
    SpectatorHotbar: "key.spectatorHotbar", Spectator, Key::Unbound;
    SpectatorOutlines: "key.spectatorOutlines", Spectator, Key::Unbound;
    SpectatorShaders: "key.toggleSpectatorShaderEffects", Spectator, Key::Unbound;
}

impl B {
    /// The hotbar binding for slot 0..8.
    pub fn hotbar(slot: usize) -> B {
        const H: [B; 9] = [B::Hotbar1, B::Hotbar2, B::Hotbar3, B::Hotbar4, B::Hotbar5, B::Hotbar6, B::Hotbar7, B::Hotbar8, B::Hotbar9];
        H[slot.min(8)]
    }
    /// The slot a hotbar binding selects.
    pub fn hotbar_slot(self) -> Option<usize> {
        let i = self as usize;
        (i >= B::Hotbar1 as usize && i <= B::Hotbar9 as usize).then(|| i - B::Hotbar1 as usize)
    }
}

/// SDL's mouse buttons (1 left, 2 middle, 3 right, 4 and 5 the extras) as GLFW numbers them.
pub fn glfw_button(sdl: u8) -> u8 {
    match sdl {
        1 => 0,
        2 => 2,
        3 => 1,
        n => n.saturating_sub(1),
    }
}

/// The character a keyboard key types (lower case), by SDL scancode: what a key that opens the
/// chat also sends as text (the game drops that first character).
pub fn kb_char(sc: u16) -> Option<char> {
    Some(match sc {
        4..=29 => (b'a' + (sc - 4) as u8) as char,
        30..=38 => (b'1' + (sc - 30) as u8) as char,
        39 => '0',
        44 => ' ',
        45 => '-',
        46 => '=',
        47 => '[',
        48 => ']',
        49 => '\\',
        51 => ';',
        52 => '\'',
        53 => '`',
        54 => ',',
        55 => '.',
        56 => '/',
        _ => return None,
    })
}

/// The options.txt name of a keyboard key (InputConstants: "key.keyboard.w"), by SDL scancode.
pub fn kb_name(sc: u16) -> Option<String> {
    let s = match sc {
        4..=29 => return Some(format!("key.keyboard.{}", (b'a' + (sc - 4) as u8) as char)),
        30..=38 => return Some(format!("key.keyboard.{}", sc - 29)),
        39 => "key.keyboard.0",
        40 => "key.keyboard.enter",
        41 => "key.keyboard.escape",
        42 => "key.keyboard.backspace",
        43 => "key.keyboard.tab",
        44 => "key.keyboard.space",
        45 => "key.keyboard.minus",
        46 => "key.keyboard.equal",
        47 => "key.keyboard.left.bracket",
        48 => "key.keyboard.right.bracket",
        49 => "key.keyboard.backslash",
        51 => "key.keyboard.semicolon",
        52 => "key.keyboard.apostrophe",
        53 => "key.keyboard.grave.accent",
        54 => "key.keyboard.comma",
        55 => "key.keyboard.period",
        56 => "key.keyboard.slash",
        57 => "key.keyboard.caps.lock",
        58..=69 => return Some(format!("key.keyboard.f{}", sc - 57)),
        70 => "key.keyboard.print.screen",
        71 => "key.keyboard.scroll.lock",
        72 => "key.keyboard.pause",
        73 => "key.keyboard.insert",
        74 => "key.keyboard.home",
        75 => "key.keyboard.page.up",
        76 => "key.keyboard.delete",
        77 => "key.keyboard.end",
        78 => "key.keyboard.page.down",
        79 => "key.keyboard.right",
        80 => "key.keyboard.left",
        81 => "key.keyboard.down",
        82 => "key.keyboard.up",
        83 => "key.keyboard.num.lock",
        84 => "key.keyboard.keypad.divide",
        85 => "key.keyboard.keypad.multiply",
        86 => "key.keyboard.keypad.subtract",
        87 => "key.keyboard.keypad.add",
        88 => "key.keyboard.keypad.enter",
        89..=97 => return Some(format!("key.keyboard.keypad.{}", sc - 88)),
        98 => "key.keyboard.keypad.0",
        99 => "key.keyboard.keypad.decimal",
        101 => "key.keyboard.menu",
        104..=115 => return Some(format!("key.keyboard.f{}", sc - 91)),
        224 => "key.keyboard.left.control",
        225 => "key.keyboard.left.shift",
        226 => "key.keyboard.left.alt",
        227 => "key.keyboard.left.win",
        228 => "key.keyboard.right.control",
        229 => "key.keyboard.right.shift",
        230 => "key.keyboard.right.alt",
        231 => "key.keyboard.right.win",
        _ => return None,
    };
    Some(String::from(s))
}

impl Key {
    /// The name options.txt stores (key.keyboard.unknown when unbound).
    pub fn name(self) -> String {
        match self {
            Key::Unbound => String::from("key.keyboard.unknown"),
            Key::Kb(sc) => kb_name(sc).unwrap_or_else(|| String::from("key.keyboard.unknown")),
            Key::Mouse(b) => match b {
                0 => String::from("key.mouse.left"),
                1 => String::from("key.mouse.right"),
                2 => String::from("key.mouse.middle"),
                n => format!("key.mouse.{}", n as u32 + 1),
            },
        }
    }

    /// The key an options.txt name stands for.
    pub fn parse(s: &str) -> Key {
        let s = s.trim();
        if s == "key.keyboard.unknown" || s.is_empty() {
            return Key::Unbound;
        }
        match s {
            "key.mouse.left" => return Key::Mouse(0),
            "key.mouse.right" => return Key::Mouse(1),
            "key.mouse.middle" => return Key::Mouse(2),
            _ => {}
        }
        if let Some(n) = s.strip_prefix("key.mouse.").and_then(|n| n.parse::<u32>().ok()) {
            return if n >= 4 { Key::Mouse((n - 1) as u8) } else { Key::Unbound };
        }
        (0..512u16).find(|sc| kb_name(*sc).as_deref() == Some(s)).map_or(Key::Unbound, Key::Kb)
    }

    /// The key's name as the Key Binds buttons show it (vanilla's getTranslatedKeyMessage).
    pub fn label(self) -> String {
        let n = self.name();
        if self == Key::Unbound {
            lang::get("key.keyboard.unknown")
        } else if lang::has(&n) {
            lang::get(&n)
        } else {
            // GLFW's key name, upper case for a letter (vanilla's lang has no entry for it).
            let last = n.rsplit('.').next().unwrap_or("");
            if last.len() == 1 { last.to_ascii_uppercase() } else { String::from(last) }
        }
    }
}

/// Every binding's key, in the order of `TABLE`, and the bind the Key Binds screen waits on.
pub struct Keymap {
    keys: Vec<Key>,
    /// The binding (its TABLE index) waiting for a key or a mouse button.
    pub listening: Option<usize>,
}

impl Keymap {
    /// The bindings options.txt sets (key_<name>), else the defaults.
    pub fn load(o: &Settings) -> Keymap {
        let keys = TABLE.iter().map(|d| o.other(&format!("key_{}", d.name)).map_or(d.def, Key::parse)).collect();
        Keymap { keys, listening: None }
    }

    /// Writes every binding into options.txt's lines, as vanilla does when it saves.
    pub fn write(&self, o: &mut Settings) {
        for (d, k) in TABLE.iter().zip(self.keys.iter()) {
            o.set_other(&format!("key_{}", d.name), &k.name());
        }
    }

    pub fn get(&self, b: B) -> Key {
        self.keys[b as usize]
    }

    pub fn at(&self, i: usize) -> Key {
        self.keys[i]
    }

    /// The binding for a keyboard scancode (the first in TABLE order).
    pub fn find_kb(&self, sc: usize) -> Option<B> {
        self.keys.iter().position(|k| *k == Key::Kb(sc as u16)).map(|i| TABLE[i].id)
    }

    /// The binding for a mouse button (GLFW numbering).
    pub fn find_mouse(&self, btn: u8) -> Option<B> {
        self.keys.iter().position(|k| *k == Key::Mouse(btn)).map(|i| TABLE[i].id)
    }

    /// Whether a binding is at its default (its Reset button is off).
    pub fn is_default(&self, i: usize) -> bool {
        self.keys[i] == TABLE[i].def
    }

    /// Whether another binding uses this one's key (the button shows red, as vanilla's
    /// KeyBindsList does with its duplicate keys).
    pub fn conflicts(&self, i: usize) -> bool {
        let k = self.keys[i];
        k != Key::Unbound && self.keys.iter().enumerate().any(|(j, x)| j != i && *x == k)
    }

    pub fn set(&mut self, i: usize, k: Key) {
        self.keys[i] = k;
    }

    pub fn reset(&mut self, i: usize) {
        self.keys[i] = TABLE[i].def;
    }

    pub fn reset_all(&mut self) {
        for (i, d) in TABLE.iter().enumerate() {
            self.keys[i] = d.def;
        }
    }
}

/// A key or mouse button pressed on the Key Binds screen while a binding waits for one. Escape
/// unbinds it (KeyBindsScreen.keyPressed). Returns false when nothing waited.
pub fn bind(m: &mut Menu, key: Key) -> bool {
    let Some(i) = m.keys.listening.take() else { return false };
    m.keys.set(i, key);
    m.keys.write(&mut m.o);
    true
}

const LEFT_INSET: f32 = 180.0;
const BUTTON_W: f32 = 75.0;
const RESET_X: f32 = 260.0;
const RESET_W: f32 = 50.0;

/// The Key Binds screen: its title, the category headers and bindings in one list, and the
/// footer (Reset Keys, Done).
pub fn layout(m: &Menu, gw: f32, gh: f32) -> Layout {
    let (w, h) = (crate::options::ceil(gw), crate::options::ceil(gh));
    let cx = crate::options::fl(w / 2.0);
    let mut l = Layout { widgets: Vec::new(), texts: Vec::new(), list: None, content: 0.0 };
    l.texts.push((lang::get("controls.keybinds.title"), cx, 12.0));
    l.widgets.push(Placed { x: cx - 155.0, y: h - 33.0 + 6.0, w: 150.0, what: W::ResetAll });
    l.widgets.push(Placed { x: cx + 5.0, y: h - 33.0 + 6.0, w: 150.0, what: W::Done });
    let (top, bottom) = (33.0f32, h - 33.0);
    let left = cx - 155.0;
    let mut y = top + 2.0 - m.scroll;
    let mut content = 4.0f32;
    let mut first = true;
    for cat in Cat::ALL {
        let members: Vec<usize> = TABLE.iter().enumerate().filter(|(_, d)| d.cat == cat).map(|(i, _)| i).collect();
        if members.is_empty() {
            continue;
        }
        // A category header (Row::Head's spacing), then its bindings at 25 units a row.
        let (height, pad) = if first { (18.0, 0.0) } else { (36.0, 18.0) };
        let cy = y + 2.0;
        if cy + pad >= top && cy + pad + 9.0 <= bottom {
            l.texts.push((format!("\u{a7}l{}", lang::get(cat.key())), cx, cy + pad + 4.0));
        }
        first = false;
        y += height;
        content += height;
        for i in members {
            let cy = y + 2.0;
            if cy >= top && cy + 20.0 <= bottom {
                l.texts.push((lang::get(TABLE[i].name), left + LEFT_INSET / 2.0, cy + 6.0));
                l.widgets.push(Placed { x: left + LEFT_INSET, y: cy, w: BUTTON_W, what: W::Bind(i) });
                l.widgets.push(Placed { x: left + RESET_X, y: cy, w: RESET_W, what: W::Reset(i) });
            }
            y += 25.0;
            content += 25.0;
        }
    }
    l.list = Some((0.0, top, w, bottom));
    l.content = content;
    l
}

/// A binding's button: its key's name, "> name <" in yellow while it waits, red on a conflict.
pub fn draw_bind(gui: &mut Gui, m: &Menu, i: usize, p: &Placed, hover: bool) {
    let waiting = m.keys.listening == Some(i);
    let name = m.keys.at(i).label();
    let (text, color) = if waiting {
        (format!("> {} <", name), 0xFFFF_FF55)
    } else if m.keys.conflicts(i) {
        (name, 0xFFFF_5555)
    } else {
        (name, 0xFFFF_FFFF)
    };
    gui.blit_nine(if hover { "widget/button_highlighted" } else { "widget/button" }, p.x, p.y, p.w, 20.0);
    let tw = gui.text_width(&text);
    gui.text(&text, p.x + (p.w - tw) / 2.0, p.y + 6.0, color);
}

/// Checks for `penryn selftest`: the names, the defaults and what options.txt round-trips.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut check = |what: &'static str, got: String, want: &'static str| out.push((what, got, want));
    check("key name of W", Key::Kb(26).name(), "key.keyboard.w");
    check("key name of shift", Key::Kb(225).name(), "key.keyboard.left.shift");
    check("key name of left button", Key::Mouse(0).name(), "key.mouse.left");
    check("key name of button 4", Key::Mouse(3).name(), "key.mouse.4");
    check("parse F2", format!("{:?}", Key::parse("key.keyboard.f2")), "Kb(59)");
    check("parse button 5", format!("{:?}", Key::parse("key.mouse.5")), "Mouse(4)");
    check("parse unknown", format!("{:?}", Key::parse("key.keyboard.bogus")), "Unbound");
    let s = Settings::parse("key_key.forward:key.keyboard.i\nkey_key.attack:key.mouse.right\n");
    let k = Keymap::load(&s);
    check("options.txt forward", format!("{:?}", k.get(B::Forward)), "Kb(12)");
    check("options.txt attack", format!("{:?}", k.get(B::Attack)), "Mouse(1)");
    check("default jump", format!("{:?}", k.get(B::Jump)), "Kb(44)");
    check("find E", format!("{:?}", k.find_kb(8)), "Some(Inventory)");
    let mut k = Keymap::load(&Settings::default());
    check("conflict on default", format!("{}", k.conflicts(B::Forward as usize)), "false");
    k.set(B::Left as usize, Key::Kb(26));
    check("conflict shown", format!("{} {}", k.conflicts(B::Left as usize), k.conflicts(B::Forward as usize)), "true true");
    k.reset(B::Left as usize);
    check("reset one", format!("{}", k.is_default(B::Left as usize)), "true");
    let mut o = Settings::default();
    k.set(B::Jump as usize, Key::Mouse(3));
    k.write(&mut o);
    let text = o.text();
    check("written line", format!("{}", text.contains("key_key.jump:key.mouse.4\n")), "true");
    check("read back", format!("{:?}", Keymap::load(&o).get(B::Jump)), "Mouse(3)");
    check("hotbar slot", format!("{:?} {:?}", B::hotbar(4).hotbar_slot(), B::Forward.hotbar_slot()), "Some(4) None");
    check("chat key typed char", format!("{:?} {:?} {:?}", kb_char(28), kb_char(56), kb_char(79)), "Some('y') Some('/') None");
    out
}
