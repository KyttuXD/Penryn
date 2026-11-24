//! Display entities (Display.BlockDisplay, Display.ItemDisplay): a block or an item drawn at the entity's place with
//! the transformation the server sends (translation, scale, two rotations, interpolated over the server's duration)
//! and the billboard setting. Plugins use them for holograms and ragdolls (a ragdoll is item displays of player heads).
//! Text displays are drawn as well (draw_texts).
//!
//! The entity data indices (26.3, read from Display's static initialisers): 8 start delta, 9 duration, 11 translation,
//! 12 scale, 13 left rotation, 14 right rotation, 15 billboard, then 23 the block state (BlockDisplay) or the item stack
//! (ItemDisplay) and 24 the item display context. A TextDisplay has 23 the text, 24 the line width, 25 the background
//! colour, 26 the text opacity and 27 the style flags; 17 is the view range (a float).

use crate::entities::{DrawCx, Entity, EntityTable};
use crate::math;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

/// Translation, scale, left rotation and right rotation (a quaternion is x, y, z, w).
pub type Tr = [[f32; 4]; 4];
const IDENT: Tr = [[0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]];

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// The entity types of the displays this draws: (block_display, item_display), -1 when the table lacks one.
pub fn kinds(table: &EntityTable) -> [i32; 2] {
    let find = |n: &str| table.types.iter().position(|t| t.name == n).map_or(-1, |i| i as i32);
    [find("block_display"), find("item_display")]
}

fn target(e: &Entity) -> Tr {
    let get = |i: u8, d: [f32; 4]| e.fvals.get(&i).copied().unwrap_or(d);
    [get(11, IDENT[0]), get(12, IDENT[1]), get(13, IDENT[2]), get(14, IDENT[3])]
}

fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let s = if dot < 0.0 { -1.0 } else { 1.0 };
    let mut q = [0.0f32; 4];
    for i in 0..4 {
        q[i] = a[i] * (1.0 - t) + b[i] * s * t;
    }
    let n = math::sqrtf(q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]);
    if n < 1e-6 {
        return b;
    }
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

/// The transformation now: the server's, or part of the way from the one it replaced (the interpolation starts
/// `start delta` ticks after the packet and takes `duration` ticks).
pub fn transformation(e: &Entity, partial: f32) -> Tr {
    let to = target(e);
    let Some(from) = e.disp_from else { return to };
    if e.disp_dur <= 0 {
        return to;
    }
    let t = ((e.age as i64 - e.disp_start) as f32 + partial) / e.disp_dur as f32;
    if t >= 1.0 {
        return to;
    }
    if t <= 0.0 {
        return from;
    }
    let l = |a: [f32; 4], b: [f32; 4]| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, 0.0];
    [l(from[0], to[0]), l(from[1], to[1]), nlerp(from[2], to[2], t), nlerp(from[3], to[3], t)]
}

/// The transformation before an entity data packet is read (None for the first one: nothing to glide from).
pub fn snapshot(e: &Entity) -> Option<Tr> {
    if e.fvals.is_empty() {
        None
    } else {
        Some(transformation(e, 1.0))
    }
}

/// After an entity data packet: a new transformation glides from the old one (Display.updateInterpolation).
pub fn data_changed(e: &mut Entity, before: Option<Tr>) {
    let Some(before) = before else { return };
    if target(e) == before {
        return;
    }
    e.disp_from = Some(before);
    e.disp_start = e.age as i64 + e.data.get(&8).copied().unwrap_or(0);
    e.disp_dur = e.data.get(&9).copied().unwrap_or(0) as i32;
}

// ------------------------------------------------------------------ a little affine algebra

/// p' = m p + t.
#[derive(Clone, Copy)]
struct A {
    m: [[f32; 3]; 3],
    t: [f32; 3],
}

fn s(a: f32) -> f32 {
    math::sin(a as f64) as f32
}
fn c(a: f32) -> f32 {
    math::cos(a as f64) as f32
}

impl A {
    fn id() -> A {
        A { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] }
    }
    fn translate(t: [f32; 3]) -> A {
        A { t, ..A::id() }
    }
    fn scale(k: [f32; 3]) -> A {
        A { m: [[k[0], 0.0, 0.0], [0.0, k[1], 0.0], [0.0, 0.0, k[2]]], t: [0.0; 3] }
    }
    fn rot_x(a: f32) -> A {
        A { m: [[1.0, 0.0, 0.0], [0.0, c(a), -s(a)], [0.0, s(a), c(a)]], t: [0.0; 3] }
    }
    fn rot_y(a: f32) -> A {
        A { m: [[c(a), 0.0, s(a)], [0.0, 1.0, 0.0], [-s(a), 0.0, c(a)]], t: [0.0; 3] }
    }
    fn rot_z(a: f32) -> A {
        A { m: [[c(a), -s(a), 0.0], [s(a), c(a), 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] }
    }
    /// JOML's rotationYXZ.
    fn rot_yxz(y: f32, x: f32, z: f32) -> A {
        A::rot_y(y).then(&A::rot_x(x)).then(&A::rot_z(z))
    }
    fn quat(q: [f32; 4]) -> A {
        let n = math::sqrtf(q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]);
        if n < 1e-6 {
            return A::id();
        }
        let (x, y, z, w) = (q[0] / n, q[1] / n, q[2] / n, q[3] / n);
        A {
            m: [
                [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
                [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
                [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
            ],
            t: [0.0; 3],
        }
    }
    /// self applied after `b` (self * b).
    fn then(&self, b: &A) -> A {
        let mut m = [[0.0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = self.m[i][0] * b.m[0][j] + self.m[i][1] * b.m[1][j] + self.m[i][2] * b.m[2][j];
            }
        }
        let t = self.apply(b.t);
        A { m, t }
    }
    fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.m[0][2] * p[2] + self.t[0],
            self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.m[1][2] * p[2] + self.t[1],
            self.m[2][0] * p[0] + self.m[2][1] * p[1] + self.m[2][2] * p[2] + self.t[2],
        ]
    }
}

fn rad(deg: f32) -> f32 {
    deg * (core::f32::consts::PI / 180.0)
}

/// Display.calculateOrientation: how the entity faces (billboard 0 fixed, 1 vertical, 2 horizontal, 3 centre).
fn orientation(e: &Entity, cam_yaw: f32, cam_pitch: f32) -> A {
    let (yaw, pitch) = (rad(e.yaw), rad(e.pitch));
    let (cy, cp) = (rad(cam_yaw), rad(cam_pitch));
    let pi = core::f32::consts::PI;
    match e.data.get(&15).copied().unwrap_or(0) {
        1 => A::rot_yxz(pi - cy, pitch, 0.0),
        2 => A::rot_yxz(-yaw, -cp, 0.0),
        3 => A::rot_yxz(pi - cy, -cp, 0.0),
        _ => A::rot_yxz(-yaw, pitch, 0.0),
    }
}

/// T * left * scale * right.
fn matrix(t: &Tr) -> A {
    A::translate([t[0][0], t[0][1], t[0][2]]).then(&A::quat(t[2])).then(&A::scale([t[1][0], t[1][1], t[1][2]])).then(&A::quat(t[3]))
}

// ------------------------------------------------------------------ the item display context (ItemDisplayContext)

/// The `display` transform of the skull item model for a context (models/item/template_skull.json): rotation
/// (degrees, x y z), translation (sixteenths) and scale; contexts it does not list are not transformed.
fn skull_context(ctx: i64) -> A {
    let (rot, tr, sc): ([f32; 3], [f32; 3], f32) = match ctx {
        2 | 1 => ([45.0, 45.0, 0.0], [0.0, 3.0, 0.0], 0.5),
        6 => ([30.0, 45.0, 0.0], [0.0, 3.0, 0.0], 1.0),
        7 => ([0.0, 0.0, 0.0], [0.0, 3.0, 0.0], 0.5),
        8 => ([0.0, 180.0, 0.0], [0.0, 4.0, 0.0], 1.0),
        _ => return A::id(),
    };
    // ItemTransform.apply: translate, rotate (XYZ), scale.
    A::translate([tr[0] / 16.0, tr[1] / 16.0, tr[2] / 16.0])
        .then(&A::rot_x(rad(rot[0])))
        .then(&A::rot_y(rad(rot[1])))
        .then(&A::rot_z(rad(rot[2])))
        .then(&A::scale([sc, sc, sc]))
}

/// An ItemTransform (rotation in degrees, translation in sixteenths, scale), applied as translate, rotate XYZ, scale.
fn item_transform(t: [f32; 9]) -> A {
    A::translate([t[3] / 16.0, t[4] / 16.0, t[5] / 16.0])
        .then(&A::rot_x(rad(t[0])))
        .then(&A::rot_y(rad(t[1])))
        .then(&A::rot_z(rad(t[2])))
        .then(&A::scale([t[6], t[7], t[8]]))
}

/// The skin URL of a player head's profile (the `minecraft:profile` component, id 72, in the stack's patch).
pub fn head_skin(patch: &[u8]) -> Option<String> {
    use crate::wire::In;
    let mut r = In::new(patch);
    let (add, remove) = (r.varint()?, r.varint()?);
    let _ = remove;
    for _ in 0..add {
        let kind = r.varint()?;
        let len = r.varint()? as usize;
        let v = r.take(len)?;
        if kind != 72 {
            continue;
        }
        let mut p = In::new(v);
        // ResolvableProfile: a partial one (0: name, id, properties) or a full one (id, name, properties).
        if p.u8()? == 0 {
            if p.u8()? != 0 {
                p.string()?;
            }
            if p.u8()? != 0 {
                p.take(16)?;
            }
        } else {
            p.take(16)?;
            p.string()?;
        }
        for _ in 0..p.varint()?.max(0) {
            let name = p.string()?;
            let value = p.string()?;
            if p.u8()? != 0 {
                p.string()?;
            }
            if name == "textures" {
                return crate::skins::parse_textures(&value)?.skin;
            }
        }
        return None;
    }
    None
}

/// One face of a head cube: the corners' signs (a, b) on the face's two axes, and the texture corners (u, v) in
/// pixels from the cube's texture origin. The face's axis and side are in `FACES`.
struct Face {
    axis: u8,
    side: f32,
    corners: [(f32, f32, u8, u8); 4],
}

// Order in each corner: the two signs on the other axes, then u and v in pixels (U+u, V+v). Axes: 0 x, 1 y, 2 z; the
// two signs belong to the remaining axes in increasing order.
const FACES: [Face; 6] = [
    // front (z -4): x, y
    Face { axis: 2, side: -1.0, corners: [(-1.0, -1.0, 8, 8), (1.0, -1.0, 16, 8), (1.0, 1.0, 16, 16), (-1.0, 1.0, 8, 16)] },
    // back (z +4)
    Face { axis: 2, side: 1.0, corners: [(1.0, -1.0, 24, 8), (-1.0, -1.0, 32, 8), (-1.0, 1.0, 32, 16), (1.0, 1.0, 24, 16)] },
    // right (x -4): y, z
    Face { axis: 0, side: -1.0, corners: [(-1.0, 1.0, 0, 8), (-1.0, -1.0, 8, 8), (1.0, -1.0, 8, 16), (1.0, 1.0, 0, 16)] },
    // left (x +4)
    Face { axis: 0, side: 1.0, corners: [(-1.0, -1.0, 16, 8), (-1.0, 1.0, 24, 8), (1.0, 1.0, 24, 16), (1.0, -1.0, 16, 16)] },
    // top (y -4 in model space, which is down): x, z
    Face { axis: 1, side: -1.0, corners: [(-1.0, 1.0, 8, 0), (1.0, 1.0, 16, 0), (1.0, -1.0, 16, 8), (-1.0, -1.0, 8, 8)] },
    // bottom
    Face { axis: 1, side: 1.0, corners: [(-1.0, -1.0, 16, 0), (1.0, -1.0, 24, 0), (1.0, 1.0, 24, 8), (-1.0, 1.0, 16, 8)] },
];

/// The vertices of a player head (the head and its hat layer), in the head item's own space after the display
/// context: the cube spans 0.5 around the origin for the fixed context.
fn skull_vertices(out: &mut Vec<V>, place: &A, col: [u8; 4]) {
    // Model space (ModelPart: y down, pixels) to the item's space: SkullBlockRenderer's scale(-1, -1, 1) with the head
    // turned 180 degrees, the base of the head at the item's (0.5, 0, 0.5) less the model's -0.5.
    let to_item = |m: [f32; 3]| -> [f32; 3] { [m[0] / 16.0, -m[1] / 16.0 - 0.5, -m[2] / 16.0] };
    for (u0, grow) in [(0.0f32, 0.0f32), (32.0, 0.25)] {
        let h = 4.0 + grow;
        for f in FACES.iter() {
            for &(a, b, du, dv) in f.corners.iter() {
                // The corner on the face: `side` on its axis, a and b on the other two (increasing axis order).
                let mut k = [0.0f32; 3];
                let mut o = [a, b].into_iter();
                for ax in 0..3 {
                    k[ax] = if ax as u8 == f.axis { f.side } else { o.next().unwrap_or(0.0) };
                }
                // The head box is -4..4 in x and z, -8..0 in y: centre y at -4.
                let m = [k[0] * h, -4.0 + k[1] * h, k[2] * h];
                out.push(V { p: place.apply(to_item(m)), uv: [(u0 + du as f32) / 64.0, dv as f32 / 64.0], c: col });
            }
        }
    }
}

// ------------------------------------------------------------------ drawing

fn gl_begin(gl: &Gl, view: &[f32; 16]) {
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Enable)(TEXTURE_2D);
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.1);
        (gl.Disable)(CULL_FACE);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
    }
}

fn gl_submit(gl: &Gl, tex: u32, v: &[V]) {
    if v.is_empty() {
        return;
    }
    unsafe {
        (gl.BindTexture)(TEXTURE_2D, tex);
        (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, v.len() as i32);
    }
}

fn gl_end(gl: &Gl) {
    unsafe {
        (gl.Enable)(CULL_FACE);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
    }
}

// ------------------------------------------------------------------ text displays

/// The entity type of text_display, -1 when the table lacks it.
pub fn text_kind(table: &EntityTable) -> i32 {
    table.types.iter().position(|t| t.name == "text_display").map_or(-1, |i| i as i32)
}

/// Where a line starts in a text of `width` (TextDisplayRenderer: style flags 8 left, 16 right, else centred).
fn line_x(flags: u8, width: i32, line: i32) -> f32 {
    if flags & 8 != 0 {
        0.0
    } else if flags & 16 != 0 {
        (width - line) as f32
    } else {
        width as f32 / 2.0 - line as f32 / 2.0
    }
}

/// Display.brightnessOverride (index 16): the packed light, block light << 4 and sky light << 20, as (sky, block);
/// -1 (or no value) is none.
fn brightness(e: &Entity) -> Option<(u8, u8)> {
    let v = e.data.get(&16).copied().unwrap_or(-1) as i32;
    if v == -1 {
        return None;
    }
    Some((((v >> 20) & 15) as u8, ((v >> 4) & 15) as u8))
}

/// The light a display is drawn in: its brightness override, else the world's light at its place.
fn light(cx: &DrawCx, e: &Entity, at: [f64; 3]) -> [f32; 3] {
    match brightness(e) {
        Some((sky, block)) => cx.lit.colour(sky, block),
        None => cx.lit.at(at),
    }
}

/// The background colour (RGBA): the default flag gives 25% black, else the colour the server sent (0x40000000 until it does).
fn background(e: &Entity, flags: u8) -> [u8; 4] {
    let v = if flags & 4 != 0 { 0x4000_0000 } else { e.data.get(&25).copied().unwrap_or(0x4000_0000) as u32 };
    [(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]
}

/// The text's alpha: Font draws a colour whose alpha is below 4 as opaque.
fn text_alpha(e: &Entity) -> u8 {
    let a = e.data.get(&26).copied().unwrap_or(255) as u8;
    if a < 4 {
        255
    } else {
        a
    }
}

/// One text display's quads: untextured (background, underlines) and glyphs from the GUI atlas, shadow first.
struct Quads {
    see: bool,
    dist: f32,
    bg: Vec<V>,
    glyphs: Vec<V>,
    solid: Vec<V>,
}

/// Font units to the entity's frame (TextDisplayRenderer: rotate 180 about y, scale -0.025, translate (1 - width / 2,
/// 1 - height), the text growing up from the entity's place).
fn text_quads(gui: &crate::gui::Gui, e: &Entity, place: &A, lc: [f32; 3], dist: f32) -> Option<Quads> {
    use crate::text::{BOLD, ITALIC, STRIKETHROUGH, UNDERLINED};
    let rich = e.disp_text.as_ref()?;
    let flags = e.data.get(&27).copied().unwrap_or(0) as u8;
    let lw = e.data.get(&24).copied().unwrap_or(200).max(1) as f32;
    let lines = crate::text::wrap(gui, rich, lw);
    let width = lines.iter().map(|l| l.iter().map(|(st, t)| crate::text::width_of(gui, t, st)).sum::<f32>() as i32).max().unwrap_or(0);
    let n = lines.len() as i32;
    let h = n * 10 - 1;
    // On which side the viewer is: layers are moved a little towards it.
    let t0 = place.apply([0.0; 3]);
    let nz = place.apply([0.0, 0.0, 1.0]);
    let side = if (nz[0] - t0[0]) * -t0[0] + (nz[1] - t0[1]) * -t0[1] + (nz[2] - t0[2]) * -t0[2] >= 0.0 { 1.0 } else { -1.0 };
    let pt = |x: f32, y: f32, layer: f32| place.apply([0.025 * (x + 1.0 - width as f32 / 2.0), 0.025 * (h as f32 - y), side * layer * 0.002]);
    let quad = |out: &mut Vec<V>, x0: f32, y0: f32, x1: f32, y1: f32, uv: [f32; 4], c: [u8; 4], z: f32| {
        out.push(V { p: pt(x0, y0, z), uv: [uv[0], uv[1]], c });
        out.push(V { p: pt(x0, y1, z), uv: [uv[0], uv[3]], c });
        out.push(V { p: pt(x1, y1, z), uv: [uv[2], uv[3]], c });
        out.push(V { p: pt(x1, y0, z), uv: [uv[2], uv[1]], c });
    };
    let mut q = Quads { see: flags & 2 != 0, dist, bg: Vec::new(), glyphs: Vec::new(), solid: Vec::new() };
    let bg = background(e, flags);
    if bg[3] != 0 {
        quad(&mut q.bg, -1.0, -1.0, width as f32 - 1.0, h as f32 - 1.0, [0.0; 4], crate::lightmap::tint(bg, lc), 0.0);
    }
    let alpha = text_alpha(e);
    for (k, line) in lines.iter().enumerate() {
        let y = k as f32 * 10.0;
        let total: f32 = line.iter().map(|(st, t)| crate::text::width_of(gui, t, st)).sum();
        let mut x = line_x(flags, width, total as i32);
        for (st, t) in line.iter() {
            let rgb = st.color.unwrap_or(0xFFFFFF);
            let col = |k: u32| [((rgb >> 16) as u8) / k as u8, ((rgb >> 8) as u8) / k as u8, (rgb as u8) / k as u8, alpha];
            let mut s = String::new();
            if st.font != 0 {
                s.push('\u{a7}');
                s.push(char::from_u32(0xF800 + st.font as u32).unwrap_or('r'));
            }
            s.push_str(t);
            let (bold, italic) = (st.flags & BOLD != 0, st.flags & ITALIC != 0);
            let adv = crate::text::width_of(gui, t, st);
            for (shadow, z) in [(true, 1.0f32), (false, 2.0)] {
                if shadow && flags & 1 == 0 {
                    continue;
                }
                let (c, o) = if shadow { (crate::lightmap::tint(col(4), lc), 1.0) } else { (crate::lightmap::tint(col(1), lc), 0.0) };
                for copy in 0..(1 + bold as i32) {
                    gui.glyph_quads(&s, |gx0, gy0, gx1, gy1, uv| {
                        let (st_, sb) = if italic { (1.0 - 0.25 * gy0, 1.0 - 0.25 * gy1) } else { (0.0, 0.0) };
                        let bx = x + o + copy as f32;
                        let (y0, y1) = (y + o + gy0, y + o + gy1);
                        q.glyphs.push(V { p: pt(bx + gx0 + st_, y0, z), uv: [uv[0], uv[1]], c });
                        q.glyphs.push(V { p: pt(bx + gx0 + sb, y1, z), uv: [uv[0], uv[3]], c });
                        q.glyphs.push(V { p: pt(bx + gx1 + sb, y1, z), uv: [uv[2], uv[3]], c });
                        q.glyphs.push(V { p: pt(bx + gx1 + st_, y0, z), uv: [uv[2], uv[1]], c });
                    });
                }
                for (on, a, b) in [(st.flags & UNDERLINED != 0, 8.0, 9.0), (st.flags & STRIKETHROUGH != 0, 3.5, 4.5)] {
                    if on {
                        quad(&mut q.solid, x + o - 1.0, y + o + a, x + o + adv, y + o + b, [0.0; 4], c, z + 0.5);
                    }
                }
            }
            x += adv;
        }
    }
    Some(q)
}

/// Draws the text displays: a background quad and the lines of text over it, 0.025 blocks a font pixel, in the entity's
/// transformation and billboard; the see-through flag draws through blocks.
fn draw_texts(gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, Entity>, partial: f32, kt: i32, cx: &DrawCx) {
    let Some(gui) = cx.gui else { return };
    let mut all: Vec<Quads> = Vec::new();
    for e in ents.values().filter(|e| e.kind == kt && e.disp_text.is_some()) {
        let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
        let p = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
        let d2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
        // Display.getViewRange: 64 blocks times the entity's view range.
        let range = 64.0 * e.data.get(&17).map_or(1.0, |&b| f32::from_bits(b as u32)) as f64;
        if d2 > range * range {
            continue;
        }
        let lc = light(cx, e, [p[0] + eye[0], p[1] + eye[1], p[2] + eye[2]]);
        let place = A::translate([p[0] as f32, p[1] as f32, p[2] as f32]).then(&orientation(e, cx.yaw, cx.pitch)).then(&matrix(&transformation(e, partial)));
        if let Some(q) = text_quads(gui, e, &place, lc, d2 as f32) {
            all.push(q);
        }
    }
    if all.is_empty() {
        return;
    }
    // Farthest first, so nearer ones blend over them.
    all.sort_by(|a, b| b.dist.partial_cmp(&a.dist).unwrap_or(core::cmp::Ordering::Equal));
    let draw = |list: &Vec<V>, tex: u32| unsafe {
        if list.is_empty() {
            return;
        }
        if tex != 0 {
            (gl.Enable)(TEXTURE_2D);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.BindTexture)(TEXTURE_2D, tex);
        } else {
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
        (gl.VertexPointer)(3, FLOAT, 24, list.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (list.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (list.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, list.len() as i32);
    };
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.Disable)(CULL_FACE);
        (gl.Disable)(FOG);
        (gl.Enable)(BLEND);
        (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.004);
        (gl.DepthMask)(0);
        (gl.DepthFunc)(LEQUAL);
    }
    for q in all.iter() {
        unsafe {
            if q.see {
                (gl.Disable)(DEPTH_TEST);
            } else {
                (gl.Enable)(DEPTH_TEST);
            }
        }
        draw(&q.bg, 0);
        draw(&q.glyphs, gui.texture());
        draw(&q.solid, 0);
    }
    unsafe {
        (gl.Enable)(DEPTH_TEST);
        (gl.DepthFunc)(LESS);
        (gl.DepthMask)(1);
        (gl.Disable)(BLEND);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        (gl.Enable)(CULL_FACE);
        (gl.Enable)(FOG);
    }
}

/// Draws the block, item and text displays: the text last, so its depth-tested but depth-free quads lie over the opaque
/// displays behind it.
pub fn draw(gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, Entity>, partial: f32, table: &EntityTable, cx: &DrawCx, overlays: &[crate::itemmodels::Entry]) {
    draw_blocks(gl, view, eye, ents, partial, table, cx, overlays);
    draw_texts(gl, view, eye, ents, partial, text_kind(table), cx);
}

fn draw_blocks(gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, Entity>, partial: f32, table: &EntityTable, cx: &DrawCx, overlays: &[crate::itemmodels::Entry]) {
    let [kb, ki] = kinds(table);
    if kb < 0 && ki < 0 {
        return;
    }
    let blocks = cx.blocks;
    let tiles_row = (cx.atlas_size.0 / 16.0) as u32;
    let mut atlas_v: Vec<V> = Vec::new();
    let mut heads: Vec<(u32, Vec<V>)> = Vec::new();
    // PENRYN_DEBUG: what became of the displays, said every few seconds. Counts: total, too far, block without a
    // state, item display without an item, no icon for its item, head without its texture yet, then drawn: flat
    // items, models, heads, blocks.
    let mut stats = [0u32; 10];
    for e in ents.values() {
        let block = e.kind == kb;
        if !(block || e.kind == ki) {
            continue;
        }
        stats[0] += 1;
        let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
        let p = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
        if p[0] * p[0] + p[1] * p[1] + p[2] * p[2] > 128.0 * 128.0 {
            stats[1] += 1;
            continue;
        }
        let lc = light(cx, e, [p[0] + eye[0], p[1] + eye[1], p[2] + eye[2]]);
        let place = A::translate([p[0] as f32, p[1] as f32, p[2] as f32])
            .then(&orientation(e, cx.yaw, cx.pitch))
            .then(&matrix(&transformation(e, partial)));
        if block {
            let Some(st) = e.data.get(&23).and_then(|&s| blocks.states.get(s as usize)) else {
                stats[2] += 1;
                continue;
            };
            stats[9] += 1;
            let first = st.first_quad as usize;
            let qs = &blocks.quads[first.min(blocks.quads.len())..(first + st.quads as usize).min(blocks.quads.len())];
            for q in qs {
                let l = q.light as f32 / 255.0;
                let base = if q.flags & crate::blocks::Q_TINTED != 0 { [124.0, 189.0, 107.0] } else { [255.0, 255.0, 255.0] };
                let col = crate::lightmap::tint([(base[0] * l) as u8, (base[1] * l) as u8, (base[2] * l) as u8, 255], lc);
                for i in 0..4 {
                    let m = [q.pos[i][0] as f32 / 1024.0, q.pos[i][1] as f32 / 1024.0, q.pos[i][2] as f32 / 1024.0];
                    atlas_v.push(V { p: place.apply(m), uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0], c: col });
                }
            }
            continue;
        }
        let Some((item, _)) = e.item else {
            stats[3] += 1;
            continue;
        };
        let ctx = e.data.get(&24).copied().unwrap_or(0);
        // ItemDisplayRenderer turns the item half a turn about the vertical.
        let place = place.then(&A::rot_y(core::f32::consts::PI));
        if blocks.items.get(item as usize).map(|n| n.as_str()) == Some("player_head") {
            let Some(tex) = head_skin(&e.item_patch).and_then(|u| crate::skins::texture_for(&u)) else {
                stats[5] += 1;
                continue;
            };
            stats[8] += 1;
            let mut v = Vec::new();
            skull_vertices(&mut v, &place.then(&skull_context(ctx)), crate::lightmap::tint([255; 4], lc));
            heads.push((tex, v));
            continue;
        }
        let shown = crate::itemmodels::icon_for(overlays, cx.icons, blocks, item, &e.item_patch);
        // Flat items at the fixed context (8) are drawn half size (generated models' display transform); a model's
        // own head (5), fixed (8) and ground (7) transforms come from items.bin.
        let k = if ctx == 8 { 0.5 } else { 1.0 };
        let place = match shown.as_ref() {
            Some(crate::gui::Icon::Model { ctx: ctxs, .. }) => {
                let t = match ctx {
                    5 => Some(ctxs[0]),
                    8 => Some(ctxs[1]),
                    7 => Some(ctxs[2]),
                    _ => None,
                };
                match t {
                    Some(t) => place.then(&item_transform(t)),
                    None => place,
                }
            }
            _ => place.then(&A::scale([k, k, k])),
        };
        match shown.as_ref() {
            Some(crate::gui::Icon::Flat(layers)) => {
                stats[6] += 1;
                for &(tile, tint) in layers.iter() {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let (u0, v0, u1, v1) = (tx / cx.atlas_size.0, ty / cx.atlas_size.1, (tx + 16.0) / cx.atlas_size.0, (ty + 16.0) / cx.atlas_size.1);
                    let col = crate::lightmap::tint(if tint != 0 { [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8, 255] } else { [255; 4] }, lc);
                    let corners = [(-0.5, 0.5, u0, v0), (-0.5, -0.5, u0, v1), (0.5, -0.5, u1, v1), (0.5, 0.5, u1, v0)];
                    for order in [[0usize, 1, 2, 3], [3, 2, 1, 0]] {
                        for i in order {
                            let (x, y, u, vv) = corners[i];
                            atlas_v.push(V { p: place.apply([x, y, 0.0]), uv: [u, vv], c: col });
                        }
                    }
                }
            }
            Some(crate::gui::Icon::Model { first, count, tint, .. }) => {
                stats[7] += 1;
                let qs = &blocks.quads[*first as usize..(*first as usize + *count as usize).min(blocks.quads.len())];
                let tc = if *tint != 0 { [(*tint >> 16) as u8, (*tint >> 8) as u8, *tint as u8] } else { [255; 3] };
                for q in qs {
                    let l = q.light as f32 / 255.0;
                    let base = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 3] };
                    let col = crate::lightmap::tint([(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255], lc);
                    for i in 0..4 {
                        let m = [q.pos[i][0] as f32 / 1024.0 - 0.5, q.pos[i][1] as f32 / 1024.0 - 0.5, q.pos[i][2] as f32 / 1024.0 - 0.5];
                        atlas_v.push(V { p: place.apply(m), uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0], c: col });
                    }
                }
            }
            _ => stats[4] += 1,
        }
    }
    if crate::sys::env("PENRYN_DEBUG").is_some() && stats[0] > 0 {
        static CALLS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        if CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) % 200 == 0 {
            crate::say!(
                "displays: {} (far {}, no state {}, no item {}, no icon {}, head no texture {}); drawn: flat {}, model {}, head {}, block {}",
                stats[0], stats[1], stats[2], stats[3], stats[4], stats[5], stats[6], stats[7], stats[8], stats[9]
            );
        }
    }
    if atlas_v.is_empty() && heads.is_empty() {
        return;
    }
    gl_begin(gl, view);
    gl_submit(gl, cx.atlas, &atlas_v);
    for (tex, v) in heads.iter() {
        gl_submit(gl, *tex, v);
    }
    gl_end(gl);
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    // 90 degrees about y turns +z into +x (JOML's rotationY).
    let p = A::rot_y(rad(90.0)).apply([0.0, 0.0, 1.0]);
    // The identity quaternion changes nothing; a quarter turn about z turns +x into +y.
    let q = A::quat([0.0, 0.0, math::sqrtf(0.5), math::sqrtf(0.5)]).apply([1.0, 0.0, 0.0]);
    // Interpolation: halfway from scale 1 to 3 is 2, and the first packet glides from nowhere.
    let mut e = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let first = snapshot(&e).is_none();
    e.fvals.insert(12, [1.0, 1.0, 1.0, 0.0]);
    let before = snapshot(&e);
    e.fvals.insert(12, [3.0, 3.0, 3.0, 0.0]);
    e.data.insert(9, 4);
    data_changed(&mut e, before);
    e.age = 2;
    let mid = transformation(&e, 0.0)[1][0];
    e.age = 9;
    let end = transformation(&e, 0.0)[1][0];
    // A player head's profile (a full one: id, name, one textures property, no signature).
    let mut prof = alloc::vec![1u8];
    prof.extend_from_slice(&[0; 16]);
    prof.push(1);
    prof.push(b'x');
    prof.push(1);
    let value = "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvYWJjIn19fQ==";
    prof.push(8);
    prof.extend_from_slice(b"textures");
    prof.push(value.len() as u8);
    prof.extend_from_slice(value.as_bytes());
    prof.push(0);
    let mut o = crate::wire::Out::new();
    o.varint(1).varint(0).varint(72).varint(prof.len() as i32).bytes(&prof);
    let patch = o.b;
    // The skull of a fixed display is centred on the origin: the head spans 0.5 each way.
    let mut v = Vec::new();
    skull_vertices(&mut v, &A::id().then(&skull_context(8)), [255; 4]);
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for q in v.iter().take(24) {
        for a in 0..3 {
            lo[a] = lo[a].min(q.p[a]);
            hi[a] = hi[a].max(q.p[a]);
        }
    }
    // A text display's text arrives as entity data 23 (kind 5, a string component) and its flags as 27.
    let mut te = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let mut pk = alloc::vec![23u8, 5, 8, 0, 5];
    pk.extend_from_slice(b"Hello");
    pk.extend_from_slice(&[27, 0, 8 | 4 | 2 | 1, 17, 3]);
    pk.extend_from_slice(&0.5f32.to_bits().to_be_bytes());
    pk.push(0xFF);
    let ok = crate::client::entity_data(&mut crate::wire::In::new(&pk), &mut te).is_some();
    let fl = te.data.get(&27).copied().unwrap_or(0) as u8;
    let widths = alloc::format!("{} {} {}", line_x(0, 100, 40), line_x(8, 100, 40), line_x(16, 100, 40));
    alloc::vec![
        ("display rotation about y", alloc::format!("{}", (p[0] - 1.0).abs() < 1e-5 && p[2].abs() < 1e-5), "true"),
        ("display quaternion", alloc::format!("{}", q[0].abs() < 1e-5 && (q[1] - 1.0).abs() < 1e-5), "true"),
        ("display first packet does not glide", alloc::format!("{}", first && e.disp_from.is_some()), "true"),
        ("display transformation glides", alloc::format!("{:.2} {:.2}", mid, end), "2.00 3.00"),
        ("display head skin url", alloc::format!("{:?}", head_skin(&patch)), "Some(\"http://textures.minecraft.net/texture/abc\")"),
        (
            "display text data",
            alloc::format!("{} {:?} {} {}", ok, te.disp_text.as_ref().map(|t| t.string()), fl, f32::from_bits(te.data.get(&17).copied().unwrap_or(0) as u32)),
            "true Some(\"Hello\") 15 0.5"
        ),
        ("display text alignment", widths, "30 0 60"),
        (
            "display text background and alpha",
            alloc::format!("{:?} {:?} {}", background(&te, 4), background(&te, 0), {
                te.data.insert(26, 2);
                text_alpha(&te)
            }),
            "[0, 0, 0, 64] [0, 0, 0, 64] 255"
        ),
        (
            "display fixed head is centred",
            alloc::format!("{:.2} {:.2} {:.2} {:.2}", lo[1], hi[1], lo[0], hi[0]),
            "-0.25 0.25 -0.25 0.25"
        ),
        ("display brightness override", {
            let mut b = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
            let none = brightness(&b);
            b.data.insert(16, -1);
            let minus = brightness(&b);
            b.data.insert(16, (7 << 4 | 12 << 20) as i64);
            alloc::format!("{:?} {:?} {:?}", none, minus, brightness(&b))
        }, "None None Some((12, 7))"),
    ]
}
