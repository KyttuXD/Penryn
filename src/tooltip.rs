//! Item stacks' data components as vanilla's tooltips show them (ItemStack.getTooltipLines and
//! addDetailsToTooltip): the name (custom_name in italics, item_name, the rarity's colour,
//! raised by enchantments), lore, enchantments and stored enchantments with their levels in
//! the tooltip_order tag's order, dyed colour, the attribute modifiers block per equipment
//! slot group, unbreakable, can_break and can_place_on, and tooltip_display's hidden parts.
//! Also every 26.3 component's stream codec, to read past the ones nothing here uses (so a
//! container's slots after an unusual stack still arrive), and the adventure mode checks.

use crate::client::Game;
use crate::gui::Gui;
use crate::lang;
use crate::nbt;
use crate::wire::In;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const CUSTOM_NAME: i32 = 6;
pub const ITEM_NAME: i32 = 9;
pub const LORE: i32 = 11;
pub const RARITY: i32 = 12;
pub const ENCHANTMENTS: i32 = 13;
pub const CAN_PLACE_ON: i32 = 14;
pub const CAN_BREAK: i32 = 15;
pub const ATTRIBUTE_MODIFIERS: i32 = 16;
pub const TOOLTIP_DISPLAY: i32 = 18;
pub const UNBREAKABLE: i32 = 4;
pub const GLINT: i32 = 21;
pub const STORED_ENCHANTMENTS: i32 = 45;
pub const DYED_COLOR: i32 = 47;

const WHITE: u32 = 0xFFFF_FFFF;
const GRAY: u32 = 0xFFAA_AAAA;
const DARK_GRAY: u32 = 0xFF55_5555;
const BLUE: u32 = 0xFF55_55FF;
const RED: u32 = 0xFFFF_5555;
const DARK_GREEN: u32 = 0xFF00_AA00;
const DARK_PURPLE: u32 = 0xFFAA_00AA;
/// Rarity.color: common white, uncommon yellow, rare aqua, epic light purple.
pub const RARITY_COLOURS: [u32; 4] = [WHITE, 0xFFFF_FF55, 0xFF55_FFFF, 0xFFFF_55FF];

// ------------------------------------------------------------------ reading past components

type Skip = fn(&mut In) -> Option<()>;

fn varint(r: &mut In) -> Option<()> {
    r.varint().map(drop)
}
fn boolean(r: &mut In) -> Option<()> {
    r.bool().map(drop)
}
fn f32(r: &mut In) -> Option<()> {
    r.take(4).map(drop)
}
fn i32(r: &mut In) -> Option<()> {
    r.take(4).map(drop)
}
fn string(r: &mut In) -> Option<()> {
    r.string().map(drop)
}
fn nbt(r: &mut In) -> Option<()> {
    nbt::skip_root(r)
}
fn optional(r: &mut In, f: Skip) -> Option<()> {
    if r.bool()? {
        f(r)?;
    }
    Some(())
}
fn list(r: &mut In, f: Skip) -> Option<()> {
    let n = r.varint()?;
    if n < 0 || n as usize > r.left() {
        return None;
    }
    for _ in 0..n {
        f(r)?;
    }
    Some(())
}
/// ByteBufCodecs.holder: 0 and the value, or the registry id + 1.
fn holder(r: &mut In, direct: Skip) -> Option<()> {
    match r.varint()? {
        0 => direct(r),
        n if n < 0 => None,
        _ => Some(()),
    }
}
/// ByteBufCodecs.holderSet: 0 and a tag, or the size + 1 and the ids.
fn holder_set(r: &mut In) -> Option<()> {
    match r.varint()? {
        0 => string(r),
        n if n < 0 => None,
        n => {
            if (n - 1) as usize > r.left() {
                return None;
            }
            for _ in 0..n - 1 {
                varint(r)?;
            }
            Some(())
        }
    }
}
fn sound(r: &mut In) -> Option<()> {
    holder(r, |r| {
        string(r)?;
        optional(r, f32)
    })
}
fn resolvable(r: &mut In, constant: Skip) -> Option<()> {
    if r.bool()? {
        constant(r)
    } else {
        string(r)
    }
}
fn trim_material(r: &mut In) -> Option<()> {
    holder(r, |r| {
        string(r)?;
        nbt(r)
    })
}
fn effect_details(r: &mut In) -> Option<()> {
    effect_details_at(r, 0)
}
fn effect_details_at(r: &mut In, depth: u32) -> Option<()> {
    if depth > 16 {
        return None;
    }
    varint(r)?;
    varint(r)?;
    boolean(r)?;
    boolean(r)?;
    boolean(r)?;
    if r.bool()? {
        effect_details_at(r, depth + 1)?;
    }
    Some(())
}
fn mob_effect(r: &mut In) -> Option<()> {
    varint(r)?;
    effect_details(r)
}
fn consume_effect(r: &mut In) -> Option<()> {
    match r.varint()? {
        0 => {
            list(r, mob_effect)?;
            f32(r)
        }
        1 => holder_set(r),
        2 => Some(()),
        3 => {
            f32(r)?;
            boolean(r)
        }
        4 => sound(r),
        _ => None,
    }
}
fn properties(r: &mut In) -> Option<()> {
    optional(r, |r| {
        list(r, |r| {
            string(r)?;
            if r.bool()? {
                string(r)
            } else {
                optional(r, string)?;
                optional(r, string)
            }
        })
    })?;
    optional(r, nbt)
}
fn component_matchers(r: &mut In) -> Option<()> {
    list(r, |r| {
        let kind = r.varint()?;
        skip_at(kind, r, 1)
    })?;
    list(r, |r| {
        boolean(r)?;
        varint(r)?;
        nbt(r)
    })
}
fn block_predicate(r: &mut In) -> Option<()> {
    optional(r, holder_set)?;
    properties(r)?;
    component_matchers(r)
}
fn attribute_modifier(r: &mut In) -> Option<()> {
    varint(r)?;
    string(r)?;
    r.take(8)?;
    varint(r)?;
    varint(r)?;
    match r.varint()? {
        0 | 1 => Some(()),
        2 => nbt(r),
        _ => None,
    }
}
fn firework_explosion(r: &mut In) -> Option<()> {
    varint(r)?;
    list(r, i32)?;
    list(r, i32)?;
    boolean(r)?;
    boolean(r)
}
fn filterable(r: &mut In, f: Skip) -> Option<()> {
    f(r)?;
    optional(r, f)
}

/// ItemStackTemplate.STREAM_CODEC: item, count and a component patch.
fn template(r: &mut In) -> Option<()> {
    template_at(r, 0)
}
fn template_at(r: &mut In, depth: u32) -> Option<()> {
    if depth > 8 {
        return None;
    }
    varint(r)?;
    varint(r)?;
    let add = r.varint()?;
    let remove = r.varint()?;
    if add < 0 || remove < 0 || add as usize > r.left() {
        return None;
    }
    for _ in 0..add {
        let kind = r.varint()?;
        skip_at(kind, r, depth + 1)?;
    }
    for _ in 0..remove {
        varint(r)?;
    }
    Some(())
}

/// Reads past one component of `kind` (DataComponents' 26.3 registry order) as its stream
/// codec writes it (horizon-protocol's item/skip.rs and item.rs).
pub fn skip(kind: i32, r: &mut In) -> Option<()> {
    skip_at(kind, r, 0)
}

fn skip_at(kind: i32, r: &mut In, depth: u32) -> Option<()> {
    if depth > 8 {
        return None;
    }
    match kind {
        4 | 20 | 34 | 120 => Some(()),
        0 | 6 | 9 | 22 | 49 | 59 | 61 | 68 | 81 | 82 => nbt(r),
        1 | 2 | 3 | 8 | 12 | 19 | 31 | 42 | 43 | 44 | 46 | 48 | 50 | 65 | 75 | 88..=108 | 110..=117 | 121 => varint(r),
        7 | 47 | 54 => r.take(4).map(drop),
        10 | 35 | 73 => string(r),
        5 => {
            boolean(r)?;
            boolean(r)?;
            f32(r)
        }
        11 => list(r, nbt),
        13 | 45 => list(r, |r| {
            varint(r)?;
            varint(r)
        }),
        14 | 15 => list(r, block_predicate),
        16 => list(r, attribute_modifier),
        17 => {
            list(r, f32)?;
            list(r, boolean)?;
            list(r, string)?;
            list(r, i32)
        }
        18 => {
            boolean(r)?;
            list(r, varint)
        }
        21 => boolean(r),
        23 => {
            varint(r)?;
            f32(r)?;
            boolean(r)
        }
        24 => {
            f32(r)?;
            varint(r)?;
            sound(r)?;
            boolean(r)?;
            list(r, consume_effect)
        }
        25 | 80 => template_at(r, depth),
        26 => {
            f32(r)?;
            optional(r, string)
        }
        27 | 33 | 67 => holder_set(r),
        28 => {
            list(r, |r| {
                holder_set(r)?;
                optional(r, f32)?;
                optional(r, boolean)
            })?;
            f32(r)?;
            varint(r)?;
            boolean(r)
        }
        29 => {
            varint(r)?;
            f32(r)
        }
        30 => r.take(24).map(drop),
        32 => {
            varint(r)?;
            sound(r)?;
            optional(r, string)?;
            optional(r, string)?;
            optional(r, holder_set)?;
            for _ in 0..5 {
                boolean(r)?;
            }
            sound(r)
        }
        36 => list(r, consume_effect),
        37 => {
            f32(r)?;
            f32(r)?;
            list(r, |r| {
                f32(r)?;
                optional(r, holder_set)?;
                f32(r)?;
                f32(r)
            })?;
            r.take(12)?;
            optional(r, holder_set)?;
            optional(r, sound)?;
            optional(r, sound)
        }
        38 => {
            boolean(r)?;
            boolean(r)?;
            optional(r, sound)?;
            optional(r, sound)
        }
        39 => {
            varint(r)?;
            varint(r)?;
            for _ in 0..3 {
                optional(r, |r| {
                    varint(r)?;
                    f32(r)?;
                    f32(r)
                })?;
            }
            f32(r)?;
            f32(r)?;
            optional(r, sound)?;
            optional(r, sound)
        }
        40 | 41 => {
            varint(r)?;
            varint(r)
        }
        51 | 52 => {
            let n = r.varint()?;
            if n < 0 || n as usize > r.left() {
                return None;
            }
            for _ in 0..n {
                template_at(r, depth + 1)?;
            }
            Some(())
        }
        // PotionContents: potion, custom colour, custom effects, custom name.
        53 => {
            optional(r, varint)?;
            optional(r, i32)?;
            list(r, mob_effect)?;
            optional(r, string)
        }
        55 => list(r, |r| {
            varint(r)?;
            varint(r)
        }),
        56 => list(r, |r| filterable(r, string)),
        57 => {
            filterable(r, string)?;
            string(r)?;
            varint(r)?;
            list(r, |r| filterable(r, nbt))?;
            boolean(r)
        }
        58 => {
            trim_material(r)?;
            holder(r, |r| {
                string(r)?;
                nbt(r)?;
                boolean(r)
            })
        }
        60 | 62 => {
            varint(r)?;
            nbt(r)
        }
        63 => holder(r, |r| {
            sound(r)?;
            f32(r)?;
            f32(r)?;
            varint(r)?;
            nbt(r)
        }),
        64 => trim_material(r),
        66 => holder(r, |r| {
            sound(r)?;
            nbt(r)?;
            f32(r)?;
            varint(r)
        }),
        69 => {
            optional(r, |r| {
                string(r)?;
                r.take(8).map(drop)
            })?;
            boolean(r)
        }
        70 => firework_explosion(r),
        71 => {
            varint(r)?;
            list(r, firework_explosion)
        }
        // ResolvableProfile: a full profile or a partial one, then the skin patch.
        72 => {
            let props = |r: &mut In| -> Option<()> {
                list(r, |r| {
                    string(r)?;
                    string(r)?;
                    optional(r, string)
                })
            };
            if r.u8()? == 0 {
                optional(r, string)?;
                optional(r, |r| r.take(16).map(drop))?;
                props(r)?;
            } else {
                r.take(16)?;
                string(r)?;
                props(r)?;
            }
            for _ in 0..3 {
                optional(r, string)?;
            }
            optional(r, boolean)
        }
        74 => list(r, |r| {
            holder(r, |r| {
                string(r)?;
                string(r)
            })?;
            varint(r)
        }),
        76 => {
            for _ in 0..4 {
                optional(r, template)?;
            }
            Some(())
        }
        77 => list(r, |r| optional(r, template)),
        78 => list(r, |r| {
            string(r)?;
            string(r)
        }),
        79 => list(r, |r| {
            varint(r)?;
            nbt(r)?;
            varint(r)?;
            varint(r)
        }),
        83 => sound(r),
        84 => resolvable(r, i32),
        85 | 86 => {
            resolvable(r, i32)?;
            resolvable(r, f32)
        }
        87 => {
            holder_set(r)?;
            f32(r)
        }
        109 => holder(r, |r| {
            varint(r)?;
            varint(r)?;
            string(r)?;
            optional(r, nbt)?;
            optional(r, nbt)
        }),
        118 | 119 => {
            for _ in 0..4 {
                nbt(r)?;
            }
            optional(r, |r| {
                for _ in 0..4 {
                    nbt(r)?;
                }
                Some(())
            })?;
            varint(r)?;
            boolean(r)
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ the components we read

/// One attribute modifier (ItemAttributeModifiers.Entry).
#[derive(Clone, Debug)]
pub struct Modifier {
    pub attribute: i32,
    pub id: String,
    pub amount: f64,
    pub op: i32,
    pub slot: i32,
    /// 0 default, 1 hidden, 2 a text of its own.
    pub display: i32,
    pub text: String,
}

/// A BlockPredicate of can_break or can_place_on: its blocks (a tag, or ids) and whether it
/// says more than the blocks (properties, NBT or components).
#[derive(Clone, Debug)]
pub struct Predicate {
    pub tag: Option<String>,
    pub ids: Option<Vec<i32>>,
    pub more: bool,
}

#[derive(Default, Clone, Debug)]
pub struct Parsed {
    pub custom_name: Option<String>,
    pub item_name: Option<String>,
    pub lore: Vec<String>,
    pub rarity: Option<i32>,
    pub enchantments: Option<Vec<(i32, i32)>>,
    pub stored: Option<Vec<(i32, i32)>>,
    pub glint: Option<bool>,
    pub modifiers: Option<Vec<Modifier>>,
    pub dyed: Option<i32>,
    pub unbreakable: bool,
    pub can_break: Option<Vec<Predicate>>,
    pub can_place: Option<Vec<Predicate>>,
    pub hide_tooltip: bool,
    pub hidden: Vec<i32>,
    /// Components in the patch (added) and removed.
    pub kinds: Vec<i32>,
    pub removed: Vec<i32>,
}

fn predicates(r: &mut In) -> Option<Vec<Predicate>> {
    let n = r.varint()?;
    let mut out = Vec::new();
    for _ in 0..n.clamp(0, 256) {
        let mut p = Predicate { tag: None, ids: None, more: false };
        if r.bool()? {
            match r.varint()? {
                0 => p.tag = Some(r.string()?),
                n if n > 0 => {
                    let mut v = Vec::new();
                    for _ in 0..(n - 1).min(4096) {
                        v.push(r.varint()?);
                    }
                    p.ids = Some(v);
                }
                _ => return None,
            }
        }
        let start = r.pos;
        properties(r)?;
        component_matchers(r)?;
        // Not just "no properties, no NBT, no components": something we cannot check.
        p.more = r.b[start..r.pos] != [0, 0, 0, 0];
        out.push(p);
    }
    Some(out)
}

fn read_value(p: &mut Parsed, kind: i32, r: &mut In) -> Option<()> {
    match kind {
        CUSTOM_NAME => p.custom_name = Some(nbt::text_italic(r)),
        ITEM_NAME => p.item_name = Some(nbt::text(r)),
        LORE => {
            let n = r.varint()?;
            for _ in 0..n.clamp(0, 256) {
                p.lore.push(nbt::text_italic(r));
            }
        }
        RARITY => p.rarity = Some(r.varint()?.clamp(0, 3)),
        ENCHANTMENTS | STORED_ENCHANTMENTS => {
            let n = r.varint()?;
            let mut v = Vec::new();
            for _ in 0..n.clamp(0, 256) {
                v.push((r.varint()?, r.varint()?));
            }
            if kind == ENCHANTMENTS {
                p.enchantments = Some(v);
            } else {
                p.stored = Some(v);
            }
        }
        GLINT => p.glint = Some(r.bool()?),
        ATTRIBUTE_MODIFIERS => {
            let n = r.varint()?;
            let mut v = Vec::new();
            for _ in 0..n.clamp(0, 256) {
                let attribute = r.varint()?;
                let id = r.string()?;
                let amount = r.f64()?;
                let op = r.varint()?;
                let slot = r.varint()?;
                let display = r.varint()?;
                let text = if display == 2 { nbt::text(r) } else { String::new() };
                v.push(Modifier { attribute, id, amount, op, slot, display, text });
            }
            p.modifiers = Some(v);
        }
        DYED_COLOR => p.dyed = Some(r.i32()?),
        UNBREAKABLE => p.unbreakable = true,
        CAN_BREAK => p.can_break = Some(predicates(r)?),
        CAN_PLACE_ON => p.can_place = Some(predicates(r)?),
        TOOLTIP_DISPLAY => {
            p.hide_tooltip = r.bool()?;
            let n = r.varint()?;
            for _ in 0..n.clamp(0, 256) {
                p.hidden.push(r.varint()?);
            }
        }
        _ => {}
    }
    Some(())
}

/// The components of a stack's patch in the delimited form creative.rs keeps (both counts,
/// then each added component's type, length and value, then the removed types).
pub fn parse(patch: &[u8]) -> Parsed {
    let mut p = Parsed::default();
    let mut r = In::new(patch);
    let (Some(add), Some(remove)) = (r.varint(), r.varint()) else { return p };
    for _ in 0..add.clamp(0, 256) {
        let (Some(kind), Some(len)) = (r.varint(), r.varint()) else { return p };
        let Some(v) = r.take(len.max(0) as usize) else { return p };
        p.kinds.push(kind);
        let _ = read_value(&mut p, kind, &mut In::new(v));
    }
    for _ in 0..remove.clamp(0, 256) {
        let Some(k) = r.varint() else { break };
        p.removed.push(k);
    }
    p
}

// ------------------------------------------------------------------ defaults (serverui.bin)

/// The items' default attribute modifiers, from serverui.bin.
pub struct Defaults {
    pub modifiers: BTreeMap<u16, Vec<Modifier>>,
}

/// Reads the item defaults section that follows the pictures in serverui.bin.
pub fn parse_defaults(d: &[u8], mut p: usize) -> Option<Defaults> {
    let rd16 = |p: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?])) };
    let n = rd16(p)?;
    p += 2;
    let mut modifiers = BTreeMap::new();
    for _ in 0..n {
        let item = rd16(p)?;
        let k = *d.get(p + 2)?;
        p += 3;
        let mut v = Vec::new();
        for _ in 0..k {
            let s = d.get(p..p + 12)?;
            let mut a = [0u8; 8];
            a.copy_from_slice(&s[1..9]);
            let flags = s[11];
            let id = match flags & 3 {
                1 => "minecraft:base_attack_damage",
                2 => "minecraft:base_attack_speed",
                _ => "",
            };
            v.push(Modifier {
                attribute: s[0] as i32,
                id: String::from(id),
                amount: f64::from_le_bytes(a),
                op: s[9] as i32,
                slot: s[10] as i32,
                display: if flags & 4 != 0 { 1 } else { 0 },
                text: String::new(),
            });
            p += 12;
        }
        modifiers.insert(item, v);
    }
    Some(Defaults { modifiers })
}

// ------------------------------------------------------------------ the lines

/// Attributes' registry order (26.3 BuiltInRegistries.ATTRIBUTE) and their sentiment
/// (1 positive, 0 neutral, -1 negative).
pub const ATTRIBUTES: [(&str, i8); 40] = [
    ("air_drag_modifier", 1),
    ("armor", 1),
    ("armor_toughness", 1),
    ("attack_damage", 1),
    ("attack_knockback", 1),
    ("attack_speed", 1),
    ("below_name_distance", 1),
    ("block_break_speed", 1),
    ("block_interaction_range", 1),
    ("bounciness", 1),
    ("burning_time", -1),
    ("camera_distance", 1),
    ("explosion_knockback_resistance", 1),
    ("entity_interaction_range", 1),
    ("fall_damage_multiplier", -1),
    ("flying_speed", 1),
    ("follow_range", 1),
    ("friction_modifier", 1),
    ("gravity", 0),
    ("jump_strength", 1),
    ("knockback_resistance", 1),
    ("luck", 1),
    ("max_absorption", 1),
    ("max_health", 1),
    ("mining_efficiency", 1),
    ("movement_efficiency", 1),
    ("movement_speed", 1),
    ("name_tag_distance", 1),
    ("oxygen_bonus", 1),
    ("safe_fall_distance", 1),
    ("scale", 0),
    ("sneaking_speed", 1),
    ("spawn_reinforcements", 1),
    ("step_height", 1),
    ("submerged_mining_speed", 1),
    ("sweeping_damage_ratio", 1),
    ("tempt_range", 1),
    ("water_movement_efficiency", 1),
    ("waypoint_transmit_range", 0),
    ("waypoint_receive_range", 0),
];

/// EquipmentSlotGroup by id (and values() order).
pub const SLOT_GROUPS: [&str; 11] = ["any", "mainhand", "offhand", "hand", "feet", "legs", "chest", "head", "armor", "body", "saddle"];

/// The enchantment tag tooltip_order, then the curses and the max level 1 ones.
const TOOLTIP_ORDER: [&str; 43] = [
    "binding_curse", "vanishing_curse", "riptide", "channeling", "wind_burst", "frost_walker", "lunge", "sharpness", "smite",
    "bane_of_arthropods", "impaling", "power", "density", "breach", "piercing", "sweeping_edge", "multishot", "fire_aspect", "flame",
    "knockback", "punch", "protection", "blast_protection", "fire_protection", "projectile_protection", "feather_falling", "fortune",
    "looting", "silk_touch", "luck_of_the_sea", "efficiency", "quick_charge", "lure", "respiration", "aqua_affinity", "soul_speed",
    "swift_sneak", "depth_strider", "thorns", "loyalty", "unbreaking", "infinity", "mending",
];
const CURSES: [&str; 2] = ["binding_curse", "vanishing_curse"];
const MAX_LEVEL_ONE: [&str; 9] = ["aqua_affinity", "binding_curse", "channeling", "flame", "infinity", "mending", "multishot", "silk_touch", "vanishing_curse"];

/// DecimalFormat("#.##").
pub fn decimal(v: f64) -> String {
    let neg = v < 0.0;
    let c = crate::math::floor(v.abs() * 100.0 + 0.5) as i64;
    let (i, f) = (c / 100, c % 100);
    let mut s = if neg && c != 0 { format!("-{}", i) } else { format!("{}", i) };
    if f != 0 {
        if f % 10 == 0 {
            s.push_str(&format!(".{}", f / 10));
        } else {
            s.push_str(&format!(".{:02}", f));
        }
    }
    s
}

fn roman(level: i32) -> String {
    let key = format!("enchantment.level.{}", level);
    if lang::has(&key) {
        lang::get(&key)
    } else {
        key
    }
}

/// Enchantment.getFullname: the name (red for curses, else grey), and the level unless both
/// it and the maximum are 1.
fn enchantment_line(name: &str, level: i32) -> (String, u32) {
    let path = name.trim_start_matches("minecraft:");
    let key = format!("enchantment.{}", name.replace(':', "."));
    let key = if name.contains(':') { key } else { format!("enchantment.minecraft.{}", path) };
    let mut t = if lang::has(&key) { lang::get(&key) } else { String::from(path) };
    if level != 1 || !MAX_LEVEL_ONE.contains(&path) {
        t.push(' ');
        t.push_str(&roman(level));
    }
    (t, if CURSES.contains(&path) { RED } else { GRAY })
}

/// ItemEnchantments.addToTooltip: the tooltip_order tag's enchantments first, then the rest.
fn enchantment_lines(list: &[(i32, i32)], names: &[String], out: &mut Vec<(String, u32)>) {
    let name_of = |id: i32| names.get(id as usize).cloned().unwrap_or_else(|| format!("minecraft:{}", id));
    let mut done = Vec::new();
    for o in TOOLTIP_ORDER.iter() {
        for (i, (id, lvl)) in list.iter().enumerate() {
            if name_of(*id).trim_start_matches("minecraft:") == *o && !done.contains(&i) {
                done.push(i);
                out.push(enchantment_line(&name_of(*id), *lvl));
            }
        }
    }
    for (i, (id, lvl)) in list.iter().enumerate() {
        if !done.contains(&i) {
            out.push(enchantment_line(&name_of(*id), *lvl));
        }
    }
}

/// ItemAttributeModifiers.Display.Default.apply for one modifier.
fn modifier_line(m: &Modifier) -> Option<(String, u32)> {
    match m.display {
        1 => return None,
        2 => return Some((m.text.clone(), WHITE)),
        _ => {}
    }
    let (name, sentiment) = ATTRIBUTES.get(m.attribute as usize).copied().unwrap_or(("unknown", 1));
    let attr = lang::get(&format!("attribute.name.{}", name));
    let mut d = m.amount;
    // With a player: the base attack damage and speed are added and shown as totals.
    let base = match m.id.as_str() {
        "minecraft:base_attack_damage" => Some(1.0),
        "minecraft:base_attack_speed" => Some(4.0),
        _ => None,
    };
    if let Some(b) = base {
        d += b;
    }
    let shown = if m.op == 1 || m.op == 2 {
        d * 100.0
    } else if name == "knockback_resistance" {
        d * 10.0
    } else {
        d
    };
    let op = m.op.clamp(0, 2);
    if base.is_some() {
        let t = lang::format(&format!("attribute.modifier.equals.{}", op), &[decimal(shown), attr]);
        return Some((format!(" {}", t), DARK_GREEN));
    }
    // Attribute.getStyle: blue for good, red for bad, grey for neutral.
    let style = |positive: bool| -> u32 {
        match sentiment {
            0 => GRAY,
            s if (s > 0) == positive => BLUE,
            _ => RED,
        }
    };
    if d > 0.0 {
        Some((lang::format(&format!("attribute.modifier.plus.{}", op), &[decimal(shown), attr]), style(true)))
    } else if d < 0.0 {
        Some((lang::format(&format!("attribute.modifier.take.{}", op), &[decimal(-shown), attr]), style(false)))
    } else {
        None
    }
}

/// ItemStack.addAttributeTooltips: per slot group, an empty line and its header, then each
/// modifier's line.
fn attribute_lines(mods: &[Modifier], out: &mut Vec<(String, u32)>) {
    for (g, group) in SLOT_GROUPS.iter().enumerate() {
        let mut first = true;
        for m in mods.iter().filter(|m| m.slot == g as i32) {
            let Some(line) = modifier_line(m) else { continue };
            if first {
                first = false;
                out.push((String::new(), WHITE));
                out.push((lang::get(&format!("item.modifiers.{}", group)), GRAY));
            }
            out.push(line);
        }
    }
}

/// AdventureModePredicate.addToTooltip: each block's name in dark grey, or "unknown" when a
/// predicate checks more than blocks.
fn predicate_lines(ps: &[Predicate], g: &Game, tags: &BTreeMap<String, Vec<i32>>, out: &mut Vec<(String, u32)>) {
    if ps.iter().any(|p| p.more || (p.tag.is_none() && p.ids.is_none())) {
        out.push((lang::get("item.canUse.unknown"), DARK_GRAY));
        return;
    }
    for p in ps {
        let ids: Vec<i32> = match (&p.tag, &p.ids) {
            (Some(t), _) => match tags.get(t.trim_start_matches('#')) {
                Some(v) => v.clone(),
                None => {
                    out.push((format!("#{}", t), DARK_GRAY));
                    continue;
                }
            },
            (_, Some(v)) => v.clone(),
            _ => Vec::new(),
        };
        for id in ids {
            let name = g.blocks.blocks.get(id as usize).map(|b| b.name.clone()).unwrap_or_default();
            out.push((lang::get(&format!("block.minecraft.{}", name)), DARK_GRAY));
        }
    }
}

/// The lines of a stack's tooltip and their base colours (getTooltipLines with a player and
/// the normal TooltipFlag). `extra`: the item's own lines (Item.appendHoverText and default
/// components) after the name, from the creative data.
pub fn lines(g: &Game, item: i32, patch: &[u8], rarity: i32, defaults: Option<&Defaults>, extra: &[String]) -> Vec<(String, u32)> {
    let p = parse(patch);
    let mut out = Vec::new();
    if p.hide_tooltip {
        return out;
    }
    let shows = |k: i32| !p.hidden.contains(&k);
    let enchanted = p.enchantments.as_ref().map_or(false, |e| !e.is_empty());
    // getRarity: enchanted items go up one (common and uncommon to rare, rare to epic).
    let base = p.rarity.unwrap_or(rarity).clamp(0, 3);
    let rarity = if !enchanted {
        base
    } else if base < 2 {
        2
    } else {
        3
    };
    let id = g.blocks.items.get(item as usize).cloned().unwrap_or_default();
    let name = match (&p.custom_name, &p.item_name) {
        // A custom name is in italics (text_italic).
        (Some(c), _) => c.clone(),
        (None, Some(n)) => n.clone(),
        _ => lang::item_name(&id),
    };
    out.push((name, RARITY_COLOURS[rarity as usize]));
    for l in extra {
        out.push((l.clone(), GRAY));
    }
    let names: Vec<String> = g.creative.registries.iter().find(|(k, _)| k == "minecraft:enchantment").map(|(_, v)| v.clone()).unwrap_or_default();
    if let (Some(s), true) = (&p.stored, shows(STORED_ENCHANTMENTS)) {
        enchantment_lines(s, &names, &mut out);
    }
    if let (Some(e), true) = (&p.enchantments, shows(ENCHANTMENTS)) {
        enchantment_lines(e, &names, &mut out);
    }
    if let (Some(_), true) = (p.dyed, shows(DYED_COLOR)) {
        out.push((lang::get("item.dyed"), GRAY));
    }
    if shows(LORE) {
        for l in p.lore.iter() {
            out.push((l.clone(), DARK_PURPLE));
        }
    }
    if shows(ATTRIBUTE_MODIFIERS) {
        let removed = p.removed.contains(&ATTRIBUTE_MODIFIERS);
        let mods = match (&p.modifiers, removed) {
            (Some(m), _) => Some(m.clone()),
            (None, false) => defaults.and_then(|d| d.modifiers.get(&(item as u16)).cloned()),
            _ => None,
        };
        if let Some(m) = mods {
            attribute_lines(&m, &mut out);
        }
    }
    if p.unbreakable && shows(UNBREAKABLE) {
        out.push((lang::get("item.unbreakable"), BLUE));
    }
    let tags = &g.ui.common.block_tags;
    if let (Some(ps), true) = (&p.can_break, shows(CAN_BREAK)) {
        out.push((String::new(), WHITE));
        out.push((lang::get("item.canBreak"), GRAY));
        predicate_lines(ps, g, tags, &mut out);
    }
    if let (Some(ps), true) = (&p.can_place, shows(CAN_PLACE_ON)) {
        out.push((String::new(), WHITE));
        out.push((lang::get("item.canPlace"), GRAY));
        predicate_lines(ps, g, tags, &mut out);
    }
    out
}

/// BlockPredicate tests of can_break / can_place_on against a block (its registry id); the
/// predicates' state properties and NBT are not checked.
pub fn predicate_allows(ps: &[Predicate], block: i32, tags: &BTreeMap<String, Vec<i32>>) -> bool {
    ps.iter().any(|p| match (&p.tag, &p.ids) {
        (Some(t), _) => tags.get(t.trim_start_matches('#')).map_or(false, |v| v.contains(&block)),
        (_, Some(v)) => v.contains(&block),
        _ => true,
    })
}

/// Player.blockActionRestricted / ItemStack.canBreakBlockInAdventureMode: in adventure mode
/// only a held item whose can_break names the block may break it; spectators never do.
pub fn may_break(g: &Game, state: u16) -> bool {
    match g.game_mode {
        3 => false,
        2 => {
            let slot = 36 + g.selected as usize;
            if g.inventory[slot].is_none() {
                return false;
            }
            let p = parse(g.creative.patches.get(slot).map_or(&[][..], |v| &v[..]));
            let block = g.blocks.state(state).block as i32;
            p.can_break.as_ref().map_or(false, |ps| predicate_allows(ps, block, &g.ui.common.block_tags))
        }
        _ => true,
    }
}

// ------------------------------------------------------------------ drawing

/// The item's lines from the creative data that vanilla adds besides the components read
/// here (Item.appendHoverText, a potion's effects...): its default stack's tooltip without the
/// name and the attribute block, when the patch has nothing that changes them.
pub fn creative_extra(d: Option<&crate::creative::Data>, item: i32, patch: &[u8]) -> Vec<String> {
    const CONTENT: [i32; 20] = [53, 57, 66, 63, 71, 70, 77, 79, 74, 76, 51, 52, 62, 60, 58, 48, 65, 55, 78, 109];
    let Some(d) = d else { return Vec::new() };
    let p = parse(patch);
    if p.kinds.iter().chain(p.removed.iter()).any(|k| CONTENT.contains(k)) {
        return Vec::new();
    }
    let Some(e) = d.entries.iter().find(|e| e.item as i32 == item && e.spec.is_empty()) else { return Vec::new() };
    let headers: Vec<String> = SLOT_GROUPS.iter().map(|g| lang::get(&format!("item.modifiers.{}", g))).collect();
    let mut out = Vec::new();
    let mut in_block = false;
    for l in e.lines.iter().skip(1) {
        if headers.contains(l) {
            in_block = true;
            continue;
        }
        let modifier = l.starts_with('+') || l.starts_with('-') || l.starts_with(' ') || l.chars().next().map_or(false, |c| c.is_ascii_digit());
        if in_block && modifier {
            continue;
        }
        in_block = false;
        out.push(l.clone());
    }
    out
}

struct Cache {
    key: (i32, Vec<u8>, usize, u32),
    lines: Vec<(String, u32)>,
}

struct Slot(core::cell::UnsafeCell<Option<Cache>>);
unsafe impl Sync for Slot {}
static CACHE: Slot = Slot(core::cell::UnsafeCell::new(None));

/// The tooltip of a stack in a container, computed once while it stays hovered.
pub fn stack_lines(g: &Game, gui: &Gui, item: i32, patch: &[u8]) -> Vec<(String, u32)> {
    let c = unsafe { &mut *CACHE.0.get() };
    let key = (item, patch.to_vec(), g.creative.registries.len(), g.ui.version);
    if let Some(c) = c.as_ref().filter(|c| c.key == key) {
        return c.lines.clone();
    }
    let rarity = gui.creative.as_ref().and_then(|d| d.items.get(item as usize)).map_or(0, |i| i.rarity as i32);
    let extra = creative_extra(gui.creative.as_ref(), item, patch);
    let lines = crate::serverhud::with_defaults(|d| lines(g, item, patch, rarity, d, &extra));
    *c = Some(Cache { key, lines: lines.clone() });
    lines
}

/// Vanilla's tooltip box with a base colour per line (an empty list draws nothing).
pub fn draw(gui: &mut Gui, lines: &[(String, u32)], mx: f32, my: f32) {
    if lines.is_empty() {
        return;
    }
    let w = lines.iter().map(|l| gui.text_width(&l.0)).fold(0.0, f32::max);
    let h = if lines.len() == 1 { 8.0 } else { 10.0 + (lines.len() - 1) as f32 * 10.0 };
    let mut x = mx + 12.0;
    let mut y = my - 12.0;
    if x + w + 4.0 > gui.width {
        x = (mx - 16.0 - w).max(4.0);
    }
    if y + h + 4.0 > gui.height {
        y = gui.height - h - 4.0;
    }
    y = y.max(4.0);
    gui.blit_nine("tooltip/background", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    gui.blit_nine("tooltip/frame", x - 12.0, y - 12.0, w + 24.0, h + 24.0);
    for (i, (l, c)) in lines.iter().enumerate() {
        gui.text(l, x, y + if i == 0 { 0.0 } else { 2.0 + i as f32 * 10.0 }, *c);
    }
}

// ------------------------------------------------------------------ self test

/// Checks of the component readers and the lines (penryn selftest; no data files needed).
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use crate::wire::Out;
    let mut out = Vec::new();
    out.push(("decimal format", format!("{} {} {} {} {}", decimal(7.0), decimal(1.6), decimal(-2.4), decimal(0.25), decimal(12.345)), "7 1.6 -2.4 0.25 12.35"));
    // A patch: custom name, lore, enchantments, attribute modifiers, unbreakable.
    let mut comps: Vec<(i32, Vec<u8>)> = Vec::new();
    let mut o = Out::new();
    o.u8(8).u16(5).bytes(b"Blade");
    comps.push((CUSTOM_NAME, o.b));
    let mut o = Out::new();
    o.varint(1).u8(8).u16(4).bytes(b"Old!");
    comps.push((LORE, o.b));
    let mut o = Out::new();
    o.varint(1).varint(0).varint(3);
    comps.push((ENCHANTMENTS, o.b));
    let mut o = Out::new();
    o.varint(1).varint(26).string("x:fast").f64(0.1).varint(2).varint(1).varint(0);
    comps.push((ATTRIBUTE_MODIFIERS, o.b));
    comps.push((UNBREAKABLE, Vec::new()));
    let mut o = Out::new();
    o.varint(comps.len() as i32).varint(1);
    for (k, v) in comps.iter() {
        o.varint(*k).varint(v.len() as i32).bytes(v);
    }
    o.varint(DYED_COLOR);
    let p = parse(&o.b);
    out.push((
        "patch parse",
        format!("{:?} {:?} {:?} {} {} {:?}", p.custom_name, p.lore, p.enchantments, p.modifiers.as_ref().map_or(0, |m| m.len()), p.unbreakable, p.removed),
        "Some(\"\u{a7}oBlade\") [\"\u{a7}oOld!\"] Some([(0, 3)]) 1 true [47]",
    ));
    let m = &p.modifiers.as_ref().unwrap()[0];
    out.push(("modifier fields", format!("{} {} {} {} {}", m.attribute, m.id, m.amount, m.op, m.slot), "26 x:fast 0.1 2 1"));
    let mut lines = Vec::new();
    enchantment_lines(&[(0, 1), (1, 5), (2, 1)], &[String::from("minecraft:mending"), String::from("minecraft:sharpness"), String::from("minecraft:binding_curse")], &mut lines);
    let colours: Vec<String> = lines.iter().map(|l| format!("{:x}", l.1)).collect();
    out.push((
        "enchantment order",
        format!("{} {} {}", colours.join(","), lines[1].0.ends_with(&roman(5)), !lines[2].0.contains(' ')),
        "ffff5555,ffaaaaaa,ffaaaaaa true true",
    ));
    // Every component reads its own bytes exactly: a stack template with a potion inside a
    // container, a profile, a tool.
    let mut o = Out::new();
    o.varint(1).bool(true).varint(5).varint(1).varint(1).varint(0).varint(53).bool(true).varint(3).bool(false).varint(0).bool(false);
    o.u8(99);
    let mut r = In::new(&o.b);
    let ok = skip(77, &mut r).is_some();
    out.push(("container component", format!("{} {:?}", ok, r.u8()), "true Some(99)"));
    let mut o = Out::new();
    o.u8(1).bytes(&[1; 16]).string("Steve").varint(1).string("textures").string("abc").bool(false);
    o.bool(false).bool(false).bool(false).bool(true).bool(true);
    o.u8(42);
    let mut r = In::new(&o.b);
    let ok = skip(72, &mut r).is_some();
    out.push(("profile component", format!("{} {:?}", ok, r.u8()), "true Some(42)"));
    let mut o = Out::new();
    o.varint(1).varint(0).string("minecraft:logs").bool(true).f32(2.0).bool(false).f32(1.0).varint(1).bool(true);
    o.u8(7);
    let mut r = In::new(&o.b);
    let ok = skip(28, &mut r).is_some();
    out.push(("tool component", format!("{} {:?}", ok, r.u8()), "true Some(7)"));
    // can_break: a predicate naming two blocks, one naming a tag.
    let mut o = Out::new();
    o.varint(2).bool(true).varint(3).varint(10).varint(11).bool(false).bool(false).varint(0).varint(0);
    o.bool(true).varint(0).string("minecraft:logs").bool(false).bool(false).varint(0).varint(0);
    let ps = predicates(&mut In::new(&o.b)).unwrap_or_default();
    let mut tags = BTreeMap::new();
    tags.insert(String::from("minecraft:logs"), alloc::vec![40, 41]);
    out.push((
        "adventure predicate",
        format!("{} {} {} {}", ps.len(), predicate_allows(&ps, 11, &tags), predicate_allows(&ps, 41, &tags), predicate_allows(&ps, 12, &tags)),
        "2 true true false",
    ));
    out
}
