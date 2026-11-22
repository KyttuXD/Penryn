//! The first-person hand (ItemInHandRenderer): the held item with its model's
//! firstperson_righthand transform, or the arm with the player's skin when the hand is empty,
//! swinging when used and lowered while a new item comes up. Flat items are extruded from
//! their texture as ItemModelGenerator does: front, back and an edge on every opaque pixel's
//! border with a transparent one.

use crate::blocks::Blocks;
use crate::entities::EntityTable;
use crate::gui::{Gui, Icon};
use crate::math;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ffi::c_void;

/// A 4x4 matrix, row-major, acting on column vectors.
#[derive(Clone, Copy)]
pub struct M([[f64; 4]; 4]);

impl M {
    pub fn id() -> M {
        M([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]])
    }
    pub fn mul(&self, b: &M) -> M {
        let mut r = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                r[i][j] = (0..4).map(|k| self.0[i][k] * b.0[k][j]).sum();
            }
        }
        M(r)
    }
    pub fn t(&self, x: f64, y: f64, z: f64) -> M {
        self.mul(&M([[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z], [0.0, 0.0, 0.0, 1.0]]))
    }
    pub fn s(&self, x: f64, y: f64, z: f64) -> M {
        self.mul(&M([[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]]))
    }
    /// A rotation by `deg` degrees about axis 0 x, 1 y, 2 z (Axis.XP and friends).
    pub fn r(&self, axis: usize, deg: f64) -> M {
        let a = math::to_rad(deg);
        let (s, c) = (math::sin(a), math::cos(a));
        let m = match axis {
            0 => [[1.0, 0.0, 0.0, 0.0], [0.0, c, -s, 0.0], [0.0, s, c, 0.0], [0.0, 0.0, 0.0, 1.0]],
            1 => [[c, 0.0, s, 0.0], [0.0, 1.0, 0.0, 0.0], [-s, 0.0, c, 0.0], [0.0, 0.0, 0.0, 1.0]],
            _ => [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
        };
        self.mul(&M(m))
    }
    pub fn apply(&self, p: [f64; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            (m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3]) as f32,
            (m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3]) as f32,
            (m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3]) as f32,
        ]
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct V {
    p: [f32; 3],
    uv: [f32; 2],
    c: [u8; 4],
}

/// A model quad: corners (block units 0..1), uvs, brightness.
type Q = ([[f64; 3]; 4], [[f32; 2]; 4], f32, [u8; 3]);

pub struct Hand {
    flat: BTreeMap<i32, Vec<Q>>,
}

/// ItemModelGenerator for one 16x16 tile at (tx, ty) in the atlas: 1/16 thick at z 7.5..8.5.
fn extrude(alpha: &[u8], aw: usize, ah: usize, tx: usize, ty: usize, tint: [u8; 3], out: &mut Vec<Q>) {
    let a = |x: i32, y: i32| -> bool {
        if !(0..16).contains(&x) || !(0..16).contains(&y) {
            return false;
        }
        let (px, py) = (tx + x as usize, ty + y as usize);
        px < aw && py < ah && alpha[py * aw + px] > 0
    };
    let uv = |x: f32, y: f32| [(tx as f32 + x) / aw as f32, (ty as f32 + y) / ah as f32];
    let (z0, z1) = (7.5 / 16.0, 8.5 / 16.0);
    // Front (south, +z) and back.
    out.push(([[0.0, 1.0, z1], [0.0, 0.0, z1], [1.0, 0.0, z1], [1.0, 1.0, z1]], [uv(0.0, 0.0), uv(0.0, 16.0), uv(16.0, 16.0), uv(16.0, 0.0)], 1.0, tint));
    out.push(([[1.0, 1.0, z0], [1.0, 0.0, z0], [0.0, 0.0, z0], [0.0, 1.0, z0]], [uv(16.0, 0.0), uv(16.0, 16.0), uv(0.0, 16.0), uv(0.0, 0.0)], 1.0, tint));
    for y in 0..16 {
        for x in 0..16 {
            if !a(x, y) {
                continue;
            }
            let (fx0, fx1) = (x as f64 / 16.0, (x + 1) as f64 / 16.0);
            let (fy0, fy1) = (1.0 - (y + 1) as f64 / 16.0, 1.0 - y as f64 / 16.0);
            let c = uv(x as f32 + 0.5, y as f32 + 0.5);
            let cs = [c; 4];
            if !a(x, y - 1) {
                out.push(([[fx0, fy1, z0], [fx0, fy1, z1], [fx1, fy1, z1], [fx1, fy1, z0]], cs, 1.0, tint));
            }
            if !a(x, y + 1) {
                out.push(([[fx0, fy0, z1], [fx0, fy0, z0], [fx1, fy0, z0], [fx1, fy0, z1]], cs, 0.5, tint));
            }
            if !a(x - 1, y) {
                out.push(([[fx0, fy0, z0], [fx0, fy0, z1], [fx0, fy1, z1], [fx0, fy1, z0]], cs, 0.6, tint));
            }
            if !a(x + 1, y) {
                out.push(([[fx1, fy0, z1], [fx1, fy0, z0], [fx1, fy1, z0], [fx1, fy1, z1]], cs, 0.6, tint));
            }
        }
    }
}

impl Hand {
    pub fn new() -> Hand {
        Hand { flat: BTreeMap::new() }
    }

    /// Draws the hand over the world. `swing` 0..1 (0: none), `equip` 0 raised .. 1 lowered,
    /// `bob` the walk phase and amount, `skin` the player's entity skin index.
    pub fn draw(
        &mut self,
        gl: &Gl,
        w: i32,
        h: i32,
        held: Option<i32>,
        gui: &Gui,
        blocks: &Blocks,
        atlas: u32,
        atlas_size: (f32, f32),
        atlas_alpha: &[u8],
        ents: Option<&EntityTable>,
        skin: usize,
        swing: f32,
        equip: f32,
        bob: (f32, f32),
    ) {
        // GameRenderer.bobView, on the hand's own pose.
        let (walk, amount) = (bob.0 as f64, bob.1 as f64);
        let base = M::id()
            .t(math::sin(walk * math::PI) * amount * 0.5, -(math::cos(walk * math::PI) * amount).abs(), 0.0)
            .r(2, math::sin(walk * math::PI) * amount * 3.0)
            .r(0, (math::cos(walk * math::PI - 0.2) * amount).abs() * 5.0);
        let sw = swing as f64;
        let sq = math::sqrt(sw);
        let mut quads: Vec<(M, Q)> = Vec::new();
        let mut tex = atlas;
        let mut tex_size = atlas_size;
        match held.and_then(|i| gui.icons.get(i as usize).map(|ic| (i, ic))) {
            Some((item, icon)) => {
                // renderArmWithItem: swing translation, applyItemArmTransform, applyItemArmAttackTransform.
                let mut m = base.t(-0.4 * math::sin(sq * math::PI), 0.2 * math::sin(sq * math::PI * 2.0), -0.2 * math::sin(sw * math::PI));
                m = m.t(0.56, -0.52 - equip as f64 * 0.6, -0.72);
                let f = math::sin(sw * sw * math::PI);
                let g2 = math::sin(sq * math::PI);
                m = m.r(1, 45.0 + f * -20.0).r(2, g2 * -20.0).r(0, g2 * -80.0).r(1, -45.0);
                // The model's display transform: translate, rotate XYZ, scale; centred.
                let d = gui.hands.get(item as usize).copied().unwrap_or([0.0, 45.0, 0.0, 0.0, 0.0, 0.0, 0.4, 0.4, 0.4]);
                m = m
                    .t(d[3] as f64 / 16.0, d[4] as f64 / 16.0, d[5] as f64 / 16.0)
                    .r(0, d[0] as f64)
                    .r(1, d[1] as f64)
                    .r(2, d[2] as f64)
                    .s(d[6] as f64, d[7] as f64, d[8] as f64)
                    .t(-0.5, -0.5, -0.5);
                match icon {
                    Icon::Flat(layers) => {
                        let list = self.flat.entry(item).or_insert_with(|| {
                            let mut out = Vec::new();
                            let tr = (atlas_size.0 / 16.0) as u32;
                            for &(tile, tint) in layers.iter() {
                                let t = if tint != 0 { [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8] } else { [255; 3] };
                                extrude(atlas_alpha, atlas_size.0 as usize, atlas_size.1 as usize, (tile as u32 % tr) as usize * 16, (tile as u32 / tr) as usize * 16, t, &mut out);
                            }
                            out
                        });
                        for q in list.iter() {
                            quads.push((m, *q));
                        }
                    }
                    Icon::Model { first, count, tint, .. } => {
                        let tc = if *tint != 0 { [(*tint >> 16) as u8, (*tint >> 8) as u8, *tint as u8] } else { [255; 3] };
                        for q in &blocks.quads[*first as usize..(*first as usize + *count as usize).min(blocks.quads.len())] {
                            let mut pos = [[0.0; 3]; 4];
                            let mut uv = [[0.0f32; 2]; 4];
                            for i in 0..4 {
                                pos[i] = [q.pos[i][0] as f64 / 1024.0, q.pos[i][1] as f64 / 1024.0, q.pos[i][2] as f64 / 1024.0];
                                uv[i] = [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0];
                            }
                            let t = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 3] };
                            quads.push((m, (pos, uv, q.light as f32 / 255.0, t)));
                        }
                    }
                    Icon::None => {}
                }
            }
            None => {
                // renderPlayerArm: the right arm (and sleeve) of the player model, with the skin.
                let Some(t) = ents else { return };
                // Our own downloaded skin (skins.rs), or the default one by the UUID.
                let own = crate::skins::local_skin();
                let mi = t.player_models[if own.map_or(skin < 9, |o| o.1) { 0 } else { 1 }];
                if mi < 0 || (own.is_none() && skin >= t.skins.len()) {
                    return;
                }
                let model = &t.models[mi as usize];
                let rect = if own.is_some() { [0, 0, 1, 1] } else { t.skins[skin] };
                let (h_, j, k) = (-0.3 * math::sin(sq * math::PI), 0.4 * math::sin(sq * math::PI * 2.0), -0.4 * math::sin(sw * math::PI));
                let l = math::sin(sw * sw * math::PI);
                let m2 = math::sin(sq * math::PI);
                let m = base
                    .t(h_ + 0.64000005, j - 0.6 + equip as f64 * -0.6, k - 0.71999997)
                    .r(1, 45.0)
                    .r(1, m2 * 70.0)
                    .r(2, l * -20.0)
                    .t(-1.0, 3.6, 3.5)
                    .r(2, 120.0)
                    .r(0, 200.0)
                    .r(1, -135.0)
                    .t(5.6, 0.0, 0.0);
                let Some(arm) = model.parts.iter().position(|p| p.name == "right_arm") else { return };
                let (aw, ah) = if own.is_some() { (1.0, 1.0) } else { t.atlas_size() };
                tex = own.map_or(t.texture(), |o| o.0);
                tex_size = (aw, ah);
                let mut stack = alloc::vec![(arm, m)];
                while let Some((pi, pm)) = stack.pop() {
                    let p = &model.parts[pi];
                    let q = p.pose;
                    let pm2 = pm.t(q[0] as f64 / 16.0, q[1] as f64 / 16.0, q[2] as f64 / 16.0);
                    for (c, n) in p.polys.iter() {
                        let mut pos = [[0.0; 3]; 4];
                        let mut uv = [[0.0f32; 2]; 4];
                        for i in 0..4 {
                            pos[i] = [c[i][0] as f64 / 16.0, c[i][1] as f64 / 16.0, c[i][2] as f64 / 16.0];
                            uv[i] = [(rect[0] as f32 + c[i][3] * rect[2] as f32) / aw, (rect[1] as f32 + c[i][4] * rect[3] as f32) / ah];
                        }
                        let light = (0.75 - 0.25 * n[1] as f32 - 0.1 * n[0].abs()).clamp(0.4, 1.0);
                        quads.push((pm2, (pos, uv, light, [255; 3])));
                    }
                    for (ci, c) in model.parts.iter().enumerate() {
                        if c.parent as usize == pi && c.parent >= 0 {
                            stack.push((ci, pm2));
                        }
                    }
                }
            }
        }
        if quads.is_empty() {
            return;
        }
        let _ = tex_size;
        let mut v: Vec<V> = Vec::with_capacity(quads.len() * 4);
        for (m, (pos, uv, l, t)) in quads.iter() {
            let c = [(t[0] as f32 * l) as u8, (t[1] as f32 * l) as u8, (t[2] as f32 * l) as u8, 255];
            for i in 0..4 {
                v.push(V { p: m.apply(pos[i]), uv: uv[i], c });
            }
        }
        let proj = crate::render::perspective_pub(70.0, w as f32 / h as f32, 0.05, 10.0);
        let ident: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        unsafe {
            (gl.Clear)(DEPTH_BUFFER_BIT);
            (gl.MatrixMode)(PROJECTION);
            (gl.LoadMatrixf)(proj.as_ptr());
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(ident.as_ptr());
            (gl.Enable)(DEPTH_TEST);
            (gl.Disable)(CULL_FACE);
            (gl.Disable)(FOG);
            (gl.Enable)(TEXTURE_2D);
            (gl.BindTexture)(TEXTURE_2D, tex);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.1);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            (gl.EnableClientState)(VERTEX_ARRAY);
            (gl.EnableClientState)(COLOR_ARRAY);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.VertexPointer)(3, FLOAT, 24, v.as_ptr() as *const c_void);
            (gl.TexCoordPointer)(2, FLOAT, 24, (v.as_ptr() as *const u8).add(12) as *const c_void);
            (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (v.as_ptr() as *const u8).add(20) as *const c_void);
            (gl.DrawArrays)(QUADS, 0, v.len() as i32);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.Enable)(CULL_FACE);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
    }
}
