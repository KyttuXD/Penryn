//! The packs' item definitions (items.bin, "PNI1", made by make_assets.py from the packs'
//! assets/<ns>/items/*.json). A stack's item_model component names one (ns:path, vanilla's
//! ItemModelResolver: the definition is items/<path>.json of its namespace), and a definition
//! that selects on custom_model_data shows the case of the stack's first string ("ns:path|string").
//! The icons are gui.bin's: a flat layer list, or a block model's quads (blocks.bin's).
//!
//! The stack's components come from its patch (creative::read_stack's delimited form, kept per
//! slot in Game.creative.patches and the containers' patches): item_model is component 10 (an
//! identifier), custom_model_data is 17 (floats, flags, strings, colours).

use crate::gui::Icon;
use crate::wire::In;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// An overlay entry: its key and icon.
pub type Entry = (String, Icon);

/// items.bin's entries (none when the file is missing or damaged).
pub fn parse(d: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    if d.len() < 8 || &d[..4] != crate::account::ITEMS_MAGIC {
        return out;
    }
    let n = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
    let mut p = 8;
    let rd16 = |p: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?])) };
    let rd32 = |p: usize| -> Option<u32> { Some(u32::from_le_bytes([*d.get(p)?, *d.get(p + 1)?, *d.get(p + 2)?, *d.get(p + 3)?])) };
    for _ in 0..n {
        let Some(&kl) = d.get(p) else { break };
        let Some(key) = d.get(p + 1..p + 1 + kl as usize) else { break };
        let key = String::from_utf8_lossy(key).into_owned();
        p += 1 + kl as usize;
        let (Some(&kind), Some(&k)) = (d.get(p), d.get(p + 1)) else { break };
        p += 2;
        let icon = match kind {
            1 => {
                let mut layers = Vec::new();
                for _ in 0..k {
                    let (Some(t), Some(c)) = (rd16(p), rd32(p + 2)) else { break };
                    layers.push((t, c));
                    p += 6;
                }
                Icon::Flat(layers)
            }
            2 => {
                let (Some(first), Some(count), Some(tint)) = (rd32(p), rd16(p + 4), rd32(p + 6)) else { break };
                let f = |i: usize| rd32(p + 10 + 4 * i).map(f32::from_bits).unwrap_or(0.0);
                let icon = Icon::Model { first, count, tint, rot: [f(0), f(1), f(2)], tr: [f(3), f(4), f(5)], sc: [f(6), f(7), f(8)] };
                p += 46;
                icon
            }
            _ => break,
        };
        out.push((key, icon));
    }
    // Sorted by key (a stable sort: the first of equal keys stays first), for find()'s binary
    // search: a pack makes this list tens of thousands long, and every dropped item looks in it
    // every frame.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// items.bin as the game files have it now (the pack stack's copy when it has one).
pub fn read() -> Vec<Entry> {
    crate::sys::read_file(&crate::packs::path("items.bin")).map(|d| parse(&d)).unwrap_or_default()
}

/// The item_model id and the first custom_model_data string of a stack's patch.
pub fn of_patch(patch: &[u8]) -> (Option<String>, Option<String>) {
    let (mut model, mut cmd) = (None, None);
    let mut r = In::new(patch);
    let (Some(add), Some(_)) = (r.varint(), r.varint()) else { return (None, None) };
    for _ in 0..add.max(0) {
        let (Some(kind), Some(len)) = (r.varint(), r.varint()) else { break };
        if len < 0 {
            break;
        }
        let Some(v) = r.take(len as usize) else { break };
        match kind {
            10 => model = In::new(v).string(),
            17 => cmd = first_string(v),
            _ => {}
        }
    }
    (model, cmd)
}

/// CustomModelData's strings: after the floats and the flags, the first one.
fn first_string(v: &[u8]) -> Option<String> {
    let mut r = In::new(v);
    let n = r.varint()?;
    r.take(4usize.checked_mul(n.max(0) as usize)?)?;
    let n = r.varint()?;
    r.take(n.max(0) as usize)?;
    let n = r.varint()?;
    if n <= 0 {
        return None;
    }
    r.string()
}

fn find(list: &[Entry], key: &str) -> Option<usize> {
    // The list is sorted by key (parse): the first entry that is not before `key`.
    let i = list.partition_point(|(k, _)| k.as_str() < key);
    list.get(i).filter(|(k, _)| k == key).map(|_| i)
}

/// The overlay a stack of registry item `registry` (minecraft:x) with `patch` shows: the
/// index into `list`, or None for the game's own icon.
pub fn pick(list: &[Entry], registry: &str, patch: &[u8]) -> Option<usize> {
    if list.is_empty() {
        return None;
    }
    let (model, cmd) = of_patch(patch);
    match model {
        Some(m) => {
            if let Some(s) = cmd {
                if let Some(i) = find(list, &format!("{}|{}", m, s)) {
                    return Some(i);
                }
            }
            find(list, &m)
        }
        // No item_model: the item's own definition (vanilla's default), a case of it first.
        None => match cmd {
            Some(s) => find(list, &format!("{}|{}", registry, s)).or_else(|| find(list, registry)),
            None => find(list, registry),
        },
    }
}

fn describe(i: &Icon) -> String {
    match i {
        Icon::Flat(l) => format!("flat {:?}", l),
        Icon::Model { first, count, tint, .. } => format!("model {} {} {}", first, count, tint),
        Icon::None => String::from("none"),
    }
}

/// Checks of items.bin's records, the patch's model components and the overlay choice.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use crate::wire::Out;
    let mut out = Vec::new();
    // items.bin: a flat icon, then a model (first quad 3, 2 quads, tint 0xFF00FF00).
    let mut d = Vec::new();
    d.extend_from_slice(crate::account::ITEMS_MAGIC);
    d.extend_from_slice(&2u32.to_le_bytes());
    d.push(13);
    d.extend_from_slice(b"ktfp:btn_back");
    d.extend_from_slice(&[1, 1]);
    d.extend_from_slice(&5u16.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.push(3);
    d.extend_from_slice(b"x|a");
    d.extend_from_slice(&[2, 0]);
    d.extend_from_slice(&3u32.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&0xFF00_FF00u32.to_le_bytes());
    for _ in 0..9 {
        d.extend_from_slice(&0f32.to_bits().to_le_bytes());
    }
    let list = parse(&d);
    out.push(("items.bin keys", list.iter().map(|e| e.0.as_str()).collect::<Vec<_>>().join(" "), "ktfp:btn_back x|a"));
    out.push(("items.bin flat icon", list.first().map_or(String::new(), |e| describe(&e.1)), "flat [(5, 0)]"));
    out.push(("items.bin model icon", list.get(1).map_or(String::new(), |e| describe(&e.1)), "model 3 2 4278255360"));
    out.push(("items.bin damaged", format!("{}", parse(&d[..30]).len()), "1"));
    out.push(("items.bin wrong magic", format!("{}", parse(b"PNXX\0\0\0\0").len()), "0"));
    // A patch: item_model "ktfp:btn_back", custom_model_data with strings ["bots"].
    let mut cmd = Out::new();
    cmd.varint(0).varint(0).varint(1).string("bots").varint(0);
    let mut model = Out::new();
    model.string("ktfp:btn_back");
    let mut p = Out::new();
    p.varint(2).varint(0);
    p.varint(10).varint(model.b.len() as i32).bytes(&model.b);
    p.varint(17).varint(cmd.b.len() as i32).bytes(&cmd.b);
    let (m, c) = of_patch(&p.b);
    out.push(("patch item_model", m.unwrap_or_default(), "ktfp:btn_back"));
    out.push(("patch custom_model_data", c.unwrap_or_default(), "bots"));
    out.push(("patch empty", format!("{:?}", of_patch(&[0, 0])), "(None, None)"));
    // The overlay: "ktfp:btn_back|bots" first, then the plain key, then none without components.
    let entries = vec![(String::from("ktfp:btn_back"), Icon::None), (String::from("ktfp:btn_back|bots"), Icon::None)];
    out.push(("overlay with a case", format!("{:?}", pick(&entries, "minecraft:paper", &p.b)), "Some(1)"));
    out.push(("overlay without a case", format!("{:?}", pick(&entries[..1], "minecraft:paper", &p.b)), "Some(0)"));
    out.push(("overlay none", format!("{:?}", pick(&entries, "minecraft:paper", &[0, 0])), "None"));
    // No item_model: the item's plain definition (the fallback of its selects, or a plain model).
    out.push(("overlay default item", format!("{:?}", pick(&entries[..1], "ktfp:btn_back", &[0, 0])), "Some(0)"));
    out
}

/// The icon a stack of registry item `item` shows: its overlay, else the game's icon for the item.
pub fn icon_for(list: &[Entry], icons: &[Icon], blocks: &crate::blocks::Blocks, item: i32, patch: &[u8]) -> Option<Icon> {
    if !list.is_empty() {
        if let Some(short) = blocks.items.get(item as usize) {
            if let Some(k) = pick(list, &format!("minecraft:{}", short), patch) {
                return Some(list[k].1.clone());
            }
        }
    }
    icons.get(item as usize).cloned()
}
