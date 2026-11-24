//! A text field's editing, shared by every box the player types in (chat, the anvil's name, the creative search, the
//! sign and book editors, the multiplayer fields, the name screen): the cursor and a selection (shift with the arrows,
//! Home and End, select all, a click or a drag, a double click for a word), Backspace and Delete by character or word,
//! and the clipboard: copy and cut take the selection (the whole text when nothing is selected, as before), paste
//! replaces it. Pure logic over a `String` and byte offsets on character boundaries, so it is tested without a window.
//!
//! Keys are SDL scancodes. The "command" key is Control, and on macOS also Command (the GUI keys): `command(keys)`.

use alloc::string::String;
use alloc::vec::Vec;

pub const SC_A: usize = 4;
pub const SC_C: usize = 6;
pub const SC_V: usize = 25;
pub const SC_X: usize = 27;
pub const SC_BACKSPACE: usize = 42;
pub const SC_DELETE: usize = 76;
pub const SC_RIGHT: usize = 79;
pub const SC_LEFT: usize = 80;
pub const SC_HOME: usize = 74;
pub const SC_END: usize = 77;
const SC_LCTRL: usize = 224;
const SC_RCTRL: usize = 228;
const SC_LGUI: usize = 227;
const SC_RGUI: usize = 231;
const SC_LSHIFT: usize = 225;
const SC_RSHIFT: usize = 229;

/// Whether the command key is held: Control, and on macOS Command too.
pub fn command(keys: &[bool]) -> bool {
    let k = |i: usize| keys.get(i).copied().unwrap_or(false);
    k(SC_LCTRL) || k(SC_RCTRL) || (cfg!(target_os = "macos") && (k(SC_LGUI) || k(SC_RGUI)))
}

pub fn shift(keys: &[bool]) -> bool {
    let k = |i: usize| keys.get(i).copied().unwrap_or(false);
    k(SC_LSHIFT) || k(SC_RSHIFT)
}

/// What typed or pasted text may hold.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Filter {
    /// SharedConstants.filterText for one line: no control characters (a pasted line break is dropped), no section sign.
    Line,
    /// As Line, but line breaks stay (a book page).
    Multi,
    /// Everything printable, including the section sign (a field that is not sent as chat).
    Free,
}

pub struct Edit {
    pub text: String,
    /// The caret, a byte offset on a character boundary.
    pub cursor: usize,
    /// The other end of the selection (equal to `cursor`: nothing selected).
    pub anchor: usize,
    /// The longest text, in UTF-16 units (what Minecraft counts); usize::MAX for no limit.
    pub max: usize,
    pub filter: Filter,
}

fn floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn prev_char(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(p, _)| p)
}

fn next_char(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(s.len(), |c| i + c.len_utf8())
}

/// StringUtil / EditBox.getWordPosition: the start of the word before `i` (skipping spaces, then the word).
fn word_left(s: &str, mut i: usize) -> usize {
    while i > 0 && s[..i].ends_with(' ') {
        i = prev_char(s, i);
    }
    while i > 0 && !s[..i].ends_with(' ') {
        i = prev_char(s, i);
    }
    i
}

/// The end of the word after `i` (EditBox: the word, then the spaces after it).
fn word_right(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s[i..].starts_with(' ') {
        i = next_char(s, i);
    }
    while i < s.len() && s[i..].starts_with(' ') {
        i = next_char(s, i);
    }
    i
}

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s` cut to at most `room` UTF-16 units, on a character.
fn cut_to(s: &str, room: usize) -> &str {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        n += c.len_utf16();
        if n > room {
            return &s[..i];
        }
    }
    s
}

impl Edit {
    pub fn new(max: usize, filter: Filter) -> Edit {
        Edit { text: String::new(), cursor: 0, anchor: 0, max, filter }
    }

    /// Replaces the text (cut to the limit); the caret goes to the end, nothing selected.
    pub fn set(&mut self, s: &str) {
        self.text = String::from(cut_to(s, self.max));
        self.cursor = self.text.len();
        self.anchor = self.cursor;
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    /// The selection as (start, end) byte offsets.
    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    fn collapse(&mut self) {
        self.anchor = self.cursor;
    }

    /// Deletes the selection (the caret goes to its start); false when nothing was selected.
    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    /// The text as the filter allows it.
    pub fn clean(&self, s: &str) -> String {
        s.chars()
            .filter(|&c| match self.filter {
                Filter::Line => c >= ' ' && c != '\u{7f}' && c != '\u{a7}',
                Filter::Multi => (c >= ' ' && c != '\u{7f}' && c != '\u{a7}') || c == '\n',
                Filter::Free => c >= ' ' && c != '\u{7f}',
            })
            .collect()
    }

    /// Types or pastes text: it replaces the selection, is cleaned and cut to the room that is left.
    pub fn insert(&mut self, s: &str) {
        let s = if self.filter == Filter::Multi { s.replace("\r\n", "\n").replace('\r', "\n") } else { String::from(s) };
        let s = self.clean(&s);
        let (a, b) = self.selection();
        let kept = utf16(&self.text) - utf16(&self.text[a..b]);
        let room = self.max.saturating_sub(kept);
        let s = cut_to(&s, room);
        if s.is_empty() && !self.has_selection() {
            return;
        }
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.collapse();
    }

    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let to = if word { word_left(&self.text, self.cursor) } else { prev_char(&self.text, self.cursor) };
        self.text.replace_range(to..self.cursor, "");
        self.cursor = to;
        self.collapse();
    }

    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let to = if word { word_right(&self.text, self.cursor) } else { next_char(&self.text, self.cursor) };
        self.text.replace_range(self.cursor..to, "");
        self.collapse();
    }

    /// Moves the caret (a selection grows when `extend`; without it a selection ends, the caret going to its edge).
    fn go(&mut self, to: usize, extend: bool) {
        self.cursor = floor(&self.text, to);
        if !extend {
            self.collapse();
        }
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            let (a, _) = self.selection();
            return self.go(a, false);
        }
        let to = if word { word_left(&self.text, self.cursor) } else { prev_char(&self.text, self.cursor) };
        self.go(to, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            let (_, b) = self.selection();
            return self.go(b, false);
        }
        let to = if word { word_right(&self.text, self.cursor) } else { next_char(&self.text, self.cursor) };
        self.go(to, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.go(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.go(self.text.len(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// A click at a character boundary (a drag keeps `extend` on).
    pub fn click(&mut self, at: usize, extend: bool) {
        self.go(at, extend);
    }

    /// A double click at `at`: the word there.
    pub fn select_word(&mut self, at: usize) {
        let at = floor(&self.text, at);
        let mut a = at;
        while a > 0 && !self.text[..a].ends_with(' ') {
            a = prev_char(&self.text, a);
        }
        let mut b = at;
        while b < self.text.len() && !self.text[b..].starts_with(' ') {
            b = next_char(&self.text, b);
        }
        self.anchor = a;
        self.cursor = b;
    }

    /// What copy puts on the clipboard: the selection, else the whole text.
    pub fn copy(&self) -> String {
        if self.has_selection() {
            String::from(self.selected())
        } else {
            self.text.clone()
        }
    }

    /// What cut puts on the clipboard; the selection (else everything) is removed.
    pub fn cut(&mut self) -> String {
        let s = self.copy();
        if !self.delete_selection() {
            self.text.clear();
            self.cursor = 0;
            self.anchor = 0;
        }
        s
    }

    /// An editing key: the arrows, Home, End, Backspace, Delete, and with the command key A, C, X and V. `clip` reads the
    /// clipboard (only called for V); `put` writes it. Returns whether the key was an editing key (the text may have
    /// changed: compare `text`).
    pub fn key(&mut self, sc: usize, command: bool, shift: bool, clip: &mut dyn FnMut() -> String, put: &mut dyn FnMut(&str)) -> bool {
        match sc {
            SC_LEFT => self.left(command, shift),
            SC_RIGHT => self.right(command, shift),
            SC_HOME => self.home(shift),
            SC_END => self.end(shift),
            SC_BACKSPACE => self.backspace(command),
            SC_DELETE => self.delete(command),
            SC_A if command => self.select_all(),
            SC_C if command => {
                let s = self.copy();
                put(&s);
            }
            SC_X if command => {
                let s = self.cut();
                put(&s);
            }
            SC_V if command => {
                let s = clip();
                self.insert(&s);
            }
            _ => return false,
        }
        true
    }
}

/// The offsets of every character boundary of `s` with the pixel width of the text up to it, for mapping a mouse x to a
/// caret: `width` measures a string. Returns the boundary nearest to `x` (pixels from the text's left edge).
pub fn index_at(s: &str, x: f32, width: &dyn Fn(&str) -> f32) -> usize {
    let mut best = 0usize;
    let mut best_d = f32::MAX;
    let mut bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    bounds.push(s.len());
    for b in bounds {
        let d = (width(&s[..b]) - x).abs();
        if d < best_d {
            best_d = d;
            best = b;
        }
    }
    best
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut none = || String::new();
    let mut sink = |_: &str| {};
    // Typing and the limit (UTF-16 units: an emoji is two).
    let mut e = Edit::new(5, Filter::Line);
    e.insert("ab\u{1F600}cdef");
    out.push(("textedit limit counts UTF-16", alloc::format!("{} {}", e.text, utf16(&e.text)), "ab\u{1F600}c 5"));
    // A selection is replaced by typing and deleted by Backspace.
    let mut e = Edit::new(64, Filter::Line);
    e.set("hello world");
    e.home(false);
    e.right(true, true); // a word and the spaces after it, as EditBox moves
    e.insert("X");
    out.push(("textedit typing replaces the selection", alloc::format!("{} {}", e.text, e.cursor), "Xworld 1"));
    e.select_all();
    e.backspace(false);
    out.push(("textedit backspace deletes the selection", alloc::format!("{:?} {}", e.text, e.cursor), "\"\" 0"));
    // Word movement and selection.
    let mut e = Edit::new(64, Filter::Line);
    e.set("one two three");
    e.left(true, true);
    out.push(("textedit shift+ctrl+left selects a word", String::from(e.selected()), "three"));
    e.left(false, false);
    out.push(("textedit left ends the selection at its start", alloc::format!("{} {}", e.cursor, e.has_selection()), "8 false"));
    // Clipboard: copy the selection (else all), cut, paste over a selection; control characters and the section sign go.
    let mut e = Edit::new(64, Filter::Line);
    e.set("alpha beta");
    let mut clip = String::new();
    e.select_word(7);
    let sel = e.copy();
    e.key(SC_X, true, false, &mut none, &mut |s| clip = String::from(s));
    out.push(("textedit double click selects a word, cut takes it", alloc::format!("{} {} {:?}", sel, clip, e.text), "beta beta \"alpha \""));
    e.key(SC_V, true, false, &mut || String::from("one\r\ntwo\u{a7}c\u{7}"), &mut sink);
    out.push(("textedit paste is cleaned", e.text.clone(), "alpha onetwoc"));
    out.push(("textedit copy with no selection takes all", {
        e.anchor = e.cursor;
        e.copy()
    }, "alpha onetwoc"));
    // A multi-line field keeps line breaks.
    let mut m = Edit::new(256, Filter::Multi);
    m.insert("a\r\nb\rc");
    out.push(("textedit multi-line keeps breaks", m.text.replace('\n', "|"), "a|b|c"));
    // Multi-byte characters: the caret never lands inside one.
    let mut e = Edit::new(64, Filter::Line);
    e.set("h\u{e9}llo");
    e.home(false);
    e.right(false, false);
    e.right(false, false);
    e.backspace(false);
    out.push(("textedit multi-byte backspace", e.text.clone(), "hllo"));
    // Mapping a mouse x to a caret (every character 6 wide).
    out.push(("textedit click position", alloc::format!("{}", index_at("abcdef", 13.0, &|s: &str| s.chars().count() as f32 * 6.0)), "2"));
    // The command key: Control everywhere, Command only on macOS.
    let mut keys = [false; 512];
    keys[SC_LGUI] = true;
    let cmd = command(&keys);
    keys[SC_LGUI] = false;
    keys[SC_LCTRL] = true;
    out.push(("textedit command key", alloc::format!("{} {}", cmd == cfg!(target_os = "macos"), command(&keys)), "true true"));
    out
}
