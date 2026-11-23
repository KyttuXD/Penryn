//! Status effects on the local player, as the server sends them (ClientboundUpdateMobEffectPacket,
//! ClientboundRemoveMobEffectPacket): what they do to movement (jump boost, levitation, slow
//! falling, dolphin's grace; speed and slowness come as attribute modifiers), how vanilla shows
//! them (Hud.extractEffects at the top right, EffectsInInventory beside the inventory), and
//! blindness, darkness and night vision on the view. The icons are effects.bin's (optional).

use crate::gui::Gui;
use crate::math::{self, FloorExt};
use crate::sdl::*;
use crate::wire::In;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::ffi::c_void;

/// The mob_effect registry in id order (26.3): name, beneficial (MobEffectCategory.BENEFICIAL;
/// neutral ones go with the harmful), colour.
pub const EFFECTS: [(&str, bool, u32); 40] = [
    ("speed", true, 3402751),
    ("slowness", false, 9154528),
    ("haste", true, 14270531),
    ("mining_fatigue", false, 4866583),
    ("strength", true, 16762624),
    ("instant_health", true, 16262179),
    ("instant_damage", false, 11101546),
    ("jump_boost", true, 16646020),
    ("nausea", false, 5578058),
    ("regeneration", true, 13458603),
    ("resistance", true, 9520880),
    ("fire_resistance", true, 16750848),
    ("water_breathing", true, 10017472),
    ("invisibility", true, 16185078),
    ("blindness", false, 2039587),
    ("night_vision", true, 12779366),
    ("hunger", false, 5797459),
    ("weakness", false, 4738376),
    ("poison", false, 8889187),
    ("wither", false, 7561558),
    ("health_boost", true, 16284963),
    ("absorption", true, 2445989),
    ("saturation", true, 16262179),
    ("glowing", false, 9740385),
    ("levitation", false, 13565951),
    ("luck", true, 5882118),
    ("unluck", false, 12624973),
    ("slow_falling", true, 15978425),
    ("conduit_power", true, 1950417),
    ("dolphins_grace", true, 8954814),
    ("bad_omen", false, 745784),
    ("hero_of_the_village", true, 4521796),
    ("darkness", false, 2696993),
    ("trial_omen", false, 1484454),
    ("raid_omen", false, 14565464),
    ("wind_charged", false, 12438015),
    ("weaving", false, 7891290),
    ("oozing", false, 10092451),
    ("infested", false, 9214860),
    ("breath_of_the_nautilus", true, 65518),
];

pub const JUMP_BOOST: u16 = 7;
pub const BLINDNESS: u16 = 14;
pub const NIGHT_VISION: u16 = 15;
pub const LEVITATION: u16 = 24;
pub const SLOW_FALLING: u16 = 27;
pub const DOLPHINS_GRACE: u16 = 29;
pub const DARKNESS: u16 = 32;
/// MobEffects.DARKNESS.setBlendDuration(22).
const DARKNESS_BLEND: f32 = 22.0;

/// One active effect (MobEffectInstance as the client keeps it).
#[derive(Clone, Copy)]
pub struct Effect {
    pub id: u16,
    pub amplifier: i32,
    /// Ticks left; -1 forever.
    pub duration: i32,
    /// The packet's flags: 1 ambient (a beacon's), 2 particles, 4 shows its icon, 8 blends in.
    pub flags: u8,
    /// MobEffectInstance.BlendState (darkness fades in and out), and last tick's.
    blend: f32,
    blend_o: f32,
}

impl Effect {
    pub fn ambient(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn show_icon(&self) -> bool {
        self.flags & 4 != 0
    }
    pub fn infinite(&self) -> bool {
        self.duration == -1
    }
    pub fn ends_within(&self, ticks: i32) -> bool {
        !self.infinite() && self.duration <= ticks
    }
    fn colour(&self) -> u32 {
        EFFECTS.get(self.id as usize).map_or(0, |e| e.2)
    }
    fn beneficial(&self) -> bool {
        EFFECTS.get(self.id as usize).map_or(false, |e| e.1)
    }
    fn name(&self) -> &'static str {
        EFFECTS.get(self.id as usize).map_or("", |e| e.0)
    }
}

/// MobEffectInstance.compareTo.
fn compare(a: &Effect, b: &Effect) -> Ordering {
    let by_colour = (a.duration > 32147 && b.duration > 32147) || (a.ambient() && b.ambient());
    if by_colour {
        a.ambient().cmp(&b.ambient()).then(a.colour().cmp(&b.colour()))
    } else {
        a.ambient()
            .cmp(&b.ambient())
            .then(a.infinite().cmp(&b.infinite()))
            .then(a.duration.cmp(&b.duration))
            .then(a.colour().cmp(&b.colour()))
    }
}

/// StringUtil.formatTickDuration at 20 ticks a second: mm:ss, or hh:mm:ss.
pub fn format_duration(ticks: i32) -> String {
    let s = ticks.max(0) / 20;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

pub(crate) struct Icons {
    pub tex: u32,
    /// uv rectangle per effect id (none: no icon).
    pub uv: Vec<Option<[f32; 4]>>,
}

pub(crate) fn load_icons(gl: &Gl) -> Option<Icons> {
    let d = crate::sys::read_file(&crate::packs::path("effects.bin"))?;
    if d.get(..4)? != crate::account::EFFECTS_MAGIC || d.len() < 10 {
        return None;
    }
    let rd = |o: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?])) };
    let (n, aw, ah) = (rd(4)? as usize, rd(6)? as usize, rd(8)? as usize);
    let mut p = 10;
    let mut by_name: Vec<(String, [f32; 4])> = Vec::new();
    for _ in 0..n {
        let l = *d.get(p)? as usize;
        let name = String::from_utf8_lossy(d.get(p + 1..p + 1 + l)?).into_owned();
        p += 1 + l;
        let (x, y) = (rd(p)? as f32, rd(p + 2)? as f32);
        let (w, h) = (*d.get(p + 4)? as f32, *d.get(p + 5)? as f32);
        p += 6;
        if w > 0.0 {
            by_name.push((name, [x / aw as f32, y / ah as f32, (x + w) / aw as f32, (y + h) / ah as f32]));
        }
    }
    let px = d.get(p..p + aw * ah * 4)?;
    let mut tex = 0u32;
    unsafe {
        (gl.GenTextures)(1, &mut tex);
        (gl.BindTexture)(TEXTURE_2D, tex);
        (gl.PixelStorei)(UNPACK_ALIGNMENT, 1);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAX_LEVEL, 0);
        (gl.TexImage2D)(TEXTURE_2D, 0, RGBA as i32, aw as i32, ah as i32, 0, RGBA, UNSIGNED_BYTE, px.as_ptr() as *const c_void);
    }
    let uv = EFFECTS.iter().map(|e| by_name.iter().find(|b| b.0 == e.0).map(|b| b.1)).collect();
    Some(Icons { tex, uv })
}

/// What the effects do to the player's movement this tick.
#[derive(Clone, Copy, Default)]
pub struct MoveEffects {
    /// LivingEntity.getJumpBoostPower: 0.1 x (amplifier + 1).
    pub jump_boost: f32,
    /// Levitation's amplifier.
    pub levitation: Option<i32>,
    pub slow_falling: bool,
    pub dolphins_grace: bool,
}

/// Blindness or darkness on the view: fog (start, end) instead of the view distance's, how dark
/// the fog and sky turn (1 black), and whether the sky is hidden.
#[derive(Clone, Copy)]
pub struct Fog {
    pub start: f32,
    pub end: f32,
    pub darkness: f32,
}

pub struct Effects {
    pub list: Vec<Effect>,
    icons: Option<Icons>,
    icons_tried: bool,
    /// The resource pack reload the icons were loaded at (packs::generation).
    icons_of: u32,
}

impl Effects {
    pub fn new() -> Effects {
        Effects { list: Vec::new(), icons: None, icons_tried: false, icons_of: 0 }
    }

    /// A new player (login, respawn, another dimension): the server sends the effects again.
    pub fn clear(&mut self) {
        self.list.clear();
    }

    pub fn get(&self, id: u16) -> Option<&Effect> {
        self.list.iter().find(|e| e.id == id)
    }

    /// ClientboundUpdateMobEffectPacket: entity, effect, amplifier, duration, flags.
    pub fn on_update(&mut self, r: &mut In, me: i32) {
        let (Some(ent), Some(id), Some(amplifier), Some(duration), Some(flags)) = (r.varint(), r.varint(), r.varint(), r.varint(), r.u8()) else {
            return;
        };
        if ent != me || id < 0 || id as usize >= EFFECTS.len() {
            return;
        }
        let id = id as u16;
        // LivingEntity.forceAddEffect: the new instance replaces the old, keeping its blend.
        let blend = match self.get(id) {
            Some(old) => (old.blend, old.blend_o),
            None if flags & 8 != 0 => (0.0, 0.0),
            None => (1.0, 1.0),
        };
        self.list.retain(|e| e.id != id);
        self.list.push(Effect { id, amplifier, duration, flags, blend: blend.0, blend_o: blend.1 });
    }

    /// ClientboundRemoveMobEffectPacket: entity, effect.
    pub fn on_remove(&mut self, r: &mut In, me: i32) {
        let (Some(ent), Some(id)) = (r.varint(), r.varint()) else { return };
        if ent == me {
            self.list.retain(|e| e.id as i32 != id);
        }
    }

    /// LivingEntity.tickEffects on the client: durations count down (the server removes them).
    pub fn tick(&mut self) {
        for e in self.list.iter_mut() {
            if e.duration > 0 {
                e.duration -= 1;
            }
            e.blend_o = e.blend;
            if e.id == DARKNESS {
                let target = if e.infinite() || e.duration > DARKNESS_BLEND as i32 { 1.0 } else { 0.0 };
                let step = 1.0 / DARKNESS_BLEND;
                e.blend = if e.blend < target { (e.blend + step).min(target) } else { (e.blend - step).max(target) };
            }
        }
    }

    pub fn movement(&self) -> MoveEffects {
        MoveEffects {
            jump_boost: self.get(JUMP_BOOST).map_or(0.0, |e| 0.1f32 * (e.amplifier + 1) as f32),
            levitation: self.get(LEVITATION).map(|e| e.amplifier),
            slow_falling: self.get(SLOW_FALLING).is_some(),
            dolphins_grace: self.get(DOLPHINS_GRACE).is_some(),
        }
    }

    /// GameRenderer.getNightVisionScale: 1, flickering out over its last 10 seconds.
    pub fn night_vision(&self, partial: f32) -> f32 {
        match self.get(NIGHT_VISION) {
            None => 0.0,
            Some(e) if !e.ends_within(200) => 1.0,
            Some(e) => 0.7 + math::sin(((e.duration as f32 - partial) * core::f32::consts::PI * 0.2) as f64) as f32 * 0.3,
        }
    }

    /// LocalPlayer.getEffectBlendFactor(DARKNESS): 0 without the effect.
    pub fn darkness_blend(&self, partial: f32) -> f32 {
        match self.get(DARKNESS) {
            Some(e) => e.blend_o + (e.blend - e.blend_o) * partial,
            None => 0.0,
        }
    }

    /// BlindnessFogEnvironment, then DarknessFogEnvironment (vanilla's order): the fog for a view
    /// that reaches `far` blocks.
    pub fn fog(&self, far: f32, partial: f32) -> Option<Fog> {
        let lerp = |t: f32, a: f32, b: f32| a + t * (b - a);
        if let Some(e) = self.get(BLINDNESS) {
            let d = if e.infinite() { 5.0 } else { lerp((e.duration as f32 / 20.0).min(1.0), far, 5.0) };
            // getModifiedDarkness: black, lifting over the last second.
            let dark = if e.ends_within(19) { (e.duration as f32 / 20.0).max(0.0) } else { 1.0 };
            return Some(Fog { start: d * 0.25, end: d, darkness: dark });
        }
        if let Some(e) = self.get(DARKNESS) {
            let k = e.blend_o + (e.blend - e.blend_o) * partial;
            let d = lerp(k, far, 15.0);
            return Some(Fog { start: d * 0.75, end: d, darkness: k });
        }
        None
    }

    /// The sky and fog colour under blindness or darkness.
    pub fn sky(&self, sky: [f32; 3], far: f32, partial: f32) -> [f32; 3] {
        match self.fog(far, partial) {
            Some(f) => {
                let k = 1.0 - f.darkness.clamp(0.0, 1.0);
                [sky[0] * k, sky[1] * k, sky[2] * k]
            }
            None => sky,
        }
    }

    fn icon_uv(&mut self, gl: &Gl, id: u16) -> Option<(u32, [f32; 4])> {
        if !self.icons_tried || self.icons_of != crate::packs::generation() {
            self.icons_tried = true;
            self.icons_of = crate::packs::generation();
            self.icons = load_icons(gl);
        }
        let i = self.icons.as_ref()?;
        Some((i.tex, (*i.uv.get(id as usize)?)?))
    }

    fn sorted(&self) -> Vec<Effect> {
        let mut v = self.list.clone();
        v.sort_by(compare);
        v
    }

    /// Whether the inventory screen (176 wide, centred) has room for its effects beside it
    /// (EffectsInInventory.canSeeEffects); then the HUD leaves them out.
    pub fn in_inventory(&self, gui: &Gui) -> bool {
        let left = ((gui.width - 176.0) / 2.0).floor();
        !self.list.is_empty() && gui.width - (left + 176.0 + 2.0) >= 32.0
    }

    /// Hud.extractEffects: the icons at the top right, the good ones in the first row and the
    /// rest below, blinking through their last 10 seconds.
    pub fn draw_hud(&mut self, gl: &Gl, gui: &mut Gui, inventory_open: bool) {
        if self.list.is_empty() || (inventory_open && self.in_inventory(gui)) {
            return;
        }
        let mut list = self.sorted();
        list.reverse();
        let (mut good, mut bad) = (0, 0);
        for e in list.iter().filter(|e| e.show_icon()) {
            let (x, y) = if e.beneficial() {
                good += 1;
                (gui.width.floor() - 25.0 * good as f32, 1.0)
            } else {
                bad += 1;
                (gui.width.floor() - 25.0 * bad as f32, 27.0)
            };
            let mut alpha = 1.0f32;
            if e.ambient() {
                gui.blit("hud/effect_background_ambient", x, y);
            } else {
                gui.blit("hud/effect_background", x, y);
                if e.ends_within(200) {
                    let d = e.duration;
                    let m = 10 - d / 20;
                    alpha = (d as f32 / 10.0 / 5.0 * 0.5).clamp(0.0, 0.5)
                        + math::cos((d as f32 * core::f32::consts::PI / 5.0) as f64) as f32 * (m as f32 / 10.0 * 0.25).clamp(0.0, 0.25);
                    alpha = alpha.clamp(0.0, 1.0);
                }
            }
            if let Some((tex, uv)) = self.icon_uv(gl, e.id) {
                let a = ((alpha * 255.0) as u32).min(255);
                gui.blit_tex(tex, x + 3.0, y + 3.0, 18.0, 18.0, uv, a << 24 | 0x00FF_FFFF);
            }
        }
    }

    /// EffectsInInventory: the effects beside the inventory window at (left, top, width), with
    /// their names and times when there is room, else icons only; the tooltip to show for the
    /// one under the mouse when its name does not fit.
    pub fn draw_inventory(&mut self, gl: &Gl, gui: &mut Gui, left: f32, top: f32, width: f32, mx: f32, my: f32) -> Option<Vec<String>> {
        let x = left + width + 2.0;
        let room = gui.width.floor() - x;
        if self.list.is_empty() || room < 32.0 {
            return None;
        }
        let max_w = if room >= 120.0 { room - 7.0 } else { 32.0 };
        let n = self.list.len() as i32;
        let spacing = if n > 5 { (132 / (n - 1)) as f32 } else { 33.0 };
        let mut y = top;
        let mut tip = None;
        for e in self.sorted() {
            let mut name = crate::lang::get(&format!("effect.minecraft.{}", e.name()));
            if (1..=9).contains(&e.amplifier) {
                name.push(' ');
                name.push_str(&crate::lang::get(&format!("enchantment.level.{}", e.amplifier + 1)));
            }
            let time = if e.infinite() { crate::lang::get("effect.duration.infinite") } else { format_duration(e.duration) };
            let w = max_w.min((32.0 + gui.text_width(&name) + 7.0).max(32.0 + gui.text_width(&time) + 7.0));
            gui.blit_nine(if e.ambient() { "container/inventory/effect_background_ambient" } else { "container/inventory/effect_background" }, x, y, w, 32.0);
            let text_room = w - 32.0 - 7.0;
            let clipped = if text_room > 0.0 {
                let too_wide = gui.text_width(&name) > text_room;
                let shown = if too_wide { clip(gui, &name, text_room) } else { name.clone() };
                gui.text(&shown, x + 32.0, y + 7.0, 0xFFFF_FFFF);
                gui.text(&time, x + 32.0, y + 7.0 + 9.0, 0xFF80_8080);
                too_wide
            } else {
                true
            };
            if clipped && mx >= x && mx <= x + w && my >= y && my <= y + spacing {
                tip = Some(alloc::vec![name, time]);
            }
            if let Some((tex, uv)) = self.icon_uv(gl, e.id) {
                gui.blit_tex(tex, x + 7.0, y + 7.0, 18.0, 18.0, uv, 0xFFFF_FFFF);
            }
            y += spacing;
        }
        tip
    }
}

/// ComponentRenderUtils.clipText: as much as fits, then "...".
fn clip(gui: &Gui, s: &str, width: f32) -> String {
    let dots = gui.text_width("...");
    let mut out = String::new();
    for c in s.chars() {
        let mut t = out.clone();
        t.push(c);
        if gui.text_width(&t) + dots > width {
            break;
        }
        out = t;
    }
    out.push_str("...");
    out
}

/// Sets the fog for blindness or darkness (inside the world pass, after its own fog).
pub fn apply_fog(gl: &Gl, f: &Fog) {
    unsafe {
        (gl.Fogf)(FOG_START, f.start);
        (gl.Fogf)(FOG_END, f.end.max(f.start + 0.01));
    }
}

/// The checks `penryn selftest` runs (no server needed).
pub fn selftest(b: &crate::blocks::Blocks) -> Vec<(&'static str, String, &'static str)> {
    use crate::physics::{Input, Mover};
    let mut out = Vec::new();
    out.push(("effect time 45 s", format_duration(900), "00:45"));
    out.push(("effect time 1 h", format_duration(72000), "01:00:00"));
    // The packet: speed II for 30 s on us, then removed.
    let mut fx = Effects::new();
    let mut o = crate::wire::Out::new();
    o.varint(7).varint(1).varint(1).varint(600).u8(4);
    fx.on_update(&mut In::new(&o.b), 7);
    let mut o2 = crate::wire::Out::new();
    o2.varint(7).varint(JUMP_BOOST as i32).varint(0).varint(-1).u8(5);
    fx.on_update(&mut In::new(&o2.b), 7);
    let mut o3 = crate::wire::Out::new();
    o3.varint(8).varint(3).varint(0).varint(100).u8(4);
    fx.on_update(&mut In::new(&o3.b), 7);
    let order: Vec<&str> = fx.sorted().iter().map(|e| e.name()).collect();
    out.push(("effects: ours only, in vanilla's order", order.join(" "), "slowness jump_boost"));
    let mut o4 = crate::wire::Out::new();
    o4.varint(7).varint(1);
    fx.on_remove(&mut In::new(&o4.b), 7);
    out.push(("effect removed", alloc::format!("{}", fx.list.len()), "1"));
    // Movement: one jump with jump boost II (0.42 + 0.2), slow falling's slow fall, levitation.
    let w = crate::test_world(b, &[]);
    let jump = |boost: f32| {
        let mut m = Mover::new([8.5, 64.0, 8.5]);
        m.on_ground = true;
        m.effects.jump_boost = boost;
        let inp = Input { jump: true, ..Input::idle() };
        m.tick(&w, b, &inp, 0.0, false, 0);
        let mut top = m.pos[1];
        for _ in 0..30 {
            m.tick(&w, b, &Input::idle(), 0.0, false, 0);
            top = top.max(m.pos[1]);
        }
        top - 64.0
    };
    out.push(("jump height", alloc::format!("{:.3}", jump(0.0)), "1.252"));
    out.push(("jump boost II height", alloc::format!("{:.3}", jump(0.2)), "2.517"));
    let mut m = Mover::new([8.5, 300.0, 8.5]);
    m.effects.slow_falling = true;
    for _ in 0..400 {
        m.tick(&w, b, &Input::idle(), 0.0, false, 0);
    }
    out.push(("slow falling: falls at most 0.49", alloc::format!("{:.3}", -m.vel[1]), "0.490"));
    let mut m = Mover::new([8.5, 64.0, 8.5]);
    m.effects.levitation = Some(0);
    for _ in 0..100 {
        m.tick(&w, b, &Input::idle(), 0.0, false, 0);
    }
    out.push(("levitation I: rises at 0.045", alloc::format!("{:.3}", m.vel[1]), "0.045"));
    out
}
