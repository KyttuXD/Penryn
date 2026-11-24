//! Books as vanilla shows them: BookViewScreen (written books the server opens, and the
//! lectern's LecternScreen), BookEditScreen (a book and quill: typing pages, turning and adding
//! them, ServerboundEditBookPacket when done) and BookSignScreen (the title, signing).

use crate::client::{Container, Game, Sound, SoundRef};
use crate::gui::Gui;
use crate::lang;
use crate::math::FloorExt;
use crate::screens::{self, Button};
use crate::textedit::{self, Filter, Edit as TEdit};
use crate::wire::{In, Out};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const C_OPEN_BOOK: i32 = 59;
pub const S_EDIT_BOOK: i32 = 24;

const BLACK: u32 = 0xFF00_0000;
const PAGE_W: f32 = 114.0;
const PAGE_LINES: usize = 14;
const PAGE_CHARS: usize = 1024;

pub struct Book {
    pub pages: Vec<String>,
    pub page: usize,
    /// A book and quill being written: its inventory slot (0-8, 40 the off hand), whether
    /// the sign screen is up, its title, whether anything changed.
    pub edit: Option<Edit>,
}

pub struct Edit {
    pub slot: i32,
    pub signing: bool,
    pub title: String,
    pub changed: bool,
    /// The page being written as a text field (cursor, selection; `ed_page` is the page it holds), the title likewise.
    pub ed: TEdit,
    pub ed_page: usize,
    pub ted: TEdit,
    /// A mouse drag is going on: 1 in the page, 2 in the title. The time and place of the last click (a double click).
    pub drag: u8,
    pub last_click: (u64, usize),
}

/// WritableBookContent: pages of raw text (each a Filterable string).
pub fn writable_pages(v: &[u8]) -> Option<Vec<String>> {
    let mut r = In::new(v);
    let n = r.varint()?;
    let mut out = Vec::new();
    for _ in 0..n.clamp(0, 100) {
        out.push(r.string()?);
        if r.bool()? {
            r.string()?;
        }
    }
    Some(out)
}

/// WrittenBookContent: title, author, generation, then pages of text components.
pub fn written_pages(v: &[u8]) -> Option<(String, String, Vec<String>)> {
    let mut r = In::new(v);
    let title = r.string()?;
    if r.bool()? {
        r.string()?;
    }
    let author = r.string()?;
    let _generation = r.varint()?;
    let n = r.varint()?;
    let mut pages = Vec::new();
    for _ in 0..n.clamp(0, 1000) {
        pages.push(crate::nbt::text(&mut r));
        if r.bool()? {
            crate::nbt::skip_root(&mut r)?;
        }
    }
    Some((title, author, pages))
}

/// The pages of a book stack's patch: written, else writable.
fn pages_of(patch: &[u8]) -> Vec<String> {
    if let Some(v) = crate::menus::component(patch, 57) {
        if let Some((_, _, p)) = written_pages(v) {
            return p;
        }
    }
    crate::menus::component(patch, 56).and_then(writable_pages).unwrap_or_default()
}

/// The hand's book (ClientboundOpenBookPacket's hand: 0 main, 1 off).
pub fn open_view(g: &Game, hand: i32) -> Option<Book> {
    let slot = if hand == 1 { 45 } else { 36 + g.selected as usize };
    g.inventory[slot]?;
    Some(Book { pages: pages_of(&g.creative.patches[slot]), page: 0, edit: None })
}

/// A book and quill in the main hand, opened to write (LocalPlayer.openItemGui).
pub fn open_edit(g: &Game) -> Option<Book> {
    let (item, _) = g.held()?;
    if crate::menus::item_name(g, item) != "writable_book" {
        return None;
    }
    let slot = 36 + g.selected as usize;
    let mut pages = crate::menus::component(&g.creative.patches[slot], 56).and_then(writable_pages).unwrap_or_default();
    if pages.is_empty() {
        pages.push(String::new());
    }
    Some(Book { pages, page: 0, edit: Some(Edit { slot: g.selected, signing: false, title: String::new(), changed: false, ed: TEdit::new(PAGE_CHARS, Filter::Multi), ed_page: usize::MAX, ted: TEdit::new(15, Filter::Line), drag: 0, last_click: (0, usize::MAX) }) })
}

/// Items a right click opens even when it hits a block (they have no block use).
pub fn used_in_air(item: &str) -> bool {
    matches!(item, "writable_book" | "written_book")
}

/// Text split into the page's lines (newlines, then wrapped to its width).
fn lines(gui: &Gui, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in text.split('\n') {
        if part.is_empty() {
            out.push(String::new());
        } else {
            out.extend(screens::wrap(gui, part, PAGE_W));
        }
    }
    out
}

/// The byte range of each line `lines` shows, in the page's text (the wrapped lines are pieces of it, a space where
/// they were split is not in either).
fn spans(gui: &Gui, text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut off = 0;
    for part in text.split('\n') {
        if part.is_empty() {
            out.push((off, off));
        } else {
            let mut pos = 0;
            for l in screens::wrap(gui, part, PAGE_W) {
                match part[pos..].find(l.as_str()) {
                    Some(i) => {
                        out.push((off + pos + i, off + pos + i + l.len()));
                        pos += i + l.len();
                    }
                    None => out.push((off + pos, off + pos)),
                }
            }
        }
        off += part.len() + 1;
    }
    out
}

/// The line the caret at byte `c` is on.
fn caret_line(sp: &[(usize, usize)], c: usize) -> usize {
    sp.iter().rposition(|s| s.0 <= c).unwrap_or(0)
}

/// The caret a line up (`dir` -1) or down (1) from `c`, at the same x; None on the first or last line.
fn vertical(text: &str, sp: &[(usize, usize)], c: usize, dir: i32, width: &dyn Fn(&str) -> f32) -> Option<usize> {
    let i = caret_line(sp, c);
    let (s, e) = *sp.get(i)?;
    let x = width(&text[s..c.clamp(s, e)]);
    let j = if dir < 0 { i.checked_sub(1)? } else { i + 1 };
    let (s2, e2) = *sp.get(j)?;
    Some(s2 + textedit::index_at(&text[s2..e2], x, width))
}

/// Types or pastes into the page: what is allowed (the Edit's limit of 1024, its filter), and as much of it as keeps the
/// page within its lines (`fits` says whether a page text does); the rest is dropped, the selection is replaced.
fn insert_fit(e: &mut TEdit, s: &str, fits: &dyn Fn(&str) -> bool) {
    let clean: Vec<char> = e.clean(&s.replace("\r\n", "\n").replace('\r', "\n")).chars().take(PAGE_CHARS).collect();
    let try_n = |e: &TEdit, n: usize| -> Option<TEdit> {
        let mut t = TEdit::new(e.max, e.filter);
        t.text = e.text.clone();
        t.cursor = e.cursor;
        t.anchor = e.anchor;
        let part: String = clean[..n].iter().collect();
        t.insert(&part);
        if fits(&t.text) {
            Some(t)
        } else {
            None
        }
    };
    if clean.is_empty() {
        return;
    }
    if let Some(t) = try_n(e, clean.len()) {
        *e = t;
        return;
    }
    // The longest prefix that fits (more text never needs fewer lines).
    let (mut lo, mut hi) = (0, clean.len());
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if try_n(e, mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if lo > 0 {
        if let Some(t) = try_n(e, lo) {
            *e = t;
        }
    }
}

/// The page's field holds the current page's text (a page turned to starts with the caret at its end).
fn sync(b: &mut Book) {
    let Some(e) = b.edit.as_mut() else { return };
    let Some(page) = b.pages.get(b.page) else { return };
    if e.ed_page != b.page || e.ed.text != *page {
        e.ed.set(page);
        e.ed_page = b.page;
    }
    if e.ted.text != e.title {
        e.ted.set(&e.title);
    }
}

/// The field's text back into the page (and the title).
fn commit(b: &mut Book) {
    let Some(e) = b.edit.as_mut() else { return };
    if e.signing {
        e.title = e.ted.text.clone();
    } else if let Some(p) = b.pages.get_mut(b.page) {
        if *p != e.ed.text {
            *p = e.ed.text.clone();
            e.changed = true;
        }
    }
}

fn left(gui: &Gui) -> f32 {
    ((gui.width - 192.0) / 2.0).floor()
}

/// The page buttons (PageButton: 23 x 13) where they are shown.
fn page_buttons(gui: &Gui, page: usize, pages: usize, can_add: bool) -> [(f32, f32, bool, bool); 2] {
    let i = left(gui);
    [(i + 116.0, 159.0, true, page + 1 < pages || can_add), (i + 43.0, 159.0, false, page > 0)]
}

fn draw_page_buttons(gui: &mut Gui, page: usize, pages: usize, can_add: bool, mx: f32, my: f32) {
    for (x, y, fwd, shown) in page_buttons(gui, page, pages, can_add) {
        if !shown {
            continue;
        }
        let over = mx >= x && mx < x + 23.0 && my >= y && my < y + 13.0;
        let s = match (fwd, over) {
            (true, false) => "widget/page_forward",
            (true, true) => "widget/page_forward_highlighted",
            (false, false) => "widget/page_backward",
            (false, true) => "widget/page_backward_highlighted",
        };
        gui.blit(s, x, y);
    }
}

fn page_hit(gui: &Gui, page: usize, pages: usize, can_add: bool, mx: f32, my: f32) -> Option<bool> {
    page_buttons(gui, page, pages, can_add).iter().find(|b| b.3 && mx >= b.0 && mx < b.0 + 23.0 && my >= b.1 && my < b.1 + 13.0).map(|b| b.2)
}

/// The book's background, page number and text.
/// `caret`: the editor's cursor, anchor (byte offsets in the page) and whether the caret is lit.
fn draw_book(gui: &mut Gui, pages: &[String], page: usize, caret: Option<(usize, usize, bool)>, dim: bool) {
    let i = left(gui);
    if dim {
        screens::dim(gui);
    }
    if let Some(a) = crate::menus::assets() {
        a.blit(gui, "bg/book", i, 2.0);
    }
    let ind = lang::format("book.pageIndicator", &[format!("{}", page + 1), format!("{}", pages.len().max(1))]);
    let w = gui.text_width(&ind);
    gui.text_plain(&ind, i - w + 192.0 - 44.0, 18.0, BLACK);
    let text = pages.get(page).map(|s| s.as_str()).unwrap_or("");
    let ls = lines(gui, text);
    let sp = spans(gui, text);
    if let Some((c, an, _)) = caret {
        let (c, an) = (c.min(text.len()), an.min(text.len()));
        // The selection: a light blue band behind each line's selected part.
        let (sa, sb) = (c.min(an), c.max(an));
        for (k, &(s, e)) in sp.iter().take(PAGE_LINES).enumerate() {
            let (a, b) = (sa.max(s), sb.min(e));
            if a < b {
                let y = 32.0 + k as f32 * 9.0;
                gui.fill(i + 36.0 + gui.text_width(&text[s..a]), y - 1.0, i + 36.0 + gui.text_width(&text[s..b]), y + 9.0, 0xFF99_CCFF);
            }
        }
    }
    for (k, l) in ls.iter().take(PAGE_LINES).enumerate() {
        gui.text_plain(l, i + 36.0, 32.0 + k as f32 * 9.0, BLACK);
    }
    if let Some((c, _, true)) = caret {
        let c = c.min(text.len());
        let k = caret_line(&sp, c).min(PAGE_LINES - 1);
        let (s, e) = sp.get(k).copied().unwrap_or((0, 0));
        let hi = if c >= text.len() { text.len().max(s) } else { c.clamp(s, e) };
        let x = gui.text_width(&text[s..hi]);
        let y = 32.0 + k as f32 * 9.0;
        if c >= text.len() {
            gui.text_plain("_", i + 36.0 + x, y, BLACK);
        } else {
            gui.fill(i + 36.0 + x, y - 1.0, i + 36.0 + x + 1.0, y + 9.0, BLACK);
        }
    }
}

fn buttons(gui: &Gui, edit: bool) -> [Button; 2] {
    let x = gui.width / 2.0;
    if edit {
        [Button::new(x - 100.0, 196.0, 98.0, lang::get("book.signButton")), Button::new(x + 2.0, 196.0, 98.0, lang::get("gui.done"))]
    } else {
        [Button::new(x - 100.0, 196.0, 200.0, lang::get("gui.done")), Button::new(x - 100.0, 196.0, 0.0, String::new()).off()]
    }
}

fn sign_buttons(gui: &Gui, title_ok: bool) -> [Button; 2] {
    let x = gui.width / 2.0;
    let mut sign = Button::new(x - 100.0, 196.0, 98.0, lang::get("book.finalizeButton"));
    sign.active = title_ok;
    [sign, Button::new(x + 2.0, 196.0, 98.0, lang::get("gui.cancel"))]
}

pub fn draw(gui: &mut Gui, b: &Book, mx: f32, my: f32, blink: bool) {
    if let Some(e) = b.edit.as_ref().filter(|e| e.signing) {
        // BookSignScreen: the title box, by-line and the warning, on the book's background.
        let i = left(gui);
        screens::dim(gui);
        if let Some(a) = crate::menus::assets() {
            a.blit(gui, "bg/book", i, 2.0);
        }
        let head = lang::get("book.editTitle");
        let cx = i + 96.0;
        gui.text_plain(&head, cx - gui.text_width(&head) / 2.0, 34.0, BLACK);
        let tx = cx - gui.text_width(&e.title) / 2.0;
        let (c, an) = if e.ted.text == e.title { (e.ted.cursor, e.ted.anchor) } else { (e.title.len(), e.title.len()) };
        if c != an {
            let (a, b) = (c.min(an), c.max(an));
            gui.fill(tx + gui.text_width(&e.title[..a]), 49.0, tx + gui.text_width(&e.title[..b]), 59.0, 0xFF99_CCFF);
        }
        gui.text_plain(&e.title, tx, 50.0, BLACK);
        if blink {
            if c >= e.title.len() {
                gui.text_plain("_", tx + gui.text_width(&e.title), 50.0, BLACK);
            } else {
                let x = tx + gui.text_width(&e.title[..c]);
                gui.fill(x, 49.0, x + 1.0, 59.0, BLACK);
            }
        }
        let by = lang::format("book.byAuthor", &[String::new()]);
        gui.text_plain(&by, cx - gui.text_width(&by) / 2.0, 60.0, 0xFF80_8080);
        for (k, l) in screens::wrap(gui, &lang::get("book.finalizeWarning"), PAGE_W).iter().enumerate() {
            gui.text_plain(l, cx - gui.text_width(l) / 2.0, 82.0 + k as f32 * 9.0, BLACK);
        }
        for bt in sign_buttons(gui, !e.title.trim().is_empty()).iter() {
            bt.draw(gui, mx, my);
        }
        return;
    }
    let caret = b.edit.as_ref().map(|e| if e.ed_page == b.page { (e.ed.cursor, e.ed.anchor, blink) } else { (usize::MAX, usize::MAX, blink) });
    draw_book(gui, &b.pages, b.page, caret, true);
    draw_page_buttons(gui, b.page, b.pages.len(), b.edit.is_some() && b.pages.len() < 100, mx, my);
    for bt in buttons(gui, b.edit.is_some()).iter().filter(|b| b.w > 0.0) {
        bt.draw(gui, mx, my);
    }
}

fn page_sound(g: &mut Game) {
    g.sounds.push(Sound { sound: SoundRef::Name(String::from("item.book.page_turn")), pos: None, volume: 1.0, pitch: 1.0, source: 10 });
}

fn ui_click(g: &mut Game) {
    g.sounds.push(Sound { sound: SoundRef::Name(String::from("ui.button.click")), pos: None, volume: 0.25, pitch: 1.0, source: 10 });
}

fn turn(g: &mut Game, b: &mut Book, forward: bool) {
    if forward {
        if b.page + 1 < b.pages.len() {
            b.page += 1;
        } else if b.edit.is_some() && b.pages.len() < 100 {
            b.pages.push(String::new());
            b.page += 1;
            if let Some(e) = b.edit.as_mut() {
                e.changed = true;
            }
        } else {
            return;
        }
    } else if b.page > 0 {
        b.page -= 1;
    } else {
        return;
    }
    page_sound(g);
}

/// BookEditScreen.saveChanges: the pages (trailing empty ones dropped) and, when signing,
/// the title.
fn save(g: &mut Game, b: &mut Book, sign: bool) -> Result<(), String> {
    let Some(e) = b.edit.as_ref() else { return Ok(()) };
    if !e.changed && !sign {
        return Ok(());
    }
    let mut pages = b.pages.clone();
    while pages.last().map_or(false, |p| p.is_empty()) {
        pages.pop();
    }
    let mut o = Out::new();
    o.varint(e.slot).varint(pages.len() as i32);
    for p in pages.iter() {
        o.string(p);
    }
    if sign {
        o.bool(true).string(e.title.trim());
    } else {
        o.bool(false);
    }
    if let Some(e) = b.edit.as_mut() {
        e.changed = false;
    }
    // BookEditScreen.saveChanges also changes the stack in hand at once.
    if !sign {
        let mut v = Out::new();
        v.varint(pages.len() as i32);
        for p in pages.iter() {
            v.string(p).bool(false);
        }
        let i = if e_slot(b) == 40 { 45 } else { 36 + e_slot(b) as usize };
        if let Some(p) = g.creative.patches.get_mut(i) {
            *p = crate::menus::set_component(p, 56, &v.b);
        }
    }
    g.conn.send(S_EDIT_BOOK, &o)
}

fn e_slot(b: &Book) -> i32 {
    b.edit.as_ref().map_or(0, |e| e.slot)
}

/// A click; true when the book closes.
pub fn click(gui: &Gui, g: &mut Game, b: &mut Book, mx: f32, my: f32) -> Result<bool, String> {
    if let Some(e) = b.edit.as_mut().filter(|e| e.signing) {
        let bs = sign_buttons(gui, !e.title.trim().is_empty());
        if bs[0].hit(mx, my) {
            ui_click(g);
            save(g, b, true)?;
            return Ok(true);
        }
        if bs[1].hit(mx, my) {
            ui_click(g);
            e.signing = false;
        } else {
            click_text(gui, b, mx, my);
        }
        return Ok(false);
    }
    if click_text(gui, b, mx, my) {
        return Ok(false);
    }
    if let Some(fwd) = page_hit(gui, b.page, b.pages.len(), b.edit.is_some() && b.pages.len() < 100, mx, my) {
        turn(g, b, fwd);
        return Ok(false);
    }
    let bs = buttons(gui, b.edit.is_some());
    if b.edit.is_some() && bs[0].hit(mx, my) {
        ui_click(g);
        if let Some(e) = b.edit.as_mut() {
            e.signing = true;
        }
        return Ok(false);
    }
    let done = if b.edit.is_some() { &bs[1] } else { &bs[0] };
    if done.hit(mx, my) {
        ui_click(g);
        save(g, b, false)?;
        return Ok(true);
    }
    Ok(false)
}

/// A key; true when the book closes (Escape saves first, as vanilla's onClose). `keys` are the held keys (the command
/// key, shift), `v` the clipboard.
pub fn key(g: &mut Game, gui: &Gui, b: &mut Book, sc: usize, keys: &[bool], v: &crate::sdl::Video) -> Result<bool, String> {
    const K_RETURN: usize = 40;
    const K_ESC: usize = 41;
    const K_PAGE_UP: usize = 75;
    const K_PAGE_DOWN: usize = 78;
    const K_DOWN: usize = 81;
    const K_UP: usize = 82;
    if sc == K_ESC {
        if let Some(e) = b.edit.as_mut().filter(|e| e.signing) {
            e.signing = false;
            return Ok(false);
        }
        save(g, b, false)?;
        return Ok(true);
    }
    match sc {
        K_PAGE_UP => turn(g, b, false),
        K_PAGE_DOWN => turn(g, b, true),
        _ => {}
    }
    sync(b);
    let (command, shift) = (textedit::command(keys), textedit::shift(keys));
    let Some(e) = b.edit.as_mut() else { return Ok(false) };
    if e.signing {
        if sc == K_RETURN && !e.title.trim().is_empty() {
            save(g, b, true)?;
            return Ok(true);
        }
        e.ted.key(sc, command, shift, &mut || v.clipboard(), &mut |s| v.set_clipboard(s));
        commit(b);
        return Ok(false);
    }
    let width = |t: &str| gui.text_width(t);
    let fits = |t: &str| lines(gui, t).len() <= PAGE_LINES;
    let text = e.ed.text.clone();
    let c = e.ed.cursor;
    match sc {
        K_RETURN => insert_fit(&mut e.ed, "\n", &fits),
        K_UP | K_DOWN => {
            let sp = spans(gui, &text);
            if let Some(to) = vertical(&text, &sp, c, if sc == K_UP { -1 } else { 1 }, &width) {
                e.ed.click(to, shift);
            }
        }
        textedit::SC_HOME | textedit::SC_END => {
            let sp = spans(gui, &text);
            let (s0, e0) = sp.get(caret_line(&sp, c)).copied().unwrap_or((0, 0));
            let to = match (sc == textedit::SC_HOME, command) {
                (true, true) => 0,
                (false, true) => text.len(),
                (true, false) => s0,
                (false, false) => e0,
            };
            e.ed.click(to, shift);
        }
        textedit::SC_V if command => insert_fit(&mut e.ed, &v.clipboard(), &fits),
        _ => {
            e.ed.key(sc, command, shift, &mut || String::new(), &mut |s| v.set_clipboard(s));
        }
    }
    commit(b);
    Ok(false)
}

/// Typed text: into the title while signing (15 characters, BookSignScreen's box), else the page, as much as still fits
/// on the page (1024 characters, 14 lines).
pub fn text(gui: &Gui, b: &mut Book, s: &str) {
    sync(b);
    let Some(e) = b.edit.as_mut() else { return };
    if e.signing {
        e.ted.insert(s);
    } else {
        insert_fit(&mut e.ed, s, &|t: &str| lines(gui, t).len() <= PAGE_LINES);
    }
    commit(b);
}

/// The caret in the page for a mouse position (GUI units): the nearest character boundary of the line there.
fn page_caret(gui: &Gui, text: &str, mx: f32, my: f32) -> Option<usize> {
    let sp = spans(gui, text);
    let k = (((my - 32.0) / 9.0).floor().max(0.0) as usize).min(sp.len().checked_sub(1)?).min(PAGE_LINES - 1);
    let (s, e) = sp[k];
    Some(s + textedit::index_at(&text[s..e], mx - (left(gui) + 36.0), &|t: &str| gui.text_width(t)))
}

/// The caret in the title for a mouse x.
fn title_caret(gui: &Gui, title: &str, mx: f32) -> usize {
    let tx = left(gui) + 96.0 - gui.text_width(title) / 2.0;
    textedit::index_at(title, mx - tx, &|t: &str| gui.text_width(t))
}

/// A left click on the page text or the title: the caret goes to the nearest character boundary, a double click
/// selects the word, and a drag selects on (`drag`). True when it hit the text.
fn click_text(gui: &Gui, b: &mut Book, mx: f32, my: f32) -> bool {
    sync(b);
    let page = b.page;
    let Some(e) = b.edit.as_mut() else { return false };
    let i = left(gui);
    let (area, which) = if e.signing {
        (mx >= i + 20.0 && mx < i + 172.0 && my >= 47.0 && my < 60.0, 2)
    } else {
        (mx >= i + 36.0 && mx < i + 36.0 + PAGE_W && my >= 32.0 && my < 32.0 + PAGE_LINES as f32 * 9.0, 1)
    };
    if !area {
        return false;
    }
    let now = crate::sys::now_ms();
    let double = e.last_click.1 == which + page * 4 && now < e.last_click.0 + 400;
    let field = if which == 2 { &mut e.ted } else { &mut e.ed };
    let at = if which == 2 { title_caret(gui, &field.text, mx) } else { page_caret(gui, &field.text, mx, my).unwrap_or(field.text.len()) };
    if double {
        field.select_word(at);
        e.last_click = (0, usize::MAX);
    } else {
        field.click(at, false);
        e.last_click = (now, which + page * 4);
    }
    e.drag = which as u8;
    true
}

/// The mouse moved with the left button `held`: a drag that began in the text selects on.
pub fn drag(gui: &Gui, b: &mut Book, mx: f32, my: f32, held: bool) {
    let Some(e) = b.edit.as_mut() else { return };
    if !held {
        e.drag = 0;
    }
    match e.drag {
        1 => {
            if let Some(at) = page_caret(gui, &e.ed.text, mx, my) {
                e.ed.click(at, true);
            }
        }
        2 => {
            let at = title_caret(gui, &e.ted.text, mx);
            e.ted.click(at, true);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ the lectern

fn lectern_pages(c: &Container) -> Vec<String> {
    c.patches.first().map(|p| pages_of(p)).unwrap_or_default()
}

fn lectern_buttons(gui: &Gui, g: &Game) -> [Button; 2] {
    let x = gui.width / 2.0;
    if g.game_mode == 0 || g.game_mode == 1 {
        [Button::new(x - 100.0, 196.0, 98.0, lang::get("gui.done")), Button::new(x + 2.0, 196.0, 98.0, lang::get("lectern.take_book"))]
    } else {
        [Button::new(x - 100.0, 196.0, 200.0, lang::get("gui.done")), Button::new(x, 196.0, 0.0, String::new()).off()]
    }
}

pub fn draw_lectern(gui: &mut Gui, g: &Game, c: &Container, mx: f32, my: f32) {
    let pages = lectern_pages(c);
    let page = (g.menus.data[0].max(0) as usize).min(pages.len().saturating_sub(1));
    draw_book(gui, &pages, page, None, false);
    draw_page_buttons(gui, page, pages.len(), false, mx, my);
    for b in lectern_buttons(gui, g).iter().filter(|b| b.w > 0.0) {
        b.draw(gui, mx, my);
    }
}

/// LecternScreen's buttons: 1 previous page, 2 next page, 3 take the book.
pub fn lectern_click(gui: &Gui, g: &mut Game, mx: f32, my: f32, button: u8) -> Result<bool, String> {
    if button != 1 {
        return Ok(true);
    }
    let Some(c) = g.container.as_ref() else { return Ok(true) };
    let pages = lectern_pages(c).len();
    let cid = c.id;
    let page = (g.menus.data[0].max(0) as usize).min(pages.saturating_sub(1));
    if let Some(fwd) = page_hit(gui, page, pages, false, mx, my) {
        crate::menus::button(g, if fwd { 2 } else { 1 })?;
        page_sound(g);
        return Ok(true);
    }
    let bs = lectern_buttons(gui, g);
    if bs[0].hit(mx, my) {
        ui_click(g);
        g.close_container(cid)?;
    } else if bs[1].hit(mx, my) {
        ui_click(g);
        crate::menus::button(g, 3)?;
    }
    Ok(true)
}

pub fn lectern_key(g: &mut Game, sc: usize) -> Result<bool, String> {
    match sc {
        75 => crate::menus::button(g, 1)?,
        78 => crate::menus::button(g, 2)?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut v = Vec::new();
    // Pasting into a page: cut where the page's lines run out (here 3 lines), the rest dropped; the limit is in UTF-16.
    let fits = |t: &str| t.split('\n').count() <= 3;
    let mut e = TEdit::new(PAGE_CHARS, Filter::Multi);
    insert_fit(&mut e, "one\r\ntwo\nthree\nfour\nfive", &fits);
    v.push(("book paste is cut at the line limit", e.text.replace('\n', "|"), "one|two|three"));
    let mut e = TEdit::new(PAGE_CHARS, Filter::Multi);
    e.set("ab");
    insert_fit(&mut e, "\n\n\n\n", &fits);
    v.push(("book enter past the last line does nothing more", e.text.replace('\n', "|"), "ab||"));
    let mut e = TEdit::new(5, Filter::Multi);
    insert_fit(&mut e, "ab\u{1F600}cdef", &fits);
    v.push(("book paste is cut at the character limit", e.text.clone(), "ab\u{1F600}c"));
    // Up and down keep the x (every character 6 px), a wrapped line is a span of the text.
    let w6 = |t: &str| t.chars().count() as f32 * 6.0;
    let text = "hello wor ld\n\nxy";
    let sp = [(0usize, 9usize), (10, 12), (13, 13), (14, 16)];
    v.push(("book caret down keeps x", format!("{:?}", vertical(text, &sp, 4, 1, &w6)), "Some(12)"));
    v.push(("book caret down onto a short line", format!("{:?} {:?}", vertical(text, &sp, 8, 1, &w6), vertical(text, &sp, 8, -1, &w6)), "Some(12) None"));
    v.push(("book caret line", format!("{} {} {}", caret_line(&sp, 0), caret_line(&sp, 11), caret_line(&sp, 15)), "0 1 3"));
    let mut o = Out::new();
    o.varint(2).string("Hello").bool(false).string("World").bool(true).string("W*rld");
    v.push(("writable book pages", format!("{:?}", writable_pages(&o.b)), "Some([\"Hello\", \"World\"])"));
    let mut o = Out::new();
    // title, no filtered title, author, generation 0, one page as the NBT string "Hi", resolved
    o.string("Tales").bool(false).string("Containers").varint(0).varint(1).u8(8).u16(2).bytes(b"Hi").bool(false).bool(true);
    v.push(("written book content", format!("{:?}", written_pages(&o.b)), "Some((\"Tales\", \"Containers\", [\"Hi\"]))"));
    // A potion stack's potion_contents (a holder, no colour, one custom effect with a hidden
    // one, no name) is read past, so a brewing stand's slots after it stay readable.
    let mut o = Out::new();
    o.varint(1).varint(900).varint(1).varint(0).varint(53).bool(true).varint(5).bool(false);
    o.varint(1).varint(1).varint(0).varint(200).bool(false).bool(true).bool(true).bool(true).varint(0).varint(100).bool(false).bool(true).bool(true).bool(false).bool(false);
    o.u8(0xAA);
    let mut r = In::new(&o.b);
    let got = crate::creative::read_stack(&mut r).map(|(item, ok, _)| format!("{:?} {} {:?}", item, ok, r.u8()));
    v.push(("potion stack read past", got.unwrap_or_default(), "Some((900, 1)) true Some(170)"));
    v
}
