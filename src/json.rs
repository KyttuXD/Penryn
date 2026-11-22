//! A small JSON reader (the server-list status answer, chat components sent as JSON) and the
//! text of a JSON chat component with its colours as the GUI's formatting codes.

use alloc::string::String;
use alloc::vec::Vec;

pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn arr(&self) -> &[J] {
        match self {
            J::Arr(v) => v,
            _ => &[],
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn hex4(&mut self) -> Option<u32> {
        let s = core::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(s, 16).ok()
    }
    fn string(&mut self) -> Option<String> {
        self.i += 1; // the opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }
                        other => other as char,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
    fn value(&mut self, depth: u32) -> Option<J> {
        if depth > 64 {
            return None;
        }
        self.ws();
        let c = *self.b.get(self.i)?;
        Some(match c {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    match *self.b.get(self.i)? {
                        b'}' => {
                            self.i += 1;
                            break;
                        }
                        b',' => self.i += 1,
                        b'"' => {
                            let k = self.string()?;
                            self.ws();
                            if self.b.get(self.i) != Some(&b':') {
                                return None;
                            }
                            self.i += 1;
                            let x = self.value(depth + 1)?;
                            v.push((k, x));
                        }
                        _ => return None,
                    }
                }
                J::Obj(v)
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    match *self.b.get(self.i)? {
                        b']' => {
                            self.i += 1;
                            break;
                        }
                        b',' => self.i += 1,
                        _ => v.push(self.value(depth + 1)?),
                    }
                }
                J::Arr(v)
            }
            b'"' => J::Str(self.string()?),
            b't' if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                J::Bool(true)
            }
            b'f' if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                J::Bool(false)
            }
            b'n' if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                J::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                let s = core::str::from_utf8(&self.b[start..self.i]).ok()?;
                J::Num(s.parse().ok()?)
            }
        })
    }
}

pub fn parse(s: &str) -> Option<J> {
    let mut p = P { b: s.as_bytes(), i: 0 };
    p.value(0)
}

const NAMES: [&str; 16] = [
    "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green",
    "aqua", "red", "light_purple", "yellow", "white",
];

fn colour_code(name: &str) -> Option<String> {
    if let Some(i) = NAMES.iter().position(|n| *n == name) {
        return Some(alloc::format!("\u{a7}{}", char::from_digit(i as u32, 16).unwrap()));
    }
    if name.len() == 7 && name.starts_with('#') {
        return Some(alloc::format!("\u{a7}{}", name));
    }
    None
}

/// A chat component's visible text (strings, arrays, objects with text / translate + with /
/// extra), its colours as "§x" codes; `parent` is the colour it inherits.
pub fn text(j: &J) -> String {
    let mut out = String::new();
    text_in(j, "", &mut out, 0);
    out
}

fn text_in(j: &J, parent: &str, out: &mut String, depth: u32) {
    if depth > 32 {
        return;
    }
    match j {
        J::Str(s) => out.push_str(s),
        J::Num(n) => out.push_str(&alloc::format!("{}", n)),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // An array is a component with the first element's style and the rest as its extra.
        J::Arr(v) => {
            for x in v.iter() {
                text_in(x, parent, out, depth + 1);
            }
        }
        J::Obj(_) => {
            let colour = j.get("color").and_then(|c| c.str()).and_then(colour_code);
            let mine = colour.clone().unwrap_or_else(|| String::from(parent));
            if let Some(c) = &colour {
                out.push_str(c);
            }
            if let Some(k) = j.get("translate").and_then(|k| k.str()) {
                let args: Vec<String> = j
                    .get("with")
                    .map(|w| {
                        w.arr()
                            .iter()
                            .map(|a| {
                                let mut s = String::new();
                                text_in(a, &mine, &mut s, depth + 1);
                                s.push_str(if mine.is_empty() { "\u{a7}r" } else { &mine });
                                s
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                match j.get("fallback").and_then(|f| f.str()) {
                    Some(f) if !crate::lang::has(k) => out.push_str(f),
                    _ => out.push_str(&crate::lang::format(k, &args)),
                }
            } else if let Some(t) = j.get("text") {
                text_in(t, &mine, out, depth + 1);
            } else if let Some(t) = j.get("") {
                text_in(t, &mine, out, depth + 1);
            }
            if let Some(extra) = j.get("extra") {
                for e in extra.arr() {
                    text_in(e, &mine, out, depth + 1);
                    if !mine.is_empty() {
                        out.push_str(&mine);
                    }
                }
            }
            if colour.is_some() {
                out.push_str(if parent.is_empty() { "\u{a7}r" } else { parent });
            }
        }
        J::Null => {}
    }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let j = parse(r#"{"version":{"name":"26.3","protocol":777},"players":{"max":20,"online":1,"sample":[{"name":"Menu","id":"x"}]},"description":{"text":"A ","extra":[{"text":"Horizon","color":"gold"}," server é"]}}"#);
    let got = match &j {
        Some(j) => alloc::format!(
            "{} {} {} {}",
            j.get("version").and_then(|v| v.get("protocol")).and_then(|p| p.num()).unwrap_or(0.0),
            j.get("players").and_then(|p| p.get("online")).and_then(|p| p.num()).unwrap_or(-1.0),
            j.get("players").and_then(|p| p.get("sample")).map_or(0, |s| s.arr().len()),
            text(j.get("description").unwrap_or(&J::Null))
        ),
        None => String::from("unreadable"),
    };
    out.push(("status JSON and its MOTD", got, "777 1 1 A \u{a7}6Horizon\u{a7}r server \u{e9}"));
    out
}
