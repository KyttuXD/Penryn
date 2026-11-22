//! Books as vanilla shows them: BookViewScreen (written books the server opens, and the
//! lectern's LecternScreen), BookEditScreen (a book and quill: typing pages, turning and adding
//! them, ServerboundEditBookPacket when done) and BookSignScreen (the title, signing).

use crate::client::{Container, Game, Sound, SoundRef};
use crate::gui::Gui;
use crate::lang;
use crate::math::FloorExt;
use crate::screens::{self, Button};
use crate::wire::{In, Out};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const C_OPEN_BOOK: i32 = 59;
pub const S_EDIT_BOOK: i32 = 24;

const BLACK: u32 = 0xFF00_0000;
const PAGE_W: f32 = 114.0;
const PAGE_LINES: usize = 14;

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
    Some(Book { pages, page: 0, edit: Some(Edit { slot: g.selected, signing: false, title: String::new(), changed: false }) })
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
fn draw_book(gui: &mut Gui, pages: &[String], page: usize, cursor: bool, dim: bool) {
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
    for (k, l) in ls.iter().take(PAGE_LINES).enumerate() {
        gui.text_plain(l, i + 36.0, 32.0 + k as f32 * 9.0, BLACK);
    }
    if cursor {
        let k = ls.len().saturating_sub(1).min(PAGE_LINES - 1);
        let x = ls.last().map_or(0.0, |l| gui.text_width(l));
        let y = 32.0 + k as f32 * 9.0;
        if text.ends_with('\n') {
            gui.text_plain("_", i + 36.0, y + 9.0, BLACK);
        } else {
            gui.text_plain("_", i + 36.0 + x, y, BLACK);
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
        let mut t = e.title.clone();
        if blink {
            t.push('_');
        }
        gui.text_plain(&t, cx - gui.text_width(&e.title) / 2.0, 50.0, BLACK);
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
    draw_book(gui, &b.pages, b.page, b.edit.is_some() && blink, true);
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
        }
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

/// A key; true when the book closes (Escape saves first, as vanilla's onClose).
pub fn key(g: &mut Game, b: &mut Book, sc: usize) -> Result<bool, String> {
    const K_RETURN: usize = 40;
    const K_ESC: usize = 41;
    const K_BACKSPACE: usize = 42;
    const K_PAGE_UP: usize = 75;
    const K_PAGE_DOWN: usize = 78;
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
    let Some(e) = b.edit.as_mut() else { return Ok(false) };
    if e.signing {
        if sc == K_BACKSPACE {
            e.title.pop();
        } else if sc == K_RETURN && !e.title.trim().is_empty() {
            save(g, b, true)?;
            return Ok(true);
        }
        return Ok(false);
    }
    let page = &mut b.pages[b.page];
    match sc {
        K_BACKSPACE => {
            if page.pop().is_some() {
                e.changed = true;
            }
        }
        K_RETURN => {
            page.push('\n');
            e.changed = true;
        }
        _ => {}
    }
    Ok(false)
}

/// Typed text: into the title while signing (16 characters), else the page, while it still
/// fits on the page (1024 characters, 14 lines).
pub fn text(gui: &Gui, b: &mut Book, s: &str) {
    let Some(e) = b.edit.as_mut() else { return };
    for ch in s.chars().filter(|c| *c >= ' ' && *c != '\u{a7}') {
        if e.signing {
            if e.title.chars().count() < 16 {
                e.title.push(ch);
            }
            continue;
        }
        let page = &mut b.pages[b.page];
        if page.chars().count() >= 1024 {
            break;
        }
        page.push(ch);
        if lines(gui, page).len() > PAGE_LINES {
            page.pop();
            break;
        }
        e.changed = true;
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
    draw_book(gui, &pages, page, false, false);
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
    let mut o = Out::new();
    o.varint(2).string("Hello").bool(false).string("World").bool(true).string("W*rld");
    v.push(("writable book pages", format!("{:?}", writable_pages(&o.b)), "Some([\"Hello\", \"World\"])"));
    let mut o = Out::new();
    // title, no filtered title, author, generation 0, one page as the NBT string "Hi", resolved
    o.string("Tales").bool(false).string("Containers").varint(0).varint(1).u8(8).u16(2).bytes(b"Hi").bool(false).bool(true);
    v.push(("written book content", format!("{:?}", written_pages(&o.b)), "Some((\"Tales\", \"Containers\", [\"Hi\"]))"));
    v
}
