//! Mob looks as vanilla's renderers choose them: baby models (26.x's own baby model layers and
//! textures), variants (the synced variant registries the server sends in configuration, and
//! the variants kept in entity data: horses, llamas, parrots, rabbits, foxes, axolotls,
//! mooshrooms, pandas, villager types and professions, tropical fish...), render layers (sheep
//! wool, collars, horse markings, villager clothes, eyes, outer layers), and armour and saddles
//! from ClientboundSetEquipmentPacket with the equipment assets' textures. Also the packets
//! behind them: the variant registries, the equipment, the tab list's player names.

use crate::entities::{Draw, Entity, EntityTable, DYE};
use crate::wire::In;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Packet ids of protocol 777 these need (Horizon's table).
pub const C_ENTITY_EVENT: i32 = 34;
pub const C_SET_EQUIPMENT: i32 = 104;

/// One entry of a variant registry: the textures it names.
#[derive(Default, Clone)]
pub struct Variant {
    pub name: String,
    /// asset_id and baby_asset_id (cat, cow, pig, chicken, frog, zombie nautilus), without the
    /// namespace: "entity/cow/cow_warm".
    pub asset: String,
    pub baby_asset: String,
    /// The model it uses ("normal", "cold", "warm").
    pub model: String,
    /// A wolf's assets: wild, tame, angry, and the baby's.
    pub wolf: [String; 3],
    pub wolf_baby: [String; 3],
}

/// The synced variant registries (minecraft:cat_variant, ...), in the order the server
/// numbers their entries.
#[derive(Default)]
pub struct Variants {
    pub regs: BTreeMap<String, Vec<Variant>>,
}

impl Variants {
    pub fn get(&self, registry: &str, id: i32) -> Option<&Variant> {
        self.regs.get(registry)?.get(usize::try_from(id).ok()?)
    }
}

fn mstr(r: &mut In) -> Option<String> {
    let n = r.u16()? as usize;
    Some(String::from_utf8_lossy(r.take(n)?).into_owned())
}

/// Reads one NBT payload of type `t`, keeping the strings it holds by their dotted path.
fn nbt_strings(r: &mut In, t: u8, path: &str, out: &mut Vec<(String, String)>, depth: u32) -> Option<()> {
    if depth > 32 {
        return None;
    }
    match t {
        0 => {}
        1 => {
            r.take(1)?;
        }
        2 => {
            r.take(2)?;
        }
        3 | 5 => {
            r.take(4)?;
        }
        4 | 6 => {
            r.take(8)?;
        }
        7 => {
            let n = r.i32()?;
            r.take(n.max(0) as usize)?;
        }
        8 => out.push((String::from(path), mstr(r)?)),
        9 => {
            let et = r.u8()?;
            let n = r.i32()?;
            // A list of ends holds nothing (and must not spin through a huge count).
            for _ in 0..if et == 0 { 0 } else { n.max(0) } {
                nbt_strings(r, et, path, out, depth + 1)?;
            }
        }
        10 => loop {
            let ct = r.u8()?;
            if ct == 0 {
                break;
            }
            let name = mstr(r)?;
            let sub = if path.is_empty() { name } else { format!("{}.{}", path, name) };
            nbt_strings(r, ct, &sub, out, depth + 1)?;
        },
        11 => {
            let n = r.i32()?;
            r.take(4 * n.max(0) as usize)?;
        }
        12 => {
            let n = r.i32()?;
            r.take(8 * n.max(0) as usize)?;
        }
        _ => return None,
    }
    Some(())
}

/// ParticleTypes.STREAM_CODEC: a `minecraft:particle_type` id (26.3's registry order, as
/// Horizon's protocol table has it) and that type's options. None for one it cannot read
/// past (an item particle's stack), so the caller stops there.
pub fn skip_particle(r: &mut In) -> Option<()> {
    match r.varint()? {
        // block, block_marker, falling_dust, dust_pillar, block_crumble: a block state.
        1 | 2 | 36 | 121 | 125 => {
            r.varint()?;
        }
        // entity_effect, tinted_leaves, flash: an ARGB colour.
        28 | 46 | 52 => {
            r.take(4)?;
        }
        // dust: colour and scale; effect, instant_effect: colour and power.
        21 | 23 | 56 => {
            r.take(8)?;
        }
        // dust_color_transition: two colours and a scale.
        22 => {
            r.take(12)?;
        }
        // dragon_breath, sculk_charge: a float.
        15 | 48 => {
            r.take(4)?;
        }
        // shriek: a delay.
        115 => {
            r.varint()?;
        }
        // trail: a target, a colour, a duration.
        59 => {
            r.take(28)?;
            r.varint()?;
        }
        // vibration: a block or an entity source, then its travel time.
        58 => {
            match r.varint()? {
                0 => {
                    r.take(8)?;
                }
                1 => {
                    r.varint()?;
                    r.take(4)?;
                }
                _ => return None,
            }
            r.varint()?;
        }
        // geyser, geyser_plume: a water count; geyser_base, geyser_poof: and an impulse.
        7 | 10 => {
            r.take(4)?;
        }
        8 | 9 => {
            r.take(8)?;
        }
        // item: an item stack template.
        57 => return None,
        _ => {}
    }
    Some(())
}

/// ClientboundRegistryDataPacket for a variant registry: each entry's name and textures.
pub fn registry_data(id: &str, r: &mut In, v: &mut Variants) {
    if !matches!(
        id,
        "minecraft:cat_variant"
            | "minecraft:cow_variant"
            | "minecraft:wolf_variant"
            | "minecraft:frog_variant"
            | "minecraft:pig_variant"
            | "minecraft:chicken_variant"
            | "minecraft:zombie_nautilus_variant"
    ) {
        return;
    }
    let n = r.varint().unwrap_or(0).max(0) as usize;
    let mut list = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        let Some(name) = r.string() else { break };
        let mut e = Variant { name, ..Variant::default() };
        if r.bool() == Some(true) {
            let mut s = Vec::new();
            let Some(t) = r.u8() else { break };
            if nbt_strings(r, t, "", &mut s, 0).is_none() {
                list.push(e);
                break;
            }
            for (k, val) in s {
                let val = String::from(val.split_once(':').map_or(val.as_str(), |x| x.1));
                match k.as_str() {
                    "asset_id" => e.asset = val,
                    "baby_asset_id" => e.baby_asset = val,
                    "model" => e.model = val,
                    "assets.wild" => e.wolf[0] = val,
                    "assets.tame" => e.wolf[1] = val,
                    "assets.angry" => e.wolf[2] = val,
                    "baby_assets.wild" => e.wolf_baby[0] = val,
                    "baby_assets.tame" => e.wolf_baby[1] = val,
                    "baby_assets.angry" => e.wolf_baby[2] = val,
                    _ => {}
                }
            }
        }
        list.push(e);
    }
    v.regs.insert(String::from(id), list);
}

/// ClientboundPlayerInfoUpdatePacket: the names of the players it adds.
pub fn player_info(r: &mut In, names: &mut BTreeMap<[u8; 16], String>) -> Option<()> {
    let actions = r.u8()?;
    let n = r.varint()?;
    for _ in 0..n.max(0) {
        let uuid = r.uuid()?;
        if actions & 1 != 0 {
            let name = r.string()?;
            names.insert(uuid, name);
            let props = r.varint()?;
            for _ in 0..props.max(0) {
                r.string()?;
                r.string()?;
                if r.bool()? {
                    r.string()?;
                }
            }
        }
        if actions & 2 != 0 && r.bool()? {
            r.take(16 + 8)?;
            let k = r.varint()?;
            r.take(k.max(0) as usize)?;
            let s = r.varint()?;
            r.take(s.max(0) as usize)?;
        }
        if actions & 4 != 0 {
            r.varint()?;
        }
        if actions & 8 != 0 {
            r.bool()?;
        }
        if actions & 16 != 0 {
            r.varint()?;
        }
        if actions & 32 != 0 && r.bool()? {
            crate::nbt::skip_root(r)?;
        }
        if actions & 64 != 0 {
            r.varint()?;
        }
        if actions & 128 != 0 {
            r.bool()?;
        }
    }
    Some(())
}

/// ClientboundPlayerInfoRemovePacket.
pub fn player_info_remove(r: &mut In, names: &mut BTreeMap<[u8; 16], String>) -> Option<()> {
    let n = r.varint()?;
    for _ in 0..n.max(0) {
        names.remove(&r.uuid()?);
    }
    Some(())
}

/// ClientboundSetEquipmentPacket: (slot, stack) pairs, the slot's top bit saying another
/// follows. `slot` reads a stack; it says false when it could not read past one.
pub fn equipment(r: &mut In, e: &mut Entity, slot: fn(&mut In) -> Option<(Option<(i32, i32)>, bool)>) -> Option<()> {
    loop {
        let s = r.u8()?;
        let (stack, ok) = slot(r)?;
        if let Some(q) = e.equipment.get_mut((s & 127) as usize) {
            *q = stack;
        }
        e.version = e.version.wrapping_add(1);
        if !ok || s & 128 == 0 {
            return Some(());
        }
    }
}

/// VillagerProfessionLayer's level badges.
const LEVELS: [&str; 5] = ["stone", "iron", "gold", "emerald", "diamond"];

/// What an entity is drawn as: its body model and texture, then the layers over it, then its
/// armour, as vanilla's renderer for its type chooses them.
pub fn look(t: &EntityTable, e: &Entity, vs: &Variants, game_time: i64) -> Vec<Draw> {
    let mut out: Vec<Draw> = Vec::new();
    let Some(ty) = t.types.get(e.kind as usize) else { return out };
    let short = ty.name.as_str();
    let baby = t.is_baby(e);
    let d = |i: u8| e.d(i);
    let m = |k: &str| t.model(&format!("minecraft:{}", k));
    let tx = |k: &str| t.tex_rect(k);
    // A baby's own texture where vanilla has one ("_baby"), else the adult's.
    let tb = |k: &str| if baby { tx(&format!("{}_baby", k)).or_else(|| tx(k)) } else { tx(k) };
    // A baby's own model layer where there is one.
    let mb = |k: &str, layer: &str| {
        if baby {
            m(&format!("{}_baby#{}", k, layer)).or_else(|| m(&format!("{}#{}", k, layer)))
        } else {
            m(&format!("{}#{}", k, layer))
        }
    };
    let push = |out: &mut Vec<Draw>, model: Option<u16>, rect: Option<[u16; 4]>, tint: u32| {
        if let (Some(model), Some(rect)) = (model, rect) {
            out.push(Draw { model, rect, tint, worn: false });
        }
    };
    let white = 0xFFFFFF;
    let mut model = mb(short, "main").or((ty.model >= 0).then_some(ty.model as u16));
    let mut rect = if ty.tex.is_empty() { None } else { tb(&ty.tex) }.or((ty.rect[2] > 0).then_some(ty.rect));
    let custom = e.custom_name.as_deref();
    let registry = format!("minecraft:{}_variant", short);
    // An unsent variant is the type's default (Cat: black, Wolf: pale, the others temperate).
    let default = match short {
        "cat" => "minecraft:black",
        "wolf" => "minecraft:pale",
        _ => "minecraft:temperate",
    };
    let variant = vs.get(&registry, e.variant).or_else(|| vs.regs.get(&registry).and_then(|l| l.iter().find(|x| x.name == default)));
    // The layer models of the body model (armour), set below for those that change it.
    match short {
        "player" | "mannequin" => {
            let i = EntityTable::skin_index(&e.uuid);
            if t.skins.len() == 18 {
                let pm = t.player_models[if i < 9 { 0 } else { 1 }];
                model = (pm >= 0).then_some(pm as u16);
                rect = Some(t.skins[i]);
            }
        }
        // The synced variant registries: textures, and the model of the climate.
        "cow" | "pig" | "chicken" | "frog" | "cat" | "zombie_nautilus" => {
            if let Some(v) = variant {
                let a = if baby && !v.baby_asset.is_empty() { &v.baby_asset } else { &v.asset };
                if let Some(r) = tx(a) {
                    rect = Some(r);
                }
                let climate = match (short, v.model.as_str()) {
                    ("zombie_nautilus", "warm") => Some(String::from("zombie_nautilus_coral")),
                    (_, "" | "normal") => None,
                    (s, c) => Some(format!("{}_{}", c, s)),
                };
                if let Some(c) = climate {
                    if let Some(mm) = mb(&c, "main") {
                        model = Some(mm);
                    }
                }
            }
        }
        // WolfRenderer.getTextureLocation: tame, angry or wild, of its variant.
        "wolf" => {
            if let Some(v) = variant {
                let tame = d(18) & 4 != 0;
                let angry = d(22) > game_time;
                let k = if tame { 1 } else if angry { 2 } else { 0 };
                let a = if baby { &v.wolf_baby[k] } else { &v.wolf[k] };
                if let Some(r) = tx(a) {
                    rect = Some(r);
                }
            }
        }
        // Horse.DATA_ID_TYPE_VARIANT: the colour in the low byte.
        "horse" => {
            const COLOURS: [&str; 7] = ["white", "creamy", "chestnut", "brown", "black", "gray", "darkbrown"];
            let c = COLOURS[(d(19) & 0xFF).clamp(0, 6) as usize];
            rect = tb(&format!("entity/horse/horse_{}", c)).or(rect);
        }
        "llama" | "trader_llama" => {
            const COLOURS: [&str; 4] = ["creamy", "white", "brown", "gray"];
            rect = tb(&format!("entity/llama/llama_{}", COLOURS[d(21).clamp(0, 3) as usize])).or(rect);
        }
        "parrot" => {
            const COLOURS: [&str; 5] = ["red_blue", "blue", "green", "yellow_blue", "grey"];
            rect = tx(&format!("entity/parrot/parrot_{}", COLOURS[d(20).clamp(0, 4) as usize])).or(rect);
        }
        // RabbitRenderer: the Toast name tag, else its type (99 the killer bunny).
        "rabbit" => {
            const KINDS: [&str; 6] = ["brown", "white", "black", "white_splotched", "gold", "salt"];
            let k = if custom == Some("Toast") {
                "toast"
            } else if d(18) == 99 {
                "caerbannog"
            } else {
                KINDS[d(18).clamp(0, 5) as usize]
            };
            rect = tb(&format!("entity/rabbit/rabbit_{}", k)).or(rect);
        }
        // FoxRenderer: red or snow, asleep (flag 32), baby.
        "fox" => {
            let snow = if d(18) == 1 { "_snow" } else { "" };
            let sleep = if d(19) & 32 != 0 { "_sleep" } else { "" };
            rect = tb(&format!("entity/fox/fox{}{}", snow, sleep)).or(rect);
        }
        "axolotl" => {
            const KINDS: [&str; 5] = ["lucy", "wild", "gold", "cyan", "blue"];
            rect = tb(&format!("entity/axolotl/axolotl_{}", KINDS[d(18).clamp(0, 4) as usize])).or(rect);
        }
        "mooshroom" => {
            let k = if d(18) == 1 { "brown" } else { "red" };
            rect = tb(&format!("entity/cow/mooshroom_{}", k)).or(rect);
        }
        // Panda.getVariant: the main gene, a recessive one only when both genes are it.
        "panda" => {
            const GENES: [&str; 7] = ["", "lazy_", "worried_", "playful_", "brown_", "weak_", "aggressive_"];
            let (main, hidden) = (d(21).clamp(0, 6), d(22).clamp(0, 6));
            let g = if (main == 4 || main == 5) && main != hidden { 0 } else { main };
            rect = tb(&format!("entity/panda/{}panda", GENES[g as usize])).or(rect);
        }
        "sniffer" if baby => rect = tx("entity/sniffer/snifflet").or(rect),
        // BeeRenderer: angry (its anger end ahead), carrying nectar (flag 8).
        "bee" => {
            let angry = if d(19) > game_time { "_angry" } else { "" };
            let nectar = if d(18) & 8 != 0 { "_nectar" } else { "" };
            rect = tb(&format!("entity/bee/bee{}{}", angry, nectar)).or(rect);
        }
        // StriderRenderer: cold out of lava (DATA_SUFFOCATING).
        "strider" if d(19) != 0 => rect = tb("entity/strider/strider_cold").or(rect),
        // GhastRenderer: shooting (DATA_IS_CHARGING), VexRenderer: charging (flag 1).
        "ghast" if d(16) != 0 => rect = tx("entity/ghast/ghast_shooting").or(rect),
        "vex" if d(16) & 1 != 0 => rect = tx("entity/illager/vex_charging").or(rect),
        // CopperGolemRenderer: its weathering.
        "copper_golem" => {
            const STATES: [&str; 4] = ["", "_exposed", "_weathered", "_oxidized"];
            rect = tx(&format!("entity/copper_golem/copper_golem{}", STATES[d(16).clamp(0, 3) as usize])).or(rect);
        }
        "salmon" => {
            model = match d(17) {
                0 => m("salmon_small#main"),
                2 => m("salmon_large#main"),
                _ => model,
            }
        }
        "pufferfish" => {
            model = m(["pufferfish_small#main", "pufferfish_medium#main", "pufferfish_big#main"][d(17).clamp(0, 2) as usize]);
            rect = tx("entity/fish/pufferfish");
        }
        // TropicalFish: the shape and its base colour, then the pattern in its colour.
        "tropical_fish" => {
            let packed = d(17);
            let large = packed & 0xFF == 1;
            let (base, pattern) = (((packed >> 16) & 15) as usize, ((packed >> 24) & 15) as usize);
            let (shape, letter) = if large { ("tropical_fish_large", "b") } else { ("tropical_fish_small", "a") };
            push(&mut out, m(&format!("{}#main", shape)), tx(&format!("entity/fish/tropical_{}", letter)), DYE[base]);
            let n = ((packed >> 8) & 0xFF).clamp(0, 5) + 1;
            push(&mut out, m(&format!("{}#pattern", shape)), tx(&format!("entity/fish/tropical_{}_pattern_{}", letter, n)), DYE[pattern]);
            return out;
        }
        "camel_husk" => {
            model = m("camel#main");
            rect = tx("entity/camel/camel_husk");
        }
        _ => {
            // Boats and rafts: their wood's model and texture.
            if let Some(wood) = short.strip_suffix("_chest_boat").or_else(|| short.strip_suffix("_chest_raft")) {
                model = m(&format!("chest_boat/{}#main", wood));
                rect = tx(&format!("entity/chest_boat/{}", wood));
            } else if let Some(wood) = short.strip_suffix("_boat").or_else(|| short.strip_suffix("_raft")) {
                model = m(&format!("boat/{}#main", wood));
                rect = tx(&format!("entity/boat/{}", wood));
            }
        }
    }
    push(&mut out, model, rect, white);
    let body_model = model;
    // ---- render layers
    let collar = |out: &mut Vec<Draw>, layer: &str, tex: &str, colour: i64| {
        let c = if (0..16).contains(&colour) { DYE[colour as usize] } else { DYE[14] };
        push(out, mb(short, layer), tb(tex), c);
    };
    match short {
        // SheepWoolUndercoatLayer (a coloured or jeb_ sheep's, sheared or not), SheepWoolLayer.
        "sheep" => {
            let wool = d(18);
            let c = DYE[(wool & 15) as usize];
            if !baby && wool & 15 != 0 {
                push(&mut out, m("sheep#wool_undercoat"), tx("entity/sheep/sheep_wool_undercoat"), c);
            }
            if wool & 0x10 == 0 {
                push(&mut out, mb("sheep", "wool"), tb("entity/sheep/sheep_wool"), c);
            }
        }
        "wolf" if d(18) & 4 != 0 => collar(&mut out, "main", "entity/wolf/wolf_collar", if e.data.contains_key(&21) { d(21) } else { 14 }),
        "cat" if d(18) & 4 != 0 => collar(&mut out, "collar", "entity/cat/cat_collar", if e.data.contains_key(&23) { d(23) } else { 14 }),
        // HorseMarkingLayer.
        "horse" => {
            const MARKS: [&str; 5] = ["", "white", "whitefield", "whitedots", "blackdots"];
            let k = ((d(19) >> 8) & 0xFF).clamp(0, 4) as usize;
            if k > 0 {
                push(&mut out, body_model, tb(&format!("entity/horse/horse_markings_{}", MARKS[k])), white);
            }
        }
        // VillagerProfessionLayer: the biome's clothes, the profession's (not a baby's), its level.
        "villager" | "zombie_villager" => {
            let folder = format!("entity/{}", short);
            let vt = t.villager_types.get(e.villager[0] as usize).map_or("plains", |s| s.as_str());
            let prof = t.villager_professions.get(e.villager[1] as usize).map_or("none", |s| s.as_str());
            // The type's clothes without the hat under a profession's full hat (or its partial
            // one over a type's full hat): the no_hat model.
            let hat = |k: usize, i: i32| t.villager_hats[k].get(i as usize).copied().unwrap_or(0);
            let (th, ph) = (hat(0, e.villager[0]), hat(1, e.villager[1]));
            let hat_shown = ph == 0 || (ph == 1 && th != 2);
            let type_model = if hat_shown { body_model } else { mb(&format!("{}_no_hat", short), "main").or(body_model) };
            push(&mut out, type_model, tx(&format!("{}/{}/{}", folder, if baby { "baby" } else { "type" }, vt)), white);
            if !baby && prof != "none" {
                push(&mut out, body_model, tx(&format!("{}/profession/{}", folder, prof)), white);
                if prof != "nitwit" {
                    let lv = LEVELS[(e.villager[2].max(1) as usize - 1).min(4)];
                    push(&mut out, body_model, tx(&format!("{}/profession_level/{}", folder, lv)), white);
                }
            }
        }
        // The eyes render type.
        "spider" | "cave_spider" => push(&mut out, body_model, tx("entity/spider/spider_eyes"), white),
        "enderman" => push(&mut out, body_model, tx("entity/enderman/enderman_eyes"), white),
        "phantom" => push(&mut out, body_model, tx("entity/phantom/phantom_eyes"), white),
        // The outer layers.
        "drowned" => push(&mut out, mb("drowned", "outer"), tb("entity/zombie/drowned_outer_layer"), white),
        "stray" => push(&mut out, m("stray#outer"), tx("entity/skeleton/stray_overlay"), white),
        "bogged" if d(16) == 0 => push(&mut out, m("bogged#outer"), tx("entity/skeleton/bogged_overlay"), white),
        "slime" => push(&mut out, m("slime#outer"), tx("entity/slime/slime"), white),
        // IronGolemCrackinessLayer: cracks below 75%, 50% and 25% of its 100 health.
        "iron_golem" => {
            let f = e.health.unwrap_or(100.0) / 100.0;
            let level = if f < 0.25 { "high" } else if f < 0.5 { "medium" } else if f < 0.75 { "low" } else { "" };
            if !level.is_empty() {
                push(&mut out, body_model, tx(&format!("entity/iron_golem/iron_golem_crackiness_{}", level)), white);
            }
        }
        "copper_golem" => {
            const STATES: [&str; 4] = ["", "_exposed", "_weathered", "_oxidized"];
            push(&mut out, body_model, tx(&format!("entity/copper_golem/copper_golem_eyes{}", STATES[d(16).clamp(0, 3) as usize])), white);
        }
        _ => {}
    }
    // ---- equipment: the body slot (horse armour, wolf armour, llama carpets), the saddle
    let equip = |slot: usize| -> Option<&crate::entities::EquipAsset> {
        let (item, _) = e.equipment[slot]?;
        let &(_, asset) = t.item_equip.get(item as usize)?;
        t.equip_assets.get(asset as usize)
    };
    let layers_of = |a: &crate::entities::EquipAsset, ltype: &str| -> Vec<([u16; 4], u32)> {
        a.layers.iter().find(|(l, _)| l == ltype).map_or(Vec::new(), |(_, v)| v.clone())
    };
    let wear = |out: &mut Vec<Draw>, model: Option<u16>, texs: &[([u16; 4], u32)]| {
        if let Some(model) = model {
            for &(rect, undyed) in texs {
                let tint = if undyed != 0 { undyed & 0xFFFFFF } else { white };
                out.push(Draw { model, rect, tint, worn: true });
            }
        }
    };
    if !baby {
        let body = equip(6);
        match short {
            "horse" | "zombie_horse" | "skeleton_horse" => {
                if let Some(a) = body {
                    let mm = if short == "horse" { m("horse_armor#main") } else { m("undead_horse_armor#main") };
                    wear(&mut out, mm, &layers_of(a, "horse_body"));
                }
            }
            "wolf" => {
                if let Some(a) = body {
                    wear(&mut out, m("wolf_armor#main"), &layers_of(a, "wolf_body"));
                }
            }
            "nautilus" | "zombie_nautilus" => {
                if let Some(a) = body {
                    wear(&mut out, m("nautilus_armor#main"), &layers_of(a, "nautilus_body"));
                }
            }
            "happy_ghast" => {
                if let Some(a) = body {
                    wear(&mut out, m("happy_ghast_harness#main"), &layers_of(a, "happy_ghast_body"));
                }
            }
            _ => {}
        }
        if let Some(a) = equip(7) {
            wear(&mut out, m(&format!("{}#saddle", short)), &layers_of(a, &format!("{}_saddle", short)));
        }
    }
    // LlamaDecorLayer: the carpet, or the trader's blanket.
    if short == "llama" || short == "trader_llama" {
        let decor = mb("llama", "decor");
        match equip(6) {
            Some(a) => wear(&mut out, decor, &layers_of(a, "llama_body")),
            None if short == "trader_llama" => push(&mut out, decor, tb("entity/equipment/llama_body/trader_llama"), white),
            None => {}
        }
    }
    // ---- HumanoidArmorLayer: the armour models of the body model.
    if let Some(base) = body_model.and_then(|i| t.models.get(i as usize)).and_then(|mm| mm.name.strip_suffix("#main")) {
        for (slot, layer) in [(5usize, "helmet"), (4, "chestplate"), (3, "leggings"), (2, "feet")] {
            let Some(a) = equip(slot) else { continue };
            let layer = if layer == "feet" { "boots" } else { layer };
            let Some(mm) = t.model(&format!("{}#{}", base, layer)) else { continue };
            let ltype = if baby { "humanoid_baby" } else if slot == 3 { "humanoid_leggings" } else { "humanoid" };
            wear(&mut out, Some(mm), &layers_of(a, ltype));
        }
    }
    out
}

/// Checks of the looks above against entities.bin, without a server.
pub fn selftest_checks(t: &EntityTable) -> Vec<(&'static str, String, &'static str)> {
    let mut v = Vec::new();
    let kind = |n: &str| t.types.iter().position(|ty| ty.name == n).map_or(-1, |i| i as i32);
    let names = |e: &Entity, vs: &Variants| -> String {
        let l = look(t, e, vs, 0);
        let tex = |r: [u16; 4]| t.textures.iter().find(|(_, &x)| x == r).map_or(String::from("?"), |(k, _)| k.clone());
        l.iter().map(|d| format!("{}={}", t.models[d.model as usize].name.trim_start_matches("minecraft:"), tex(d.rect))).collect::<Vec<_>>().join(" ")
    };
    let mut vs = Variants::default();
    // A registry as a server sends it: the entries' NBT with their asset ids.
    let mut nbt = Vec::new();
    let entry = |nbt: &mut Vec<u8>, name: &str, fields: &[(&str, &str)]| {
        let s = |b: &mut Vec<u8>, x: &str| {
            b.extend_from_slice(&(x.len() as u16).to_be_bytes());
            b.extend_from_slice(x.as_bytes());
        };
        nbt.push(name.len() as u8);
        nbt.extend_from_slice(name.as_bytes());
        nbt.push(1);
        nbt.push(10);
        for (k, val) in fields {
            nbt.push(8);
            s(nbt, k);
            s(nbt, val);
        }
        nbt.push(0);
    };
    nbt.push(2);
    entry(&mut nbt, "minecraft:temperate", &[("asset_id", "minecraft:entity/cow/cow_temperate"), ("baby_asset_id", "minecraft:entity/cow/cow_temperate_baby"), ("model", "normal")]);
    entry(&mut nbt, "minecraft:warm", &[("asset_id", "minecraft:entity/cow/cow_warm"), ("baby_asset_id", "minecraft:entity/cow/cow_warm_baby"), ("model", "warm")]);
    registry_data("minecraft:cow_variant", &mut In::new(&nbt), &mut vs);
    let mut cow = Entity::new(kind("cow"), [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    cow.variant = 1;
    v.push(("warm cow", names(&cow, &vs), "warm_cow#main=entity/cow/cow_warm"));
    cow.data.insert(16, 1);
    v.push(("baby warm cow", names(&cow, &vs), "warm_cow_baby#main=entity/cow/cow_warm_baby"));
    v.push(("baby cow size", format!("{:?}", t.dims(&cow)), "(0.45, 0.7)"));
    let mut sheep = Entity::new(kind("sheep"), [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    sheep.data.insert(18, 14);
    v.push((
        "red sheep",
        names(&sheep, &vs),
        "sheep#main=entity/sheep/sheep sheep#wool_undercoat=entity/sheep/sheep_wool_undercoat sheep#wool=entity/sheep/sheep_wool",
    ));
    let mut zombie = Entity::new(kind("zombie"), [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let item = |n: &str| t.item_equip.iter().enumerate().find(|(_, (s, a))| *s == 5 && t.equip_assets.get(*a as usize).map_or(false, |x| x.name == n)).map(|(i, _)| i as i32);
    zombie.equipment[5] = item("iron").map(|i| (i, 1));
    zombie.data.insert(16, 1);
    v.push(("baby zombie, iron helmet", names(&zombie, &vs), "zombie_baby#main=entity/zombie/zombie_baby zombie_baby#helmet=entity/equipment/humanoid_baby/iron"));
    let mut villager = Entity::new(kind("villager"), [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let idx = |l: &Vec<String>, n: &str| l.iter().position(|x| x == n).unwrap_or(0) as i32;
    villager.villager = [idx(&t.villager_types, "desert"), idx(&t.villager_professions, "librarian"), 3];
    v.push((
        "desert librarian",
        names(&villager, &vs),
        "villager#main=entity/villager/villager villager_no_hat#main=entity/villager/type/desert villager#main=entity/villager/profession/librarian villager#main=entity/villager/profession_level/gold",
    ));
    // LivingEntity.DATA_EFFECT_PARTICLES: an entity_effect (a colour), a dust (colour and
    // scale) and a trail are read past, so the values after them (a baby flag) are kept.
    let mut b = alloc::vec![28u8, 1, 2, 3, 4, 21];
    b.extend_from_slice(&[0; 8]);
    b.push(59);
    b.extend_from_slice(&[0; 28]);
    b.extend_from_slice(&[40, 0xAB]);
    let mut r = In::new(&b);
    let ok = (0..3).all(|_| skip_particle(&mut r).is_some());
    v.push(("effect particles read past", format!("{} {:?}", ok, r.rest()), "true [171]"));
    let mut horse = Entity::new(kind("horse"), [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    horse.data.insert(19, 4 | 3 << 8);
    v.push(("black horse, white dots", names(&horse, &vs), "horse#main=entity/horse/horse_black horse#main=entity/horse/horse_markings_whitedots"));
    v
}
