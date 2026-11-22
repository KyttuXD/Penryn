//! Minecraft's GUI drawn as vanilla draws it: its own sprites (gui.bin, made by
//! tools/make_assets.py from the player's game), the bitmap font with vanilla's glyph widths
//! and shadow, nine-slice widgets, and item icons (flat item layers, or 3D block models with
//! their `display.gui` transform). Coordinates are vanilla's GUI units: the window divided by
//! the GUI scale.

use crate::blocks::Blocks;
use crate::math;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

#[derive(Clone, Copy)]
pub struct Sprite {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Nine-slice: border, and the size the border is drawn at.
    pub nine: Option<(u16, u16, u16)>,
}

#[derive(Clone, Copy)]
struct Glyph {
    x: u16,
    y: u16,
    cw: u8,
    ch: u8,
    adv: u8,
    height: u8,
    yoff: i8,
}

pub enum Icon {
    None,
    Flat(Vec<(u16, u32)>),
    Model { first: u32, count: u16, tint: u32, rot: [f32; 3], tr: [f32; 3], sc: [f32; 3] },
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

#[derive(PartialEq, Clone, Copy)]
enum Tex {
    None,
    Gui,
    Blocks,
    Invert,
    /// A texture of its own (a player's downloaded skin or cape).
    Own(u32),
    /// Another texture (e.g. the status effect icons of effects.bin).
    Ext(u32),
}

pub struct Part {
    pub name: String,
    pub parent: i16,
    /// x, y, z (pixels), xRot, yRot, zRot (radians), xScale, yScale, zScale.
    pub pose: [f32; 9],
    /// Four corners (x, y, z in pixels, u, v of the texture), and the normal.
    pub polys: Vec<([[f32; 5]; 4], [f32; 3])>,
}

pub struct Model {
    pub name: String,
    pub parts: Vec<Part>,
}

/// Entity models as tools/make_assets.py writes them (vanilla's ModelPart trees).
pub fn parse_models(d: &[u8], p: &mut usize) -> Vec<Model> {
    let f32_at = |o: usize| f32::from_bits(rd32(d, o));
    let n = rd32(d, *p) as usize;
    *p += 4;
    let mut models = Vec::new();
    for _ in 0..n {
        let l = d[*p] as usize;
        let name = String::from_utf8_lossy(&d[*p + 1..*p + 1 + l]).into_owned();
        *p += 1 + l;
        let np = rd32(d, *p) as usize;
        *p += 4;
        let mut parts = Vec::with_capacity(np);
        for _ in 0..np {
            let l = d[*p] as usize;
            let pname = String::from_utf8_lossy(&d[*p + 1..*p + 1 + l]).into_owned();
            *p += 1 + l;
            let parent = rd16(d, *p) as i16;
            *p += 2;
            let mut pose = [0f32; 9];
            for (i, v) in pose.iter_mut().enumerate() {
                *v = f32_at(*p + 4 * i);
            }
            *p += 36;
            let npoly = rd16(d, *p) as usize;
            *p += 2;
            let mut polys = Vec::with_capacity(npoly);
            for _ in 0..npoly {
                let mut c = [[0f32; 5]; 4];
                for k in 0..4 {
                    for j in 0..5 {
                        c[k][j] = f32_at(*p + 4 * j);
                    }
                    *p += 20;
                }
                let nrm = [f32_at(*p), f32_at(*p + 4), f32_at(*p + 8)];
                *p += 12;
                polys.push((c, nrm));
            }
            parts.push(Part { name: pname, parent, pose, polys });
        }
        models.push(Model { name, parts });
    }
    models
}

pub struct Gui {
    tex: u32,
    blocks_tex: u32,
    aw: f32,
    ah: f32,
    /// The block atlas size, for item layer tiles.
    bw: f32,
    bh: f32,
    pub sprites: BTreeMap<String, Sprite>,
    glyphs: BTreeMap<u32, Glyph>,
    pub icons: Vec<Icon>,
    pub models: Vec<Model>,
    /// Per item: display.firstperson_righthand (rotation, translation, scale).
    pub hands: Vec<[f32; 9]>,
    /// clouds.png: which of its 256 x 256 pixels are cloud.
    pub clouds: Vec<bool>,
    pub clouds_side: usize,
    /// The creative inventory's tabs and stacks (the end of gui.bin).
    pub creative: Option<crate::creative::Data>,
    pub scale: f32,
    /// The GUI Scale option (0: automatic).
    pub fixed_scale: i32,
    pub width: f32,
    pub height: f32,
    /// Text drawn bigger (vanilla's pose scale: titles 4x, subtitles 2x); normally 1.
    pub text_scale: f32,
    segs: Vec<(Tex, Vec<V>)>,
}

pub fn rd16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
pub fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

pub fn argb(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8]
}

impl Gui {
    pub fn load(gl: &Gl, d: &[u8], blocks_tex: u32, blocks_size: (f32, f32)) -> Result<Gui, String> {
        if d.len() < 8 || &d[..4] != crate::account::GUI_MAGIC {
            return Err(String::from("gui.bin is damaged"));
        }
        let (aw, ah) = (rd16(d, 4) as usize, rd16(d, 6) as usize);
        let mut p = 8 + aw * ah * 4;
        if d.len() < p + 4 {
            return Err(String::from("gui.bin is short"));
        }
        let mut tex = 0u32;
        unsafe {
            (gl.GenTextures)(1, &mut tex);
            (gl.BindTexture)(TEXTURE_2D, tex);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
            (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, aw as i32, ah as i32, 0, RGBA, UNSIGNED_BYTE, d[8..].as_ptr() as *const c_void);
        }
        let mut sprites = BTreeMap::new();
        let n = rd32(d, p) as usize;
        p += 4;
        for _ in 0..n {
            let l = d[p] as usize;
            let name = String::from_utf8_lossy(&d[p + 1..p + 1 + l]).into_owned();
            p += 1 + l;
            let s = Sprite {
                x: rd16(d, p),
                y: rd16(d, p + 2),
                w: rd16(d, p + 4),
                h: rd16(d, p + 6),
                nine: if d[p + 8] != 0 { Some((d[p + 9] as u16, rd16(d, p + 10), rd16(d, p + 12))) } else { None },
            };
            p += 14;
            sprites.insert(name, s);
        }
        // Keep which clouds.png pixels are cloud, to build the cloud layer from.
        let (mut clouds, mut clouds_side) = (Vec::new(), 0usize);
        if let Some(c) = sprites.get("sky/clouds") {
            clouds_side = c.w as usize;
            for y in 0..c.h as usize {
                for x in 0..c.w as usize {
                    let o = 8 + ((c.y as usize + y) * aw + c.x as usize + x) * 4;
                    clouds.push(d.get(o + 3).map_or(false, |a| *a > 0));
                }
            }
        }
        let mut glyphs = BTreeMap::new();
        let n = rd32(d, p) as usize;
        p += 4;
        for _ in 0..n {
            let cp = rd32(d, p);
            glyphs.insert(cp, Glyph {
                x: rd16(d, p + 4),
                y: rd16(d, p + 6),
                cw: d[p + 8],
                ch: d[p + 9],
                adv: d[p + 10],
                height: d[p + 11],
                yoff: d[p + 12] as i8,
            });
            p += 14;
        }
        let models = parse_models(d, &mut p);
        let n = rd32(d, p) as usize;
        p += 4;
        let mut hands = Vec::with_capacity(n);
        for _ in 0..n {
            let mut v = [0f32; 9];
            for (i, x) in v.iter_mut().enumerate() {
                *x = f32::from_bits(rd32(d, p + 4 * i));
            }
            hands.push(v);
            p += 36;
        }
        let n = rd32(d, p) as usize;
        p += 4;
        let mut lang = BTreeMap::new();
        for _ in 0..n {
            let kl = rd16(d, p) as usize;
            let k = String::from_utf8_lossy(&d[p + 2..p + 2 + kl]).into_owned();
            p += 2 + kl;
            let vl = rd16(d, p) as usize;
            let v = String::from_utf8_lossy(&d[p + 2..p + 2 + vl]).into_owned();
            p += 2 + vl;
            lang.insert(k, v);
        }
        crate::lang::set(lang);
        let n = rd32(d, p) as usize;
        p += 4;
        let mut icons = Vec::with_capacity(n);
        for _ in 0..n {
            let kind = d[p];
            let k = d[p + 1] as usize;
            p += 2;
            match kind {
                1 => {
                    let mut layers = Vec::new();
                    for _ in 0..k {
                        layers.push((rd16(d, p), rd32(d, p + 2)));
                        p += 6;
                    }
                    icons.push(Icon::Flat(layers));
                }
                2 => {
                    let first = rd32(d, p);
                    let count = rd16(d, p + 4);
                    let tint = rd32(d, p + 6);
                    p += 10;
                    let f = |i: usize| f32::from_bits(rd32(d, p + 4 * i));
                    icons.push(Icon::Model {
                        first,
                        count,
                        tint,
                        rot: [f(0), f(1), f(2)],
                        tr: [f(3), f(4), f(5)],
                        sc: [f(6), f(7), f(8)],
                    });
                    p += 36;
                }
                _ => icons.push(Icon::None),
            }
        }
        Ok(Gui {
            tex,
            blocks_tex,
            aw: aw as f32,
            ah: ah as f32,
            bw: blocks_size.0,
            bh: blocks_size.1,
            sprites,
            glyphs,
            icons,
            models,
            hands,
            clouds,
            clouds_side,
            creative: crate::creative::Data::from_gui_bin(d),
            scale: 2.0,
            fixed_scale: 0,
            width: 0.0,
            height: 0.0,
            text_scale: 1.0,
            segs: Vec::new(),
        })
    }

    /// Vanilla's automatic GUI scale: the largest that keeps at least 320 x 240 units.
    pub fn begin(&mut self, w: i32, h: i32) {
        let mut s = 1;
        while s < 6 && s != self.fixed_scale && w / (s + 1) >= 320 && h / (s + 1) >= 240 {
            s += 1;
        }
        self.scale = s as f32;
        self.width = w as f32 / self.scale;
        self.height = h as f32 / self.scale;
        self.segs.clear();
    }

    fn seg(&mut self, t: Tex) -> &mut Vec<V> {
        if self.segs.last().map_or(true, |s| s.0 != t) {
            self.segs.push((t, Vec::new()));
        }
        &mut self.segs.last_mut().unwrap().1
    }

    fn quad(&mut self, t: Tex, x0: f32, y0: f32, x1: f32, y1: f32, u0: f32, v0: f32, u1: f32, v1: f32, c: [u8; 4]) {
        let s = self.seg(t);
        s.push(V { p: [x0, y0, 0.0], uv: [u0, v0], c });
        s.push(V { p: [x0, y1, 0.0], uv: [u0, v1], c });
        s.push(V { p: [x1, y1, 0.0], uv: [u1, v1], c });
        s.push(V { p: [x1, y0, 0.0], uv: [u1, v0], c });
    }

    pub fn fill(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: u32) {
        self.quad(Tex::None, x0, y0, x1, y1, 0.0, 0.0, 0.0, 0.0, argb(color));
    }

    /// Part of a sprite: (u, v, w, h) in the sprite's pixels, drawn at (x, y) unscaled.
    pub fn blit_part(&mut self, name: &str, x: f32, y: f32, u: f32, v: f32, w: f32, h: f32) {
        let Some(s) = self.sprites.get(name).copied() else { return };
        let (aw, ah) = (self.aw, self.ah);
        self.quad(
            Tex::Gui,
            x,
            y,
            x + w,
            y + h,
            (s.x as f32 + u) / aw,
            (s.y as f32 + v) / ah,
            (s.x as f32 + u + w) / aw,
            (s.y as f32 + v + h) / ah,
            [255; 4],
        );
    }

    /// A rectangle of another GL texture (uv 0..1), tinted by `c` (ARGB).
    pub fn blit_tex(&mut self, tex: u32, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], c: u32) {
        self.quad(Tex::Ext(tex), x, y, x + w, y + h, uv[0], uv[1], uv[2], uv[3], argb(c));
    }

    pub fn blit(&mut self, name: &str, x: f32, y: f32) {
        let Some(s) = self.sprites.get(name).copied() else { return };
        self.blit_part(name, x, y, 0.0, 0.0, s.w as f32, s.h as f32);
    }

    /// The crosshair: drawn inverting what is behind it, as vanilla does.
    pub fn blit_invert(&mut self, name: &str, x: f32, y: f32) {
        let Some(s) = self.sprites.get(name).copied() else { return };
        let (aw, ah) = (self.aw, self.ah);
        self.quad(
            Tex::Invert,
            x,
            y,
            x + s.w as f32,
            y + s.h as f32,
            s.x as f32 / aw,
            s.y as f32 / ah,
            (s.x + s.w) as f32 / aw,
            (s.y + s.h) as f32 / ah,
            [255; 4],
        );
    }

    /// A nine-slice sprite stretched to w x h (buttons, tooltips).
    pub fn blit_nine(&mut self, name: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(s) = self.sprites.get(name).copied() else { return };
        let Some((b, _, _)) = s.nine else {
            return self.blit_part(name, x, y, 0.0, 0.0, w.min(s.w as f32), h.min(s.h as f32));
        };
        let b = b as f32;
        let (sw, sh) = (s.w as f32, s.h as f32);
        let xs = [(0.0, b, x, b), (b, sw - 2.0 * b, x + b, w - 2.0 * b), (sw - b, b, x + w - b, b)];
        let ys = [(0.0, b, y, b), (b, sh - 2.0 * b, y + b, h - 2.0 * b), (sh - b, b, y + h - b, b)];
        let (aw, ah) = (self.aw, self.ah);
        for &(su, suw, dx, dw) in &xs {
            for &(sv, svh, dy, dh) in &ys {
                if dw <= 0.0 || dh <= 0.0 {
                    continue;
                }
                // The middle is repeated in tiles of its own size, as vanilla tiles it.
                let mut ty = 0.0;
                while ty < dh {
                    let th = (dh - ty).min(svh);
                    let mut tx = 0.0;
                    while tx < dw {
                        let tw = (dw - tx).min(suw);
                        self.quad(
                            Tex::Gui,
                            dx + tx,
                            dy + ty,
                            dx + tx + tw,
                            dy + ty + th,
                            (s.x as f32 + su) / aw,
                            (s.y as f32 + sv) / ah,
                            (s.x as f32 + su + tw) / aw,
                            (s.y as f32 + sv + th) / ah,
                            [255; 4],
                        );
                        tx += suw.max(1.0);
                    }
                    ty += svh.max(1.0);
                }
            }
        }
    }

    // ---------------------------------------------------------------- text

    pub fn text_width(&self, s: &str) -> f32 {
        let mut w = 0.0;
        let mut bold = false;
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\u{a7}' {
                match it.next() {
                    Some('#') => {
                        for _ in 0..6 {
                            it.next();
                        }
                        bold = false;
                    }
                    Some('l') | Some('L') => bold = true,
                    Some(d) if d.is_ascii_hexdigit() || d == 'r' || d == 'R' => bold = false,
                    _ => {}
                }
                continue;
            }
            w += self.glyphs.get(&(c as u32)).map_or(6, |g| g.adv) as f32 + bold as i32 as f32;
        }
        w
    }

    /// A glyph's quad, slanted for italics (vanilla moves the top right and the bottom left).
    fn glyph_quad_at(&mut self, g: Glyph, x: f32, y: f32, italic: bool, c: [u8; 4]) {
        let (aw, ah, k) = (self.aw, self.ah, self.text_scale);
        let scale = g.height as f32 / g.ch as f32;
        let (w, h) = (g.cw as f32 * scale * k, g.height as f32 * k);
        let gy = y + g.yoff as f32 * k;
        let (u0, v0, u1, v1) = (g.x as f32 / aw, g.y as f32 / ah, (g.x as f32 + g.cw as f32) / aw, (g.y as f32 + g.ch as f32) / ah);
        let (st, sb) = if italic { ((1.0 - 0.25 * g.yoff as f32) * k, (1.0 - 0.25 * (g.yoff as f32 + g.height as f32)) * k) } else { (0.0, 0.0) };
        let s = self.seg(Tex::Gui);
        s.push(V { p: [x + st, gy, 0.0], uv: [u0, v0], c });
        s.push(V { p: [x + sb, gy + h, 0.0], uv: [u0, v1], c });
        s.push(V { p: [x + w + sb, gy + h, 0.0], uv: [u1, v1], c });
        s.push(V { p: [x + w + st, gy, 0.0], uv: [u1, v0], c });
    }

    /// Draws text; "§x" formatting codes as vanilla's StringDecomposer reads them: a colour
    /// (0-f vanilla's sixteen, "§#RRGGBB" any) clears the formats, k obfuscated, l bold,
    /// m strikethrough, n underlined, o italic, r back to `base`. `shadow` draws each colour
    /// at a quarter (vanilla's drop shadow).
    fn glyphs_at(&mut self, s: &str, x: f32, y: f32, base: [u8; 4], shadow: bool) -> f32 {
        const CODES: [u32; 16] = [
            0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55,
            0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
        ];
        let tint = |rgb: u32| -> [u8; 4] {
            let k = if shadow { 4 } else { 1 };
            [((rgb >> 16) as u8) / k, ((rgb >> 8) as u8) / k, (rgb as u8) / k, base[3]]
        };
        let mut c = base;
        let mut cx = x;
        let k = self.text_scale;
        // bold, italic, underlined, strikethrough, obfuscated
        let (mut bold, mut italic, mut under, mut strike, mut obf) = (false, false, false, false, false);
        let mut it = s.chars().peekable();
        while let Some(ch) = it.next() {
            if ch == '\u{a7}' {
                let mut colour = |v: [u8; 4]| {
                    c = v;
                    (bold, italic, under, strike, obf) = (false, false, false, false, false);
                };
                match it.next() {
                    Some('#') => {
                        let hex: String = (0..6).filter_map(|_| it.next()).collect();
                        if let Ok(v) = u32::from_str_radix(&hex, 16) {
                            colour(tint(v));
                        }
                    }
                    Some(d) if d.is_ascii_hexdigit() => colour(tint(CODES[d.to_digit(16).unwrap() as usize])),
                    Some('r') | Some('R') => colour(base),
                    Some('k') | Some('K') => obf = true,
                    Some('l') | Some('L') => bold = true,
                    Some('m') | Some('M') => strike = true,
                    Some('n') | Some('N') => under = true,
                    Some('o') | Some('O') => italic = true,
                    _ => {}
                }
                continue;
            }
            let mut g = match self.glyphs.get(&(ch as u32)).copied() {
                Some(g) => g,
                None => match self.glyphs.get(&('?' as u32)).copied() {
                    Some(g) => g,
                    None => continue,
                },
            };
            if obf && ch != ' ' {
                // FontSet.getRandomGlyph: another glyph of the same width.
                let seed = (crate::sys::now_ms() / 50) as u32 ^ (cx as u32).wrapping_mul(2654435761);
                let mut r = seed;
                for _ in 0..24 {
                    r = r.wrapping_mul(1103515245).wrapping_add(12345);
                    let cand = 33 + (r >> 16) % 94;
                    if let Some(o) = self.glyphs.get(&cand).copied() {
                        if o.adv == g.adv {
                            g = o;
                            break;
                        }
                    }
                }
            }
            let adv = (g.adv as f32 + bold as i32 as f32) * k;
            if g.cw > 0 {
                self.glyph_quad_at(g, cx, y, italic, c);
                if bold {
                    self.glyph_quad_at(g, cx + k, y, italic, c);
                }
            }
            if under {
                self.quad(Tex::None, cx - k, y + 8.0 * k, cx + adv, y + 9.0 * k, 0.0, 0.0, 0.0, 0.0, c);
            }
            if strike {
                self.quad(Tex::None, cx - k, y + 3.5 * k, cx + adv, y + 4.5 * k, 0.0, 0.0, 0.0, 0.0, c);
            }
            cx += adv;
        }
        cx - x
    }

    /// The glyphs of `s` (formatting codes left out) as font-pixel rectangles from (0, 0)
    /// rightwards and down, each with its uv in the GUI atlas: text drawn in the world (name tags).
    pub fn glyph_quads(&self, s: &str, mut f: impl FnMut(f32, f32, f32, f32, [f32; 4])) {
        let mut cx = 0.0;
        let mut it = s.chars();
        while let Some(ch) = it.next() {
            if ch == '\u{a7}' {
                if it.next() == Some('#') {
                    for _ in 0..6 {
                        it.next();
                    }
                }
                continue;
            }
            let Some(g) = self.glyphs.get(&(ch as u32)).or_else(|| self.glyphs.get(&('?' as u32))).copied() else { continue };
            if g.cw > 0 {
                let w = g.cw as f32 * g.height as f32 / g.ch as f32;
                let (u0, v0) = (g.x as f32 / self.aw, g.y as f32 / self.ah);
                let (u1, v1) = ((g.x as f32 + g.cw as f32) / self.aw, (g.y as f32 + g.ch as f32) / self.ah);
                f(cx, g.yoff as f32, cx + w, g.yoff as f32 + g.height as f32, [u0, v0, u1, v1]);
            }
            cx += g.adv as f32;
        }
    }

    /// Text with vanilla's drop shadow (one unit down-right, a quarter of the colour).
    pub fn text(&mut self, s: &str, x: f32, y: f32, color: u32) -> f32 {
        let c = argb(color);
        let sh = [c[0] / 4, c[1] / 4, c[2] / 4, c[3]];
        let k = self.text_scale;
        self.glyphs_at(s, x + k, y + k, sh, true);
        self.glyphs_at(s, x, y, c, false)
    }

    pub fn text_plain(&mut self, s: &str, x: f32, y: f32, color: u32) -> f32 {
        self.glyphs_at(s, x, y, argb(color), false)
    }

    pub fn text_centered(&mut self, s: &str, cx: f32, y: f32, color: u32) {
        let w = self.text_width(s);
        self.text(s, cx - w / 2.0, y, color);
    }

    /// The experience level: green with a black outline.
    pub fn text_outlined(&mut self, s: &str, x: f32, y: f32, color: u32) {
        for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            self.glyphs_at(s, x + dx, y + dy, [0, 0, 0, 255], false);
        }
        self.glyphs_at(s, x, y, argb(color), false);
    }

    // ---------------------------------------------------------------- items

    /// An item's icon at (x, y), 16 x 16 units, with its count (vanilla's renderItemDecorations).
    pub fn item(&mut self, b: &Blocks, item: i32, count: i32, x: f32, y: f32) {
        let tiles_row = (self.bw / 16.0) as u32;
        match self.icons.get(item as usize) {
            Some(Icon::Flat(layers)) => {
                let layers = layers.clone();
                for (tile, tint) in layers {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let c = if tint != 0 { argb(tint | 0xFF00_0000) } else { [255; 4] };
                    self.quad(Tex::Blocks, x, y, x + 16.0, y + 16.0, tx / self.bw, ty / self.bh, (tx + 16.0) / self.bw, (ty + 16.0) / self.bh, c);
                }
            }
            Some(Icon::Model { first, count: n, tint, rot, tr, sc }) => {
                let (first, n, tint, rot, tr, sc) = (*first, *n, *tint, *rot, *tr, *sc);
                self.model_icon(b, first, n, tint, rot, tr, sc, x, y);
            }
            _ => {}
        }
        if count > 1 {
            let t = alloc::format!("{}", count);
            let w = self.text_width(&t);
            self.text(&t, x + 19.0 - 2.0 - w, y + 6.0 + 3.0, 0xFFFF_FFFF);
        }
    }

    fn model_icon(&mut self, b: &Blocks, first: u32, n: u16, tint: u32, rot: [f32; 3], tr: [f32; 3], sc: [f32; 3], x: f32, y: f32) {
        let r = |d: f32| math::to_rad(d as f64);
        let (sx, cx_) = (math::sin(r(rot[0])), math::cos(r(rot[0])));
        let (sy, cy) = (math::sin(r(rot[1])), math::cos(r(rot[1])));
        let (sz, cz) = (math::sin(r(rot[2])), math::cos(r(rot[2])));
        let tf = |p: [f64; 3]| -> [f64; 3] {
            // centre, scale, rotate (z, y, x applied to the point as vanilla's XYZ euler), translate
            let mut v = [(p[0] - 0.5) * sc[0] as f64, (p[1] - 0.5) * sc[1] as f64, (p[2] - 0.5) * sc[2] as f64];
            v = [v[0] * cz - v[1] * sz, v[0] * sz + v[1] * cz, v[2]];
            v = [v[0] * cy + v[2] * sy, v[1], -v[0] * sy + v[2] * cy];
            v = [v[0], v[1] * cx_ - v[2] * sx, v[1] * sx + v[2] * cx_];
            [v[0] + tr[0] as f64 / 16.0, v[1] + tr[1] as f64 / 16.0, v[2] + tr[2] as f64 / 16.0]
        };
        let tc = if tint != 0 { argb(tint | 0xFF00_0000) } else { [255; 4] };
        let quads = &b.quads[first as usize..(first as usize + n as usize).min(b.quads.len())];
        let mut faces: Vec<(f64, [V; 4])> = Vec::with_capacity(quads.len());
        for q in quads {
            let mut vs = [V { p: [0.0; 3], uv: [0.0; 2], c: [255; 4] }; 4];
            let mut depth = 0.0;
            let l = q.light as f32 / 255.0;
            let base = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 4] };
            let c = [(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255];
            for i in 0..4 {
                let p = tf([q.pos[i][0] as f64 / 1024.0, q.pos[i][1] as f64 / 1024.0, q.pos[i][2] as f64 / 1024.0]);
                depth += p[2];
                vs[i] = V {
                    p: [x + 8.0 + p[0] as f32 * 16.0, y + 8.0 - p[1] as f32 * 16.0, 0.0],
                    uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0],
                    c,
                };
            }
            faces.push((depth, vs));
        }
        // Painter's order: far faces first (the GUI has no depth buffer).
        faces.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
        let s = self.seg(Tex::Blocks);
        for (_, vs) in faces {
            s.extend_from_slice(&vs);
        }
    }

    // ---------------------------------------------------------------- entities

    /// Draws an entity model with a skin sprite: feet at (x, y), `scale` GUI units a block,
    /// turned by `body_yaw` (degrees), the head by `head_yaw` and `head_pitch` more.
    pub fn entity(&mut self, model: &str, skin: &str, x: f32, y: f32, scale: f32, body_yaw: f32, head_yaw: f32, head_pitch: f32) {
        let Some(sp) = self.sprites.get(skin).copied() else { return };
        let r = [sp.x as f32 / self.aw, sp.y as f32 / self.ah, sp.w as f32 / self.aw, sp.h as f32 / self.ah];
        self.entity_uv(model, Tex::Gui, r, x, y, scale, body_yaw, head_yaw, head_pitch);
    }

    /// `entity` with a texture of its own (uvs 0..1 over it): a player's downloaded skin or cape.
    pub fn entity_tex(&mut self, model: &str, tex: u32, x: f32, y: f32, scale: f32, body_yaw: f32, head_yaw: f32, head_pitch: f32) {
        self.entity_uv(model, Tex::Own(tex), [0.0, 0.0, 1.0, 1.0], x, y, scale, body_yaw, head_yaw, head_pitch);
    }

    /// `r`: the skin's rectangle in uvs (u, v, width, height).
    fn entity_uv(&mut self, model: &str, tex: Tex, r: [f32; 4], x: f32, y: f32, scale: f32, body_yaw: f32, head_yaw: f32, head_pitch: f32) {
        let Some(mi) = self.models.iter().position(|m| m.name == model) else { return };
        type M = [[f64; 4]; 3];
        fn mul(a: &M, b: &M) -> M {
            let mut r = [[0.0; 4]; 3];
            for i in 0..3 {
                for j in 0..4 {
                    r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + if j == 3 { a[i][3] } else { 0.0 };
                }
            }
            r
        }
        let rot = |axis: usize, a: f64| -> M {
            let (s, c) = (math::sin(a), math::cos(a));
            match axis {
                0 => [[1.0, 0.0, 0.0, 0.0], [0.0, c, -s, 0.0], [0.0, s, c, 0.0]],
                1 => [[c, 0.0, s, 0.0], [0.0, 1.0, 0.0, 0.0], [-s, 0.0, c, 0.0]],
                _ => [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            }
        };
        let parts = &self.models[mi].parts;
        let mut mats: Vec<M> = Vec::with_capacity(parts.len());
        let body = rot(1, math::to_rad(body_yaw as f64));
        for p in parts.iter() {
            let base = if p.parent >= 0 { mats[p.parent as usize] } else { body };
            let q = p.pose;
            let mut m = mul(&base, &[[1.0, 0.0, 0.0, q[0] as f64], [0.0, 1.0, 0.0, q[1] as f64], [0.0, 0.0, 1.0, q[2] as f64]]);
            let (mut xr, mut yr) = (q[3] as f64, q[4] as f64);
            if p.name == "head" {
                yr += math::to_rad(head_yaw as f64);
                xr += math::to_rad(head_pitch as f64);
            }
            m = mul(&m, &rot(2, q[5] as f64));
            m = mul(&m, &rot(1, yr));
            m = mul(&m, &rot(0, xr));
            m = mul(&m, &[[q[6] as f64, 0.0, 0.0, 0.0], [0.0, q[7] as f64, 0.0, 0.0], [0.0, 0.0, q[8] as f64, 0.0]]);
            mats.push(m);
        }
        let k = scale / 16.0;
        let mut faces: Vec<(f64, [V; 4])> = Vec::new();
        for (pi, p) in parts.iter().enumerate() {
            let m = &mats[pi];
            for (c, n) in p.polys.iter() {
                let nx = m[0][0] * n[0] as f64 + m[0][1] * n[1] as f64 + m[0][2] * n[2] as f64;
                let ny = m[1][0] * n[0] as f64 + m[1][1] * n[1] as f64 + m[1][2] * n[2] as f64;
                let nz = m[2][0] * n[0] as f64 + m[2][1] * n[1] as f64 + m[2][2] * n[2] as f64;
                if nz > 0.05 {
                    continue; // facing away (the viewer looks along +Z)
                }
                let l = (0.72 - 0.28 * ny - 0.12 * nx.abs()).clamp(0.35, 1.0);
                let col = [(255.0 * l) as u8, (255.0 * l) as u8, (255.0 * l) as u8, 255];
                let mut vs = [V { p: [0.0; 3], uv: [0.0; 2], c: col }; 4];
                let mut depth = 0.0;
                for i in 0..4 {
                    let v = c[i];
                    let px = m[0][0] * v[0] as f64 + m[0][1] * v[1] as f64 + m[0][2] * v[2] as f64 + m[0][3];
                    let py = m[1][0] * v[0] as f64 + m[1][1] * v[1] as f64 + m[1][2] * v[2] as f64 + m[1][3];
                    let pz = m[2][0] * v[0] as f64 + m[2][1] * v[1] as f64 + m[2][2] * v[2] as f64 + m[2][3];
                    depth += pz;
                    vs[i] = V {
                        p: [x + px as f32 * k, y + (py as f32 - 24.0) * k, 0.0],
                        uv: [r[0] + v[3] * r[2], r[1] + v[4] * r[3]],
                        c: col,
                    };
                }
                faces.push((depth, vs));
            }
        }
        faces.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));
        let s = self.seg(tex);
        for (_, vs) in faces {
            s.extend_from_slice(&vs);
        }
    }

    // ---------------------------------------------------------------- output

    pub fn texture(&self) -> u32 {
        self.tex
    }

    /// A glyph of the font for text drawn elsewhere (signs in the world): its uv (u0, v0, u1,
    /// v1), its quad from the pen (x, y, w, h, in font pixels) and its advance; '?' for
    /// characters the font lacks.
    pub fn glyph_quad(&self, ch: char) -> Option<([f32; 4], [f32; 4], f32)> {
        let g = self.glyphs.get(&(ch as u32)).or_else(|| self.glyphs.get(&('?' as u32)))?;
        let uv = [g.x as f32 / self.aw, g.y as f32 / self.ah, (g.x as f32 + g.cw as f32) / self.aw, (g.y as f32 + g.ch as f32) / self.ah];
        let w = if g.ch > 0 { g.cw as f32 * g.height as f32 / g.ch as f32 } else { 0.0 };
        Some((uv, [0.0, g.yoff as f32, w, g.height as f32], g.adv as f32))
    }

    /// A sprite's rectangle in the GUI atlas as uv (u0, v0, u1, v1).
    pub fn sprite_uv(&self, name: &str) -> Option<[f32; 4]> {
        let s = self.sprites.get(name)?;
        Some([s.x as f32 / self.aw, s.y as f32 / self.ah, (s.x + s.w) as f32 / self.aw, (s.y + s.h) as f32 / self.ah])
    }

    pub fn flush(&mut self, gl: &Gl, w: i32, h: i32) {
        let (gw, gh) = (self.width, self.height);
        let m: [f32; 16] = [2.0 / gw, 0.0, 0.0, 0.0, 0.0, -2.0 / gh, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, -1.0, 1.0, 0.0, 1.0];
        unsafe {
            (gl.Viewport)(0, 0, w, h);
            (gl.Disable)(DEPTH_TEST);
            (gl.Disable)(CULL_FACE);
            (gl.Disable)(FOG);
            (gl.Enable)(BLEND);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.004);
            (gl.MatrixMode)(PROJECTION);
            (gl.LoadMatrixf)(m.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadIdentity)();
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        }
        let segs = core::mem::take(&mut self.segs);
        for (t, v) in segs.iter() {
            if v.is_empty() {
                continue;
            }
            unsafe {
                match t {
                    Tex::None => (gl.Disable)(TEXTURE_2D),
                    Tex::Gui | Tex::Invert => {
                        (gl.Enable)(TEXTURE_2D);
                        (gl.BindTexture)(TEXTURE_2D, self.tex);
                    }
                    Tex::Blocks => {
                        (gl.Enable)(TEXTURE_2D);
                        (gl.BindTexture)(TEXTURE_2D, self.blocks_tex);
                    }
                    Tex::Own(t) | Tex::Ext(t) => {
                        (gl.Enable)(TEXTURE_2D);
                        (gl.BindTexture)(TEXTURE_2D, *t);
                    }
                }
                if *t == Tex::Invert {
                    (gl.BlendFunc)(ONE_MINUS_DST_COLOR, ONE_MINUS_SRC_COLOR);
                } else {
                    (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
                }
                (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
                (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            }
        }
        unsafe {
            (gl.Disable)(TEXTURE_2D);
            (gl.Disable)(BLEND);
            (gl.Disable)(ALPHA_TEST);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
        self.segs = segs;
        self.segs.clear();
    }
}
