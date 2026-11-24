//! Network NBT (a nameless root tag), read only far enough to skip it or to get the plain text
//! of a chat component.

use crate::wire::In;
use alloc::string::String;

/// Skips one tag payload of type `t`. `None` on malformed data.
fn skip(r: &mut In, t: u8, depth: u32) -> Option<()> {
    if depth > 64 {
        return None;
    }
    match t {
        0 => {}
        1 => {
            r.take(1)?;
        }
        2 => {
            r.take(2)?;
        }
        3 | 5 => {
            r.take(4)?;
        }
        4 | 6 => {
            r.take(8)?;
        }
        7 => {
            let n = r.i32()?;
            r.take(n.max(0) as usize)?;
        }
        8 => {
            let n = r.u16()?;
            r.take(n as usize)?;
        }
        9 => {
            let et = r.u8()?;
            let n = r.i32()?;
            // A list of End tags holds nothing: a count there would only spin.
            if et == 0 && n > 0 {
                return None;
            }
            for _ in 0..n.max(0) {
                skip(r, et, depth + 1)?;
            }
        }
        10 => loop {
            let ct = r.u8()?;
            if ct == 0 {
                break;
            }
            let n = r.u16()?;
            r.take(n as usize)?;
            skip(r, ct, depth + 1)?;
        },
        11 => {
            let n = r.i32()?;
            r.take(4 * n.max(0) as usize)?;
        }
        12 => {
            let n = r.i32()?;
            r.take(8 * n.max(0) as usize)?;
        }
        _ => return None,
    }
    Some(())
}

/// Skips a network NBT value (type byte, then payload).
/// A dimension type's NBT (the registry's entry): its min_y and height, the defaults when a
/// field is missing. None on malformed data.
pub fn dimension_height(r: &mut In) -> Option<(i32, i32)> {
    let t = r.u8()?;
    if t != 10 {
        skip(r, t, 0)?;
        return None;
    }
    let (mut min_y, mut height) = (-64, 384);
    loop {
        let tt = r.u8()?;
        if tt == 0 {
            break;
        }
        let name = mstr(r)?;
        match (name.as_str(), tt) {
            ("min_y", 3) => min_y = r.i32()?,
            ("height", 3) => height = r.i32()?,
            _ => skip(r, tt, 1)?,
        }
    }
    Some((min_y, height))
}

pub fn skip_root(r: &mut In) -> Option<()> {
    let t = r.u8()?;
    skip(r, t, 0)
}

fn mstr(r: &mut In) -> Option<String> {
    let n = r.u16()? as usize;
    // Modified UTF-8 is UTF-8 for everything chat uses.
    Some(String::from_utf8_lossy(r.take(n)?).into_owned())
}

/// A component colour as a formatting code: "§c" for the named colours, "§#RRGGBB" for others.
fn colour_code(name: &str) -> Option<String> {
    const NAMES: [&str; 16] = [
        "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue",
        "green", "aqua", "red", "light_purple", "yellow", "white",
    ];
    if let Some(i) = NAMES.iter().position(|n| *n == name) {
        return Some(alloc::format!("\u{a7}{}", char::from_digit(i as u32, 16).unwrap()));
    }
    if name.len() == 7 && name.starts_with('#') {
        return Some(alloc::format!("\u{a7}{}", name));
    }
    None
}

/// A component's visible text, assembled as vanilla does: its own text (or its translation
/// with the "with" arguments in place), then its "extra" children, with its colour as
/// formatting codes the GUI font understands.
fn text_of(r: &mut In, t: u8, out: &mut String, depth: u32) -> Option<()> {
    text_in(r, t, out, depth, "")
}

fn text_in(r: &mut In, t: u8, out: &mut String, depth: u32, parent: &str) -> Option<()> {
    if depth > 32 {
        return None;
    }
    match t {
        8 => out.push_str(&mstr(r)?),
        // Numbers given as translation arguments ("%s" with an int).
        1 => out.push_str(&alloc::format!("{}", r.u8()? as i8)),
        2 => out.push_str(&alloc::format!("{}", r.u16()? as i16)),
        3 => out.push_str(&alloc::format!("{}", r.i32()?)),
        4 => out.push_str(&alloc::format!("{}", r.i64()?)),
        5 => out.push_str(&alloc::format!("{}", r.f32()?)),
        6 => out.push_str(&alloc::format!("{}", r.f64()?)),
        9 => {
            let et = r.u8()?;
            let n = r.i32()?;
            if et == 0 && n > 0 {
                return None;
            }
            for _ in 0..n.max(0) {
                text_of(r, et, out, depth + 1)?;
            }
        }
        10 => {
            let (mut text, mut key, mut fallback) = (None, None, None);
            let mut args: alloc::vec::Vec<(u8, alloc::vec::Vec<u8>)> = alloc::vec::Vec::new();
            let mut extras: alloc::vec::Vec<(u8, alloc::vec::Vec<u8>)> = alloc::vec::Vec::new();
            let mut colour = String::new();
            loop {
                let ct = r.u8()?;
                if ct == 0 {
                    break;
                }
                let name = mstr(r)?;
                match (name.as_str(), ct) {
                    ("text", 8) | ("", 8) => text = Some(mstr(r)?),
                    ("translate", 8) => key = Some(mstr(r)?),
                    ("fallback", 8) => fallback = Some(mstr(r)?),
                    ("color", 8) => colour = colour_code(&mstr(r)?).unwrap_or_default(),
                    // Children are read after the colour is known: keep their bytes.
                    ("with", _) | ("extra", _) => {
                        let start = r.pos;
                        skip(r, ct, depth + 1)?;
                        let bytes = r.b[start..r.pos].to_vec();
                        if name == "with" { args.push((ct, bytes)) } else { extras.push((ct, bytes)) }
                    }
                    _ => skip(r, ct, depth + 1)?,
                }
            }
            let mine = if colour.is_empty() { String::from(parent) } else { colour.clone() };
            let render = |ct: u8, bytes: &[u8], each: bool| -> alloc::vec::Vec<String> {
                let mut rr = In::new(bytes);
                let mut v = alloc::vec::Vec::new();
                if ct == 9 && each {
                    let (Some(et), Some(n)) = (rr.u8(), rr.i32()) else { return v };
                    for _ in 0..n.max(0) {
                        let mut a = String::new();
                        if text_in(&mut rr, et, &mut a, depth + 1, &mine).is_none() {
                            break;
                        }
                        v.push(a);
                    }
                } else {
                    let mut a = String::new();
                    let _ = text_in(&mut rr, ct, &mut a, depth + 1, &mine);
                    v.push(a);
                }
                v
            };
            let args: alloc::vec::Vec<String> = args.iter().flat_map(|(ct, b)| render(*ct, b, true)).map(|a| alloc::format!("{}{}{}", a, if mine.is_empty() { "\u{a7}r" } else { "" }, mine)).collect();
            let mut extra = String::new();
            for (ct, b) in extras.iter() {
                for e in render(*ct, b, true) {
                    extra.push_str(&e);
                    extra.push_str(&mine);
                }
            }
            if !colour.is_empty() {
                out.push_str(&colour);
            }
            if let Some(k) = key {
                if !crate::lang::has(&k) {
                    if let Some(f) = fallback {
                        out.push_str(&f);
                    } else {
                        out.push_str(&crate::lang::format(&k, &args));
                    }
                } else {
                    out.push_str(&crate::lang::format(&k, &args));
                }
            } else if let Some(t) = text {
                out.push_str(&t);
            }
            out.push_str(&extra);
            if !colour.is_empty() {
                // Back to the parent's colour after this component.
                out.push_str(if parent.is_empty() { "\u{a7}r" } else { parent });
            }
        }
        _ => skip(r, t, depth + 1)?,
    }
    Some(())
}

/// Skips a tag payload whose type byte (`t`) was already read: compound entries and list
/// elements read elsewhere (block entities).
pub fn skip_payload(r: &mut In, t: u8) -> Option<()> {
    skip(r, t, 0)
}

/// The text of a chat component whose type byte (`t`) was already read (a list element or a
/// compound entry), with its colours as formatting codes. `None` on malformed data.
pub fn text_payload(r: &mut In, t: u8) -> Option<String> {
    let mut s = String::new();
    text_of(r, t, &mut s, 0)?;
    Some(s)
}

/// A component sent as network NBT as text with formatting codes: its colours, bold, italic,
/// underline, strikethrough, obfuscation and font (text.rs reads the whole component).
pub fn text(r: &mut In) -> String {
    text_under(r, &crate::text::Style::default())
}

/// The same under a default style the component's own keys override (vanilla's italic for a
/// custom name, italic and dark purple for lore).
pub fn text_italic(r: &mut In) -> String {
    let st = crate::text::Style { flags: crate::text::ITALIC, set: crate::text::ITALIC, ..crate::text::Style::default() };
    text_under(r, &st)
}

/// A component's plain text, without formatting (vanilla's getString: the anvil's name field).
pub fn text_plain(r: &mut In) -> String {
    match crate::text::read_tag(r) {
        Some(t) => crate::text::component(&t).string(),
        None => String::new(),
    }
}

fn text_under(r: &mut In, parent: &crate::text::Style) -> String {
    let Some(t) = crate::text::read_tag(r) else { return String::new() };
    let rich = crate::text::component(&t);
    let mut under = crate::text::Rich::default();
    under.append(&rich, parent);
    let s = under.legacy();
    // The first run starts from the drawing colour anyway.
    match s.strip_prefix("\u{a7}r") {
        Some(rest) => String::from(rest),
        None => s,
    }
}
