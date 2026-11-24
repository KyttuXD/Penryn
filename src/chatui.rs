//! The chat on screen as vanilla 26.3 draws and runs it: ChatComponent (messages fading after
//! 10 s, 20 lines and scrolling while the chat is open, the message tags' indicators and the
//! "Modified" icon, hover tooltips and click events), ChatScreen with its EditBox (cursor,
//! history, clipboard) and CommandSuggestions (the coloured command, the suggestion list above
//! the input, Tab and the arrow keys, argument usage, parse errors in red, the server's
//! suggestions), ConfirmLinkScreen and the command confirmation, and Hud's titles, subtitles
//! and action bar.

use crate::client::Game;
use crate::textedit::{self, Edit, Filter};
use crate::gui::Gui;
use crate::talk::Tag;
use crate::text::{self, Click, Hover, Rich, Style};
use alloc::string::String;
use alloc::vec::Vec;

// SDL scancodes.
const K_RETURN: usize = 40;
const K_ESC: usize = 41;
const K_TAB: usize = 43;
const K_PAGEUP: usize = 75;
const K_PAGEDOWN: usize = 78;
const K_DOWN: usize = 81;
const K_UP: usize = 82;
const K_KP_ENTER: usize = 88;

/// CommandSuggestions.ARGUMENT_STYLES: aqua, yellow, green, light purple, gold.
const ARGUMENT_COLORS: [char; 5] = ['b', 'e', 'a', 'd', '6'];
/// ChatScreen's suggestion list: 10 lines, filled 0xD0000000.
const LIST_LINES: usize = 10;
const FILL: u32 = 0xD000_0000;

/// One suggestion: its text, its tooltip, and the input range it replaces.
#[derive(Clone)]
struct Suggestion {
    text: String,
    tooltip: Option<Rich>,
    start: usize,
    end: usize,
}

/// CommandSuggestions.SuggestionsList.
struct List {
    items: Vec<Suggestion>,
    sel: usize,
    offset: usize,
    tab_cycles: bool,
    original: String,
}

const SIGNATURE_REQUIRED: &str = "multiplayer.confirm_command.signature_required";

/// What a confirmation screen asks.
enum Confirm {
    /// ConfirmLinkScreen (chat.link.confirm): open, copy or not.
    Url(String),
    /// ClientPacketListener's command confirmation: the command and the message's key.
    Command(String, &'static str),
}

pub enum Act {
    None,
    /// Close the chat screen.
    Close,
}

pub struct ChatUi {
    /// The input line: text, caret, selection (textedit.rs), 256 UTF-16 units, no control characters or section signs.
    pub ed: Edit,
    /// A mouse drag that started in the input line, and the last click (time, x) for the double click.
    dragging: bool,
    last_click: (u64, f32),
    /// Browsing the history: the index into it, and the line being written before.
    history: Option<usize>,
    draft: String,
    list: Option<List>,
    /// What the suggestions would be (pendingSuggestions), shown by Tab or automatically.
    pending: Vec<Suggestion>,
    /// A server request in flight: its id.
    waiting: Option<i32>,
    keep: bool,
    /// The usage lines above the input (formatted text), their x and width.
    usage: Vec<String>,
    usage_x: f32,
    usage_w: f32,
    /// The input drawn with CommandSuggestions.formatChat's colours.
    formatted: String,
    scroll: usize,
    confirm: Option<Confirm>,
    focused_at: u64,
    test_ticks: u64,
}

impl Default for ChatUi {
    fn default() -> ChatUi {
        ChatUi {
            ed: Edit::new(256, Filter::Line),
            dragging: false,
            last_click: (0, 0.0),
            history: None,
            draft: String::new(),
            list: None,
            pending: Vec::new(),
            waiting: None,
            keep: false,
            usage: Vec::new(),
            usage_x: 0.0,
            usage_w: 0.0,
            formatted: String::new(),
            scroll: 0,
            confirm: None,
            focused_at: 0,
            test_ticks: 0,
        }
    }
}

fn char_floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// StringReader-style: where the last word of `s` starts (CommandSuggestions.getLastWordIndex).
fn last_word(s: &str) -> usize {
    s.rfind(' ').map_or(0, |i| i + 1)
}

impl ChatUi {
    pub fn new() -> ChatUi {
        ChatUi::default()
    }

    /// ChatScreen.init: the chat opened with `initial` typed.
    pub fn open(&mut self, g: &mut Game, initial: &str) {
        *self = ChatUi { test_ticks: self.test_ticks, ..ChatUi::default() };
        self.ed.set(initial);
        self.focused_at = crate::sys::now_ms();
        self.edited(g);
    }

    fn set_input(&mut self, g: &mut Game, s: &str) {
        self.ed.set(s);
        self.edited(g);
    }

    /// EditBox.insertText.
    pub fn type_text(&mut self, g: &mut Game, s: &str) {
        if self.confirm.is_some() {
            return;
        }
        // SharedConstants.filterText (Filter::Line); typing over a selection replaces it.
        let before = (self.ed.text.len(), self.ed.cursor, self.ed.anchor);
        self.ed.insert(s);
        if before != (self.ed.text.len(), self.ed.cursor, self.ed.anchor) {
            self.edited(g);
        }
    }

    /// CommandSuggestions.updateCommandInfo (the EditBox's responder).
    fn edited(&mut self, g: &mut Game) {
        if !self.keep {
            self.list = None;
        }
        self.usage.clear();
        self.formatted.clear();
        let cursor = self.ed.cursor;
        let text = self.ed.text.clone();
        if text.starts_with('/') && g.talk.tree.is_some() {
            let (parse, ctx) = {
                let tree = g.talk.tree.as_ref().unwrap();
                let parse = tree.parse(&text, 1);
                self.formatted = format_command(&text, &parse, tree);
                let mut ctx = None;
                if cursor >= 1 && (self.list.is_none() || !self.keep) {
                    let (node, start) = tree.suggestion_context(&parse, cursor);
                    let players: Vec<String> = g.talk.names.values().cloned().collect();
                    let (sugs, ask) = tree.suggest(node, &text[..cursor], start, &players, &g.blocks.items);
                    self.pending = sugs.into_iter().map(|t| Suggestion { text: t, tooltip: None, start, end: cursor }).collect();
                    ctx = Some((node, start, ask));
                }
                (parse, ctx)
            };
            if let Some((node, start, ask)) = ctx {
                self.waiting = None;
                if ask {
                    if let Ok(id) = g.ask_suggestions(&text[..cursor]) {
                        self.waiting = Some(id);
                    }
                }
                self.usage_info(g, &parse, node, start);
            }
        } else if !text.starts_with('/') {
            // Chat: Tab completes the last word with the players' names.
            let before = &text[..cursor];
            let start = last_word(before);
            let word = before[start..].to_ascii_lowercase();
            let mut names: Vec<String> = g.talk.names.values().filter(|n| n.to_ascii_lowercase().starts_with(&word)).cloned().collect();
            crate::commands::sort_suggestions(&mut names, &word);
            self.pending = names.into_iter().map(|t| Suggestion { text: t, tooltip: None, start, end: cursor }).collect();
        } else {
            self.pending.clear();
        }
    }

    /// CommandSuggestions.updateUsageInfo: errors in red at the end of the input when nothing
    /// is suggested, else the usage of the arguments that can come next, in grey; and the
    /// suggestion list shown (autoSuggestions).
    fn usage_info(&mut self, g: &Game, parse: &crate::commands::Parse, node: usize, start: usize) {
        let Some(tree) = g.talk.tree.as_ref() else { return };
        let input = &self.ed.text;
        let mut trailing = false;
        if self.ed.cursor == input.len() {
            if self.pending.is_empty() && self.waiting.is_none() && !parse.errors.is_empty() {
                let mut literals = 0;
                for e in &parse.errors {
                    if e.literal {
                        literals += 1;
                    } else {
                        self.usage.push(exception(input, &e.key, &e.args, e.at));
                    }
                }
                if literals > 0 {
                    self.usage.push(exception(input, "command.unknown.command", &[], parse.cursor));
                }
            } else if parse.cursor < input.len() {
                trailing = true;
            }
        }
        self.usage_x = 0.0;
        self.usage_w = 0.0;
        if self.usage.is_empty() {
            let lines: Vec<String> = tree.smart_usage(node).into_iter().filter(|(c, _)| tree.nodes[*c].kind != 1).map(|(_, u)| alloc::format!("\u{a7}7{}", u)).collect();
            if !lines.is_empty() {
                self.usage = lines;
                self.usage_x = start as f32;
            } else if trailing {
                if let Some((k, a, at)) = parse.exception(input) {
                    self.usage.push(exception(input, &k, &a, at));
                }
            }
        }
        if !self.pending.is_empty() {
            self.show_list(false);
        }
    }

    /// CommandSuggestions.showSuggestions: the list from what is pending.
    fn show_list(&mut self, _narrate: bool) {
        if self.pending.is_empty() {
            return;
        }
        let mut l = List { items: self.pending.clone(), sel: 0, offset: 0, tab_cycles: false, original: self.ed.text.clone() };
        // sortSuggestions: those starting with the typed word first (already sorted).
        l.sel = 0;
        self.list = Some(l);
    }

    fn apply(&self, l: &List, i: usize) -> (String, usize) {
        let s = &l.items[i];
        let o = &l.original;
        let (a, b) = (char_floor(o, s.start), char_floor(o, s.end.max(s.start)));
        let mut t = String::from(&o[..a]);
        t.push_str(&s.text);
        let cur = t.len();
        t.push_str(&o[b..]);
        (t, cur)
    }

    /// SuggestionsList.useSuggestion.
    fn use_suggestion(&mut self, g: &mut Game) {
        let Some(l) = self.list.take() else { return };
        let (t, cur) = self.apply(&l, l.sel);
        self.keep = true;
        self.ed.set(&t);
        self.ed.cursor = char_floor(&self.ed.text, cur);
        self.ed.anchor = self.ed.cursor;
        self.list = Some(l);
        self.edited(g);
        self.keep = false;
        if let Some(l) = self.list.as_mut() {
            l.tab_cycles = true;
        }
    }

    fn cycle(&mut self, d: i32) {
        if let Some(l) = self.list.as_mut() {
            let n = l.items.len() as i32;
            l.sel = (l.sel as i32 + d).rem_euclid(n) as usize;
            if l.sel < l.offset {
                l.offset = l.sel;
            } else if l.sel >= l.offset + LIST_LINES {
                l.offset = l.sel + 1 - LIST_LINES;
            }
        }
    }

    /// ChatScreen.keyPressed with CommandSuggestions and the EditBox.
    pub fn key(&mut self, g: &mut Game, sc: usize, keys: &[bool], v: &crate::sdl::Video) -> Result<Act, String> {
        let shift = textedit::shift(keys);
        let command = textedit::command(keys);
        if let Some(c) = self.confirm.take() {
            match sc {
                K_ESC => {}
                K_RETURN | K_KP_ENTER => return self.confirmed(g, c, 0, v),
                _ => self.confirm = Some(c),
            }
            return Ok(Act::None);
        }
        // The suggestion list first.
        if self.list.is_some() {
            match sc {
                K_UP => {
                    self.cycle(-1);
                    self.list.as_mut().unwrap().tab_cycles = false;
                    return Ok(Act::None);
                }
                K_DOWN => {
                    self.cycle(1);
                    self.list.as_mut().unwrap().tab_cycles = false;
                    return Ok(Act::None);
                }
                K_TAB => {
                    if self.list.as_ref().unwrap().tab_cycles {
                        self.cycle(if shift { -1 } else { 1 });
                    }
                    self.use_suggestion(g);
                    return Ok(Act::None);
                }
                K_ESC => {
                    self.list = None;
                    return Ok(Act::None);
                }
                _ => {}
            }
        } else if sc == K_TAB {
            self.show_list(true);
            return Ok(Act::None);
        }
        match sc {
            K_ESC => return Ok(Act::Close),
            K_RETURN | K_KP_ENTER => {
                let line = core::mem::take(&mut self.ed.text);
                self.ed.cursor = 0;
                self.ed.anchor = 0;
                g.send_line(&line)?;
                return Ok(Act::Close);
            }
            K_UP => self.history_move(g, -1),
            K_DOWN => self.history_move(g, 1),
            K_PAGEUP => self.scroll_by(g, 19),
            K_PAGEDOWN => self.scroll_by(g, -19),
            _ => {
                // The EditBox: arrows, Home, End, Backspace, Delete, select all, copy, cut, paste.
                let before = (self.ed.text.clone(), self.ed.cursor, self.ed.anchor);
                if self.ed.key(sc, command, shift, &mut || v.clipboard(), &mut |t| v.set_clipboard(t))
                    && before != (self.ed.text.clone(), self.ed.cursor, self.ed.anchor)
                {
                    self.edited(g);
                }
            }
        }
        Ok(Act::None)
    }

    /// ChatScreen.moveInHistory.
    fn history_move(&mut self, g: &mut Game, d: i32) {
        let n = g.talk.history.len();
        let cur = self.history.unwrap_or(n) as i32;
        let next = (cur + d).clamp(0, n as i32) as usize;
        if next == cur as usize {
            return;
        }
        if self.history.is_none() {
            self.draft = self.ed.text.clone();
        }
        if next == n {
            self.history = None;
            let d = self.draft.clone();
            self.set_input(g, &d);
        } else {
            self.history = Some(next);
            let h = g.talk.history[next].clone();
            self.set_input(g, &h);
        }
    }

    /// ChatComponent.scrollChat.
    fn scroll_by(&mut self, g: &Game, n: i32) {
        let total: usize = g.talk.msgs.iter().map(|m| m.wrapped.as_ref().map_or(1, |w| w.1.len())).sum();
        let max = total.saturating_sub(20) as i32;
        self.scroll = (self.scroll as i32 + n).clamp(0, max) as usize;
    }

    pub fn wheel(&mut self, g: &Game, dy: i32, shift: bool) {
        if let Some(l) = self.list.as_mut() {
            let n = l.items.len();
            if n > LIST_LINES {
                l.offset = (l.offset as i32 - dy).clamp(0, (n - LIST_LINES) as i32) as usize;
            }
            return;
        }
        self.scroll_by(g, if shift { dy } else { dy * 7 });
    }

    /// The suggestion list's rectangle (SuggestionsList's rect): x, top, width, rows.
    fn list_rect(&self, gui: &Gui) -> Option<(f32, f32, f32, usize)> {
        let l = self.list.as_ref()?;
        let w = l.items.iter().map(|s| gui.text_width(&s.text)).fold(0.0, f32::max);
        let start = l.items.first().map_or(0, |s| s.start);
        let sx = 4.0 + gui.text_width(&l.original[..char_floor(&l.original, start)]);
        let field = gui.width - 4.0;
        let x = sx.clamp(0.0, (4.0 + field - w).max(0.0)) - 1.0;
        let rows = l.items.len().min(LIST_LINES);
        let top = gui.height - 12.0 - 3.0 - rows as f32 * 12.0;
        Some((x, top, w + 1.0, rows))
    }

    /// The input line's band (ChatScreen: 2, height - 14 to width - 2, height - 2).
    fn in_input(&self, gui: &Gui, mx: f32, my: f32) -> bool {
        mx >= 2.0 && mx < gui.width - 2.0 && my >= gui.height - 14.0 && my < gui.height - 2.0
    }

    /// EditBox.displayPos: how far the text is scrolled left to keep the caret in view.
    fn scroll_x(&self, gui: &Gui) -> f32 {
        let pre_w = gui.text_width(&self.ed.text[..self.ed.cursor]);
        let field = gui.width - 8.0;
        if pre_w > field { pre_w - field } else { 0.0 }
    }

    /// The character boundary nearest to the mouse x, from the text as it is drawn.
    fn index_at(&self, gui: &Gui, mx: f32) -> usize {
        textedit::index_at(&self.ed.text, mx - 4.0 + self.scroll_x(gui), &|s| gui.text_width(s))
    }

    /// The mouse moved with the left button `held`: a drag that started in the input line extends the selection.
    pub fn drag(&mut self, gui: &Gui, mx: f32, held: bool) {
        if !held || self.confirm.is_some() {
            self.dragging = false;
        }
        if self.dragging {
            let at = self.index_at(gui, mx);
            self.ed.click(at, true);
        }
    }

    /// ChatScreen.mouseClicked: a suggestion, a chat component's click event (or its
    /// insertion with shift), or a confirmation's button.
    pub fn click(&mut self, g: &mut Game, gui: &Gui, mx: f32, my: f32, button: u8, shift: bool, v: &crate::sdl::Video) -> Result<Act, String> {
        if button != 1 {
            return Ok(Act::None);
        }
        if let Some(c) = self.confirm.take() {
            let b = confirm_buttons(gui, &c);
            for (i, bt) in b.iter().enumerate() {
                if bt.hit(mx, my) {
                    return self.confirmed(g, c, i, v);
                }
            }
            self.confirm = Some(c);
            return Ok(Act::None);
        }
        if let Some((x, top, w, rows)) = self.list_rect(gui) {
            if mx >= x && mx < x + w && my >= top && my < top + rows as f32 * 12.0 {
                let i = ((my - top) / 12.0) as usize + self.list.as_ref().unwrap().offset;
                if let Some(l) = self.list.as_mut() {
                    l.sel = i.min(l.items.len() - 1);
                }
                self.use_suggestion(g);
                return Ok(Act::None);
            }
        }
        if self.in_input(gui, mx, my) {
            // EditBox.mouseClicked: the caret goes to the nearest character, shift extends the selection, a double
            // click selects the word, and holding the button drags a selection.
            let at = self.index_at(gui, mx);
            let now = crate::sys::now_ms();
            let double = now.saturating_sub(self.last_click.0) < 400 && (mx - self.last_click.1).abs() < 3.0;
            if double {
                self.ed.select_word(at);
                self.last_click = (0, 0.0);
            } else {
                self.ed.click(at, shift);
                self.last_click = (now, mx);
            }
            self.dragging = !double;
            self.edited(g);
            return Ok(Act::None);
        }
        let Some((msg, st)) = style_at(g, gui, self.scroll, mx, my) else { return Ok(Act::None) };
        let ev = g.talk.msgs.get(msg).and_then(|m| m.rich.events_of(&st)).cloned().unwrap_or_default();
        if shift {
            if let Some(ins) = ev.insertion {
                self.type_text(g, &ins);
                return Ok(Act::None);
            }
        }
        match ev.click {
            Some(Click::OpenUrl(u)) => {
                if u.starts_with("http://") || u.starts_with("https://") {
                    self.confirm = Some(Confirm::Url(u));
                }
            }
            Some(Click::RunCommand(c)) => self.unattended(g, &c)?,
            Some(Click::SuggestCommand(c)) => self.set_input(g, &c),
            Some(Click::CopyToClipboard(c)) => v.set_clipboard(&c),
            _ => {}
        }
        Ok(Act::None)
    }

    /// ClientPacketListener.sendUnattendedCommand (from the chat screen): a clicked command runs
    /// at once when it parses, signs nothing and needs no elevated permissions; otherwise
    /// a confirmation asks (run it unsigned anyway, or, for one that would sign chat, copy
    /// it to the clipboard: it can only be sent from the chat box).
    fn unattended(&mut self, g: &mut Game, command: &str) -> Result<(), String> {
        // Commands.trimOptionalPrefix.
        let command = String::from(command.strip_prefix('/').unwrap_or(command));
        let key = match g.talk.tree.as_ref() {
            Some(t) => {
                let p = t.parse(&command, 0);
                if !p.valid(&command) {
                    "multiplayer.confirm_command.parse_errors"
                } else if !t.signable(&command, &p).is_empty() {
                    "multiplayer.confirm_command.signature_required"
                } else if t.restricted(&p) {
                    "multiplayer.confirm_command.permissions_required"
                } else {
                    return g.send_command_unsigned(&command);
                }
            }
            None => "multiplayer.confirm_command.parse_errors",
        };
        self.confirm = Some(Confirm::Command(command, key));
        Ok(())
    }

    fn confirmed(&mut self, g: &mut Game, c: Confirm, button: usize, v: &crate::sdl::Video) -> Result<Act, String> {
        match c {
            Confirm::Url(u) => match button {
                0 => {
                    if !crate::sys::open_url(&u) {
                        crate::warn!("cannot open {}", u);
                    }
                }
                1 => v.set_clipboard(&u),
                _ => {}
            },
            Confirm::Command(cmd, key) => {
                if button == 0 {
                    if key == SIGNATURE_REQUIRED {
                        v.set_clipboard(&alloc::format!("/{}", cmd));
                    } else {
                        g.send_command_unsigned(&cmd)?;
                    }
                }
            }
        }
        Ok(Act::None)
    }

    /// A server's suggestions arriving (ClientboundCommandSuggestionsPacket) for the input.
    pub fn frame(&mut self, g: &mut Game) {
        let Some((id, start, len, list)) = g.talk.suggestions.take() else { return };
        if Some(id) != self.waiting {
            return;
        }
        self.waiting = None;
        let end = start + len;
        for s in list {
            if !self.pending.iter().any(|p| p.text == s.text) {
                self.pending.push(Suggestion { text: s.text, tooltip: s.tooltip, start, end });
            }
        }
        let typed = &self.ed.text[char_floor(&self.ed.text, start)..char_floor(&self.ed.text, self.ed.cursor)];
        let mut texts: Vec<String> = self.pending.iter().map(|p| p.text.clone()).collect();
        crate::commands::sort_suggestions(&mut texts, typed);
        let mut sorted = Vec::new();
        for t in texts {
            if let Some(p) = self.pending.iter().find(|p| p.text == t) {
                sorted.push(p.clone());
            }
        }
        self.pending = sorted;
        if !self.pending.is_empty() {
            self.usage.clear();
            if self.list.is_none() || !self.keep {
                self.show_list(false);
            }
        } else if let Some(t) = g.talk.tree.as_ref() {
            // Nothing from the server either: the usage again, errors included.
            if self.ed.text.starts_with('/') {
                let p = t.parse(&self.ed.text, 1);
                let (node, start) = t.suggestion_context(&p, self.ed.cursor);
                self.usage.clear();
                self.usage_info(g, &p, node, start);
            }
        }
    }

    /// Test hooks: PENRYN_TEST_SAY="A;B" sends lines (chat or commands) a second apart,
    /// PENRYN_TEST_CHAT=TEXT opens the chat with TEXT typed (after the lines), and
    /// PENRYN_TEST_CHAT_KEYS="tab,down,mouse=X:Y,click" works it. Returns true when the chat
    /// screen should open; `mouse` is moved for hover pictures.
    pub fn test(&mut self, g: &mut Game, mouse: &mut (f32, f32), gui: Option<&Gui>, v: &crate::sdl::Video) -> Result<bool, String> {
        if !g.loaded_sent {
            return Ok(false);
        }
        let say = crate::sys::env("PENRYN_TEST_SAY").unwrap_or_default();
        let chat = crate::sys::env("PENRYN_TEST_CHAT");
        if say.is_empty() && chat.is_none() {
            return Ok(false);
        }
        self.test_ticks += 1;
        let ticks = self.test_ticks;
        let lines: Vec<&str> = if say.is_empty() { Vec::new() } else { say.split(';').collect() };
        for (i, line) in lines.iter().enumerate() {
            if ticks == 60 + 20 * i as u64 {
                g.send_line(line)?;
            }
        }
        let open_at = 60 + 20 * lines.len() as u64 + 20;
        let mut open = false;
        if let Some(t) = chat {
            if ticks == open_at {
                self.open(g, &t);
                open = true;
            }
            let keys = crate::sys::env("PENRYN_TEST_CHAT_KEYS").unwrap_or_default();
            for (i, k) in keys.split(',').enumerate() {
                if ticks != open_at + 20 + 10 * i as u64 {
                    continue;
                }
                match k {
                    "tab" => {
                        self.key(g, K_TAB, &[false; 512], v)?;
                    }
                    "down" => {
                        self.key(g, K_DOWN, &[false; 512], v)?;
                    }
                    "click" => {
                        if let Some(gui) = gui {
                            self.click(g, gui, mouse.0, mouse.1, 1, false, v)?;
                        }
                    }
                    _ => {
                        let found = k.strip_prefix("find=").or_else(|| k.strip_prefix("clickon="));
                        if let (Some(t), Some(gui)) = (found, gui) {
                            // The mouse onto the newest chat line holding `t`.
                            let lines = visible(g, gui, true, self.scroll, crate::sys::now_ms());
                            let bottom = math_floor(gui.height - 40.0);
                            for (i, (mi, li, _)) in lines.iter().enumerate() {
                                let l = &g.talk.msgs[*mi].wrapped.as_ref().unwrap().1[*li];
                                let plain: String = l.iter().map(|r| r.1.as_str()).collect();
                                if let Some(at) = plain.find(t) {
                                    *mouse = (4.0 + gui.text_width(&plain[..at]) + 2.0, bottom - i as f32 * LINE - 4.0);
                                    if k.starts_with("clickon=") {
                                        self.click(g, gui, mouse.0, mouse.1, 1, false, v)?;
                                    }
                                    break;
                                }
                            }
                        } else if let Some((x, y)) = k.strip_prefix("mouse=").and_then(|m| m.split_once(':')) {
                            *mouse = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                        } else if let Some(t) = k.strip_prefix("type=") {
                            self.type_text(g, t);
                        }
                    }
                }
            }
        }
        Ok(open)
    }
}

/// CommandSyntaxException's message as CommandSuggestions shows it: red, with the position
/// and the input before it ("...<--[HERE]").
fn exception(input: &str, key: &str, args: &[String], at: usize) -> String {
    let msg = crate::lang::format(key, args);
    let at = char_floor(input, at);
    let from = char_floor(input, at.saturating_sub(10));
    let ctx = alloc::format!("{}{}{}", if at > 10 { "..." } else { "" }, &input[from..at], crate::lang::get("command.context.here"));
    alloc::format!("\u{a7}c{}", crate::lang::format("command.context.parse_error", &[msg, alloc::format!("{}", at), ctx]))
}

/// CommandSuggestions.formatText: the arguments in turn aqua, yellow, green, light purple and
/// gold; the rest grey; what did not parse red.
fn format_command(input: &str, p: &crate::commands::Parse, tree: &crate::commands::Tree) -> String {
    let mut out = String::new();
    let mut last = 0usize;
    let mut k = 0usize;
    for (s, e) in p.argument_ranges(tree) {
        let (s, e) = (char_floor(input, s), char_floor(input, e));
        if s < last || s >= input.len() {
            break;
        }
        out.push_str("\u{a7}7");
        out.push_str(&input[last..s]);
        out.push('\u{a7}');
        out.push(ARGUMENT_COLORS[k % 5]);
        out.push_str(&input[s..e]);
        k += 1;
        last = e;
    }
    let cur = char_floor(input, p.cursor);
    if cur > last && cur <= input.len() {
        out.push_str("\u{a7}7");
        out.push_str(&input[last..cur]);
        last = cur;
    }
    if last < input.len() {
        out.push_str(if p.cursor < input.len() { "\u{a7}c" } else { "\u{a7}7" });
        out.push_str(&input[last..]);
    }
    out
}

// ------------------------------------------------------------------ the message lines

const LINE: f32 = 9.0;

/// The message lines on screen, newest first: (message index, line index), with the chat open
/// (`scroll` lines back) or not (lines of the last 10 s).
fn visible(g: &mut Game, gui: &Gui, focused: bool, scroll: usize, now: u64) -> Vec<(usize, usize, f32)> {
    let width = 320.0f32.min((gui.width - 8.0).max(40.0));
    let mut out = Vec::new();
    let mut skipped = 0;
    let per_page = if focused { 20 } else { 10 };
    for (mi, m) in g.talk.msgs.iter_mut().enumerate().rev() {
        let w = if m.tag.color().is_some() && matches!(m.tag, Tag::Modified(_)) { width - 15.0 } else { width };
        if m.wrapped.as_ref().map_or(true, |x| x.0 != w) {
            m.wrapped = Some((w, text::wrap(gui, &m.rich, w)));
        }
        let age = now.saturating_sub(m.at) as f32 / 50.0;
        let alpha = if focused {
            1.0
        } else {
            let t = ((1.0 - age / 200.0) * 10.0).clamp(0.0, 1.0);
            t * t
        };
        let n = m.wrapped.as_ref().unwrap().1.len();
        for li in (0..n).rev() {
            if skipped < scroll {
                skipped += 1;
                continue;
            }
            if out.len() >= per_page {
                return out;
            }
            out.push((mi, li, alpha));
        }
        if out.len() >= per_page {
            break;
        }
    }
    out
}

/// The message and style under the mouse in the open chat.
fn style_at(g: &mut Game, gui: &Gui, scroll: usize, mx: f32, my: f32) -> Option<(usize, Style)> {
    let bottom = math_floor(gui.height - 40.0);
    let lines = visible(g, gui, true, scroll, crate::sys::now_ms());
    let i = ((bottom - my) / LINE) as i32;
    if my > bottom || i < 0 || i as usize >= lines.len() {
        return None;
    }
    let (mi, li, _) = lines[i as usize];
    let m = &g.talk.msgs[mi];
    let line = &m.wrapped.as_ref()?.1[li];
    text::run_at(gui, line, mx - 4.0).map(|s| (mi, *s))
}

fn math_floor(v: f32) -> f32 {
    crate::math::floor(v as f64) as f32
}

/// The tooltip of a hover event (HoverEvent: text wrapped at max(width / 2, 200); an item's
/// name; an entity's name, type and UUID).
fn hover_lines(gui: &Gui, h: &Hover) -> Vec<String> {
    let w = (gui.width / 2.0).max(200.0);
    match h {
        Hover::Text(r) => text::wrap(gui, r, w).iter().map(text::line_legacy).collect(),
        Hover::Item { id, count, name } => {
            let n = match name {
                Some(r) => r.legacy(),
                None => crate::lang::item_name(id.strip_prefix("minecraft:").unwrap_or(id)),
            };
            let _ = count;
            alloc::vec![n]
        }
        Hover::Entity { kind, uuid, name } => {
            let mut v = Vec::new();
            if let Some(n) = name {
                v.push(n.legacy());
            }
            let ty = crate::lang::get(&alloc::format!("entity.{}", kind.replace(':', ".")));
            v.push(crate::lang::format("gui.entity_tooltip.type", &[ty]));
            v.push(String::from(uuid.as_str()));
            v
        }
    }
}

/// ChatComponent's lines (and, with the chat open, ChatScreen's input, suggestions and
/// tooltips), Hud's title and action bar.
pub fn draw(gui: &mut Gui, g: &mut Game, ui: &mut ChatUi, open: bool, show_chat: bool, partial: f32, mx: f32, my: f32) {
    let now = crate::sys::now_ms();
    if open {
        ui.frame(g);
    }
    titles(gui, g, partial);
    if !show_chat {
        return;
    }
    let lines = visible(g, gui, open, if open { ui.scroll } else { 0 }, now);
    let width = 320.0f32.min((gui.width - 8.0).max(40.0));
    let bottom = math_floor(gui.height - 40.0);
    // Backgrounds, then the text, then the tags (ChatComponent: fill -4 .. width + 8 behind
    // each line, translated 4 right).
    for (i, (_, _, a)) in lines.iter().enumerate() {
        let b = bottom - i as f32 * LINE;
        let bg = ((0x80 as f32 * a) as u32) << 24;
        gui.fill(0.0, b - LINE, width + 12.0, b, bg);
    }
    let hovered_line = if open && my <= bottom { Some(((bottom - my) / LINE) as usize) } else { None };
    let mut hovered_msg = None;
    for (i, (mi, li, a)) in lines.iter().enumerate() {
        let b = bottom - i as f32 * LINE;
        let alpha = (255.0 * a) as u32;
        if alpha <= 3 {
            continue;
        }
        let m = &g.talk.msgs[*mi];
        let line = &m.wrapped.as_ref().unwrap().1[*li];
        gui.text(&text::line_legacy(line), 4.0, b - 8.0, alpha << 24 | 0xFFFFFF);
        if let Some(c) = m.tag.color() {
            gui.fill(0.0, b - LINE, 2.0, b, alpha << 24 | c);
        }
        if hovered_line == Some(i) {
            hovered_msg = Some(*mi);
        }
    }
    if !open {
        return;
    }
    // The Modified icon after the message's last line, while the mouse is on the message.
    let mut tag_tip: Option<Rich> = None;
    for (i, (mi, li, _)) in lines.iter().enumerate() {
        let b = bottom - i as f32 * LINE;
        let m = &g.talk.msgs[*mi];
        let n = m.wrapped.as_ref().unwrap().1.len();
        let over_bar = mx >= 0.0 && mx < 2.0 && my >= b - LINE && my < b;
        if over_bar {
            tag_tip = m.tag.text();
        }
        if *li + 1 == n && matches!(m.tag, Tag::Modified(_)) {
            let line = &m.wrapped.as_ref().unwrap().1[*li];
            let x = 4.0 + line.iter().map(|(s, t)| text::width_of(gui, t, s)).sum::<f32>() + 4.0;
            let y = b - 9.0 - 1.0;
            let over_icon = mx >= x && mx < x + 9.0 && my >= y && my < b - 1.0;
            if over_icon {
                tag_tip = m.tag.text();
            }
            if hovered_msg == Some(*mi) || over_icon {
                gui.blit("icon/chat_modified", x, y);
            }
        }
    }
    // The input (ChatScreen: the band behind it, the EditBox at 4, height - 12).
    let h = gui.height;
    gui.fill(2.0, h - 14.0, gui.width - 2.0, h - 2.0, 0x8000_0000);
    let shown = if ui.formatted.is_empty() { alloc::format!("\u{a7}r{}", ui.ed.text) } else { ui.formatted.clone() };
    let field = gui.width - 8.0;
    // EditBox.displayPos: keep the cursor in view.
    let pre_w = gui.text_width(&ui.ed.text[..ui.ed.cursor]);
    let shift = ui.scroll_x(gui);
    let tx = 4.0 - shift;
    // The selection: EditBox.extractHighlight, a blue band behind the selected characters.
    if ui.ed.has_selection() && ui.confirm.is_none() {
        let (a, b) = ui.ed.selection();
        let x0 = (tx + gui.text_width(&ui.ed.text[..a])).clamp(4.0, 4.0 + field);
        let x1 = (tx + gui.text_width(&ui.ed.text[..b])).clamp(4.0, 4.0 + field);
        gui.fill(x0, h - 13.0, x1, h - 12.0 + 9.0 + 1.0, 0xFF33_99FF);
    }
    gui.text(&shown, tx, h - 12.0, 0xFFE0_E0E0);
    let blink = (now.saturating_sub(ui.focused_at) / 300) % 2 == 0;
    let cx = tx + pre_w;
    // The selected suggestion's rest, grey, after the cursor (EditBox.setSuggestion).
    let mut hint = String::new();
    if let Some(l) = ui.list.as_ref() {
        let (t, _) = ui.apply(l, l.sel);
        if t.starts_with(ui.ed.text.as_str()) && t.len() > ui.ed.text.len() {
            hint = String::from(&t[ui.ed.text.len()..]);
        }
    }
    if ui.ed.cursor == ui.ed.text.len() && !hint.is_empty() {
        gui.text(&hint, cx, h - 12.0, 0xFF80_8080);
    }
    if blink && ui.confirm.is_none() {
        if ui.ed.cursor == ui.ed.text.len() && hint.is_empty() {
            gui.text("_", cx, h - 12.0, 0xFFE0_E0E0);
        } else {
            gui.fill(cx, h - 13.0, cx + 1.0, h - 12.0 + 9.0 + 1.0, 0xFFD0_D0D0);
        }
    }
    // The suggestions above it, or the usage / errors.
    if let Some((x, top, w, rows)) = ui.list_rect(gui) {
        let l = ui.list.as_ref().unwrap();
        let over = l.offset > 0;
        let more = l.items.len() > l.offset + rows;
        if over || more {
            gui.fill(x, top - 1.0, x + w, top, FILL);
            gui.fill(x, top + rows as f32 * 12.0, x + w, top + rows as f32 * 12.0 + 1.0, FILL);
            let mut i = 0.0;
            while i < w {
                if over {
                    gui.fill(x + i, top - 1.0, x + i + 1.0, top, 0xFFFF_FFFF);
                }
                if more {
                    gui.fill(x + i, top + rows as f32 * 12.0, x + i + 1.0, top + rows as f32 * 12.0 + 1.0, 0xFFFF_FFFF);
                }
                i += 2.0;
            }
        }
        let mut tip = None;
        for r in 0..rows {
            let idx = l.offset + r;
            let y = top + r as f32 * 12.0;
            gui.fill(x, y, x + w, y + 12.0, FILL);
            let hovered = mx > x && mx < x + w && my > y && my < y + 12.0;
            let sel = idx == l.sel;
            if sel {
                tip = l.items[idx].tooltip.clone();
            }
            gui.text(&l.items[idx].text, x + 1.0, y + 2.0, if sel || hovered { 0xFFFF_FF00 } else { 0xFFAA_AAAA });
        }
        if let Some(t) = tip {
            let lines: Vec<String> = text::wrap(gui, &t, 200.0).iter().map(text::line_legacy).collect();
            crate::screens::tooltip(gui, &lines, mx, my);
        }
    } else if !ui.usage.is_empty() {
        let w = ui.usage.iter().map(|u| gui.text_width(u)).fold(0.0, f32::max);
        let x = if ui.usage_x > 0.0 {
            (4.0 + gui.text_width(&ui.ed.text[..char_floor(&ui.ed.text, ui.usage_x as usize)])).clamp(0.0, (gui.width - w).max(0.0))
        } else {
            0.0
        };
        let uw = if ui.usage_x > 0.0 { w } else { gui.width };
        let _ = ui.usage_w;
        for (i, u) in ui.usage.iter().enumerate() {
            let y = h - 27.0 - 12.0 * i as f32;
            gui.fill(x - 1.0, y, x + uw + 1.0, y + 12.0, FILL);
            gui.text(u, x, y + 2.0, 0xFFFF_FFFF);
        }
    }
    // A hover event's tooltip, or the message tag's text.
    if ui.confirm.is_none() {
        let hover = style_at(g, gui, ui.scroll, mx, my).and_then(|(mi, st)| g.talk.msgs.get(mi).and_then(|m| m.rich.events_of(&st)).and_then(|e| e.hover.clone()));
        if let Some(hv) = hover {
            let lines = hover_lines(gui, &hv);
            crate::screens::tooltip(gui, &lines, mx, my);
        } else if let Some(t) = tag_tip {
            let lines: Vec<String> = text::wrap(gui, &t, 210.0).iter().map(text::line_legacy).collect();
            crate::screens::tooltip(gui, &lines, mx, my);
        }
    }
    if let Some(c) = ui.confirm.as_ref() {
        draw_confirm(gui, c, mx, my);
    }
}

fn confirm_buttons(gui: &Gui, c: &Confirm) -> Vec<crate::screens::Button> {
    let (cx, y) = (gui.width / 2.0, confirm_layout(gui, c).1);
    match c {
        Confirm::Url(_) => alloc::vec![
            crate::screens::Button::new(cx - 154.0, y, 100.0, crate::lang::get("gui.yes")),
            crate::screens::Button::new(cx - 50.0, y, 100.0, crate::lang::get("chat.copy")),
            crate::screens::Button::new(cx + 54.0, y, 100.0, crate::lang::get("gui.no")),
        ],
        // openSendConfirmationWindow: "Copy to Clipboard" for a command that would sign chat,
        // "Run Command" otherwise; "Back" to the chat screen.
        Confirm::Command(_, key) => alloc::vec![
            crate::screens::Button::new(
                cx - 152.0,
                y,
                150.0,
                crate::lang::get(if *key == SIGNATURE_REQUIRED { "chat.copy" } else { "multiplayer.confirm_command.run_command" })
            ),
            crate::screens::Button::new(cx + 2.0, y, 150.0, crate::lang::get("gui.back")),
        ],
    }
}

/// ConfirmScreen's layout: a column 8 apart, centred on the screen: the title, the message
/// (wrapped at width - 50, centred), ConfirmLinkScreen's warning, then the buttons 16 lower.
/// The lines with their y and colour, and the buttons' y.
fn confirm_layout(gui: &Gui, c: &Confirm) -> (Vec<(String, f32, u32)>, f32) {
    let mut blocks: Vec<Vec<(String, u32)>> = Vec::new();
    match c {
        Confirm::Url(u) => {
            blocks.push(alloc::vec![(crate::lang::get("chat.link.confirm"), 0xFFFF_FFFF)]);
            blocks.push(text::wrap(gui, &Rich::plain(u), gui.width - 50.0).iter().map(|l| (text::line_legacy(l), 0xFFFF_FFFF)).collect());
            blocks.push(alloc::vec![(crate::lang::get("chat.link.warning"), 0xFFFF_CCCC)]);
        }
        Confirm::Command(cmd, key) => {
            blocks.push(alloc::vec![(crate::lang::get("multiplayer.confirm_command.title"), 0xFFFF_FFFF)]);
            let msg = crate::lang::format(key, &[alloc::format!("\u{a7}e{}\u{a7}r", cmd)]);
            blocks.push(text::wrap(gui, &Rich::from_legacy(&msg), gui.width - 50.0).iter().map(|l| (text::line_legacy(l), 0xFFFF_FFFF)).collect());
        }
    }
    let text_h: f32 = blocks.iter().map(|b| b.len() as f32 * 9.0 + 8.0).sum();
    let h = text_h + 16.0 + 20.0;
    let mut y = ((gui.height - h) / 2.0).max(4.0);
    let mut lines = Vec::new();
    for b in blocks {
        for (l, c) in b {
            lines.push((l, y, c));
            y += 9.0;
        }
        y += 8.0;
    }
    (lines, y + 16.0)
}

fn draw_confirm(gui: &mut Gui, c: &Confirm, mx: f32, my: f32) {
    crate::screens::dim(gui);
    let (lines, _) = confirm_layout(gui, c);
    for (l, y, col) in lines.iter() {
        gui.text_centered(l, gui.width / 2.0, *y, *col);
    }
    for b in confirm_buttons(gui, c) {
        b.draw(gui, mx, my);
    }
}

/// Hud.extractTitle and extractOverlayMessage.
fn titles(gui: &mut Gui, g: &Game, partial: f32) {
    let t = &g.talk;
    let (w, h) = (gui.width, gui.height);
    if let Some((r, ticks)) = t.overlay.as_ref() {
        let f = *ticks as f32 - partial;
        let a = ((f * 255.0 / 20.0) as i32).min(255);
        if a > 8 {
            let s = r.legacy();
            let tw = gui.text_width(&s);
            gui.text(&s, w / 2.0 - tw / 2.0, h - 68.0 - 4.0, (a as u32) << 24 | 0xFFFFFF);
        }
    }
    if t.title.is_some() && t.title_ticks > 0 {
        let (fi, st, fo) = t.title_times;
        let f = t.title_ticks as f32 - partial;
        let mut a = 255i32;
        if t.title_ticks > fo + st {
            a = (((fi + st + fo) as f32 - f) * 255.0 / fi.max(1) as f32) as i32;
        }
        if t.title_ticks <= fo {
            a = (f * 255.0 / fo.max(1) as f32) as i32;
        }
        let a = a.clamp(0, 255);
        if a > 8 {
            let col = (a as u32) << 24 | 0xFFFFFF;
            let old = gui.text_scale;
            if let Some(r) = t.title.as_ref() {
                let s = r.legacy();
                gui.text_scale = 4.0;
                let tw = gui.text_width(&s) * 4.0;
                gui.text(&s, w / 2.0 - tw / 2.0, h / 2.0 - 40.0, col);
            }
            if let Some(r) = t.subtitle.as_ref() {
                let s = r.legacy();
                gui.text_scale = 2.0;
                let tw = gui.text_width(&s) * 2.0;
                gui.text(&s, w / 2.0 - tw / 2.0, h / 2.0 + 10.0, col);
            }
            gui.text_scale = old;
        }
    }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut c = ChatUi::default();
    c.ed.insert("say h\u{e9}llo w\u{f6}rld");
    c.ed.select_word(7);
    out.push(("chat word select", String::from(c.ed.selected()), "h\u{e9}llo"));
    c.ed.insert("\u{a7}x\ny");
    out.push(("chat input filter", c.ed.text.clone(), "say xy w\u{f6}rld"));
    out
}
