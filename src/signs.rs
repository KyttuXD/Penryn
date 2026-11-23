//! Block entities' extra data, and signs: their text drawn in the world as vanilla's
//! StandingSignRenderer and HangingSignRenderer place it (front and back, dye colour, glowing
//! text with its outline), and the sign editing screen the server opens after a sign is placed
//! (AbstractSignEditScreen, SignEditScreen, HangingSignEditScreen).
//!
//! In 26.3 the sign boards themselves are block models (template_sign_rot_*), drawn with the
//! world; only the text is drawn here. The editing screen's sign pictures (gui/signs/*.png and
//! gui/hanging_signs/*.png) come from signs.bin, which is optional.

use crate::blocks::Blocks;
use crate::client::Game;
use crate::gui::Gui;
use crate::sdl::*;
use crate::wire::{In, Out};
use crate::world::World;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_void;

pub const SIGNS_MAGIC: &[u8; 4] = b"PNT1";

/// DyeColor's text colours, in its order (white .. black).
const DYE_NAMES: [&str; 16] = [
    "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue",
    "brown", "green", "red", "black",
];
const DYE_TEXT: [u32; 16] = [
    0xFFFFFF, 0xFF681F, 0xFF00FF, 0x9AC0CD, 0xFFFF00, 0xBFFF00, 0xFF69B4, 0x808080, 0xD3D3D3, 0x00FFFF, 0xA020F0, 0x0000FF,
    0x8B4513, 0x00FF00, 0xFF0000, 0x000000,
];
const BLACK: u8 = 15;
/// AbstractSignRenderer.BLACK_TEXT_OUTLINE_COLOR: the outline of glowing black text.
const BLACK_OUTLINE: u32 = 0xF0EBCC;
/// Glowing text has its outline within 16 blocks (AbstractSignRenderer.OUTLINE_RENDER_DISTANCE).
const OUTLINE_DIST2: f64 = 16.0 * 16.0;
/// Block entities draw within 64 blocks (BlockEntityRenderer.getViewDistance).
const VIEW_DIST2: f64 = 64.0 * 64.0;

/// One side of a sign: its four lines (chat components as text with colour codes), dye colour
/// and glow ink.
#[derive(Clone, Default)]
pub struct SignText {
    pub lines: [String; 4],
    pub color: u8,
    pub glow: bool,
}

impl SignText {
    fn empty() -> SignText {
        SignText { color: BLACK, ..Default::default() }
    }
    fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.is_empty())
    }
}

#[derive(Clone)]
pub struct Sign {
    pub front: SignText,
    pub back: SignText,
    pub waxed: bool,
}

/// The sign editing screen's state.
pub struct Editor {
    pub pos: [i32; 3],
    pub front: bool,
    pub lines: [String; 4],
    pub line: usize,
    /// The cursor in the line, in characters (TextFieldHelper's cursor; no selection).
    pub cursor: usize,
    pub opened: u64,
    pub hanging: bool,
    pub wall: bool,
    pub wood: String,
    /// The text colour while editing (the dye's, or its dark shade without glow ink).
    pub color: u32,
}

/// A sign block, from its state: hanging or not, on a wall or not, its rotation (degrees, as
/// RotationSegment.convertToDegrees or Direction.toYRot) and its wood.
pub struct Kind<'a> {
    pub hanging: bool,
    pub wall: bool,
    pub angle: f64,
    pub wood: &'a str,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// The editing screen's sign pictures (signs.bin).
struct Pictures {
    tex: u32,
    w: f32,
    h: f32,
    /// Per wood: the gui/signs picture (24 x 26) and the gui/hanging_signs one (16 x 16).
    woods: BTreeMap<String, ([u16; 2], [u16; 2])>,
}

/// The block entities the server sent, by position; only signs keep their data for now.
pub struct BlockEntities {
    pub signs: BTreeMap<(i32, i32, i32), Sign>,
    /// The server asked to edit this sign (ClientboundOpenSignEditorPacket): position, front.
    pub open_request: Option<([i32; 3], bool)>,
    pub editor: Option<Editor>,
    /// Bumped on every change to the signs (their text mesh is made again).
    pub version: u32,
    mesh: RefCell<TextMesh>,
    /// signs.bin: not tried yet, missing (None), or loaded.
    pictures: Option<Option<Pictures>>,
    /// The resource pack reload the pictures were loaded at (packs::generation).
    pictures_of: u32,
}

impl BlockEntities {
    pub fn new() -> BlockEntities {
        BlockEntities { signs: BTreeMap::new(), open_request: None, editor: None, version: 0, mesh: RefCell::new(TextMesh::default()), pictures: None, pictures_of: 0 }
    }

    /// Everything goes (a new dimension or level).
    pub fn clear(&mut self) {
        self.signs.clear();
        self.version += 1;
    }

    /// A chunk column was unloaded.
    pub fn forget(&mut self, cx: i32, cz: i32) {
        let n = self.signs.len();
        self.signs.retain(|k, _| !(k.0 >> 4 == cx && k.2 >> 4 == cz));
        if self.signs.len() != n {
            self.version += 1;
        }
    }

    /// A block changed: its block entity goes unless the block is still a sign (vanilla keeps a
    /// block entity while the new state's block has the same type).
    pub fn block_changed(&mut self, blocks: &Blocks, old: u16, x: i32, y: i32, z: i32, state: u16) {
        if self.signs.contains_key(&(x, y, z)) {
            // Signs and hanging signs are two block entity types: one does not become the other.
            let hanging = |s: u16| sign_kind(blocks, s).map(|k| k.hanging);
            if hanging(state).is_none() || (hanging(old).is_some() && hanging(state) != hanging(old)) {
                self.signs.remove(&(x, y, z));
            }
            self.version += 1;
        }
    }

    /// The block entity list at the end of a chunk packet
    /// (ClientboundLevelChunkPacketData.BlockEntityInfo).
    pub fn read_chunk(&mut self, cx: i32, cz: i32, r: &mut In) -> Option<()> {
        self.forget(cx, cz);
        let n = r.varint()?;
        for _ in 0..n.max(0) {
            let xz = r.u8()?;
            let y = r.u16()? as i16 as i32;
            let _kind = r.varint()?;
            let (x, z) = (cx * 16 + (xz >> 4) as i32, cz * 16 + (xz & 15) as i32);
            self.read_tag(x, y, z, r)?;
        }
        Some(())
    }

    /// One block entity's data (a nameless network compound, or an empty tag).
    pub fn read_tag(&mut self, x: i32, y: i32, z: i32, r: &mut In) -> Option<()> {
        let t = r.u8()?;
        if t == 0 {
            return Some(());
        }
        if t != 10 {
            return crate::nbt::skip_payload(r, t);
        }
        let (mut front, mut back, mut waxed, mut is_sign) = (SignText::empty(), SignText::empty(), false, false);
        loop {
            let ct = r.u8()?;
            if ct == 0 {
                break;
            }
            let name = nbt_key(r)?;
            match (name, ct) {
                (b"front_text", 10) => {
                    front = sign_text(r)?;
                    is_sign = true;
                }
                (b"back_text", 10) => {
                    back = sign_text(r)?;
                    is_sign = true;
                }
                (b"is_waxed", 1) => waxed = r.u8()? != 0,
                _ => crate::nbt::skip_payload(r, ct)?,
            }
        }
        if is_sign {
            self.signs.insert((x, y, z), Sign { front, back, waxed });
            self.version += 1;
        }
        Some(())
    }
}

fn nbt_key<'a>(r: &mut In<'a>) -> Option<&'a [u8]> {
    let n = r.u16()? as usize;
    r.take(n)
}

/// SignText's codec: messages (four components), color, has_glowing_text.
fn sign_text(r: &mut In) -> Option<SignText> {
    let mut s = SignText::empty();
    loop {
        let ct = r.u8()?;
        if ct == 0 {
            break;
        }
        let name = nbt_key(r)?;
        match (name, ct) {
            (b"messages", 9) => {
                let et = r.u8()?;
                let n = r.i32()?;
                if et == 0 && n > 0 {
                    return None;
                }
                for i in 0..n.max(0) as usize {
                    let line = crate::nbt::text_payload(r, et)?;
                    if i < 4 {
                        s.lines[i] = line;
                    }
                }
            }
            (b"color", 8) => {
                let c = nbt_key(r)?;
                if let Some(i) = DYE_NAMES.iter().position(|n| n.as_bytes() == c) {
                    s.color = i as u8;
                }
            }
            (b"has_glowing_text", 1) => s.glow = r.u8()? != 0,
            _ => crate::nbt::skip_payload(r, ct)?,
        }
    }
    Some(s)
}

/// What sign a block state is, from its block's name and its place among the block's states
/// (vanilla orders states by property name, the last varying fastest: rotation then
/// waterlogged; attached, rotation, waterlogged for ceiling hanging signs).
pub fn sign_kind(blocks: &Blocks, s: u16) -> Option<Kind<'_>> {
    let st = blocks.states.get(s as usize)?;
    let name = blocks.blocks.get(st.block as usize)?.name.as_str();
    if !name.ends_with("_sign") {
        return None;
    }
    let mut first = s;
    while first > 0 && blocks.states[first as usize - 1].block == st.block {
        first -= 1;
    }
    let idx = (s - first) as u32;
    // Direction.toYRot: south 0, west 90, north 180, east 270 (facing: 0 north 1 south 2 west 3 east).
    let wall_rot = [180.0, 0.0, 90.0, 270.0].get(st.facing as usize).copied().unwrap_or(0.0);
    // RotationSegment.convertToDegrees (integer maths: no fmod on Mac OS X 10.6's stubs).
    let rot16 = |i: u32| (i / 2 % 16) as f64 * 22.5;
    Some(if let Some(w) = name.strip_suffix("_wall_hanging_sign") {
        Kind { hanging: true, wall: true, angle: wall_rot, wood: w }
    } else if let Some(w) = name.strip_suffix("_hanging_sign") {
        Kind { hanging: true, wall: false, angle: rot16(idx), wood: w }
    } else if let Some(w) = name.strip_suffix("_wall_sign") {
        Kind { hanging: false, wall: true, angle: wall_rot, wood: w }
    } else {
        Kind { hanging: false, wall: false, angle: rot16(idx), wood: name.strip_suffix("_sign").unwrap_or(name) }
    })
}

/// ARGB.scaleRGB(color, 0.4): the shade of text without glow ink.
fn dark(c: u32) -> u32 {
    let f = |v: u32| ((v & 255) as f32 * 0.4) as u32;
    f(c >> 16) << 16 | f(c >> 8) << 8 | f(c)
}

/// AbstractSignRenderer.getDarkColor.
fn dark_color(t: &SignText) -> u32 {
    if t.color == BLACK && t.glow {
        BLACK_OUTLINE
    } else {
        dark(DYE_TEXT[t.color as usize & 15])
    }
}

/// The part of a line that fits `max` font pixels (Font.split's first line: broken at the last
/// space that fits, or between characters).
fn fit<'a>(gui: &Gui, s: &'a str, max: f32) -> &'a str {
    let (mut w, mut last_space, mut code) = (0.0, None, 0);
    for (i, c) in s.char_indices() {
        if code > 0 {
            code -= 1;
            if code == 0 && c == '#' {
                code = 6;
            }
            continue;
        }
        if c == '\u{a7}' {
            code = 1;
            continue;
        }
        if c == ' ' {
            last_space = Some(i);
        }
        w += gui.glyph_quad(c).map_or(0.0, |g| g.2);
        if w > max {
            return &s[..last_space.unwrap_or(i)];
        }
    }
    s
}

/// An affine transform, three rows of four.
type M = [[f64; 4]; 3];

fn translate(m: &mut M, t: [f64; 3]) {
    for r in m.iter_mut() {
        r[3] += r[0] * t[0] + r[1] * t[1] + r[2] * t[2];
    }
}

/// Axis.YP.rotationDegrees(deg), applied after `m`.
fn rot_y(m: &mut M, deg: f64) {
    let a = crate::math::to_rad(deg);
    let (s, c) = (crate::math::sin(a), crate::math::cos(a));
    for r in m.iter_mut() {
        let (x, z) = (r[0], r[2]);
        r[0] = x * c - z * s;
        r[2] = x * s + z * c;
    }
}

fn scale(m: &mut M, s: [f64; 3]) {
    for r in m.iter_mut() {
        for j in 0..3 {
            r[j] *= s[j];
        }
    }
}

/// The text transformation of a sign side (StandingSignRenderer / HangingSignRenderer
/// .textTransformation), from the block's corner.
fn text_transform(k: &Kind, front: bool, at: [f64; 3]) -> M {
    let mut m: M = [[1.0, 0.0, 0.0, at[0]], [0.0, 1.0, 0.0, at[1]], [0.0, 0.0, 1.0, at[2]]];
    if k.hanging {
        translate(&mut m, [0.5, 0.9375, 0.5]);
        rot_y(&mut m, -k.angle);
        translate(&mut m, [0.0, -0.3125, 0.0]);
        if !front {
            rot_y(&mut m, 180.0);
        }
        translate(&mut m, [0.0, -0.32, 0.073]);
        let s = 0.0140625;
        scale(&mut m, [s, -s, s]);
    } else {
        translate(&mut m, [0.5, 0.5, 0.5]);
        rot_y(&mut m, -k.angle);
        if k.wall {
            translate(&mut m, [0.0, -0.3125, -0.4375]);
        }
        if !front {
            rot_y(&mut m, 180.0);
        }
        translate(&mut m, [0.0, 0.33333334, 0.046666667]);
        let s = 0.010416667;
        scale(&mut m, [s, -s, s]);
    }
    m
}

/// Line height and widest line, in font pixels (SignBlockEntity / HangingSignBlockEntity).
fn metrics(hanging: bool) -> (i32, f32) {
    if hanging {
        (9, 60.0)
    } else {
        (10, 90.0)
    }
}

/// Pushes a line's glyphs: `base` the colour (formatting codes change it unless `fixed`).
fn line_quads(out: &mut Vec<V>, gui: &Gui, m: &M, s: &str, x: f32, y: f32, base: u32, fixed: bool) {
    const CODES: [u32; 16] = [
        0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55,
        0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
    ];
    let rgba = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255];
    let mut c = rgba(base);
    let mut pen = x;
    let p = |px: f32, py: f32| -> [f32; 3] {
        let (px, py) = (px as f64, py as f64);
        [
            (m[0][0] * px + m[0][1] * py + m[0][3]) as f32,
            (m[1][0] * px + m[1][1] * py + m[1][3]) as f32,
            (m[2][0] * px + m[2][1] * py + m[2][3]) as f32,
        ]
    };
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        if ch == '\u{a7}' {
            match it.next() {
                Some('#') => {
                    let hex: String = (0..6).filter_map(|_| it.next()).collect();
                    if let (Ok(v), false) = (u32::from_str_radix(&hex, 16), fixed) {
                        c = rgba(v);
                    }
                }
                Some(d) if d.is_ascii_hexdigit() && !fixed => c = rgba(CODES[d.to_digit(16).unwrap() as usize]),
                Some('r') if !fixed => c = rgba(base),
                _ => {}
            }
            continue;
        }
        let Some((uv, q, adv)) = gui.glyph_quad(ch) else { continue };
        if q[2] > 0.0 {
            let (x0, y0, x1, y1) = (pen + q[0], y + q[1], pen + q[0] + q[2], y + q[1] + q[3]);
            out.push(V { p: p(x0, y0), uv: [uv[0], uv[1]], c });
            out.push(V { p: p(x0, y1), uv: [uv[0], uv[3]], c });
            out.push(V { p: p(x1, y1), uv: [uv[2], uv[3]], c });
            out.push(V { p: p(x1, y0), uv: [uv[2], uv[1]], c });
        }
        pen += adv;
    }
}

/// One side's text (AbstractSignRenderer.submitSignText): plain text and outlines into
/// `under`, glowing text over its outline into `over`.
fn side_quads(under: &mut Vec<V>, over: &mut Vec<V>, gui: &Gui, m: &M, t: &SignText, hanging: bool, near: bool) {
    let (lh, max) = metrics(hanging);
    let darkc = dark_color(t);
    let (color, outline) = if t.glow {
        let c = DYE_TEXT[t.color as usize & 15];
        (c, t.color == BLACK || near)
    } else {
        (darkc, false)
    };
    let top = 4 * lh / 2;
    for (i, line) in t.lines.iter().enumerate() {
        let s = fit(gui, line, max);
        if s.is_empty() {
            continue;
        }
        let w = gui.text_width(s) as i32;
        let x = (-w / 2) as f32;
        let y = (i as i32 * lh - top) as f32;
        if outline {
            // Font.drawInBatch8xOutline: the outline colour around, the text in front.
            for (dx, dy) in [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 0.0), (1.0, 0.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)] {
                line_quads(under, gui, m, s, x + dx, y + dy, darkc, true);
            }
            line_quads(over, gui, m, s, x, y, color, false);
        } else {
            line_quads(under, gui, m, s, x, y, color, false);
        }
    }
}

/// The sign text's vertex buffer, made again only when the signs or what is drawn of them
/// changes (one more within 64 blocks, an outline within 16), not every frame.
#[derive(Default)]
pub struct TextMesh {
    vbo: u32,
    under: i32,
    over: i32,
    version: u32,
    shape: u64,
    anchor: [i32; 3],
    /// Each side's text in the buffer, to light them one by one.
    ranges: Vec<Piece>,
}

/// One side of a sign in the text buffer: its block, whether the text glows (full light), the
/// vertices in the `under` part and (from the start of the `over` part) in the `over` part.
struct Piece {
    pos: [i32; 3],
    glow: bool,
    under: (i32, i32),
    over: (i32, i32),
}

/// Draws every sign's text near the eye. `view` is the view matrix (column-major); called
/// between the opaque world and the water.
pub fn draw_world(gl: &Gl, view: &[f32; 16], eye: [f64; 3], be: &BlockEntities, world: &World, blocks: &Blocks, gui: &Gui, lit: &crate::lightmap::Lit) {
    if be.signs.is_empty() {
        return;
    }
    let mut mesh = be.mesh.borrow_mut();
    // Which signs are drawn, and which with an outline: a rebuild when that changes.
    let mut shape = 0xcbf29ce484222325u64;
    for &(x, y, z) in be.signs.keys() {
        let c = [x as f64 + 0.5 - eye[0], y as f64 + 0.5 - eye[1], z as f64 + 0.5 - eye[2]];
        let d2 = c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
        let flags = (d2 <= VIEW_DIST2) as u64 | ((d2 < OUTLINE_DIST2) as u64) << 1;
        shape = (shape ^ flags ^ (x as u64) << 2 ^ (z as u64) << 22 ^ (y as u64) << 42).wrapping_mul(0x100000001b3);
    }
    if mesh.vbo == 0 || mesh.version != be.version || mesh.shape != shape {
        let anchor = [crate::math::ifloor(eye[0]), crate::math::ifloor(eye[1]), crate::math::ifloor(eye[2])];
        let (mut under, mut over) = (Vec::new(), Vec::new());
        mesh.ranges.clear();
        for (&(x, y, z), sign) in be.signs.iter() {
            let c = [x as f64 + 0.5 - eye[0], y as f64 + 0.5 - eye[1], z as f64 + 0.5 - eye[2]];
            let d2 = c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
            if d2 > VIEW_DIST2 {
                continue;
            }
            let Some(k) = sign_kind(blocks, world.get(x, y, z)) else { continue };
            let at = [(x - anchor[0]) as f64, (y - anchor[1]) as f64, (z - anchor[2]) as f64];
            for (t, front) in [(&sign.front, true), (&sign.back, false)] {
                if !t.is_empty() {
                    let m = text_transform(&k, front, at);
                    let (u0, o0) = (under.len() as i32, over.len() as i32);
                    side_quads(&mut under, &mut over, gui, &m, t, k.hanging, d2 < OUTLINE_DIST2);
                    mesh.ranges.push(Piece { pos: [x, y, z], glow: t.glow, under: (u0, under.len() as i32 - u0), over: (o0, over.len() as i32 - o0) });
                }
            }
        }
        mesh.under = under.len() as i32;
        mesh.over = over.len() as i32;
        under.extend_from_slice(&over);
        unsafe {
            if mesh.vbo == 0 {
                (gl.GenBuffers)(1, &mut mesh.vbo);
            }
            (gl.BindBuffer)(ARRAY_BUFFER, mesh.vbo);
            (gl.BufferData)(ARRAY_BUFFER, (under.len() * 24) as isize, under.as_ptr() as *const c_void, STATIC_DRAW);
        }
        mesh.version = be.version;
        mesh.shape = shape;
        mesh.anchor = anchor;
    }
    if mesh.under + mesh.over == 0 {
        return;
    }
    // The text sits a hair in front of the board: drawn scaled toward the eye, which moves
    // nothing on screen but wins the depth test (outlines and plain text 0.1 %, glowing text
    // over its outline 0.15 %).
    let t = [mesh.anchor[0] as f64 - eye[0], mesh.anchor[1] as f64 - eye[1], mesh.anchor[2] as f64 - eye[2]];
    let matrix = |k: f64| -> [f32; 16] {
        let mut m = [0f32; 16];
        for j in 0..3 {
            for r in 0..4 {
                m[j * 4 + r] = (view[j * 4 + r] as f64 * k) as f32;
            }
        }
        for r in 0..4 {
            let v = view[r] as f64 * t[0] + view[4 + r] as f64 * t[1] + view[8 + r] as f64 * t[2];
            m[12 + r] = (v * k + view[12 + r] as f64) as f32;
        }
        m
    };
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.BindBuffer)(ARRAY_BUFFER, mesh.vbo);
        (gl.Disable)(CULL_FACE);
        (gl.Disable)(BLEND);
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, gui.texture());
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.1);
        (gl.EnableClientState)(VERTEX_ARRAY);
        (gl.EnableClientState)(COLOR_ARRAY);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.VertexPointer)(3, FLOAT, 24, core::ptr::null());
        (gl.TexCoordPointer)(2, FLOAT, 24, 12 as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, 20 as *const c_void);
    }
    // Each side is lit by its sign's block (glowing text: fully).
    lit.tint_begin(gl);
    for pass in 0..2 {
        unsafe { (gl.LoadMatrixf)(matrix(if pass == 0 { 1.0 - 0.0010 } else { 1.0 - 0.0015 }).as_ptr()) };
        for r in mesh.ranges.iter() {
            let (start, n) = if pass == 0 { (r.under.0, r.under.1) } else { (mesh.under + r.over.0, r.over.1) };
            if n > 0 {
                lit.tint_set(gl, if r.glow { [1.0; 3] } else { lit.cell(r.pos[0], r.pos[1], r.pos[2]) });
                unsafe { (gl.DrawArrays)(QUADS, start, n) };
            }
        }
    }
    lit.tint_end(gl);
    unsafe {
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.Enable)(CULL_FACE);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
    }
}

// ---------------------------------------------------------------- the editing screen

/// Plain text: the formatting codes taken out (Component.getString).
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\u{a7}' {
            if it.next() == Some('#') {
                for _ in 0..6 {
                    it.next();
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Opens the editor the server asked for; false when the block there is not a sign.
pub fn open_editor(g: &mut Game, pos: [i32; 3], front: bool) -> bool {
    let state = g.world.get(pos[0], pos[1], pos[2]);
    let Some(k) = sign_kind(&g.blocks, state) else { return false };
    let (hanging, wall, wood) = (k.hanging, k.wall, String::from(k.wood));
    let be = &mut g.block_entities;
    let sign = be.signs.entry((pos[0], pos[1], pos[2])).or_insert_with(|| Sign {
        front: SignText::empty(),
        back: SignText::empty(),
        waxed: false,
    });
    let t = if front { &sign.front } else { &sign.back };
    let color = if t.glow { DYE_TEXT[t.color as usize & 15] } else { dark_color(t) };
    let lines = [plain(&t.lines[0]), plain(&t.lines[1]), plain(&t.lines[2]), plain(&t.lines[3])];
    let cursor = lines[0].chars().count();
    be.version += 1;
    be.editor = Some(Editor { pos, front, lines, line: 0, cursor, opened: crate::sys::now_ms(), hanging, wall, wood, color });
    true
}

/// The edited lines into the sign in the world as they are typed (AbstractSignEditScreen.setMessage).
fn show_in_world(be: &mut BlockEntities) {
    let Some(ed) = be.editor.as_ref() else { return };
    if let Some(sign) = be.signs.get_mut(&(ed.pos[0], ed.pos[1], ed.pos[2])) {
        let t = if ed.front { &mut sign.front } else { &mut sign.back };
        t.lines = ed.lines.clone();
        be.version += 1;
    }
}

/// The byte offset of character `n` of `s` (its end when shorter).
fn byte_at(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
}

/// The editor goes when its sign does (AbstractSignEditScreen.isValid; the server closes it
/// when the player walks away).
pub fn editor_valid(g: &Game) -> bool {
    g.block_entities.editor.as_ref().map_or(false, |ed| sign_kind(&g.blocks, g.world.get(ed.pos[0], ed.pos[1], ed.pos[2])).is_some())
}

/// Typed text into the current line, as far as it fits the sign
/// (SharedConstants.isAllowedChatCharacter, and the sign's widest line).
pub fn type_text(g: &mut Game, gui: &Gui, s: &str) {
    let Some(ed) = g.block_entities.editor.as_mut() else { return };
    let (_, max) = metrics(ed.hanging);
    for c in s.chars() {
        if c == '\u{a7}' || (c as u32) < 32 || c as u32 == 127 {
            continue;
        }
        let line = &mut ed.lines[ed.line];
        let at = byte_at(line, ed.cursor);
        line.insert(at, c);
        if gui.text_width(line) > max || line.len() > 384 {
            line.remove(at);
        } else {
            ed.cursor += 1;
        }
    }
    show_in_world(&mut g.block_entities);
}

/// A key on the editor; true when it closes (Escape, or the Done button by keyboard).
pub fn key(g: &mut Game, sc: usize) -> Result<bool, String> {
    const K_RETURN: usize = 40;
    const K_ESC: usize = 41;
    const K_BACKSPACE: usize = 42;
    const K_DOWN: usize = 81;
    const K_UP: usize = 82;
    const K_KP_ENTER: usize = 88;
    const K_DELETE: usize = 76;
    const K_HOME: usize = 74;
    const K_END: usize = 77;
    const K_RIGHT: usize = 79;
    const K_LEFT: usize = 80;
    let Some(ed) = g.block_entities.editor.as_mut() else { return Ok(true) };
    let len = ed.lines[ed.line].chars().count();
    match sc {
        K_ESC => {
            close(g)?;
            return Ok(true);
        }
        // Another line: the cursor to its end (TextFieldHelper.setCursorToEnd).
        K_UP | K_DOWN | K_RETURN | K_KP_ENTER => {
            ed.line = if sc == K_UP { (ed.line + 3) & 3 } else { (ed.line + 1) & 3 };
            ed.cursor = ed.lines[ed.line].chars().count();
        }
        K_LEFT => ed.cursor = ed.cursor.saturating_sub(1),
        K_RIGHT => ed.cursor = (ed.cursor + 1).min(len),
        K_HOME => ed.cursor = 0,
        K_END => ed.cursor = len,
        K_BACKSPACE | K_DELETE => {
            let at = if sc == K_BACKSPACE { ed.cursor.checked_sub(1) } else { Some(ed.cursor).filter(|c| *c < len) };
            if let Some(at) = at {
                let line = &mut ed.lines[ed.line];
                let b = byte_at(line, at);
                line.remove(b);
                ed.cursor = at;
                show_in_world(&mut g.block_entities);
            }
        }
        _ => {}
    }
    Ok(false)
}

/// Closes the editor and sends the text (AbstractSignEditScreen.removed:
/// ServerboundSignUpdatePacket: position, four lines, the side as a VarInt, 1 front).
pub fn close(g: &mut Game) -> Result<(), String> {
    let Some(ed) = g.block_entities.editor.take() else { return Ok(()) };
    let mut o = Out::new();
    o.i64(((ed.pos[0] as i64 & 0x3FFFFFF) << 38) | ((ed.pos[2] as i64 & 0x3FFFFFF) << 12) | (ed.pos[1] as i64 & 0xFFF));
    for l in ed.lines.iter() {
        o.string(l);
    }
    o.varint(ed.front as i32);
    g.conn.send(crate::ids::play::S_SIGN_UPDATE, &o)
}

/// The Done button (init: width / 2 - 100, height / 4 + 144, 200 x 20).
pub fn done_button(gui: &Gui) -> crate::screens::Button {
    crate::screens::Button::new(gui.width / 2.0 - 100.0, gui.height / 4.0 + 144.0, 200.0, crate::lang::get("gui.done"))
}

fn load_pictures(gl: &Gl) -> Option<Pictures> {
    let d = crate::sys::read_file(&crate::packs::path("signs.bin"))?;
    if d.get(..4)? != SIGNS_MAGIC {
        return None;
    }
    let rd16 = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (w, h) = (rd16(4)? as usize, rd16(6)? as usize);
    let mut p = 8 + w * h * 4;
    let pixels = d.get(8..p)?;
    let n = *d.get(p)? as usize;
    p += 1;
    let mut woods = BTreeMap::new();
    for _ in 0..n {
        let l = *d.get(p)? as usize;
        let name = String::from_utf8_lossy(d.get(p + 1..p + 1 + l)?).into_owned();
        p += 1 + l;
        woods.insert(name, ([rd16(p)?, rd16(p + 2)?], [rd16(p + 4)?, rd16(p + 6)?]));
        p += 8;
    }
    let mut tex = 0u32;
    unsafe {
        (gl.GenTextures)(1, &mut tex);
        (gl.BindTexture)(TEXTURE_2D, tex);
        (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
        (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, pixels.as_ptr() as *const c_void);
    }
    Some(Pictures { tex, w: w as f32, h: h as f32, woods })
}

/// The sign's picture behind the text, in GUI units: SignEditScreen draws gui/signs/<wood>
/// (24 x 26, only the board's 12 rows for a wall sign) at 3.9 times, HangingSignEditScreen
/// gui/hanging_signs/<wood> (16 x 16) at 4.5 times.
fn draw_picture(gl: &Gl, gui: &Gui, pics: &Pictures, ed: &Editor, cx: f32, cy: f32) {
    let Some(&(sp, hp)) = pics.woods.get(&ed.wood) else { return };
    let (src, sw, sh, dx0, dy0, k) = if ed.hanging {
        (hp, 16.0, 16.0, -8.0, -8.0 - 13.0 / 4.5, 4.5)
    } else {
        (sp, 24.0, if ed.wall { 12.0 } else { 26.0 }, -12.0, -13.0 + 27.0 / 3.9, 3.9)
    };
    if src[0] == 0xFFFF {
        return;
    }
    let (x0, y0) = (cx + dx0 * k, cy + dy0 * k);
    let (x1, y1) = (x0 + sw * k, y0 + sh * k);
    let (u0, v0) = (src[0] as f32 / pics.w, src[1] as f32 / pics.h);
    let (u1, v1) = ((src[0] as f32 + sw) / pics.w, (src[1] as f32 + sh) / pics.h);
    let c = [255u8; 4];
    let v = [
        V { p: [x0, y0, 0.0], uv: [u0, v0], c },
        V { p: [x0, y1, 0.0], uv: [u0, v1], c },
        V { p: [x1, y1, 0.0], uv: [u1, v1], c },
        V { p: [x1, y0, 0.0], uv: [u1, v0], c },
    ];
    let m: [f32; 16] = [2.0 / gui.width, 0.0, 0.0, 0.0, 0.0, -2.0 / gui.height, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, -1.0, 1.0, 0.0, 1.0];
    unsafe {
        (gl.MatrixMode)(PROJECTION);
        (gl.LoadMatrixf)(m.as_ptr());
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadIdentity)();
        (gl.Disable)(DEPTH_TEST);
        (gl.Enable)(BLEND);
        (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, pics.tex);
        (gl.EnableClientState)(VERTEX_ARRAY);
        (gl.EnableClientState)(COLOR_ARRAY);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        let base = v.as_ptr() as *const u8;
        (gl.VertexPointer)(3, FLOAT, 24, base as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, base.add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, base.add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, 4);
        (gl.Disable)(TEXTURE_2D);
        (gl.Disable)(BLEND);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
    }
}

/// The editing screen (AbstractSignEditScreen.extractRenderState): the darkened world, the
/// title, the sign's picture, its four lines with the cursor, and Done. What is in `gui` so
/// far is drawn first, so the picture (its own texture) lies under the text.
pub fn draw_editor(gui: &mut Gui, gl: &Gl, w: i32, h: i32, g: &mut Game, mx: f32, my: f32) {
    let be = &mut g.block_entities;
    if be.pictures.is_none() || be.pictures_of != crate::packs::generation() {
        be.pictures_of = crate::packs::generation();
        be.pictures = Some(load_pictures(gl));
    }
    let Some(ed) = be.editor.as_ref() else { return };
    crate::screens::dim(gui);
    let cx = gui.width / 2.0;
    let title = crate::lang::get(if ed.hanging { "hanging_sign.edit" } else { "sign.edit" });
    gui.text_centered(&title, cx, 40.0, crate::screens::WHITE);
    let cy = if ed.hanging { 125.0 } else { 90.0 };
    if let Some(Some(pics)) = be.pictures.as_ref() {
        gui.flush(gl, w, h);
        draw_picture(gl, gui, pics, ed, cx, cy);
        gui.begin(w, h);
    } else if ed.hanging {
        gui.fill(cx - 36.0, cy - 49.0, cx + 36.0, cy + 23.0, 0xFF9C_7F4E);
    } else {
        gui.fill(cx - 46.8, cy - 23.7, cx + 46.8, cy + 23.1, 0xFF9C_7F4E);
    }
    // SignEditScreen scales its text by 0.9765628, which rounds to the same pixels.
    let (lh, _) = metrics(ed.hanging);
    let top = (4 * lh / 2) as f32;
    let blink = (crate::sys::now_ms().saturating_sub(ed.opened) / 300) % 2 == 0;
    let color = 0xFF00_0000 | ed.color;
    for (i, line) in ed.lines.iter().enumerate() {
        let lw = gui.text_width(line);
        let x = cx + (-(lw as i32) / 2) as f32;
        let y = cy + i as f32 * lh as f32 - top;
        gui.text_plain(line, x, y, color);
        if i == ed.line && blink {
            let at = byte_at(line, ed.cursor);
            if at >= line.len() {
                // TextCursorUtils.extractAppendCursor: "_" after the text.
                gui.text_plain("_", x + lw, y, color);
            } else {
                // extractInsertCursor: a bar the line's height before the character.
                let cx = x + gui.text_width(&line[..at]);
                gui.fill(cx, y - 1.0, cx + 1.0, y + lh as f32 - 1.0, color);
            }
        }
    }
    done_button(gui).draw(gui, mx, my);
}

/// Test input: types lines into an open editor (PENRYN_TEST_INPUT "N" closes it after 2 s,
/// "M" leaves it open for a screenshot).
pub fn test_type(g: &mut Game, gui: &Gui, close_after: bool) -> Result<bool, String> {
    let Some(ed) = g.block_entities.editor.as_ref() else { return Ok(false) };
    if ed.lines.iter().all(|l| l.is_empty()) {
        for (i, s) in ["Penryn", "signs work", "on 10.6", ""].iter().enumerate() {
            if let Some(ed) = g.block_entities.editor.as_mut() {
                ed.line = i;
                ed.cursor = 0;
            }
            type_text(g, gui, s);
        }
        if let Some(ed) = g.block_entities.editor.as_mut() {
            ed.line = 0;
            ed.cursor = 3;
        }
    }
    let opened = g.block_entities.editor.as_ref().map_or(0, |e| e.opened);
    if close_after && crate::sys::now_ms() > opened + 2000 {
        close(g)?;
        return Ok(true);
    }
    Ok(false)
}

/// Selftest: a sign's data as the server sends it, read back, and the text placement.
pub fn selftest(blocks: Option<&Blocks>) -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    // {front_text:{messages:["Hi",{text:"Red",color:"red"},"",""],color:"blue",has_glowing_text:1b},is_waxed:1b}
    let mut d: Vec<u8> = Vec::new();
    let s = |d: &mut Vec<u8>, v: &str| {
        d.extend_from_slice(&(v.len() as u16).to_be_bytes());
        d.extend_from_slice(v.as_bytes());
    };
    d.push(10);
    d.push(10);
    s(&mut d, "front_text");
    d.push(9);
    s(&mut d, "messages");
    d.push(10);
    d.extend_from_slice(&4i32.to_be_bytes());
    for (text, colour) in [("Hi", None), ("Red", Some("red")), ("", None), ("", None)] {
        d.push(8);
        s(&mut d, "text");
        s(&mut d, text);
        if let Some(c) = colour {
            d.push(8);
            s(&mut d, "color");
            s(&mut d, c);
        }
        d.push(0);
    }
    d.push(8);
    s(&mut d, "color");
    s(&mut d, "blue");
    d.push(1);
    s(&mut d, "has_glowing_text");
    d.push(1);
    d.push(0);
    d.push(1);
    s(&mut d, "is_waxed");
    d.push(1);
    d.push(0);
    let mut be = BlockEntities::new();
    let ok = be.read_tag(1, 2, 3, &mut In::new(&d)).is_some();
    let got = be.signs.get(&(1, 2, 3)).map_or(String::from("none"), |sg| {
        alloc::format!("{} {:?} {:?} {} {} {}", ok, sg.front.lines[0], plain(&sg.front.lines[1]), DYE_NAMES[sg.front.color as usize], sg.front.glow, sg.waxed)
    });
    out.push(("sign block entity read", got, "true \"Hi\" \"Red\" blue true true"));
    // A malformed tag (a list of End tags with a huge count) fails at once instead of spinning.
    let mut bad: Vec<u8> = alloc::vec![10, 10];
    s(&mut bad, "front_text");
    bad.push(9);
    s(&mut bad, "messages");
    bad.push(0);
    bad.extend_from_slice(&i32::MAX.to_be_bytes());
    let mut be2 = BlockEntities::new();
    let t0 = crate::sys::now_ms();
    let failed = be2.read_tag(0, 0, 0, &mut In::new(&bad)).is_none();
    out.push(("malformed sign tag rejected fast", alloc::format!("{} {}", failed, crate::sys::now_ms() - t0 < 50), "true true"));
    // The transform: a standing sign at rotation 0 shows its front text to the south, above
    // the block's middle; a wall sign facing north sits against the block's south side.
    let k = Kind { hanging: false, wall: false, angle: 0.0, wood: "oak" };
    let m = text_transform(&k, true, [0.0; 3]);
    let o = [m[0][3], m[1][3], m[2][3]];
    out.push(("standing sign text origin", alloc::format!("{:.4} {:.4} {:.4}", o[0], o[1], o[2]), "0.5000 0.8333 0.5467"));
    let k = Kind { hanging: false, wall: true, angle: 180.0, wood: "oak" };
    let m = text_transform(&k, true, [0.0; 3]);
    out.push(("wall sign (north) text origin", alloc::format!("{:.4} {:.4} {:.4}", m[0][3], m[1][3], m[2][3]), "0.5000 0.5208 0.8908"));
    if let Some(b) = blocks {
        let first = (0..b.states.len()).find(|&i| b.blocks[b.states[i].block as usize].name == "oak_sign");
        let got = first.map_or(String::from("no oak_sign"), |f| {
            let k = sign_kind(b, f as u16 + 2 * 5 + 1);
            k.map_or(String::from("not a sign"), |k| alloc::format!("{} {} {} {}", k.hanging, k.wall, k.angle, k.wood))
        });
        out.push(("oak_sign rotation=5 state", got, "false false 112.5 oak"));
        let first = (0..b.states.len()).find(|&i| b.blocks[b.states[i].block as usize].name == "spruce_wall_hanging_sign");
        let got = first.map_or(String::from("none"), |f| {
            let k = sign_kind(b, f as u16 + 2);
            k.map_or(String::from("not a sign"), |k| alloc::format!("{} {} {} {}", k.hanging, k.wall, k.angle, k.wood))
        });
        out.push(("spruce_wall_hanging_sign facing=south", got, "true true 0 spruce"));
    }
    out
}
