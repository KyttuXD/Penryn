//! Maps as vanilla shows them: ClientboundMapItemDataPacket's colours (MapColor's table and
//! brightness) and decorations into a 128 x 128 texture per map, drawn in the first-person
//! hand (renderTwoHandedMap, renderOneHandedMap) and in item frames; and item frames and glow
//! item frames themselves (ItemFrameRenderer): the frame model, and the item or map in it.

use crate::entities::Entity;
use crate::hand::M;
use crate::math;
use crate::sdl::*;
use crate::wire::In;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ffi::c_void;

pub const C_MAP_ITEM_DATA: i32 = 52;
pub const ITEM_FRAME: i32 = 74;
pub const GLOW_ITEM_FRAME: i32 = 61;
/// Entity data keys Penryn keeps for frames beside the synced ones: the spawn packet's data
/// (the facing), and the map id of the framed stack.
pub const SPAWN_DATA: u8 = 0xFE;
pub const FRAMED_MAP: u8 = 0xFD;

pub struct Decoration {
    pub kind: i32,
    pub x: i8,
    pub y: i8,
    pub rot: i8,
}

pub struct MapData {
    pub colors: Vec<u8>,
    pub decorations: Vec<Decoration>,
    pub scale: i8,
    pub locked: bool,
    /// Its texture (0 until drawn once).
    pub tex: u32,
    dirty: bool,
}

pub struct Maps {
    maps: BTreeMap<i32, MapData>,
}

pub fn is_frame(kind: i32) -> bool {
    kind == ITEM_FRAME || kind == GLOW_ITEM_FRAME
}

/// MapColor.getColorFromPackedId: the colour of a map pixel (ARGB; 0 is transparent).
pub fn pixel(colours: &[u32], id: u8) -> u32 {
    let base = (id >> 2) as usize;
    if base == 0 {
        return 0;
    }
    let c = colours.get(base).copied().unwrap_or(0);
    let m = [180u32, 220, 255, 135][(id & 3) as usize];
    let ch = |s: u32| ((c >> s) & 0xFF) * m / 255;
    0xFF00_0000 | ch(16) << 16 | ch(8) << 8 | ch(0)
}

/// The map_id component in a stack's patch (creative::read_stack's form).
pub fn map_id(patch: &[u8]) -> Option<i32> {
    crate::menus::component(patch, 48).and_then(|v| In::new(v).varint())
}

/// The map_id of an item stack as it is on the wire (ItemStack.OPTIONAL_STREAM_CODEC).
pub fn map_id_in_stack(raw: &[u8]) -> Option<i32> {
    let mut r = In::new(raw);
    if r.varint()? <= 0 {
        return None;
    }
    r.varint()?;
    let add = r.varint()?;
    r.varint()?;
    for _ in 0..add {
        let kind = r.varint()?;
        if kind == 48 {
            return r.varint();
        }
        crate::client::skip_component(kind, &mut r)?;
    }
    None
}

impl Maps {
    pub fn new() -> Maps {
        Maps { maps: BTreeMap::new() }
    }

    pub fn get(&self, id: i32) -> Option<&MapData> {
        self.maps.get(&id)
    }

    /// ClientboundMapItemDataPacket: id, scale, locked, the decorations (when sent), the patch.
    pub fn packet(&mut self, r: &mut In) -> Option<()> {
        let id = r.varint()?;
        let scale = r.u8()? as i8;
        let locked = r.bool()?;
        let m = self.maps.entry(id).or_insert_with(|| MapData { colors: alloc::vec![0; 128 * 128], decorations: Vec::new(), scale, locked, tex: 0, dirty: true });
        m.scale = scale;
        m.locked = locked;
        if r.bool()? {
            let n = r.varint()?;
            m.decorations.clear();
            for _ in 0..n.clamp(0, 1024) {
                let kind = r.varint()?;
                let (x, y, rot) = (r.u8()? as i8, r.u8()? as i8, r.u8()? as i8);
                if r.bool()? {
                    crate::nbt::skip_root(r)?;
                }
                m.decorations.push(Decoration { kind, x, y, rot: rot & 15 });
            }
        }
        let w = r.u8()? as usize;
        if w > 0 {
            let h = r.u8()? as usize;
            let (x0, y0) = (r.u8()? as usize, r.u8()? as usize);
            let n = r.varint()? as usize;
            let px = r.take(n)?;
            for j in 0..h {
                for i in 0..w {
                    if x0 + i < 128 && y0 + j < 128 {
                        if let Some(c) = px.get(i + j * w) {
                            m.colors[x0 + i + (y0 + j) * 128] = *c;
                        }
                    }
                }
            }
            m.dirty = true;
        }
        Some(())
    }

    /// Uploads the maps that changed (before drawing).
    pub fn upload(&mut self, gl: &Gl) {
        let Some(a) = crate::menus::assets() else { return };
        for m in self.maps.values_mut().filter(|m| m.dirty) {
            m.dirty = false;
            let mut px = Vec::with_capacity(128 * 128 * 4);
            for &c in m.colors.iter() {
                let p = pixel(&a.map_colours, c);
                px.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
            unsafe {
                if m.tex == 0 {
                    (gl.GenTextures)(1, &mut m.tex);
                    (gl.BindTexture)(TEXTURE_2D, m.tex);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
                }
                (gl.BindTexture)(TEXTURE_2D, m.tex);
                (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
                (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, 128, 128, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// Quads to draw, each run with its texture.
struct Batch {
    runs: Vec<(u32, Vec<V>)>,
    /// The light colour the quads added from now on are multiplied by.
    tint: [f32; 3],
}

impl Batch {
    fn new() -> Batch {
        Batch { runs: Vec::new(), tint: [1.0; 3] }
    }
    fn quad(&mut self, tex: u32, m: &M, p: [[f64; 3]; 4], uv: [[f32; 2]; 4], c: [u8; 4]) {
        if self.runs.last().map_or(true, |r| r.0 != tex) {
            self.runs.push((tex, Vec::new()));
        }
        let c = crate::lightmap::tint(c, self.tint);
        let v = &mut self.runs.last_mut().unwrap().1;
        for i in 0..4 {
            v.push(V { p: m.apply(p[i]), uv: uv[i], c });
        }
    }
    fn draw(&self, gl: &Gl) {
        unsafe {
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.Enable)(TEXTURE_2D);
            for (tex, v) in self.runs.iter() {
                if v.is_empty() {
                    continue;
                }
                (gl.BindTexture)(TEXTURE_2D, *tex);
                (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
                (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            }
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
    }
}

/// MapRenderer.render in map space (0..128, y down), after `m`: the map's picture and its
/// decorations (in a frame only those shown on frames). `z` steps the layers apart.
fn map_quads(b: &mut Batch, m: &M, data: &MapData, in_frame: bool, z: f64, light: u8) {
    let Some(a) = crate::menus::assets() else { return };
    let c = [light, light, light, 255];
    if data.tex != 0 {
        b.quad(data.tex, m, [[0.0, 128.0, -z], [128.0, 128.0, -z], [128.0, 0.0, -z], [0.0, 0.0, -z]], [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]], c);
    }
    for (k, d) in data.decorations.iter().enumerate() {
        let Some((sprite, on_frame)) = a.decorations.get(d.kind as usize) else { continue };
        if in_frame && !on_frame {
            continue;
        }
        let Some(uv) = a.uv(sprite) else { continue };
        let dm = m
            .t(d.x as f64 / 2.0 + 64.0, d.y as f64 / 2.0 + 64.0, -z * 2.0)
            .r(2, d.rot as f64 * 360.0 / 16.0)
            .s(4.0, 4.0, 3.0)
            .t(-0.125, 0.125, 0.0);
        let dz = -(k as f64) * 0.001;
        b.quad(a.tex, &dm, [[-1.0, 1.0, dz], [1.0, 1.0, dz], [1.0, -1.0, dz], [-1.0, -1.0, dz]], [[uv[0], uv[1]], [uv[2], uv[1]], [uv[2], uv[3]], [uv[0], uv[3]]], c);
    }
}

/// ItemInHandRenderer.renderMap after the arm's pose: the background, then the map.
fn hand_map(b: &mut Batch, m: &M, data: Option<&MapData>) {
    let Some(a) = crate::menus::assets() else { return };
    let m = m.r(1, 180.0).r(2, 180.0).s(0.38, 0.38, 0.38).t(-0.5, -0.5, 0.0).s(0.0078125, 0.0078125, 0.0078125);
    let bg = if data.is_some() { "map/checkerboard" } else { "map/background" };
    if let Some(uv) = a.uv(bg) {
        b.quad(a.tex, &m, [[-7.0, 135.0, 0.0], [135.0, 135.0, 0.0], [135.0, -7.0, 0.0], [-7.0, -7.0, 0.0]], [[uv[0], uv[3]], [uv[2], uv[3]], [uv[2], uv[1]], [uv[0], uv[1]]], [255; 4]);
    }
    if let Some(d) = data {
        map_quads(b, &m, d, false, 0.01, 255);
    }
}

/// The held map(s) in first person. `main`/`off`: the map ids (Some(None): a map without
/// data yet) in each hand; `pitch` the view's; `swing` 0..1, `equip` 1 lowered .. 0.
/// Returns false when the main hand holds no map (hand.rs draws it).
pub fn draw_hand(gl: &Gl, w: i32, h: i32, maps: &Maps, main: Option<Option<i32>>, off_empty: bool, pitch: f32, swing: f32, equip: f32, bob: (f32, f32), lc: [f32; 3]) -> bool {
    let Some(main) = main else { return false };
    if crate::menus::assets().is_none() {
        return false;
    }
    let (walk, amount) = (bob.0 as f64, bob.1 as f64);
    let base = M::id()
        .t(crate::math::sin(walk * crate::math::PI) * amount * 0.5, -(crate::math::cos(walk * crate::math::PI) * amount).abs(), 0.0)
        .r(2, crate::math::sin(walk * crate::math::PI) * amount * 3.0)
        .r(0, (crate::math::cos(walk * crate::math::PI - 0.2) * amount).abs() * 5.0);
    let data = main.and_then(|id| maps.get(id));
    let sw = swing as f64;
    let sq = crate::math::sqrt(sw);
    let pi = crate::math::PI;
    let mut b = Batch::new();
    b.tint = lc;
    if off_empty {
        // renderTwoHandedMap.
        let g = -0.2 * crate::math::sin(sw * pi);
        let hh = -0.4 * crate::math::sin(sq * pi);
        let mut m = base.t(0.0, -g / 2.0, hh);
        let f = (1.0 - pitch as f64 / 45.0 + 0.1).clamp(0.0, 1.0);
        let tilt = -crate::math::cos(f * pi) * 0.5 + 0.5;
        m = m.t(0.0, 0.04 + equip as f64 * -1.2 + tilt * -0.5, -0.72).r(0, tilt * -85.0);
        let j = crate::math::sin(sq * pi);
        m = m.r(0, j * 20.0).s(2.0, 2.0, 2.0);
        hand_map(&mut b, &m, data);
    } else {
        // renderOneHandedMap, the right arm.
        let f = 1.0;
        let mut m = base.t(f * 0.125, -0.125, 0.0);
        m = m.t(f * 0.51, -0.08 + equip as f64 * -1.2, -0.75);
        let hh = crate::math::sin(sq * pi);
        let i = -0.5 * hh;
        let j = 0.4 * crate::math::sin(sq * pi * 2.0);
        let k = -0.3 * crate::math::sin(sw * pi);
        m = m.t(f * i, j - 0.3 * hh, k).r(0, hh * -45.0).r(1, f * hh * -30.0);
        hand_map(&mut b, &m, data);
    }
    let proj = crate::render::perspective_pub(70.0, w as f32 / h as f32, 0.05, 10.0);
    let ident: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    unsafe {
        (gl.Clear)(DEPTH_BUFFER_BIT);
        (gl.MatrixMode)(PROJECTION);
        (gl.LoadMatrixf)(proj.as_ptr());
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(ident.as_ptr());
        // Painter's order: the background, the picture, the decorations.
        (gl.Disable)(DEPTH_TEST);
        (gl.Disable)(CULL_FACE);
        (gl.Disable)(FOG);
        (gl.Enable)(BLEND);
        (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA);
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.004);
    }
    b.draw(gl);
    unsafe {
        (gl.Disable)(BLEND);
        (gl.Disable)(ALPHA_TEST);
        (gl.Enable)(CULL_FACE);
        (gl.Enable)(DEPTH_TEST);
    }
    true
}

/// Direction.from3DDataValue's unit steps (down, up, north, south, west, east).
const STEPS: [[f64; 3]; 6] = [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0], [-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]];

/// ItemFrameRenderer: every item frame within 64 blocks, its model and what it holds.
pub fn draw_frames(
    gl: &Gl,
    view: &[f32; 16],
    eye: [f64; 3],
    ents: &BTreeMap<i32, Entity>,
    maps: &Maps,
    icons: &[crate::gui::Icon],
    overlays: &[crate::itemmodels::Entry],
    blocks: &crate::blocks::Blocks,
    atlas: u32,
    atlas_size: (f32, f32),
    lit: &crate::lightmap::Lit,
) {
    let Some(a) = crate::menus::assets() else { return };
    let mut b = Batch::new();
    let mut flat = Batch::new();
    let tiles_row = (atlas_size.0 / 16.0) as u32;
    for e in ents.values().filter(|e| is_frame(e.kind)) {
        // The spawn packet gives the frame's block (HangingEntity.recreateFromPacket); the
        // renderer's step of 0.46875 out from its centre brings it back to the block's centre.
        let c = [math::floor(e.pos[0]) + 0.5, math::floor(e.pos[1]) + 0.5, math::floor(e.pos[2]) + 0.5];
        let p = [c[0] - eye[0], c[1] - eye[1], c[2] - eye[2]];
        if p[0] * p[0] + p[1] * p[1] + p[2] * p[2] > 64.0 * 64.0 {
            continue;
        }
        // HangingEntity.DATA_DIRECTION (index 8) once synced, else the spawn packet's.
        let dir = e.data.get(&8).or(e.data.get(&SPAWN_DATA)).copied().unwrap_or(3).clamp(0, 5) as usize;
        let st = STEPS[dir];
        let (xr, yr) = if dir >= 2 {
            let yrot = [0.0, 0.0, 180.0, 0.0, 90.0, 270.0][dir];
            (0.0, 180.0 - yrot)
        } else {
            (-90.0 * st[1], 180.0)
        };
        let m = M::id().t(p[0], p[1], p[2]).r(0, xr).r(1, yr);
        // The frame's light is the block's it hangs in; a glow frame has at least block light 5
        // and lights what it holds as block 13, sky 15 (GlowItemFrameRenderer).
        let glow = e.kind == GLOW_ITEM_FRAME;
        let lc = if glow { lit.at_min_block(c, 5) } else { lit.at(c) };
        b.tint = lc;
        let item_lc = if glow { lit.colour(15, 13) } else { lc };
        flat.tint = item_lc;
        let invisible = e.data.get(&0).map_or(false, |f| f & 0x20 != 0);
        let map = e.data.get(&FRAMED_MAP).map(|v| *v as i32).filter(|_| e.item.is_some());
        if !invisible {
            let model = &a.frames[(if glow { 2 } else { 0 } + map.is_some() as usize).min(a.frames.len().saturating_sub(1))];
            let fm = m.t(-0.5, -0.5, -0.5);
            for (c, shade) in model.iter() {
                let l = (shade * 255.0) as u8;
                b.quad(a.tex, &fm, [0, 1, 2, 3].map(|i| [c[i][0] as f64, c[i][1] as f64, c[i][2] as f64]), [0, 1, 2, 3].map(|i| [c[i][3], c[i][4]]), [l, l, l, 255]);
            }
        }
        let Some((item, _)) = e.item else { continue };
        // ItemFrame.DATA_ROTATION is index 10 (8 is the direction, 9 the item).
        let rot = (e.data.get(&10).copied().unwrap_or(0) & 7) as f64;
        let im = m.t(0.0, 0.0, if invisible { 0.5 } else { 0.4375 });
        if let Some(id) = map {
            let mm = im.r(2, (rot as i64 % 4 * 2) as f64 * 45.0).r(2, 180.0).s(0.0078125, 0.0078125, 0.0078125).t(-64.0, -64.0, 0.0).t(0.0, 0.0, -1.0);
            if let Some(d) = maps.get(id) {
                b.tint = item_lc;
                map_quads(&mut b, &mm, d, true, 0.1, 255);
            }
            continue;
        }
        // ItemDisplayContext.FIXED: half size, turned to face out of the frame.
        let im = im.r(2, rot * 45.0).s(0.5, 0.5, 0.5).r(1, 180.0);
        let shown = crate::itemmodels::icon_for(overlays, icons, blocks, item, &e.item_patch);
        match shown.as_ref() {
            Some(crate::gui::Icon::Flat(layers)) => {
                for &(tile, tint) in layers.iter() {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let (u0, v0, u1, v1) = (tx / atlas_size.0, ty / atlas_size.1, (tx + 16.0) / atlas_size.0, (ty + 16.0) / atlas_size.1);
                    let col = if tint != 0 { [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8, 255] } else { [255; 4] };
                    let q = [[-0.5, 0.5, 0.0], [-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.5, 0.5, 0.0]];
                    let uv = [[u0, v0], [u0, v1], [u1, v1], [u1, v0]];
                    flat.quad(atlas, &im, q, uv, col);
                    flat.quad(atlas, &im, [q[3], q[2], q[1], q[0]], [uv[3], uv[2], uv[1], uv[0]], col);
                }
            }
            Some(crate::gui::Icon::Model { first, count, tint, .. }) => {
                let bm = im.s(0.5, 0.5, 0.5).t(-0.5, -0.5, -0.5);
                let tc = if *tint != 0 { [(*tint >> 16) as u8, (*tint >> 8) as u8, *tint as u8] } else { [255; 3] };
                for q in &blocks.quads[*first as usize..(*first as usize + *count as usize).min(blocks.quads.len())] {
                    let l = q.light as f32 / 255.0;
                    let base = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 3] };
                    let col = [(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255];
                    let pos = [0, 1, 2, 3].map(|i| [q.pos[i][0] as f64 / 1024.0, q.pos[i][1] as f64 / 1024.0, q.pos[i][2] as f64 / 1024.0]);
                    let uv = [0, 1, 2, 3].map(|i| [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0]);
                    flat.quad(atlas, &bm, pos, uv, col);
                }
            }
            _ => {}
        }
    }
    if b.runs.is_empty() && flat.runs.is_empty() {
        return;
    }
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.1);
        (gl.Disable)(CULL_FACE);
    }
    b.draw(gl);
    flat.draw(gl);
    unsafe {
        (gl.Enable)(CULL_FACE);
        (gl.Disable)(ALPHA_TEST);
    }
}

pub fn selftest() -> Vec<(&'static str, alloc::string::String, &'static str)> {
    let mut v = Vec::new();
    // A map packet: id 3, scale 0, unlocked, one decoration (player at 0,0 facing 8), a 2x1
    // patch at 10,20.
    let mut o = crate::wire::Out::new();
    o.varint(3).u8(0).bool(false).bool(true).varint(1).varint(0).u8(0).u8(0).u8(8).bool(false);
    o.u8(2).u8(1).u8(10).u8(20).varint(2).bytes(&[5, 6]);
    let mut maps = Maps::new();
    let ok = maps.packet(&mut In::new(&o.b)).is_some();
    let got = maps.get(3).map(|m| alloc::format!("{} {} {} {}", ok, m.colors[10 + 20 * 128], m.colors[11 + 20 * 128], m.decorations.len()));
    v.push(("map item data: patch and decorations", got.unwrap_or_default(), "true 5 6 1"));
    // A wire stack: 1 filled map with map_id 42.
    let mut s = crate::wire::Out::new();
    s.varint(1).varint(1000).varint(1).varint(0).varint(48).varint(42);
    v.push(("map_id of a framed stack", alloc::format!("{:?}", map_id_in_stack(&s.b)), "Some(42)"));
    v
}
