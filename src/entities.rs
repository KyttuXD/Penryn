//! Entities the server tracks for us, and drawing them as vanilla does: each type's own model
//! (vanilla's ModelPart tree, exported by horizon-prepare) with its texture, turned by its
//! body yaw, the head by its head yaw and pitch, legs and arms swinging as it walks
//! (HumanoidModel / QuadrupedModel.setupAnim), falling over when it dies
//! (LivingEntityRenderer.setupRotations) and red while hurt or dying. Which model and textures
//! a mob wears (baby models, variants, render layers, armour) is `mobs::look`; held items and
//! name tags are drawn here too. Dropped items float and spin as icons.

use crate::gui::{parse_models, rd16, rd32, Model};
use crate::math;
use crate::sdl::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
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
    /// yHeadRotO, yBodyRotO and xRotO: the head, body and pitch at the start of the last tick
    /// (LivingEntityRenderer lerps them by the partial tick).
    pub prev_head_yaw: f32,
    pub prev_body_yaw: f32,
    pub prev_pitch: f32,
    /// LivingEntity.lerpHeadTo (ClientboundRotateHeadPacket): the head turns to this yaw over
    /// `lerp_head_steps` ticks.
    pub lerp_head_yaw: f32,
    pub lerp_head_steps: u32,
    /// LivingEntity.SwingState: the ticks into the current swing (None when idle), its animation
    /// at the last tick and the tick before, and whether the off hand swings.
    pub swing: Option<u32>,
    pub swing_anim: f32,
    pub swing_old: f32,
    pub swing_off: bool,
    pub hurt: i32,
    /// A dropped item's stack, and its component patch (itemmodels.rs).
    pub item: Option<(i32, i32)>,
    pub item_patch: Vec<u8>,
    /// Its byte, VarInt and VarLong entity data values, by index (a sheep's wool is 18 in 26.3).
    pub data: BTreeMap<u8, i64>,
    pub age: u32,
    /// Vector3f and Quaternionf entity data by index (x, y, z, w): a display entity's transformation (display.rs).
    pub fvals: BTreeMap<u8, [f32; 4]>,
    /// A display entity's transformation interpolation: the transformation when it started (translation, scale,
    /// left and right rotation), the age it starts at, and its length in ticks.
    pub disp_from: Option<[[f32; 4]; 4]>,
    pub disp_start: i64,
    pub disp_dur: i32,
    /// ClientboundSetEquipmentPacket: per EquipmentSlot (main hand, off hand, feet, legs, chest,
    /// head, body, saddle) the item and count.
    pub equipment: [Option<(i32, i32)>; 8],
    /// DATA_CUSTOM_NAME, as plain text.
    pub custom_name: Option<String>,
    /// DATA_HEALTH_ID once the server sends it (a full-health mob's is never sent).
    pub health: Option<f32>,
    /// LivingEntity.deathTime: ticks since its health reached 0.
    pub death_time: i32,
    /// VillagerData: type, profession (registry ids) and level.
    pub villager: [i32; 3],
    /// The entry of its variant registry (cat, cow, wolf, frog, pig, chicken, zombie nautilus),
    /// -1 for none sent.
    pub variant: i32,
    /// Bumped whenever something its looks depend on changes.
    pub version: u32,
    /// Where the server says it is, and the steps left to get there (InterpolationHandler:
    /// each client tick covers 1/steps of the way, so movement is smooth whatever the packets'
    /// timing).
    pub target: [f64; 3],
    pub target_yaw: f32,
    pub target_pitch: f32,
    pub steps: u32,
    /// The client-side animation the poses read (mobanim.rs): the chicken's flap, the elytra's
    /// wings, the swim amount.
    pub anim: crate::mobanim::Anim,
}

impl Entity {
    pub fn new(kind: i32, uuid: [u8; 16], pos: [f64; 3], yaw: f32, pitch: f32, head_yaw: f32) -> Entity {
        Entity {
            kind,
            uuid,
            pos,
            prev: pos,
            target: pos,
            target_yaw: yaw,
            target_pitch: pitch,
            steps: 0,
            yaw,
            pitch,
            head_yaw,
            body_yaw: yaw,
            on_ground: false,
            limb_pos: 0.0,
            limb_speed: 0.0,
            prev_limb_speed: 0.0,
            prev_head_yaw: head_yaw,
            prev_body_yaw: yaw,
            prev_pitch: pitch,
            lerp_head_yaw: head_yaw,
            lerp_head_steps: 0,
            swing: None,
            swing_anim: 0.0,
            swing_old: 0.0,
            swing_off: false,
            hurt: 0,
            item: None,
            item_patch: Vec::new(),
            data: BTreeMap::new(),
            age: 0,
            fvals: BTreeMap::new(),
            disp_from: None,
            disp_start: 0,
            disp_dur: 0,
            equipment: [None; 8],
            custom_name: None,
            health: None,
            death_time: 0,
            villager: [0, 0, 1],
            variant: -1,
            version: 0,
            anim: crate::mobanim::Anim::new(),
        }
    }

    /// Its data value at `index` (0 when not sent).
    pub fn d(&self, index: u8) -> i64 {
        self.data.get(&index).copied().unwrap_or(0)
    }

    /// LivingEntity.isDeadOrDying.
    pub fn dying(&self) -> bool {
        self.health.map_or(false, |h| h <= 0.0)
    }

    /// Sets where the server says the entity is, to be reached over `steps` ticks
    /// (InterpolationHandler.setTargetPos; 3 for movement packets and teleports).
    pub fn set_target(&mut self, pos: [f64; 3], yaw: f32, pitch: f32, steps: u32) {
        self.target = pos;
        self.target_yaw = yaw;
        self.target_pitch = pitch;
        self.steps = steps;
    }

    /// One tick of InterpolationHandler.interpolate: a share of the way to the target.
    fn interpolate(&mut self) {
        if self.steps == 0 {
            return;
        }
        let k = 1.0 / self.steps as f64;
        for a in 0..3 {
            self.pos[a] += (self.target[a] - self.pos[a]) * k;
        }
        let k32 = (1.0 / self.steps as f32).min(1.0);
        self.yaw += wrap_deg(self.target_yaw - self.yaw) * k32;
        self.pitch += (self.target_pitch - self.pitch) * k32;
        self.steps -= 1;
        if self.steps == 0 {
            self.pos = self.target;
            self.yaw = self.target_yaw;
            self.pitch = self.target_pitch;
        }
    }

    /// The client tick (LivingEntity.tick): the start of the tick is kept for the frames drawn
    /// until the next one; then the glide, the swing, the head and body turns (aiStep,
    /// tickHeadTurn), the walk animation (calculateEntityAnimation), the hurt and death times.
    pub fn tick(&mut self) {
        self.prev_head_yaw = self.head_yaw;
        self.prev_body_yaw = self.body_yaw;
        self.prev_pitch = self.pitch;
        // A rider keeps the place its vehicle gave it as its previous one (riding.rs).
        if !self.data.contains_key(&crate::riding::D_RIDING) {
            self.prev = self.pos;
        }
        self.interpolate();
        self.tick_swing();
        let (dx, dz) = (self.pos[0] - self.prev[0], self.pos[2] - self.prev[2]);
        let sq = (dx * dx + dz * dz) as f32;
        let dist = math::sqrt(sq as f64) as f32;
        // WalkAnimationState: stopped for the dead and the riders, else follows the distance.
        if self.dying() || self.data.contains_key(&crate::riding::D_RIDING) {
            self.prev_limb_speed = 0.0;
            self.limb_speed = 0.0;
            self.limb_pos = 0.0;
        } else {
            let target = (dist * 4.0).min(1.0);
            self.prev_limb_speed = self.limb_speed;
            self.limb_speed += (target - self.limb_speed) * 0.4;
            self.limb_pos += self.limb_speed;
        }
        // Mob.tickHeadTurn: the body turns to the way it walks (or to the head while swinging),
        // 30% of the way a tick, and the head may turn 50 degrees from it at most.
        let mut turn_to = self.body_yaw;
        if sq > 0.0025 {
            let walk = (math::atan2(dz, dx) * (180.0 / math::PI)) as f32 - 90.0;
            let diff = (wrap_deg(self.yaw) - walk).abs();
            turn_to = if diff > 95.0 && diff < 265.0 { walk - 180.0 } else { walk };
        }
        if self.swing_anim > 0.0 {
            turn_to = self.yaw;
        }
        // LivingEntity.aiStep: the head turns to where the server's packet said, over 3 ticks.
        if self.lerp_head_steps > 0 {
            self.head_yaw += wrap_deg(self.lerp_head_yaw - self.head_yaw) / self.lerp_head_steps as f32;
            self.lerp_head_steps -= 1;
        }
        if self.death_time == 0 {
            self.body_yaw += wrap_deg(turn_to - self.body_yaw) * 0.3;
            let head_diff = wrap_deg(self.yaw - self.body_yaw);
            if head_diff.abs() > 50.0 {
                self.body_yaw += head_diff - head_diff.signum() * 50.0;
            }
        }
        if self.hurt > 0 {
            self.hurt -= 1;
        }
        // LivingEntity.tickDeath: the client counts too (the server removes it at 20).
        if self.dying() {
            self.death_time = (self.death_time + 1).min(20);
        } else {
            self.death_time = 0;
        }
        crate::mobanim::tick(self);
        self.age += 1;
    }

    /// LivingEntity.lerpHeadTo (ClientboundRotateHeadPacket): the head turns over 3 ticks.
    pub fn lerp_head_to(&mut self, yaw: f32) {
        self.lerp_head_yaw = yaw;
        self.lerp_head_steps = 3;
    }

    /// LivingEntity.SwingState.startIfAble: a swing starts unless one is past its first half.
    pub fn swing_arm(&mut self, off: bool) {
        if let Some(t) = self.swing {
            if t > 0 && t <= SWING_TICKS / 2 {
                return;
            }
        }
        self.swing = Some(0);
        self.swing_off = off;
    }

    /// SwingState.tick: the animation goes from 0 to 1 over the swing's ticks.
    fn tick_swing(&mut self) {
        self.swing_old = self.swing_anim;
        match self.swing {
            Some(t) => {
                self.swing_anim = (t as f32 / SWING_TICKS as f32).min(1.0);
                if t > SWING_TICKS {
                    self.swing = None;
                    self.swing_anim = 0.0;
                } else {
                    self.swing = Some(t + 1);
                }
            }
            None => self.swing_anim = 0.0,
        }
    }

    /// SwingState.getAnimation(partial): the swing's progress in this frame, None when idle.
    pub fn swing_progress(&self, partial: f32) -> Option<f32> {
        self.swing?;
        let (a, b) = (self.swing_old, self.swing_anim);
        Some(if b >= a { a + (b - a) * partial } else { a + (b + 1.0 - a) * partial })
    }

    /// WalkAnimationState.position(partial) (scaled by its position scale: 3 for babies, which
    /// LivingEntity.updateWalkAnimation passes): the swing's position in this frame.
    pub fn walk_pos(&self, partial: f32, scale: f32) -> f32 {
        (self.limb_pos - self.limb_speed * (1.0 - partial)) * scale
    }

    /// The body's yaw in this frame (Mth.rotLerp of yBodyRotO and yBodyRot).
    pub fn body_rot(&self, partial: f32) -> f32 {
        self.prev_body_yaw + wrap_deg(self.body_yaw - self.prev_body_yaw) * partial
    }

    /// The head's yaw in this frame (Mth.rotLerp of yHeadRotO and yHeadRot).
    pub fn head_rot(&self, partial: f32) -> f32 {
        self.prev_head_yaw + wrap_deg(self.head_yaw - self.prev_head_yaw) * partial
    }

    /// The pitch in this frame.
    pub fn pitch_lerp(&self, partial: f32) -> f32 {
        self.prev_pitch + (self.pitch - self.prev_pitch) * partial
    }
}

/// LivingEntity.SwingState's default swing: 6 ticks (the attack, the arm's chop).
pub const SWING_TICKS: u32 = 6;

/// HumanoidModel.setupAttackAnimation for a WHACK swing: `a` is the swing's progress (0 to 1).
/// The body turns with the swing, the arms move out from it, and the swinging arm chops.
fn attack_part(name: &str, a: f32, off: bool, head_x: f64, at: &mut [f64; 3], turn: &mut [f64; 3]) {
    let a = a as f64;
    // The main hand is the right one; the off hand swings the left arm.
    let mut by = math::sin(math::sqrt(a) * 2.0 * math::PI) * 0.2;
    if off {
        by = -by;
    }
    match name {
        "body" => {
            turn[1] = by;
            return;
        }
        "right_arm" => {
            at[0] = -math::cos(by) * 5.0;
            at[2] = math::sin(by) * 5.0;
            turn[1] += by;
        }
        "left_arm" => {
            at[0] = math::cos(by) * 5.0;
            at[2] = -math::sin(by) * 5.0;
            turn[1] += by;
            turn[0] += by;
        }
        _ => return,
    }
    if (name == "left_arm") == off {
        // Ease.outQuart(a).
        let q = 1.0 - a;
        let aa = math::sin((1.0 - q * q * q * q) * math::PI);
        let bb = math::sin(a * math::PI) * -(head_x - 0.7) * 0.75;
        turn[0] -= aa * 1.2 + bb;
        turn[1] += by * 2.0;
        turn[2] += math::sin(a * math::PI) * -0.4;
    }
}

pub fn wrap_deg(a: f32) -> f32 {
    // Mth.wrapDegrees, without a float remainder (no libm on Mac OS X 10.6 builds).
    (math::wrap(a as f64 + 180.0, 360.0) - 180.0) as f32
}

/// One entity type as entities.bin describes it.
pub struct TypeInfo {
    pub name: String,
    /// Its `#main` model (-1: none) and default texture.
    pub model: i16,
    pub rect: [u16; 4],
    pub tex: String,
    /// The hitbox (EntityType.dimensions) and a baby's.
    pub size: (f32, f32),
    pub baby_size: (f32, f32),
    /// The entity data index of its baby flag (AgeableMob.DATA_BABY_ID, the zombies' and the
    /// piglins'), 0 if it is never a baby.
    pub baby_index: u8,
}

/// An equipment asset: its layers by layer type (humanoid, humanoid_leggings, horse_body...),
/// each texture with the colour it has when undyed (0: not dyeable).
pub struct EquipAsset {
    pub name: String,
    pub layers: Vec<(String, Vec<([u16; 4], u32)>)>,
}

/// One model drawn with one texture: what `mobs::look` gives for an entity.
#[derive(Clone, Copy)]
pub struct Draw {
    pub model: u16,
    pub rect: [u16; 4],
    /// RGB multiplied in.
    pub tint: u32,
    /// Drawn on an invisible entity too (armour).
    pub worn: bool,
}

pub struct EntityTable {
    tex: u32,
    aw: f32,
    ah: f32,
    pub models: Vec<Model>,
    pub model_names: BTreeMap<String, u16>,
    pub types: Vec<TypeInfo>,
    /// The slim and wide player models, and the 18 default skins (9 slim, then 9 wide).
    pub player_models: [i16; 2],
    pub skins: Vec<[u16; 4]>,
    /// The `minecraft:player` entity type id.
    pub player_type: i32,
    /// Every entity texture by name ("entity/cow/cow_warm").
    pub textures: BTreeMap<String, [u16; 4]>,
    pub villager_types: Vec<String>,
    pub villager_professions: Vec<String>,
    /// Their textures' hats (VillagerMetadataSection.Hat: 0 none, 1 partial, 2 full).
    pub villager_hats: [Vec<u8>; 2],
    pub equip_assets: Vec<EquipAsset>,
    /// Per item: its equippable slot (0xFF none) and asset (0xFFFF none), and its model's
    /// display.thirdperson_righthand (rotation, translation, scale).
    pub item_equip: Vec<(u8, u16)>,
    pub item_third: Vec<[f32; 9]>,
    /// The looks of the entities drawn lately, by entity id: (version key, draws).
    cache: RefCell<BTreeMap<i32, (u32, Vec<Draw>)>>,
    /// The vertex lists of the last frame (bodies, held items), kept for their capacity.
    scratch: RefCell<(Vec<V>, Vec<V>)>,
}

/// DyeColor.textureDiffuseColor, by dye id.
pub const DYE: [u32; 16] = [
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

/// What drawing entities needs from the rest of the game.
pub struct DrawCx<'a> {
    pub variants: &'a crate::mobs::Variants,
    /// Player names by UUID (the tab list) as their name tags show them (teams' prefixes and
    /// colours; empty when the team hides it).
    pub names: &'a BTreeMap<[u8; 16], String>,
    /// The below-name objective's line per player (serverui.rs).
    pub below: &'a BTreeMap<[u8; 16], String>,
    pub game_time: i64,
    pub icons: &'a [crate::gui::Icon],
    pub blocks: &'a crate::blocks::Blocks,
    pub atlas: u32,
    pub atlas_size: (f32, f32),
    pub gui: Option<&'a crate::gui::Gui>,
    /// The camera's look (degrees), and the reach for the entity under the crosshair.
    pub yaw: f32,
    pub pitch: f32,
    pub reach: f64,
    /// The view's vertical half-angle's tangent and the window's aspect: entities outside the
    /// sides of the frustum are not posed or drawn.
    pub tan_y: f32,
    pub aspect: f32,
    /// The world's light at a place, for lighting each entity.
    pub lit: &'a crate::lightmap::Lit<'a>,
}

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

fn tr(x: f64, y: f64, z: f64) -> M {
    [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z]]
}

fn sc(x: f64, y: f64, z: f64) -> M {
    [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0]]
}

fn apply(t: &M, v: [f64; 3]) -> [f32; 3] {
    [
        (t[0][0] * v[0] + t[0][1] * v[1] + t[0][2] * v[2] + t[0][3]) as f32,
        (t[1][0] * v[0] + t[1][1] * v[1] + t[1][2] * v[2] + t[1][3]) as f32,
        (t[2][0] * v[0] + t[2][1] * v[1] + t[2][2] * v[2] + t[2][3]) as f32,
    ]
}

fn rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
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

    /// A model layer by its name ("minecraft:cow_baby#main").
    pub fn model(&self, name: &str) -> Option<u16> {
        self.model_names.get(name).copied()
    }

    /// A texture by its name ("entity/cow/cow_warm").
    pub fn tex_rect(&self, name: &str) -> Option<[u16; 4]> {
        self.textures.get(name).copied()
    }

    /// entities.bin without the GL texture (`load` uploads it).
    pub fn parse(d: &[u8]) -> Result<EntityTable, String> {
        if d.len() < 8 || &d[..4] != crate::account::ENTITIES_MAGIC {
            return Err(String::from("entities.bin is damaged"));
        }
        let (aw, ah) = (rd16(d, 4) as usize, rd16(d, 6) as usize);
        let mut p = 8 + aw * ah * 4;
        if d.len() < p + 4 {
            return Err(String::from("entities.bin is short"));
        }
        let f = |o: usize| f32::from_bits(rd32(d, o));
        let s = |p: &mut usize| -> String {
            let l = d[*p] as usize;
            let v = String::from_utf8_lossy(&d[*p + 1..*p + 1 + l]).into_owned();
            *p += 1 + l;
            v
        };
        let rect = |o: usize| [rd16(d, o), rd16(d, o + 2), rd16(d, o + 4), rd16(d, o + 6)];
        let models = parse_models(d, &mut p);
        let model_names = models.iter().enumerate().map(|(i, m)| (m.name.clone(), i as u16)).collect();
        let n = rd32(d, p) as usize;
        p += 4;
        let mut types = Vec::with_capacity(n);
        for _ in 0..n {
            let model = rd16(d, p) as i16;
            let r = rect(p + 2);
            let size = (f(p + 10), f(p + 14));
            let baby_size = (f(p + 18), f(p + 22));
            let baby_index = d[p + 26];
            p += 27;
            let name = s(&mut p);
            let tex = s(&mut p);
            types.push(TypeInfo { name, model, rect: r, tex, size, baby_size, baby_index });
        }
        let player_models = [rd16(d, p) as i16, rd16(d, p + 2) as i16];
        let n = rd32(d, p + 4) as usize;
        p += 8;
        let mut skins = Vec::with_capacity(n);
        for _ in 0..n {
            skins.push(rect(p));
            p += 8;
        }
        let player_type = rd32(d, p) as i32;
        p += 4;
        let n = rd32(d, p) as usize;
        p += 4;
        let mut textures = BTreeMap::new();
        for _ in 0..n {
            let name = s(&mut p);
            textures.insert(name, rect(p));
            p += 8;
        }
        let mut lists = [Vec::new(), Vec::new()];
        let mut hats = [Vec::new(), Vec::new()];
        for (l, h) in lists.iter_mut().zip(hats.iter_mut()) {
            let n = rd16(d, p) as usize;
            p += 2;
            for _ in 0..n {
                l.push(s(&mut p));
                h.push(d[p]);
                p += 1;
            }
        }
        let [villager_types, villager_professions] = lists;
        let villager_hats = hats;
        let n = rd16(d, p) as usize;
        p += 2;
        let mut equip_assets = Vec::with_capacity(n);
        for _ in 0..n {
            let name = s(&mut p);
            let nl = d[p] as usize;
            p += 1;
            let mut layers = Vec::with_capacity(nl);
            for _ in 0..nl {
                let ltype = s(&mut p);
                let nt = d[p] as usize;
                p += 1;
                let mut texs = Vec::with_capacity(nt);
                for _ in 0..nt {
                    let t = s(&mut p);
                    let undyed = rd32(d, p);
                    p += 4;
                    if let Some(r) = textures.get(&t) {
                        texs.push((*r, undyed));
                    }
                }
                layers.push((ltype, texs));
            }
            equip_assets.push(EquipAsset { name, layers });
        }
        let n = rd32(d, p) as usize;
        p += 4;
        let mut item_equip = Vec::with_capacity(n);
        let mut item_third = Vec::with_capacity(n);
        for _ in 0..n {
            item_equip.push((d[p], rd16(d, p + 1)));
            let mut t = [0f32; 9];
            for (i, v) in t.iter_mut().enumerate() {
                *v = f(p + 3 + 4 * i);
            }
            item_third.push(t);
            p += 39;
        }
        Ok(EntityTable {
            tex: 0,
            aw: aw as f32,
            ah: ah as f32,
            models,
            model_names,
            types,
            player_models,
            skins,
            player_type,
            textures,
            villager_types,
            villager_professions,
            villager_hats,
            equip_assets,
            item_equip,
            item_third,
            cache: RefCell::new(BTreeMap::new()),
            scratch: RefCell::new((Vec::new(), Vec::new())),
        })
    }

    pub fn load(gl: &Gl, d: &[u8]) -> Result<EntityTable, String> {
        let mut t = Self::parse(d)?;
        let (aw, ah) = (t.aw as i32, t.ah as i32);
        unsafe {
            (gl.GenTextures)(1, &mut t.tex);
            (gl.BindTexture)(TEXTURE_2D, t.tex);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
            (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
            (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, aw, ah, 0, RGBA, UNSIGNED_BYTE, d[8..].as_ptr() as *const c_void);
        }
        Ok(t)
    }

    /// Whether the entity is a baby (AgeableMob.isBaby: its synced flag, for a type that can be one).
    pub fn is_baby(&self, e: &Entity) -> bool {
        self.types.get(e.kind as usize).map_or(false, |t| t.baby_index > 0 && e.d(t.baby_index) != 0)
    }

    /// The entity's hitbox: its type's, a baby's, a slime's size.
    pub fn dims(&self, e: &Entity) -> (f32, f32) {
        let Some(t) = self.types.get(e.kind as usize) else { return (0.6, 1.8) };
        if t.baby_index > 0 && e.d(t.baby_index) != 0 {
            return t.baby_size;
        }
        match t.name.as_str() {
            // Slime.getDefaultDimensions: scaled by the size.
            "slime" | "magma_cube" => {
                let s = e.data.get(&18).copied().unwrap_or(1).max(1) as f32;
                (t.size.0 * s, t.size.1 * s)
            }
            // A crouching player (Pose.CROUCHING).
            "player" if e.d(6) == 5 => (0.6, 1.5),
            _ => t.size,
        }
    }

    /// The model pose of every part (LivingEntityRenderer: position, body turn, the death
    /// fall, the species' scale; the parts' setupAnim), in pixels from the model's origin.
    fn pose(&self, m: &Model, e: &Entity, partial: f32, at: [f64; 3], items: &[String], mats: &mut Vec<M>) {
        let ty = self.types.get(e.kind as usize);
        let name = ty.map_or("", |t| t.name.as_str());
        let mut base = mul(&tr(at[0], at[1], at[2]), &rot(1, math::to_rad((180.0 - e.body_rot(partial)) as f64)));
        if e.death_time > 0 {
            // setupRotations: fall over to the side (spiders onto their backs) over a second.
            let f = math::sqrt(((e.death_time as f64 + partial as f64 - 1.0) / 20.0 * 1.6).max(0.0)).min(1.0);
            let flip = if matches!(name, "spider" | "cave_spider" | "silverfish" | "endermite") { 180.0 } else { 90.0 };
            base = mul(&base, &rot(2, math::to_rad(f * flip)));
        }
        // LivingEntityRenderer.isEntityUpsideDown: named Dinnerbone or Grumm.
        if matches!(e.custom_name.as_deref(), Some("Dinnerbone" | "Grumm")) {
            base = mul(&mul(&base, &tr(0.0, self.dims(e).1 as f64 + 0.1, 0.0)), &rot(2, math::PI));
        }
        // Swimming, gliding, sleeping players; a crouching one sits lower (poses.rs).
        base = crate::poses::setup_rotations(name, e, partial, base);
        base = mul(&base, &sc(-1.0, -1.0, 1.0));
        match name {
            // AvatarRenderer.scale.
            "player" | "mannequin" => base = mul(&base, &sc(0.9375, 0.9375, 0.9375)),
            // SlimeRenderer.scale: the size.
            "slime" | "magma_cube" => {
                let s = e.data.get(&18).copied().unwrap_or(1).max(1) as f64 * 0.999;
                base = mul(&base, &sc(s, s, s));
            }
            // PhantomRenderer.scale: 15% bigger per size, the model moved down to the box.
            "phantom" => {
                let s = 1.0 + 0.15 * e.d(16) as f64;
                base = mul(&mul(&base, &sc(s, s, s)), &tr(0.0, 1.3125, 0.1875));
            }
            _ => {}
        }
        base = mul(&mul(&base, &tr(0.0, -1.501, 0.0)), &sc(1.0 / 16.0, 1.0 / 16.0, 1.0 / 16.0));
        // Boats and minecarts have their renderers' own transform.
        if let Some(b) = crate::poses::vehicle_base(name, e, at) {
            base = b;
        }
        let speed = e.prev_limb_speed + (e.limb_speed - e.prev_limb_speed) * partial;
        // The limb swing's position in this frame (babies swing 3 times as fast: positionScale).
        let scale = if self.is_baby(e) { 3.0 } else { 1.0 };
        let amount = speed.min(1.0) as f64;
        let head_y = math::to_rad(wrap_deg(e.head_rot(partial) - e.body_rot(partial)) as f64);
        let head_x = math::to_rad(e.pitch_lerp(partial) as f64);
        let attack = e.swing_progress(partial).filter(|a| *a > 0.0);
        // The poses of this frame (mobanim.rs): the walk, the arms' items and bows, the
        // overrides of zombies, skeletons and piglins, the bob and the swimmer's strokes.
        let f = crate::mobanim::Frame::new(
            name,
            e,
            partial,
            items,
            self.is_baby(e),
            e.walk_pos(partial, scale) as f64,
            amount,
            (head_y, head_x),
            attack.map(|a| a as f64),
        );
        let before = crate::mobanim::swing_before_attack(&f);
        mats.clear();
        for p in m.parts.iter() {
            let parent = if p.parent >= 0 { mats[p.parent as usize] } else { base };
            let q = p.pose;
            let mut at_part = [q[0] as f64, q[1] as f64, q[2] as f64];
            let mut turn = [q[3] as f64, q[4] as f64, q[5] as f64];
            crate::mobanim::walk(&f, &p.name, &mut at_part, &mut turn);
            // PiglinModel: swingWeaponDown sets the arms, then the humanoid attack moves them on.
            if before {
                crate::mobanim::override_arm(&f, &p.name, &mut turn);
            }
            if let Some(a) = attack {
                attack_part(&p.name, a as f32, e.swing_off, head_x, &mut at_part, &mut turn);
            }
            if !before {
                crate::mobanim::override_arm(&f, &p.name, &mut turn);
            }
            // Riding, crouching, rowing (poses.rs).
            crate::poses::part(name, e, partial, &p.name, &mut at_part, &mut turn);
            crate::mobanim::post(&f, &p.name, &mut turn);
            let [xr, yr, zr] = turn;
            // parent * translation * rotations * scale, leaving out the identity ones: most
            // parts turn about one axis or none, and every rot() costs two software sines.
            let mut t = parent;
            let (px, py, pz) = (at_part[0], at_part[1], at_part[2]);
            for i in 0..3 {
                t[i][3] += parent[i][0] * px + parent[i][1] * py + parent[i][2] * pz;
            }
            if zr != 0.0 {
                t = mul(&t, &rot(2, zr));
            }
            if yr != 0.0 {
                t = mul(&t, &rot(1, yr));
            }
            if xr != 0.0 {
                t = mul(&t, &rot(0, xr));
            }
            let (sx, sy, sz) = (q[6] as f64, q[7] as f64, q[8] as f64);
            if sx != 1.0 || sy != 1.0 || sz != 1.0 {
                t = mul(&t, &sc(sx, sy, sz));
            }
            mats.push(t);
        }
    }

    /// The light colour an entity is drawn with (EntityRenderer.getPackedLightCoords): the light of
    /// the block at its eye, full block light while it burns. `at` is its place in the world.
    pub fn light(&self, e: &Entity, at: [f64; 3], lit: &crate::lightmap::Lit) -> [f32; 3] {
        let eye = if e.kind == self.player_type { 1.62 } else { self.dims(e).1 as f64 * 0.85 };
        let fire = e.d(0) & 1 != 0;
        lit.at_min_block([at[0], at[1] + eye, at[2]], if fire { 15 } else { 0 })
    }

    fn emit(&self, out: &mut Vec<V>, m: &Model, mats: &[M], rect: [u16; 4], tint: [u8; 3], red: bool, lc: [f32; 3]) {
        let (aw, ah) = (self.aw, self.ah);
        for (pi, part) in m.parts.iter().enumerate() {
            let t = &mats[pi];
            for (c, n) in part.polys.iter() {
                // The base scale is 1/16 (and flips y): scale the normal back to unit length.
                let ny = 16.0 * (t[1][0] * n[0] as f64 + t[1][1] * n[1] as f64 + t[1][2] * n[2] as f64);
                // Model space is y-down: a normal pointing up in the world has ny > 0 here.
                let l = (0.7 + 0.3 * ny.clamp(-1.0, 1.0)) as f32;
                let tl = |k: usize| (tint[k] as f32 * l) as u8;
                // The hurt overlay (OverlayTexture red over 70% of the colour).
                let col = if red {
                    [(77.0 * l + 0.7 * tl(0) as f32).min(255.0) as u8, (0.7 * tl(1) as f32) as u8, (0.7 * tl(2) as f32) as u8, 255]
                } else {
                    [tl(0), tl(1), tl(2), 255]
                };
                // The light colour comes last, over the hurt overlay (entity.fsh).
                let col = crate::lightmap::tint(col, lc);
                for k in 0..4 {
                    let v = c[k];
                    out.push(V {
                        p: apply(t, [v[0] as f64, v[1] as f64, v[2] as f64]),
                        uv: [(rect[0] as f32 + v[3] * rect[2] as f32) / aw, (rect[1] as f32 + v[4] * rect[3] as f32) / ah],
                        c: col,
                    });
                }
            }
        }
    }

    /// The draws of an entity, rebuilt in the cache only when something they depend on changed.
    fn looks<'c>(&self, cache: &'c mut BTreeMap<i32, (u32, Vec<Draw>)>, id: i32, e: &Entity, cx: &DrawCx) -> &'c [Draw] {
        // A wolf's or a bee's anger running out changes its texture.
        let angry = [19u8, 22].iter().any(|i| e.data.get(i).map_or(false, |&t| t > cx.game_time)) as u32;
        let key = e.version.wrapping_mul(2) | angry;
        let slot = cache.entry(id).or_insert_with(|| (key ^ 1, Vec::new()));
        if slot.0 != key {
            *slot = (key, crate::mobs::look(self, e, cx.variants, cx.game_time));
        }
        &slot.1
    }

    /// Draws every entity near the eye (vertices relative to the eye, under `view`): bodies,
    /// layers and armour, held items, then name tags.
    pub fn draw(&self, gl: &Gl, view: &[f32; 16], eye: [f64; 3], ents: &BTreeMap<i32, Entity>, partial: f32, skip: i32, cx: &DrawCx) {
        let mut scratch = self.scratch.borrow_mut();
        let (v, items) = &mut *scratch;
        v.clear();
        items.clear();
        let mut tags: Vec<([f64; 3], &str, bool)> = Vec::new();
        let mut cache = self.cache.borrow_mut();
        if cache.len() > ents.len() * 2 + 64 {
            cache.retain(|id, _| ents.contains_key(id));
        }
        // The entity under the crosshair (Minecraft.crosshairPickEntity), for its name.
        let (yr, pr) = (math::to_rad(cx.yaw as f64), math::to_rad(cx.pitch as f64));
        let dir = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let picked = self.pick(ents, eye, dir, cx.reach, skip).map(|h| h.0);
        let (mut mats, mut body): (Vec<M>, Vec<M>) = (Vec::new(), Vec::new());
        // The view's forward axis (its third row, negated).
        let fwd = [-view[2] as f64, -view[6] as f64, -view[10] as f64];
        let right = [view[0] as f64, view[4] as f64, view[8] as f64];
        let up = [view[1] as f64, view[5] as f64, view[9] as f64];
        let (ty, tx) = (cx.tan_y as f64, cx.tan_y as f64 * cx.aspect as f64);
        // PENRYN_DEBUG: what became of each player within 40 blocks, said every few seconds.
        let debug = crate::sys::env("PENRYN_DEBUG").is_some();
        let mut said: Vec<String> = Vec::new();
        for (&id, e) in ents.iter() {
            if id == skip || e.item.is_some() {
                continue;
            }
            let is_player = debug && e.kind == self.player_type;
            let lerp = |a: f64, b: f64| a + (b - a) * partial as f64;
            let p = [lerp(e.prev[0], e.pos[0]) - eye[0], lerp(e.prev[1], e.pos[1]) - eye[1], lerp(e.prev[2], e.pos[2]) - eye[2]];
            let dist2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
            if p[0] * p[0] + p[2] * p[2] > 96.0 * 96.0 {
                if is_player {
                    said.push(alloc::format!("player {}: {:.1} blocks, too far", id, math::sqrt(dist2)));
                }
                continue;
            }
            // Behind the camera (with room for its size and name tag): nothing of it shows.
            let (w, h) = self.dims(e);
            let radius = w as f64 + h as f64 + 2.0;
            let z = p[0] * fwd[0] + p[1] * fwd[1] + p[2] * fwd[2];
            if z < -radius {
                if is_player {
                    said.push(alloc::format!("player {}: {:.1} blocks, behind the camera", id, math::sqrt(dist2)));
                }
                continue;
            }
            // Outside a side of the view's pyramid by more than the model's size (with slack for
            // the plane normals not being unit length): nothing of it shows.
            if ty > 0.0 {
                let (xs, ys) = (p[0] * right[0] + p[1] * right[1] + p[2] * right[2], p[0] * up[0] + p[1] * up[1] + p[2] * up[2]);
                let rs = radius * 1.5;
                if xs.abs() > z.max(0.0) * tx + rs * (1.0 + tx) || ys.abs() > z.max(0.0) * ty + rs * (1.0 + ty) {
                    if is_player {
                        said.push(alloc::format!("player {}: {:.1} blocks, outside the view", id, math::sqrt(dist2)));
                    }
                    continue;
                }
            }
            let invisible = e.d(0) & 0x20 != 0;
            if is_player {
                said.push(alloc::format!(
                    "player {}: {:.1} blocks, flags {:#x}, invisible {}, own skin {}, parts {}, pos {:.1} {:.1} {:.1}",
                    id, math::sqrt(dist2), e.d(0), invisible, crate::skins::custom(&e.uuid), self.looks(&mut cache, id, e, cx).len(), e.pos[0], e.pos[1], e.pos[2]
                ));
            }
            let red = e.hurt > 0 || e.death_time > 0;
            let lc = self.light(e, [p[0] + eye[0], p[1] + eye[1], p[2] + eye[2]], cx.lit);
            let held = [e.equipment[0].is_some(), e.equipment[1].is_some()];
            let looks = self.looks(&mut cache, id, e, cx);
            // A player with a downloaded skin: skins.rs draws the body (and cape); armour,
            // held items and the name tag are still drawn here.
            let own_skin = e.kind == self.player_type && crate::skins::custom(&e.uuid);
            let mut first: Option<usize> = None;
            let mut posed = -1i32;
            for dr in looks.iter() {
                if invisible && !dr.worn {
                    continue;
                }
                let Some(m) = self.models.get(dr.model as usize) else { continue };
                // The layers of one model (a villager's type, profession and level, a sheep's
                // wool) share its pose.
                if posed != dr.model as i32 {
                    self.pose(m, e, partial, p, &cx.blocks.items, &mut mats);
                    posed = dr.model as i32;
                }
                // EquipmentLayerRenderer draws with NO_OVERLAY: armour never turns red.
                if !(own_skin && !dr.worn) {
                    self.emit(v, m, &mats, dr.rect, rgb(dr.tint), red && !dr.worn, lc);
                }
                if first.is_none() {
                    first = Some(dr.model as usize);
                    if held[0] || held[1] {
                        body.clear();
                        body.extend_from_slice(&mats);
                    }
                }
            }
            // ItemInHandLayer: the items in the arms of a humanoid model.
            if held[0] || held[1] {
                if first.is_none() {
                    if let Some(dr) = looks.first() {
                        self.pose(&self.models[dr.model as usize], e, partial, p, &cx.blocks.items, &mut body);
                        first = Some(dr.model as usize);
                    }
                }
                let mats = &body;
                if let Some(m) = first.and_then(|mi| self.models.get(mi)) {
                    // Mob.isLeftHanded (DATA_MOB_FLAGS_ID 2), or a player's or mannequin's main
                    // arm (Avatar.DATA_PLAYER_MAIN_HAND, 0 left).
                    let avatar = matches!(self.types.get(e.kind as usize).map(|t| t.name.as_str()), Some("player" | "mannequin"));
                    let left_handed = if avatar { e.data.get(&15) == Some(&0) } else { e.d(15) & 2 != 0 };
                    for (slot, arm) in [(0usize, "right_arm"), (1, "left_arm")] {
                        let Some((item, _)) = e.equipment[slot] else { continue };
                        let arm = if left_handed { if arm == "right_arm" { "left_arm" } else { "right_arm" } } else { arm };
                        let Some(ai) = m.parts.iter().position(|q| q.name == arm) else { continue };
                        self.held_item(items, &mats[ai], item, arm == "left_arm", cx, lc);
                    }
                }
            }
            // The name tag: players' names, named mobs' when shown or looked at.
            if !invisible && e.death_time == 0 {
                let discrete = e.d(0) & 2 != 0;
                let range = if discrete { 32.0 } else { 64.0 };
                let name = if e.kind == self.player_type {
                    e.custom_name.as_deref().or_else(|| cx.names.get(&e.uuid).map(|s| s.as_str()))
                } else if e.d(3) != 0 || picked == Some(id) {
                    e.custom_name.as_deref()
                } else {
                    None
                };
                if let (Some(n), true) = (name.filter(|n| !n.is_empty()), dist2 < range * range) {
                    let mut h = self.dims(e).1 as f64 + 0.5;
                    // AvatarRenderer: the below-name score within 10 blocks, the name above it.
                    if let (Some(b), true) = (cx.below.get(&e.uuid), e.kind == self.player_type && dist2 < 100.0) {
                        tags.push(([p[0], p[1] + h, p[2]], b.as_str(), discrete));
                        h += 9.0 * 1.15 * 0.025;
                    }
                    tags.push(([p[0], p[1] + h, p[2]], n, discrete));
                }
            }
        }
        if debug {
            static CALLS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            if CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) % 200 == 0 {
                for l in said.iter().take(12) {
                    crate::say!("{}", l);
                }
            }
        }
        unsafe {
            (gl.MatrixMode)(MODELVIEW);
            (gl.LoadMatrixf)(view.as_ptr());
            (gl.Enable)(TEXTURE_2D);
            (gl.Enable)(ALPHA_TEST);
            (gl.AlphaFunc)(GREATER, 0.1);
            (gl.Disable)(CULL_FACE);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.BindBuffer)(ARRAY_BUFFER, 0);
            // Vanilla's LEQUAL: a layer on the same faces as the body (a baby sheep's wool,
            // villager clothes) draws over it.
            (gl.DepthFunc)(LEQUAL);
            for (tex, list) in [(self.tex, &*v), (cx.atlas, &*items)] {
                if list.is_empty() {
                    continue;
                }
                (gl.BindTexture)(TEXTURE_2D, tex);
                (gl.VertexPointer)(3, FLOAT, 24, list.as_ptr() as *const c_void);
                (gl.TexCoordPointer)(2, FLOAT, 24, (list.as_ptr() as *const u8).add(12) as *const c_void);
                (gl.ColorPointer)(4, UNSIGNED_BYTE, 24, (list.as_ptr() as *const u8).add(20) as *const c_void);
                (gl.DrawArrays)(QUADS, 0, list.len() as i32);
            }
            (gl.DepthFunc)(LESS);
            (gl.Enable)(CULL_FACE);
            (gl.Disable)(ALPHA_TEST);
            (gl.Disable)(TEXTURE_2D);
            (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        }
        if let (Some(gui), false) = (cx.gui, tags.is_empty()) {
            name_tags(gl, view, gui, &mut tags);
        }
    }

    /// ItemInHandLayer.submitArmWithItem: the item at the end of the arm, with its model's
    /// third-person transform (mirrored in the left hand).
    fn held_item(&self, out: &mut Vec<V>, arm: &M, item: i32, left: bool, cx: &DrawCx, lc: [f32; 3]) {
        let d = self.item_third.get(item as usize).copied().unwrap_or([0.0, 0.0, 0.0, 0.0, 3.0, 1.0, 0.55, 0.55, 0.55]);
        if d[6] == 0.0 {
            return; // a special renderer's item (shield, banner, head...): not drawn here
        }
        let k = if left { -1.0 } else { 1.0 };
        // The arm's pose is in pixels: back to blocks first.
        let mut m = mul(arm, &sc(16.0, 16.0, 16.0));
        m = mul(&m, &rot(0, -math::PI / 2.0));
        m = mul(&m, &rot(1, math::PI));
        m = mul(&m, &tr(k / 16.0, 0.125, -0.625));
        m = mul(&m, &tr(k * d[3] as f64 / 16.0, d[4] as f64 / 16.0, d[5] as f64 / 16.0));
        m = mul(&m, &rot(0, math::to_rad(d[0] as f64)));
        m = mul(&m, &rot(1, math::to_rad(k * d[1] as f64)));
        m = mul(&m, &rot(2, math::to_rad(k * d[2] as f64)));
        m = mul(&m, &sc(d[6] as f64, d[7] as f64, d[8] as f64));
        m = mul(&m, &tr(-0.5, -0.5, -0.5));
        let (aw, ah) = cx.atlas_size;
        match cx.icons.get(item as usize) {
            Some(crate::gui::Icon::Flat(layers)) => {
                let tiles_row = (aw / 16.0) as u32;
                for &(tile, tint) in layers.iter() {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let (u0, v0, u1, v1) = (tx / aw, ty / ah, (tx + 16.0) / aw, (ty + 16.0) / ah);
                    let c = if tint != 0 { rgb(tint) } else { [255; 3] };
                    // ItemModelGenerator's front and back, 1/16 apart.
                    for (z, l, order) in [(8.5 / 16.0, 1.0, [0usize, 1, 2, 3]), (7.5 / 16.0, 0.8, [3, 2, 1, 0])] {
                        let corners = [(0.0, 1.0, u0, v0), (0.0, 0.0, u0, v1), (1.0, 0.0, u1, v1), (1.0, 1.0, u1, v0)];
                        let col = crate::lightmap::tint([(c[0] as f32 * l) as u8, (c[1] as f32 * l) as u8, (c[2] as f32 * l) as u8, 255], lc);
                        for i in order {
                            let (x, y, u, vv) = corners[i];
                            out.push(V { p: apply(&m, [x, y, z]), uv: [u, vv], c: col });
                        }
                    }
                }
            }
            Some(crate::gui::Icon::Model { first, count, tint, .. }) => {
                let qs = &cx.blocks.quads[*first as usize..(*first as usize + *count as usize).min(cx.blocks.quads.len())];
                let tc = if *tint != 0 { rgb(*tint) } else { [255; 3] };
                for q in qs {
                    let l = q.light as f32 / 255.0;
                    let base = if q.flags & crate::blocks::Q_TINTED != 0 { tc } else { [255; 3] };
                    let col = crate::lightmap::tint([(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255], lc);
                    for i in 0..4 {
                        let pp = [q.pos[i][0] as f64 / 1024.0, q.pos[i][1] as f64 / 1024.0, q.pos[i][2] as f64 / 1024.0];
                        out.push(V { p: apply(&m, pp), uv: [q.uv[i][0] as f32 / 65535.0, q.uv[i][1] as f32 / 65535.0], c: col });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Name tags (EntityRenderer.submitNameTag): the text facing the camera half a block above the
/// box, 0.025 blocks a font pixel, over vanilla's 25% black background; seen through walls
/// at half alpha, then drawn again in full where nothing hides it (only once, dimmed, while
/// the entity sneaks).
fn name_tags(gl: &Gl, view: &[f32; 16], gui: &crate::gui::Gui, tags: &mut Vec<([f64; 3], &str, bool)>) {
    // The camera's right and up in the world: the rows of the view rotation.
    let right = [view[0] as f64, view[4] as f64, view[8] as f64];
    let up = [view[1] as f64, view[5] as f64, view[9] as f64];
    // Farthest first, so nearer tags blend over them.
    tags.sort_by(|a, b| {
        let d = |p: &[f64; 3]| p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
        d(&b.0).partial_cmp(&d(&a.0)).unwrap_or(core::cmp::Ordering::Equal)
    });
    let mut bg: Vec<V> = Vec::new();
    let mut faint: Vec<V> = Vec::new();
    let mut full: Vec<V> = Vec::new();
    let (mut bg_d, mut faint_d) = (Vec::new(), Vec::new());
    for (at, text, discrete) in tags.iter() {
        let w = gui.text_width(text);
        // SubmitNodeCollection.submitNameTag: -width / 2 as a float.
        let x0 = -w / 2.0;
        let place = |x: f32, y: f32| -> [f32; 3] {
            let (x, y) = (x as f64 * 0.025, -(y as f64) * 0.025);
            [(at[0] + right[0] * x + up[0] * y) as f32, (at[1] + right[1] * x + up[1] * y) as f32, (at[2] + right[2] * x + up[2] * y) as f32]
        };
        let quad = |out: &mut Vec<V>, x0: f32, y0: f32, x1: f32, y1: f32, uv: [f32; 4], c: [u8; 4]| {
            out.push(V { p: place(x0, y0), uv: [uv[0], uv[1]], c });
            out.push(V { p: place(x0, y1), uv: [uv[0], uv[3]], c });
            out.push(V { p: place(x1, y1), uv: [uv[2], uv[3]], c });
            out.push(V { p: place(x1, y0), uv: [uv[2], uv[1]], c });
        };
        let b = if *discrete { &mut bg_d } else { &mut bg };
        // Font.PreparedTextBuilder.markBackground: one unit left and up, to the line's 9 down.
        quad(b, x0 - 1.0, -1.0, x0 + w, 9.0, [0.0; 4], [0, 0, 0, 0x40]);
        let f = if *discrete { &mut faint_d } else { &mut faint };
        gui.glyph_quads(text, |gx0, gy0, gx1, gy1, uv| {
            quad(f, x0 + gx0, gy0, x0 + gx1, gy1, uv, [255, 255, 255, 0x80]);
            if !*discrete {
                quad(&mut full, x0 + gx0, gy0, x0 + gx1, gy1, uv, [255; 4]);
            }
        });
    }
    let draw = |list: &Vec<V>, textured: bool| unsafe {
        if list.is_empty() {
            return;
        }
        if textured {
            (gl.Enable)(TEXTURE_2D);
            (gl.EnableClientState)(TEXTURE_COORD_ARRAY);
            (gl.BindTexture)(TEXTURE_2D, gui.texture());
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
        // Seen through: the background and the faint text over everything.
        (gl.Disable)(DEPTH_TEST);
        draw(&bg, false);
        draw(&faint, true);
        (gl.Enable)(DEPTH_TEST);
        // Sneaking: only where nothing hides it.
        draw(&bg_d, false);
        draw(&faint_d, true);
        // The full text where nothing hides it.
        draw(&full, true);
        (gl.DepthMask)(1);
        (gl.Disable)(BLEND);
        (gl.Disable)(ALPHA_TEST);
        (gl.Disable)(TEXTURE_2D);
        (gl.DisableClientState)(TEXTURE_COORD_ARRAY);
        (gl.Enable)(CULL_FACE);
        (gl.Enable)(FOG);
    }
}

impl EntityTable {
    /// The player model (slim or wide) of `e` at `at` (relative to the eye) for skins.rs:
    /// corners with uvs 0..1 over the whole skin, and their colour.
    pub fn player_quads(&self, e: &Entity, at: [f64; 3], partial: f32, slim: bool, items: &[String]) -> Vec<([f32; 3], [f32; 2], [u8; 4])> {
        let mut v: Vec<V> = Vec::new();
        let mi = self.player_models[if slim { 0 } else { 1 }];
        if mi >= 0 {
            // A rectangle the size of the atlas maps the model's uvs onto themselves.
            let m = &self.models[mi as usize];
            let mut mats: Vec<M> = Vec::new();
            self.pose(m, e, partial, at, items, &mut mats);
            self.emit(&mut v, m, &mats, [0, 0, self.aw as u16, self.ah as u16], [255; 3], e.hurt > 0 || e.death_time > 0, [1.0; 3]);
        }
        v.iter().map(|q| (q.p, q.uv, q.c)).collect()
    }

    /// The nearest entity the ray from `eye` along `dir` hits within `reach`, with the
    /// distance and where on it (relative to its position).
    pub fn pick(&self, ents: &BTreeMap<i32, Entity>, eye: [f64; 3], dir: [f64; 3], reach: f64, skip: i32) -> Option<(i32, f64, [f64; 3])> {
        let mut best: Option<(i32, f64, [f64; 3])> = None;
        for (&id, e) in ents.iter() {
            if id == skip || e.item.is_some() || e.dying() {
                continue;
            }
            let (w, h) = self.dims(e);
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
    overlays: &[crate::itemmodels::Entry],
    blocks: &crate::blocks::Blocks,
    atlas: u32,
    atlas_size: (f32, f32),
    lit: &crate::lightmap::Lit,
    skip_kinds: &[i32],
) {
    let mut v: Vec<V> = Vec::new();
    let tiles_row = (atlas_size.0 / 16.0) as u32;
    for e in ents.values() {
        let Some((item, _)) = e.item else { continue };
        if crate::maps::is_frame(e.kind) || skip_kinds.contains(&e.kind) {
            continue; // maps.rs draws what is in item frames
        }
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
        // The item's light: the block at its (low) eye.
        let lc = lit.at([p[0] + eye[0], p[1] + eye[1] + 0.2, p[2] + eye[2]]);
        let shown = crate::itemmodels::icon_for(overlays, icons, blocks, item, &e.item_patch);
        match shown.as_ref() {
            Some(crate::gui::Icon::Flat(layers)) => {
                for &(tile, tint) in layers.iter() {
                    let (tx, ty) = ((tile as u32 % tiles_row) as f32 * 16.0, (tile as u32 / tiles_row) as f32 * 16.0);
                    let (u0, v0, u1, v1) = (tx / atlas_size.0, ty / atlas_size.1, (tx + 16.0) / atlas_size.0, (ty + 16.0) / atlas_size.1);
                    let col = crate::lightmap::tint(if tint != 0 { [(tint >> 16) as u8, (tint >> 8) as u8, tint as u8, 255] } else { [255; 4] }, lc);
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
                    let col = crate::lightmap::tint([(base[0] as f32 * l) as u8, (base[1] as f32 * l) as u8, (base[2] as f32 * l) as u8, 255], lc);
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

/// Selftest: movement glides to the server's target over its steps (InterpolationHandler).
pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    let mut e = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    e.set_target([3.0, 0.0, 0.0], 0.0, 0.0, 3);
    let mut xs = alloc::vec::Vec::new();
    for _ in 0..3 {
        e.interpolate();
        xs.push(e.pos[0]);
    }
    let even = (xs[0] - 1.0).abs() < 1e-9 && (xs[1] - 2.0).abs() < 1e-9 && (xs[2] - 3.0).abs() < 1e-9;
    let mut f = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    f.set_target([1.0, 0.0, 0.0], 0.0, 0.0, 3);
    for _ in 0..10 {
        f.interpolate();
    }
    // A frame draws from the start of the tick to its end (xo, yRotO): one tick of a glide
    // from 0 to 1 in 2 steps leaves prev at 0 and pos at 0.5, so the half frame sits at 0.25.
    let mut g = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    g.set_target([1.0, 0.0, 0.0], 0.0, 0.0, 2);
    g.tick();
    let half = g.prev[0] + (g.pos[0] - g.prev[0]) * 0.5;
    let glided = g.prev[0] == 0.0 && g.pos[0] == 0.5;
    // Walking: the limb position lags the partial tick by the speed (WalkAnimationState.position).
    let mut w = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    w.limb_pos = 10.0;
    w.limb_speed = 0.5;
    // Head and body: the head turns to 90 over 3 ticks; the body drags behind a head past 50.
    let mut h = Entity::new(0, [0; 16], [0.0; 3], 100.0, 0.0, 0.0);
    h.body_yaw = 0.0;
    h.prev_body_yaw = 0.0;
    h.tick();
    let body_drag = h.body_yaw;
    let mut hd = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    hd.lerp_head_to(90.0);
    for _ in 0..3 {
        hd.tick();
    }
    // Swing: 6 ticks of a swing, its progress lerped by the partial tick (SwingState).
    let mut s = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    s.swing_arm(false);
    s.tick();
    s.tick();
    let sw = s.swing_progress(0.5).unwrap_or(-1.0);
    for _ in 0..6 {
        s.tick();
    }
    alloc::vec![
        ("entity glides in equal steps", alloc::format!("{}", even), "true"),
        ("entity stops on its target", alloc::format!("{}", f.pos[0] == 1.0 && f.steps == 0), "true"),
        ("frame draws from the tick's start to its end", alloc::format!("{}", glided && (half - 0.25).abs() < 1e-6), "true"),
        ("walk position lags the partial tick", alloc::format!("{}", (w.walk_pos(0.5, 1.0) - 9.75).abs() < 1e-6), "true"),
        ("baby walk position is 3 times as far", alloc::format!("{}", (w.walk_pos(0.5, 3.0) - 29.25).abs() < 1e-5), "true"),
        ("head turns to its packet's yaw over 3 ticks", alloc::format!("{}", (hd.head_yaw - 90.0).abs() < 1e-3), "true"),
        ("body drags behind a head turned past 50 degrees", alloc::format!("{}", (body_drag - 50.0).abs() < 1e-4), "true"),
        ("swing progress lerps by the partial tick", alloc::format!("{}", (sw - 1.0 / 12.0).abs() < 1e-5), "true"),
        ("swing ends after its ticks", alloc::format!("{}", s.swing.is_none() && s.swing_progress(0.0).is_none()), "true"),
    ]
}
