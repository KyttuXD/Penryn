//! Text components as vanilla 26.3 sends them (network NBT): literal, translatable (with the
//! game's translations and their arguments), keybind, score, selector, nbt and object
//! contents, every style (colours named or hex, bold, italic, underlined, strikethrough,
//! obfuscated, font, insertion, shadow colour) inherited as `Style.applyTo` does, and the hover
//! and click events. A component becomes `Rich`: runs of text with one style each, which the
//! GUI font draws through formatting codes (`legacy`), and which the chat screen hit-tests for
//! the events under the mouse.

use crate::wire::In;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

// ------------------------------------------------------------------ NBT

/// A network NBT tag, read whole (chat components, registry entries).
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bytes(Vec<u8>),
    Str(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    Ints(Vec<i32>),
    Longs(Vec<i64>),
}

fn mstr(r: &mut In) -> Option<String> {
    let n = r.u16()? as usize;
    Some(String::from_utf8_lossy(r.take(n)?).into_owned())
}

fn payload(r: &mut In, t: u8, depth: u32) -> Option<Tag> {
    if depth > 64 {
        return None;
    }
    Some(match t {
        1 => Tag::Byte(r.u8()? as i8),
        2 => Tag::Short(r.u16()? as i16),
        3 => Tag::Int(r.i32()?),
        4 => Tag::Long(r.i64()?),
        5 => Tag::Float(r.f32()?),
        6 => Tag::Double(r.f64()?),
        7 => {
            let n = r.i32()?.max(0) as usize;
            Tag::Bytes(r.take(n)?.to_vec())
        }
        8 => Tag::Str(mstr(r)?),
        9 => {
            let et = r.u8()?;
            let n = r.i32()?.max(0) as usize;
            if n > r.left() {
                return None;
            }
            let mut v = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                v.push(payload(r, et, depth + 1)?);
            }
            Tag::List(v)
        }
        10 => {
            let mut v = Vec::new();
            loop {
                let ct = r.u8()?;
                if ct == 0 {
                    break;
                }
                let name = mstr(r)?;
                v.push((name, payload(r, ct, depth + 1)?));
            }
            Tag::Compound(v)
        }
        11 => {
            let n = r.i32()?.max(0) as usize;
            if n * 4 > r.left() {
                return None;
            }
            Tag::Ints((0..n).map(|_| r.i32().unwrap_or(0)).collect())
        }
        12 => {
            let n = r.i32()?.max(0) as usize;
            if n * 8 > r.left() {
                return None;
            }
            Tag::Longs((0..n).map(|_| r.i64().unwrap_or(0)).collect())
        }
        _ => return None,
    })
}

/// A nameless network tag (its type byte, then the payload), as packets carry components.
pub fn read_tag(r: &mut In) -> Option<Tag> {
    let t = r.u8()?;
    payload(r, t, 0)
}

impl Tag {
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(v) => v.iter().find(|(k, _)| k == key).map(|(_, t)| t),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Tag::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Tag::Byte(v) => Some(*v as i64),
            Tag::Short(v) => Some(*v as i64),
            Tag::Int(v) => Some(*v as i64),
            Tag::Long(v) => Some(*v),
            Tag::Float(v) => Some(*v as i64),
            Tag::Double(v) => Some(*v as i64),
            _ => None,
        }
    }
    /// A boolean as NBT keeps it (a byte), or "true"/"false".
    pub fn truth(&self) -> Option<bool> {
        match self {
            Tag::Str(s) => Some(s == "true"),
            t => t.int().map(|v| v != 0),
        }
    }
    pub fn list(&self) -> &[Tag] {
        match self {
            Tag::List(v) => v,
            _ => &[],
        }
    }
    /// A primitive's text, as `TranslatableContents` shows a non-component argument.
    fn plain(&self) -> Option<String> {
        Some(match self {
            Tag::Str(s) => s.clone(),
            Tag::Byte(v) => alloc::format!("{}", v),
            Tag::Short(v) => alloc::format!("{}", v),
            Tag::Int(v) => alloc::format!("{}", v),
            Tag::Long(v) => alloc::format!("{}", v),
            Tag::Float(v) => alloc::format!("{}", v),
            Tag::Double(v) => alloc::format!("{}", v),
            _ => return None,
        })
    }
}

// ------------------------------------------------------------------ styles and events

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINED: u8 = 4;
pub const STRIKETHROUGH: u8 = 8;
pub const OBFUSCATED: u8 = 16;

#[derive(Clone, Debug, PartialEq)]
pub enum Click {
    OpenUrl(String),
    OpenFile(String),
    RunCommand(String),
    SuggestCommand(String),
    CopyToClipboard(String),
    ChangePage(i32),
    /// show_dialog and custom: nothing a chat line can do here.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hover {
    Text(Box<Rich>),
    /// An item stack: its id, count and custom name if the components carry one.
    Item { id: String, count: i32, name: Option<Box<Rich>> },
    /// An entity: its type id, its UUID (as text) and its name.
    Entity { kind: String, uuid: String, name: Option<Box<Rich>> },
}

/// What a run's mouse events are (Style's clickEvent, hoverEvent and insertion).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Events {
    pub click: Option<Click>,
    pub hover: Option<Hover>,
    pub insertion: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Style {
    /// 0xRRGGBB; None: the parent's (or the drawing colour).
    pub color: Option<u32>,
    pub flags: u8,
    /// Which flags this style sets (true or false), for inheritance.
    pub set: u8,
    pub shadow: Option<u32>,
    /// 0 default, else a font other than minecraft:default (drawn with the default glyphs:
    /// the uniform and alt fonts are not in gui.bin).
    pub font: u8,
    /// 1-based index into `Rich::events`; 0 none.
    pub events: u16,
}

impl Style {
    /// `Style.applyTo(parent)`: what this style leaves unset comes from the parent.
    fn under(self, parent: &Style) -> Style {
        Style {
            color: self.color.or(parent.color),
            flags: (self.flags & self.set) | (parent.flags & !self.set),
            set: self.set | parent.set,
            shadow: self.shadow.or(parent.shadow),
            font: if self.font != 0 { self.font } else { parent.font },
            events: if self.events != 0 { self.events } else { parent.events },
        }
    }
}

/// The 16 named colours (ChatFormatting), in code order.
pub const NAMES: [&str; 16] = [
    "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green",
    "aqua", "red", "light_purple", "yellow", "white",
];
pub const CODES: [u32; 16] = [
    0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55, 0x55FFFF,
    0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
];

/// TextColor.parseColor: a name or #RRGGBB.
pub fn parse_color(s: &str) -> Option<u32> {
    if let Some(i) = NAMES.iter().position(|n| *n == s) {
        return Some(CODES[i]);
    }
    let h = s.strip_prefix('#')?;
    if h.len() == 6 {
        return u32::from_str_radix(h, 16).ok();
    }
    None
}

// ------------------------------------------------------------------ rich text

/// Runs of text, each with its style, and the events they point at.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rich {
    pub runs: Vec<(Style, String)>,
    pub events: Vec<Events>,
}

impl Rich {
    pub fn plain(s: &str) -> Rich {
        let mut r = Rich::default();
        r.push(Style::default(), s);
        r
    }
    pub fn styled(s: &str, color: u32, flags: u8) -> Rich {
        let mut r = Rich::default();
        r.push(Style { color: Some(color), flags, set: flags, ..Style::default() }, s);
        r
    }
    pub fn push(&mut self, st: Style, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut() {
            if last.0 == st {
                last.1.push_str(s);
                return;
            }
        }
        self.runs.push((st, String::from(s)));
    }
    /// Appends another's runs (their events re-numbered), under `parent`.
    pub fn append(&mut self, o: &Rich, parent: &Style) {
        let base = self.events.len() as u16;
        self.events.extend(o.events.iter().cloned());
        for (st, s) in &o.runs {
            let mut st = *st;
            if st.events != 0 {
                st.events += base;
            }
            self.push(st.under(parent), s);
        }
    }
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.1.is_empty())
    }
    /// Component.getString: the text without styles.
    pub fn string(&self) -> String {
        let mut s = String::new();
        for r in &self.runs {
            s.push_str(&r.1);
        }
        s
    }
    pub fn events_of(&self, st: &Style) -> Option<&Events> {
        if st.events == 0 {
            None
        } else {
            self.events.get(st.events as usize - 1)
        }
    }
    /// The text with formatting codes for the GUI font: each run starts from the drawing
    /// colour ("§r") with its own colour and formats.
    pub fn legacy(&self) -> String {
        let mut out = String::new();
        for (st, s) in &self.runs {
            push_codes(&mut out, st);
            out.push_str(s);
        }
        out
    }
    /// Reads formatting codes back into runs (chat lines given as text).
    pub fn from_legacy(s: &str) -> Rich {
        let mut r = Rich::default();
        let mut st = Style::default();
        let mut cur = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\u{a7}' {
                cur.push(c);
                continue;
            }
            let Some(d) = it.next() else { break };
            r.push(st, &core::mem::take(&mut cur));
            let d = d.to_ascii_lowercase();
            if d == '#' {
                let hex: String = (0..6).filter_map(|_| it.next()).collect();
                st = Style { color: u32::from_str_radix(&hex, 16).ok(), ..Style::default() };
            } else if let Some(v) = d.to_digit(16) {
                st = Style { color: Some(CODES[v as usize]), ..Style::default() };
            } else {
                let f = match d {
                    'k' => OBFUSCATED,
                    'l' => BOLD,
                    'm' => STRIKETHROUGH,
                    'n' => UNDERLINED,
                    'o' => ITALIC,
                    'r' => {
                        st = Style::default();
                        0
                    }
                    _ => 0,
                };
                st.flags |= f;
                st.set |= f;
            }
        }
        r.push(st, &cur);
        r
    }
}

pub fn push_codes(out: &mut String, st: &Style) {
    out.push_str("\u{a7}r");
    if let Some(c) = st.color {
        if let Some(i) = CODES.iter().position(|v| *v == c) {
            out.push('\u{a7}');
            out.push(char::from_digit(i as u32, 16).unwrap());
        } else {
            out.push_str(&alloc::format!("\u{a7}#{:06x}", c & 0xFFFFFF));
        }
    }
    for (f, ch) in [(OBFUSCATED, 'k'), (BOLD, 'l'), (STRIKETHROUGH, 'm'), (UNDERLINED, 'n'), (ITALIC, 'o')] {
        if st.flags & f != 0 {
            out.push('\u{a7}');
            out.push(ch);
        }
    }
}

// ------------------------------------------------------------------ components

/// The style keys of a component compound.
fn style_of(t: &Tag, events: &mut Vec<Events>, depth: u32) -> Style {
    let mut st = Style::default();
    if let Some(c) = t.get("color").and_then(|c| c.str()).and_then(parse_color) {
        st.color = Some(c);
    }
    for (key, f) in [("bold", BOLD), ("italic", ITALIC), ("underlined", UNDERLINED), ("strikethrough", STRIKETHROUGH), ("obfuscated", OBFUSCATED)] {
        if let Some(v) = t.get(key).and_then(|v| v.truth()) {
            st.set |= f;
            if v {
                st.flags |= f;
            }
        }
    }
    if let Some(v) = t.get("shadow_color") {
        st.shadow = match v {
            Tag::List(l) if l.len() == 4 => {
                let c = |i: usize| match &l[i] {
                    Tag::Float(f) => (f.clamp(0.0, 1.0) * 255.0) as u32,
                    _ => 0,
                };
                Some(c(3) << 24 | c(0) << 16 | c(1) << 8 | c(2))
            }
            v => v.int().map(|i| i as u32),
        };
    }
    if let Some(f) = t.get("font").and_then(|f| f.str()) {
        st.font = if f == "minecraft:default" || f == "default" { 0 } else { 1 };
    }
    let mut ev = Events::default();
    if let Some(c) = t.get("click_event").or_else(|| t.get("clickEvent")) {
        ev.click = click_of(c);
    }
    if let Some(h) = t.get("hover_event").or_else(|| t.get("hoverEvent")) {
        ev.hover = hover_of(h, depth);
    }
    if let Some(i) = t.get("insertion").and_then(|i| i.str()) {
        ev.insertion = Some(String::from(i));
    }
    if ev != Events::default() {
        events.push(ev);
        st.events = events.len() as u16;
    }
    st
}

fn click_of(c: &Tag) -> Option<Click> {
    let action = c.get("action")?.str()?;
    // 26.3 names each action's field; older servers send "value".
    let s = |k: &str| c.get(k).or_else(|| c.get("value")).and_then(|v| v.str()).map(String::from);
    Some(match action {
        "open_url" => Click::OpenUrl(s("url")?),
        "open_file" => Click::OpenFile(s("path")?),
        "run_command" => Click::RunCommand(s("command")?),
        "suggest_command" => Click::SuggestCommand(s("command")?),
        "copy_to_clipboard" => Click::CopyToClipboard(s("value")?),
        "change_page" => Click::ChangePage(c.get("page").or_else(|| c.get("value")).and_then(|v| v.int()).unwrap_or(1) as i32),
        _ => Click::Other,
    })
}

fn hover_of(h: &Tag, depth: u32) -> Option<Hover> {
    let action = h.get("action")?.str()?;
    match action {
        "show_text" => {
            let v = h.get("value").or_else(|| h.get("contents"))?;
            Some(Hover::Text(Box::new(component_at(v, depth + 1))))
        }
        "show_item" => {
            let src = h.get("contents").unwrap_or(h);
            let id = src.get("id").and_then(|v| v.str()).unwrap_or("minecraft:air");
            let count = src.get("count").and_then(|v| v.int()).unwrap_or(1) as i32;
            let name = src
                .get("components")
                .and_then(|c| c.get("minecraft:custom_name").or_else(|| c.get("minecraft:item_name")))
                .map(|n| Box::new(component_at(n, depth + 1)));
            Some(Hover::Item { id: String::from(id), count, name })
        }
        "show_entity" => {
            let src = h.get("contents").unwrap_or(h);
            let kind = src.get("id").or_else(|| src.get("type")).and_then(|v| v.str()).unwrap_or("minecraft:pig");
            let uuid = match src.get("uuid") {
                Some(Tag::Ints(v)) if v.len() == 4 => {
                    let mut b = [0u8; 16];
                    for (i, x) in v.iter().enumerate() {
                        b[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
                    }
                    crate::wire::uuid_text(&b)
                }
                Some(Tag::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let name = src.get("name").map(|n| Box::new(component_at(n, depth + 1)));
            Some(Hover::Entity { kind: String::from(kind), uuid, name })
        }
        _ => None,
    }
}

/// A component tag as text runs.
pub fn component(t: &Tag) -> Rich {
    component_at(t, 0)
}

fn component_at(t: &Tag, depth: u32) -> Rich {
    let mut out = Rich::default();
    build(t, &Style::default(), &mut out, depth);
    out
}

/// Reads a component from a packet.
pub fn read(r: &mut In) -> Rich {
    match read_tag(r) {
        Some(t) => component(&t),
        None => Rich::default(),
    }
}

/// The text of a key bound by default to `name` (KeybindContents), as the controls screen
/// names it.
pub fn keybind(name: &str) -> String {
    const DEFAULTS: [(&str, &str); 26] = [
        ("key.jump", "key.keyboard.space"),
        ("key.sneak", "key.keyboard.left.shift"),
        ("key.sprint", "key.keyboard.left.control"),
        ("key.left", "key.keyboard.a"),
        ("key.right", "key.keyboard.d"),
        ("key.back", "key.keyboard.s"),
        ("key.forward", "key.keyboard.w"),
        ("key.attack", "key.mouse.left"),
        ("key.pickItem", "key.mouse.middle"),
        ("key.use", "key.mouse.right"),
        ("key.drop", "key.keyboard.q"),
        ("key.inventory", "key.keyboard.e"),
        ("key.chat", "key.keyboard.t"),
        ("key.playerlist", "key.keyboard.tab"),
        ("key.command", "key.keyboard.slash"),
        ("key.socialInteractions", "key.keyboard.p"),
        ("key.screenshot", "key.keyboard.f2"),
        ("key.togglePerspective", "key.keyboard.f5"),
        ("key.fullscreen", "key.keyboard.f11"),
        ("key.swapOffhand", "key.keyboard.f"),
        ("key.saveToolbarActivator", "key.keyboard.c"),
        ("key.loadToolbarActivator", "key.keyboard.x"),
        ("key.advancements", "key.keyboard.l"),
        ("key.quickActions", "key.keyboard.g"),
        ("key.spectatorOutlines", "key.keyboard.unknown"),
        ("key.smoothCamera", "key.keyboard.unknown"),
    ];
    let key = if let Some(n) = name.strip_prefix("key.hotbar.") {
        alloc::format!("key.keyboard.{}", n)
    } else if let Some((_, k)) = DEFAULTS.iter().find(|(n, _)| *n == name) {
        String::from(*k)
    } else {
        return crate::lang::get(name);
    };
    if crate::lang::has(&key) {
        return crate::lang::get(&key);
    }
    // InputConstants: a printable key's name is its character, upper case.
    let last = key.rsplit('.').next().unwrap_or("");
    if last.chars().count() == 1 {
        return last.to_ascii_uppercase();
    }
    crate::lang::get(&key)
}

fn build(t: &Tag, parent: &Style, out: &mut Rich, depth: u32) {
    if depth > 24 {
        return;
    }
    match t {
        Tag::Compound(entries) => {
            // A heterogeneous list's element wrapped as {"": value}.
            if entries.len() == 1 && entries[0].0.is_empty() {
                build(&entries[0].1, parent, out, depth + 1);
                return;
            }
            let own = style_of(t, &mut out.events, depth);
            let st = own.under(parent);
            if let Some(s) = t.get("text").and_then(|v| v.plain()) {
                out.push(st, &s);
            } else if let Some(key) = t.get("translate").and_then(|v| v.str()) {
                let pat = if crate::lang::has(key) {
                    crate::lang::get(key)
                } else if let Some(f) = t.get("fallback").and_then(|v| v.str()) {
                    String::from(f)
                } else {
                    String::from(key)
                };
                let args: &[Tag] = t.get("with").map(|w| w.list()).unwrap_or(&[]);
                translate(&pat, args, &st, out, depth);
            } else if let Some(k) = t.get("keybind").and_then(|v| v.str()) {
                out.push(st, &keybind(k));
            } else if let Some(s) = t.get("score") {
                // Resolved by the server (ComponentUtils.updateForEntity); an unresolved
                // score shows nothing, an old one its "value".
                if let Some(v) = s.get("value").and_then(|v| v.plain()) {
                    out.push(st, &v);
                }
            } else if t.get("selector").is_some() || t.get("nbt").is_some() {
                // Unresolved entity selectors and NBT paths show nothing on the client.
            } else if t.get("object").is_some() || t.get("sprite").is_some() || t.get("player").is_some() {
                // Object contents (sprites, heads): their fallback text.
                if let Some(f) = t.get("fallback") {
                    build(f, &st, out, depth + 1);
                }
            }
            for e in t.get("extra").map(|e| e.list()).unwrap_or(&[]) {
                build(e, &st, out, depth + 1);
            }
        }
        Tag::List(items) => {
            // ComponentSerialization: the first element, the others appended to it.
            if let Some((first, rest)) = items.split_first() {
                let mut head = Rich::default();
                build(first, &Style::default(), &mut head, depth + 1);
                let base = head.runs.first().map(|r| r.0).unwrap_or_default();
                out.append(&head, parent);
                let inherit = Style { events: 0, ..base }.under(parent);
                for e in rest {
                    build(e, &inherit, out, depth + 1);
                }
            }
        }
        other => {
            if let Some(s) = other.plain() {
                out.push(*parent, &s);
            }
        }
    }
}

/// TranslatableContents.decomposeTemplate: the pattern's text in the component's style, each
/// %s / %n$s an argument (a component, or a primitive's text), %% a percent sign.
fn translate(pat: &str, args: &[Tag], st: &Style, out: &mut Rich, depth: u32) {
    let b: Vec<char> = pat.chars().collect();
    let mut lit = String::new();
    let mut next = 0usize;
    let mut i = 0;
    let arg = |out: &mut Rich, lit: &mut String, n: usize| {
        out.push(*st, &core::mem::take(lit));
        if let Some(a) = args.get(n) {
            build(a, st, out, depth + 1);
        }
    };
    while i < b.len() {
        if b[i] == '%' && i + 1 < b.len() {
            if b[i + 1] == '%' {
                lit.push('%');
                i += 2;
                continue;
            }
            if b[i + 1] == 's' {
                arg(out, &mut lit, next);
                next += 1;
                i += 2;
                continue;
            }
            let mut j = i + 1;
            let mut n = 0usize;
            while j < b.len() && b[j].is_ascii_digit() {
                n = n * 10 + b[j] as usize - '0' as usize;
                j += 1;
            }
            if j + 1 < b.len() && j > i + 1 && b[j] == '$' && b[j + 1] == 's' {
                arg(out, &mut lit, n.saturating_sub(1));
                i = j + 2;
                continue;
            }
        }
        lit.push(b[i]);
        i += 1;
    }
    out.push(*st, &lit);
}

/// A minimal JSON reader for components given as JSON text (login disconnects, old servers):
/// it builds the same tags as NBT.
pub fn json_tag(s: &str) -> Option<Tag> {
    let b = s.as_bytes();
    let mut p = 0;
    let t = json_value(b, &mut p, 0)?;
    Some(t)
}

fn ws(b: &[u8], p: &mut usize) {
    while *p < b.len() && (b[*p] as char).is_ascii_whitespace() {
        *p += 1;
    }
}

fn json_string(b: &[u8], p: &mut usize) -> Option<String> {
    if b.get(*p) != Some(&b'"') {
        return None;
    }
    *p += 1;
    let mut out: Vec<u8> = Vec::new();
    while *p < b.len() {
        let c = b[*p];
        *p += 1;
        match c {
            b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
            b'\\' => {
                let e = *b.get(*p)?;
                *p += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' | b'f' => {}
                    b'u' => {
                        let hex = core::str::from_utf8(b.get(*p..*p + 4)?).ok()?;
                        *p += 4;
                        let mut cp = u32::from_str_radix(hex, 16).ok()?;
                        if (0xD800..0xDC00).contains(&cp) && b.get(*p) == Some(&b'\\') && b.get(*p + 1) == Some(&b'u') {
                            let lo = u32::from_str_radix(core::str::from_utf8(b.get(*p + 2..*p + 6)?).ok()?, 16).ok()?;
                            *p += 6;
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                        }
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(char::from_u32(cp).unwrap_or('?').encode_utf8(&mut buf).as_bytes());
                    }
                    other => out.push(other),
                }
            }
            _ => out.push(c),
        }
    }
    None
}

fn json_value(b: &[u8], p: &mut usize, depth: u32) -> Option<Tag> {
    if depth > 64 {
        return None;
    }
    ws(b, p);
    match *b.get(*p)? {
        b'"' => json_string(b, p).map(Tag::Str),
        b'{' => {
            *p += 1;
            let mut v = Vec::new();
            loop {
                ws(b, p);
                if b.get(*p) == Some(&b'}') {
                    *p += 1;
                    break;
                }
                let k = json_string(b, p)?;
                ws(b, p);
                if b.get(*p) != Some(&b':') {
                    return None;
                }
                *p += 1;
                let val = json_value(b, p, depth + 1)?;
                v.push((k, val));
                ws(b, p);
                match b.get(*p) {
                    Some(b',') => *p += 1,
                    Some(b'}') => {
                        *p += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            Some(Tag::Compound(v))
        }
        b'[' => {
            *p += 1;
            let mut v = Vec::new();
            loop {
                ws(b, p);
                if b.get(*p) == Some(&b']') {
                    *p += 1;
                    break;
                }
                v.push(json_value(b, p, depth + 1)?);
                ws(b, p);
                match b.get(*p) {
                    Some(b',') => *p += 1,
                    Some(b']') => {
                        *p += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            Some(Tag::List(v))
        }
        b't' if b[*p..].starts_with(b"true") => {
            *p += 4;
            Some(Tag::Byte(1))
        }
        b'f' if b[*p..].starts_with(b"false") => {
            *p += 5;
            Some(Tag::Byte(0))
        }
        b'n' if b[*p..].starts_with(b"null") => {
            *p += 4;
            Some(Tag::Str(String::new()))
        }
        _ => {
            let start = *p;
            while *p < b.len() && matches!(b[*p], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *p += 1;
            }
            let s = core::str::from_utf8(&b[start..*p]).ok()?;
            if s.is_empty() {
                return None;
            }
            if s.contains('.') || s.contains('e') || s.contains('E') {
                Some(Tag::Double(crate::text::parse_f64(s)?))
            } else {
                s.parse::<i64>().ok().map(|v| if v as i32 as i64 == v { Tag::Int(v as i32) } else { Tag::Long(v) })
            }
        }
    }
}

/// A decimal number (no exponent needed by chat): digits, a point, digits.
pub fn parse_f64(s: &str) -> Option<f64> {
    s.parse::<f64>().ok()
}

// ------------------------------------------------------------------ layout

/// One wrapped line: its runs (event indices still into the message's `Rich`).
pub type Line = Vec<(Style, String)>;

/// ComponentRenderUtils.wrapComponents through StringSplitter: lines at most `width` units,
/// broken after the last space that fits (or mid-word when none does), newlines kept.
pub fn wrap(gui: &crate::gui::Gui, r: &Rich, width: f32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut line: Line = Vec::new();
    let mut w = 0.0f32;
    // Where the last space of the line is: (run index, byte index after it, width there).
    let mut brk: Option<(usize, usize, f32)> = None;
    let mut buf = [0u8; 4];
    for (st, s) in &r.runs {
        let bold = st.flags & BOLD != 0;
        for ch in s.chars() {
            if ch == '\n' {
                lines.push(core::mem::take(&mut line));
                w = 0.0;
                brk = None;
                continue;
            }
            let cw = gui.text_width(ch.encode_utf8(&mut buf)) + bold as i32 as f32;
            if w + cw > width && !(line.is_empty() && ch == ' ') && w > 0.0 {
                // Break at the last space (dropping it), or here.
                let mut rest: Line = Vec::new();
                if let Some((ri, bi, _)) = brk {
                    if ri < line.len() {
                        let tail = line.split_off(ri);
                        let mut first = true;
                        for (rst, rs) in tail {
                            if first {
                                let (a, b) = rs.split_at(bi.min(rs.len()));
                                let a = String::from(a.trim_end_matches(' '));
                                if !a.is_empty() {
                                    line.push((rst, a));
                                }
                                if !b.is_empty() {
                                    rest.push((rst, String::from(b)));
                                }
                                first = false;
                            } else {
                                rest.push((rst, rs));
                            }
                        }
                    }
                }
                lines.push(core::mem::take(&mut line));
                line = rest;
                w = line.iter().map(|(s2, t)| width_of(gui, t, s2)).sum();
                brk = None;
                if ch == ' ' && line.is_empty() {
                    continue;
                }
            }
            match line.last_mut() {
                Some(last) if last.0 == *st => last.1.push(ch),
                _ => line.push((*st, String::from(ch))),
            }
            w += cw;
            if ch == ' ' {
                let ri = line.len() - 1;
                brk = Some((ri, line[ri].1.len(), w));
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

pub fn width_of(gui: &crate::gui::Gui, s: &str, st: &Style) -> f32 {
    let w = gui.text_width(s);
    if st.flags & BOLD != 0 {
        w + s.chars().count() as f32
    } else {
        w
    }
}

pub fn line_legacy(l: &Line) -> String {
    let mut out = String::new();
    for (st, s) in l {
        push_codes(&mut out, st);
        out.push_str(s);
    }
    out
}

/// The run of a line under x (in units from the line's start).
pub fn run_at<'a>(gui: &crate::gui::Gui, l: &'a Line, x: f32) -> Option<&'a Style> {
    if x < 0.0 {
        return None;
    }
    let mut cx = 0.0;
    for (st, s) in l {
        let w = width_of(gui, s, st);
        if x < cx + w {
            return Some(st);
        }
        cx += w;
    }
    None
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let t = json_tag(r##"{"text":"a","color":"red","bold":true,"extra":[{"text":"b","italic":true,"color":"#123456"},"c"]}"##).unwrap();
    let r = component(&t);
    out.push(("component styles", r.legacy(), "\u{a7}r\u{a7}c\u{a7}la\u{a7}r\u{a7}#123456\u{a7}l\u{a7}ob\u{a7}r\u{a7}c\u{a7}lc"));
    let t = json_tag(r#"{"translate":"x.%s.y","fallback":"<%2$s> %1$s %%","with":["one",{"text":"two","color":"aqua"}]}"#).unwrap();
    out.push(("translatable fallback with args", component(&t).legacy(), "\u{a7}r<\u{a7}r\u{a7}btwo\u{a7}r> one %"));
    let t = json_tag(r#"{"text":"link","click_event":{"action":"open_url","url":"https://example.com"},"hover_event":{"action":"show_text","value":"hi"}}"#).unwrap();
    let r = component(&t);
    let ev = r.events_of(&r.runs[0].0).cloned().unwrap_or_default();
    out.push(("click and hover events", alloc::format!("{:?} {}", ev.click, matches!(ev.hover, Some(Hover::Text(_)))), "Some(OpenUrl(\"https://example.com\")) true"));
    let back = Rich::from_legacy("\u{a7}cred \u{a7}lbold\u{a7}r plain");
    out.push(("formatting codes read back", back.legacy(), "\u{a7}r\u{a7}cred \u{a7}r\u{a7}c\u{a7}lbold\u{a7}r plain"));
    out
}

/// `Component.translatable(key, args...).withStyle(st)` with arguments already made:
/// the pattern (or the key itself) with each %s / %n$s an argument.
pub fn translate_rich(key: &str, args: &[&Rich], st: Style) -> Rich {
    let pat = crate::lang::get(key);
    let b: Vec<char> = pat.chars().collect();
    let mut out = Rich::default();
    let mut lit = String::new();
    let (mut next, mut i) = (0usize, 0usize);
    while i < b.len() {
        if b[i] == '%' && i + 1 < b.len() {
            let mut n = None;
            let mut j = i + 1;
            if b[j] == '%' {
                lit.push('%');
                i += 2;
                continue;
            }
            if b[j] == 's' {
                n = Some(next);
                next += 1;
                j += 1;
            } else {
                let mut k = 0usize;
                while j < b.len() && b[j].is_ascii_digit() {
                    k = k * 10 + b[j] as usize - '0' as usize;
                    j += 1;
                }
                if j + 1 < b.len() && j > i + 1 && b[j] == '$' && b[j + 1] == 's' {
                    n = Some(k.saturating_sub(1));
                    j += 2;
                }
            }
            if let Some(n) = n {
                out.push(st, &core::mem::take(&mut lit));
                if let Some(a) = args.get(n) {
                    out.append(a, &st);
                }
                i = j;
                continue;
            }
        }
        lit.push(b[i]);
        i += 1;
    }
    out.push(st, &lit);
    out
}
