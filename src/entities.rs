//! Entities the server tracks for us, and drawing them as vanilla does: each type's own model
//! (vanilla's ModelPart tree, exported by horizon-prepare) with its texture, turned by its
//! body yaw, the head by its head yaw and pitch, legs and arms swinging as it walks
//! (HumanoidModel / QuadrupedModel.setupAnim). Dropped items float and spin as icons.

use crate::gui::{parse_models, rd16, Model};
use crate::math;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

pub struct Entity {
    pub kind: i32,
    pub uuid: [u8; 16],
    pub pos: [f64; 3],
    pub prev: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub body_yaw: f32,
    pub on_ground: bool,
    /// WalkAnimationState: position and speed of the limb swing.
    pub limb_pos: f32,
    pub limb_speed: f32,
    pub prev_limb_speed: f32,
    pub hurt: i32,
    /// A dropped item's stack.
    pub item: Option<(i32, i32)>,
    /// Its byte and VarInt entity data values, by index (a sheep's wool is 18 in 26.3).
    pub data: BTreeMap<u8, i64>,
    pub age: u32,
}

impl Entity {
    pub fn new(kind: i32, uuid: [u8; 16], pos: [f64; 3], yaw: f32, pitch: f32, head_yaw: f32) -> Entity {
        Entity {
            kind,
            uuid,
            pos,
            prev: pos,
            yaw,
            pitch,
            head_yaw,
            body_yaw: yaw,
            on_ground: false,
            limb_pos: 0.0,
            limb_speed: 0.0,
            prev_limb_speed: 0.0,
            hurt: 0,
            item: None,
            data: BTreeMap::new(),
            age: 0,
        }
    }

    /// The client tick: walk animation from the distance moved (LivingEntity.calculateEntityAnimation),
    /// the body turning towards where the head looks.
    pub fn tick(&mut self) {
        let (dx, dz) = (self.pos[0] - self.prev[0], self.pos[2] - self.prev[2]);
        let dist = math::sqrt(dx * dx + dz * dz) as f32;
        let target = (dist * 4.0).min(1.0);
        self.prev_limb_speed = self.limb_speed;
        self.limb_speed += (target - self.limb_speed) * 0.4;
        self.limb_pos += self.limb_speed;
        // Body rotation lags the head (BodyRotationControl), at most 75 degrees behind.
        let mut d = wrap_deg(self.head_yaw - self.body_yaw);
        if dist > 0.0025 {
            d = wrap_deg(self.yaw - self.body_yaw) * 0.3;
        } else if d.abs() > 75.0 {
            d -= 75.0 * d.signum();
        } else {
            d = 0.0;
        }
        self.body_yaw += d;
        if self.hurt > 0 {
            self.hurt -= 1;
        }
        self.age += 1;
        self.prev = self.pos;
    }
}

fn wrap_deg(a: f32) -> f32 {
    // Mth.wrapDegrees, without a float remainder (no libm on Mac OS X 10.6 builds).
    (math::wrap(a as f64 + 180.0, 360.0) - 180.0) as f32
}

pub struct EntityTable {
    tex: u32,
    aw: f32,
    ah: f32,
    pub models: Vec<Model>,
    /// Per entity type: model index (-1: none) and texture rectangle.
    pub types: Vec<(i16, [u16; 4])>,
    /// Per entity type: its hitbox width and height (EntityType.dimensions).
    pub sizes: Vec<(f32, f32)>,
    /// Render layers: (type, model, texture rectangle, kind: 1 wool, 2 undercoat).
    pub layers: Vec<(usize, usize, [u16; 4], u8)>,
    /// The slim and wide player models, and the 18 default skins (9 slim, then 9 wide).
    pub player_models: [i16; 2],
    pub skins: Vec<[u16; 4]>,
    /// The `minecraft:player` entity type id.
    pub player_type: i32,
}

/// DyeColor.textureDiffuseColor, by dye id.
const DYE: [u32; 16] = [
    0xF9FFFE, 0xF9801D, 0xC74EBD, 0x3AB3DA, 0xFED83D, 0x80C71F, 0xF38BAA, 0x474F52, 0x9D9D97, 0x169C9C, 0x8932B8, 0x3C44AA,
    0x835432, 0x5E7C16, 0xB02E26, 0x1D1D21,
];

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

impl EntityTable {
    pub fn texture(&self) -> u32 {
        self.tex
    }
    pub fn atlas_size(&self) -> (f32, f32) {
        (self.aw, self.ah)
    }

    /// DefaultPlayerSkin's index (0-17) for a UUID.
    pub fn skin_index(uuid: &[u8; 16]) -> usize {
        let msb = u64::from_be_bytes(uuid[..8].try_into().unwrap());
        let lsb = u64::from_be_bytes(uuid[8..].try_into().unwrap());
        let hilo = msb ^ lsb;
        (((hilo >> 32) as i32) ^ (hilo as i32)).rem_euclid(18) as usize
    }

    pub fn load(gl: &Gl, d: &[u8]) -> Result<EntityTable, String> {
        if d.len() < 8 || &d[..4] != crate::account::ENTITIES_MAGIC {
            return Err(String::from("entities.bin is damaged"));
        }
        let (aw, ah) = (rd16(d, 4) as usize, rd16(d, 6) as usize);
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
        let mut p = 8 + aw * ah * 4;
        let models = parse_models(d, &mut p);
        let n = crate::gui::rd32(d, p) as usize;
        p += 4;
        let mut types = Vec::with_capacity(n);
        let mut sizes = Vec::with_capacity(n);
        for _ in 0..n {
            let mi = rd16(d, p) as i16;
            types.push((mi, [rd16(d, p + 2), rd16(d, p + 4), rd16(d, p + 6), rd16(d, p + 8)]));
            let f = |o: usize| f32::from_bits(crate::gui::rd32(d, o));
            sizes.push((f(p + 10), f(p + 14)));
            p += 18;
        }
        let mut layers = Vec::new();
        if p + 4 <= d.len() {
            let n = crate::gui::rd32(d, p) as usize;
            p += 4;
            for _ in 0..n {
                let ti = rd16(d, p) as usize;
                let mi = rd16(d, p + 2) as i16;
                let rect = [rd16(d, p + 4), rd16(d, p + 6), rd16(d, p + 8), rd16(d, p + 10)];
                if mi >= 0 {
                    layers.push((ti, mi as usize, rect, d[p + 12]));
                }
                p += 13;
            }
        }
        let mut player_models = [-1i16; 2];
        let mut skins = Vec::new();
        if p + 8 <= d.len() {
            player_models = [rd16(d, p) as i16, rd16(d, p + 2) as i16];
            let n = crate::gui::rd32(d, p + 4) as usize;
            p += 8;
            for _ in 0..n {
                skins.push([rd16(d, p), rd16(d, p + 2), rd16(d, p + 4), rd16(d, p + 6)]);
                p += 8;
            }
        }
        let player_type = if p + 4 <= d.len() { crate::gui::rd32(d, p) as i32 } else { -1 };
        Ok(EntityTable { tex, aw: aw as f32, ah: ah as f32, models, types, sizes, layers, player_models, skins, player_type })
    }

    /// Draws every entity near the eye (vertices relative to the eye, under `view`).
    pub fn draw(&self, gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, Entity>, partial: f32, skip: i32) {
        let mut v: Vec<V> = Vec::new();
        for (&id, e) in ents.iter() {
            if id == skip {
                continue;
            }
            let Some(&(mut mi, mut rect)) = self.types.get(e.kind as usize) else { continue };
            if e.kind == self.player_type && self.skins.len() == 18 {
                // DefaultPlayerSkin by the UUID's hash: slim skins first, then wide.
                let msb = u64::from_be_bytes(e.uuid[..8].try_into().unwrap());
                let lsb = u64::from_be_bytes(e.uuid[8..].try_into().unwrap());
                let hilo = msb ^ lsb;
                let i = (((hilo >> 32) as i32) ^ (hilo as i32)).rem_euclid(18) as usize;
                mi = self.player_models[if i < 9 { 0 } else { 1 }];
                rect = self.skins[i];
            }
            if mi < 0 || rect[2] == 0 {
                continue;
            }
            let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
            let p = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
            if p[0] * p[0] + p[2] * p[2] > 96.0 * 96.0 {
                continue;
            }
            self.model(&mut v, &self.models[mi as usize], rect, p, e, partial, [255; 3]);
            // SheepWoolLayer: the wool in the sheep's colour, or the undercoat once sheared.
            for &(ti, lm, lrect, kind) in self.layers.iter() {
                if ti != e.kind as usize {
                    continue;
                }
                let wool = e.data.get(&18).copied().unwrap_or(0);
                let sheared = wool & 0x10 != 0;
                if (kind == 1 && sheared) || (kind == 2 && !sheared) {
                    continue;
                }
                let c = if kind == 1 { DYE[(wool & 15) as usize] } else { 0xFFFFFF };
                self.model(&mut v, &self.models[lm], lrect, p, e, partial, [(c >> 16) as u8, (c >> 8) as u8, c as u8]);
            }
        }
        if v.is_empty() {
            return;
        }
        unsafe {
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(view.as_ptr());
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, self.tex);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.1);
            (gl.Disable)(CULL_FACE);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
            (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
            (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            (gl.Enable)(CULL_FACE);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
    }

    fn model(&self, out: &mut Vec<V>, m: &Model, rect: [u16; 4], at: [f64; 3], e: &Entity, partial: f32, tint: [u8; 3]) {
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
        fn rot(axis: usize, a: f64) -> M {
            let (s, c) = (math::sin(a), math::cos(a));
            match axis {
                0 => [[1.0, 0.0, 0.0, 0.0], [0.0, c, -s, 0.0], [0.0, s, c, 0.0]],
                1 => [[c, 0.0, s, 0.0], [0.0, 1.0, 0.0, 0.0], [-s, 0.0, c, 0.0]],
                _ => [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            }
        }
        // translate(pos) * Ry(180 - bodyYaw) * scale(-1, -1, 1) * translate(0, -1.501, 0), in pixels/16.
        let yaw = math::to_rad((180.0 - e.body_yaw) as f64);
        let base = mul(
            &mul(&[[1.0, 0.0, 0.0, at[0]], [0.0, 1.0, 0.0, at[1]], [0.0, 0.0, 1.0, at[2]]], &rot(1, yaw)),
            &[[-1.0 / 16.0, 0.0, 0.0, 0.0], [0.0, -1.0 / 16.0, 0.0, 1.501], [0.0, 0.0, 1.0 / 16.0, 0.0]],
        );
        let speed = e.prev_limb_speed + (e.limb_speed - e.prev_limb_speed) * partial;
        let swing = e.limb_pos as f64 * 0.6662;
        let amount = speed.min(1.0) as f64;
        let head_y = math::to_rad(wrap_deg(e.head_yaw - e.body_yaw) as f64);
        let head_x = math::to_rad(e.pitch as f64);
        let mut mats: Vec<M> = Vec::with_capacity(m.parts.len());
        for p in m.parts.iter() {
            let parent = if p.parent >= 0 { mats[p.parent as usize] } else { base };
            let q = p.pose;
            let (mut xr, mut yr) = (q[3] as f64, q[4] as f64);
            match p.name.as_str() {
                "head" => {
                    yr += head_y;
                    xr += head_x;
                }
                // HumanoidModel: legs 1.4, arms 1.0 (x 0.5 of 2.0), opposite phases.
                "right_leg" => xr += math::cos(swing) * 1.4 * amount,
                "left_leg" => xr += math::cos(swing + math::PI) * 1.4 * amount,
                "right_arm" => xr += math::cos(swing + math::PI) * amount,
                "left_arm" => xr += math::cos(swing) * amount,
                // QuadrupedModel
                "right_hind_leg" | "left_front_leg" => xr += math::cos(swing) * 1.4 * amount,
                "left_hind_leg" | "right_front_leg" => xr += math::cos(swing + math::PI) * 1.4 * amount,
                _ => {}
            }
            let mut t = mul(&parent, &[[1.0, 0.0, 0.0, q[0] as f64], [0.0, 1.0, 0.0, q[1] as f64], [0.0, 0.0, 1.0, q[2] as f64]]);
            t = mul(&t, &rot(2, q[5] as f64));
            t = mul(&t, &rot(1, yr));
            t = mul(&t, &rot(0, xr));
            t = mul(&t, &[[q[6] as f64, 0.0, 0.0, 0.0], [0.0, q[7] as f64, 0.0, 0.0], [0.0, 0.0, q[8] as f64, 0.0]]);
            mats.push(t);
        }
        let hurt = e.hurt > 0;
        let (aw, ah) = (self.aw, self.ah);
        for (pi, part) in m.parts.iter().enumerate() {
            let t = &mats[pi];
            for (c, n) in part.polys.iter() {
                // The base scale is 1/16 (and flips y): scale the normal back to unit length.
                let ny = 16.0 * (t[1][0] * n[0] as f64 + t[1][1] * n[1] as f64 + t[1][2] * n[2] as f64);
                // Model space is y-down: a normal pointing up in the world has ny > 0 here.
                let l = (0.7 + 0.3 * ny.clamp(-1.0, 1.0)) as f32;
                let tl = |k: usize| (tint[k] as f32 * l) as u8;
                let col = if hurt { [255, (110.0 * l) as u8, (110.0 * l) as u8, 255] } else { [tl(0), tl(1), tl(2), 255] };
                for k in 0..4 {
                    let v = c[k];
                    let x = t[0][0] * v[0] as f64 + t[0][1] * v[1] as f64 + t[0][2] * v[2] as f64 + t[0][3];
                    let y = t[1][0] * v[0] as f64 + t[1][1] * v[1] as f64 + t[1][2] * v[2] as f64 + t[1][3];
                    let z = t[2][0] * v[0] as f64 + t[2][1] * v[1] as f64 + t[2][2] * v[2] as f64 + t[2][3];
                    out.push(V {
                        p: [x as f32, y as f32, z as f32],
                        uv: [(rect[0] as f32 + v[3] * rect[2] as f32) / aw, (rect[1] as f32 + v[4] * rect[3] as f32) / ah],
                        c: col,
                    });
                }
            }
        }
    }
}

impl EntityTable {
    /// The nearest entity the ray from `eye` along `dir` hits within `reach`, with the
    /// distance and where on it (relative to its position).
    pub fn pick(&self, ents: &BTreeMap<i32, Entity>, eye: [f64; 3], dir: [f64; 3], reach: f64, skip: i32) -> Option<(i32, f64, [f64; 3])> {
        let mut best: Option<(i32, f64, [f64; 3])> = None;
        for (&id, e) in ents.iter() {
            if id == skip || e.item.is_some() {
                continue;
            }
            let (w, h) = self.sizes.get(e.kind as usize).copied().unwrap_or((0.6, 1.8));
            let r = w as f64 / 2.0;
            let lo = [e.pos[0] - r, e.pos[1], e.pos[2] - r];
            let hi = [e.pos[0] + r, e.pos[1] + h as f64, e.pos[2] + r];
            let (mut tmin, mut tmax) = (0.0f64, reach);
            let mut hit = true;
            for a in 0..3 {
                if dir[a].abs() < 1e-12 {
                    if eye[a] < lo[a] || eye[a] > hi[a] {
                        hit = false;
                        break;
                    }
                    continue;
                }
                let (mut t1, mut t2) = ((lo[a] - eye[a]) / dir[a], (hi[a] - eye[a]) / dir[a]);
                if t1 > t2 {
                    core::mem::swap(&mut t1, &mut t2);
                }
                tmin = tmin.max(t1);
                tmax = tmax.min(t2);
                if tmin > tmax {
                    hit = false;
                    break;
                }
            }
            if hit && best.map_or(true, |b| tmin < b.1) {
                let at = [eye[0] + dir[0] * tmin - e.pos[0], eye[1] + dir[1] * tmin - e.pos[1], eye[2] + dir[2] * tmin - e.pos[2]];
                best = Some((id, tmin, at));
            }
        }
        best
    }
}

/// Dropped items (ItemEntityRenderer): the item's icon spinning and bobbing above the
/// ground; flat items as a two-sided card at half size, block items as their model at a quarter.
pub fn draw_items(
    gl: &Gl,
    view: &[f32; 16],
    eye: [f64; 3],
    ents: &BTreeMap<i32, Entity>,
    partial: f32,
    icons: &[crate::gui::Icon],
    blocks: &crate::blocks::Blocks,
    atlas: u32,
    atlas_size: (f32, f32),
) {
    let mut v: Vec<V> = Vec::new();
    let tiles_row = (atlas_size.0 / 16.0) as u32;
    for e in ents.values() {
        let Some((item, _)) = e.item else { continue };
        let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
        let p = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
        if p[0] * p[0] + p[2] * p[2] > 48.0 * 48.0 {
            continue;
        }
        let t = e.age as f64 + partial as f64;
        let bob = math::sin(t / 10.0) * 0.1 + 0.1;
        let spin = t / 20.0;
        let (s, c) = (math::sin(spin), math::cos(spin));
        let place = |x: f64, y: f64, z: f64| -> [f32; 3] { [(p[0] + x * c + z * s) as f32, (p[1] + y + bob) as f32, (p[2] - x * s + z * c) as f32] };
        match icons.get(item as usize) {
            Some(crate::gui::Icon::Flat(layers)) => {
                for &(tile, tint) in layers.iter() {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let (u0, v0, u1, v1) = (tx / atlas_size.0, ty / atlas_size.1, (tx + 16.0) / atlas_size.0, (ty + 16.0) / atlas_size.1);
                    let col = if tint != 0 { [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8, 255] } else { [255; 4] };
                    let (h, y0) = (0.25, 0.125);
                    let corners = [(-h, y0 + 2.0 * h, u0, v0), (-h, y0, u0, v1), (h, y0, u1, v1), (h, y0 + 2.0 * h, u1, v0)];
                    for order in [[0, 1, 2, 3], [3, 2, 1, 0]] {
                        for i in order {
                            let (x, y, u, vv) = corners[i];
                            v.push(V { p: place(x, y, 0.0), uv: [u, vv], c: col });
                        }
                    }
                }
            }
            Some(crate::gui::Icon::Model { first, count, tint, .. }) => {
                let qs = &blocks.quads[*first as usize..(*first as usize + *count as usize).min(blocks.quads.len())];
                let tc = if *tint != 0 { [(*tint >> 16) as u8, (*tint >> 8) as u8, *tint as u8] } else { [255; 3] };
                for q in qs {
                    let l = q.light as f32 / 255.0;
                    let base = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 3] };
                    let col = [(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255];
                    for i in 0..4 {
                        let m = |a: usize| (q.pos[i][a] as f64 / 1024.0 - 0.5) * 0.25;
                        v.push(V {
                            p: place(m(0), m(1) + 0.125 + 0.0625, m(2)),
                            uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0],
                            c: col,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if v.is_empty() {
        return;
    }
    unsafe {
        (gl.MatrixMode)(MODELVIEW);
        (gl.LoadMatrixf)(view.as_ptr());
        (gl.Enable)(TEXTURE_2D);
        (gl.BindTexture)(TEXTURE_2D, atlas);
        (gl.Enable)(ALPHA_TEST);
        (gl.AlphaFunc)(GREATER, 0.1);
        (gl.Disable)(CULL_FACE);
        (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
        (gl.BindBuffer)(ARRAY_BUFFER, 0);
        (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
        (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
        (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
        (gl.DrawArrays)(QUADS, 0, v.len() as i32);
        (gl.Enable)(CULL_FACE);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
    }
}
