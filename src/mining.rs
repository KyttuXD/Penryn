//! Block-breaking speed as vanilla works it out (Player.getDestroySpeed and
//! BlockState.getDestroyProgress): the held stack's tool component (its rules, or the default
//! speed), the mining_efficiency, block_break_speed and submerged_mining_speed attributes the
//! server sends, haste, conduit power and mining fatigue, and the air penalty.

use crate::wire::In;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The minecraft:tool data component's type id (26.3).
pub const TOOL: i32 = 28;

/// A rule's blocks: a tag, or registry ids.
#[derive(Clone, Debug, PartialEq)]
pub enum Set {
    Tag(String),
    Ids(Vec<i32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub blocks: Set,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub rules: Vec<Rule>,
    pub default_speed: f32,
}

/// What a stack's patch says about its tool component.
#[derive(Debug, PartialEq)]
pub enum Patch {
    /// Not in the patch: the item's own default applies.
    Default,
    /// Removed by the patch: not a tool.
    Removed,
    Tool(Tool),
}

fn read_tool(r: &mut In) -> Option<Tool> {
    let n = r.varint()?;
    let mut rules = Vec::new();
    for _ in 0..n.clamp(0, 256) {
        let blocks = match r.varint()? {
            0 => Set::Tag(r.string()?),
            k if k > 0 => {
                if (k - 1) as usize > r.left() {
                    return None;
                }
                let mut v = Vec::new();
                for _ in 0..k - 1 {
                    v.push(r.varint()?);
                }
                Set::Ids(v)
            }
            _ => return None,
        };
        let speed = if r.bool()? { Some(r.f32()?) } else { None };
        let correct_for_drops = if r.bool()? { Some(r.bool()?) } else { None };
        rules.push(Rule { blocks, speed, correct_for_drops });
    }
    let default_speed = r.f32()?;
    Some(Tool { rules, default_speed })
}

/// The tool component in a stack's patch (the delimited form creative.rs keeps).
pub fn from_patch(patch: &[u8]) -> Patch {
    let mut r = In::new(patch);
    let (Some(add), Some(remove)) = (r.varint(), r.varint()) else { return Patch::Default };
    let mut found = Patch::Default;
    for _ in 0..add.clamp(0, 256) {
        let (Some(kind), Some(len)) = (r.varint(), r.varint()) else { return found };
        let Some(v) = r.take(len.max(0) as usize) else { return found };
        if kind == TOOL {
            if let Some(t) = read_tool(&mut In::new(v)) {
                found = Patch::Tool(t);
            }
        }
    }
    for _ in 0..remove.clamp(0, 256) {
        match r.varint() {
            Some(k) if k == TOOL => return Patch::Removed,
            Some(_) => {}
            None => break,
        }
    }
    found
}

fn in_set(s: &Set, block: i32, tags: &BTreeMap<String, Vec<i32>>) -> bool {
    match s {
        Set::Tag(t) => tags.get(t.trim_start_matches('#')).map_or(false, |v| v.contains(&block)),
        Set::Ids(v) => v.contains(&block),
    }
}

impl Tool {
    /// Tool.getMiningSpeed: the first matching rule with a speed, else the default.
    pub fn speed(&self, block: i32, tags: &BTreeMap<String, Vec<i32>>) -> f64 {
        for r in &self.rules {
            if let Some(s) = r.speed {
                if in_set(&r.blocks, block, tags) {
                    return s as f64;
                }
            }
        }
        self.default_speed as f64
    }

    /// Tool.isCorrectForDrops: the first matching rule that says.
    pub fn correct_for_drops(&self, block: i32, tags: &BTreeMap<String, Vec<i32>>) -> bool {
        for r in &self.rules {
            if let Some(c) = r.correct_for_drops {
                if in_set(&r.blocks, block, tags) {
                    return c;
                }
            }
        }
        false
    }
}

/// Everything Player.getDestroySpeed reads besides the block.
pub struct Inputs {
    /// ItemStack.getDestroySpeed of the held stack for the block.
    pub tool: f64,
    /// The player's mining_efficiency attribute (default 0).
    pub efficiency: f64,
    /// block_break_speed (default 1).
    pub break_speed: f64,
    /// submerged_mining_speed (default 0.2), used with the eyes in water.
    pub submerged: f64,
    pub haste: Option<i32>,
    pub conduit: Option<i32>,
    pub fatigue: Option<i32>,
    pub eye_in_water: bool,
    pub on_ground: bool,
}

/// Player.getDestroySpeed.
pub fn destroy_speed(i: &Inputs) -> f64 {
    let mut s = i.tool;
    if s > 1.0 {
        s += i.efficiency;
    }
    // MobEffectUtil.getDigSpeedAmplification: the larger of haste and conduit power.
    if i.haste.is_some() || i.conduit.is_some() {
        let amp = i.haste.unwrap_or(0).max(i.conduit.unwrap_or(0));
        s *= 1.0 + (amp + 1) as f64 * 0.2;
    }
    if let Some(a) = i.fatigue {
        // Math.pow(0.3, amplifier + 1): 0.3, 0.09, 0.027, 0.0081 ...
        for _ in 0..(a.clamp(0, 254) + 1) {
            s *= 0.3;
        }
    }
    s *= i.break_speed;
    if i.eye_in_water {
        s *= i.submerged;
    }
    if !i.on_ground {
        s /= 5.0;
    }
    s
}

/// BlockState.getDestroyProgress per tick: speed / hardness / 30 (100 without the right tool).
pub fn progress(speed: f64, hardness: f64, harvest: bool) -> f64 {
    if hardness <= 0.0 {
        return 1.0;
    }
    speed / hardness / if harvest { 30.0 } else { 100.0 }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    use crate::wire::Out;
    let mut out = Vec::new();
    let base = || Inputs { tool: 8.0, efficiency: 0.0, break_speed: 1.0, submerged: 0.2, haste: None, conduit: None, fatigue: None, eye_in_water: false, on_ground: true };
    let p = |i: &Inputs, h: f64, harvest: bool| format!("{:.4}", progress(destroy_speed(i), h, harvest));
    out.push(("mining: diamond pickaxe on stone", p(&base(), 1.5, true), "0.1778"));
    let mut i = base();
    i.efficiency = 26.0; // efficiency V: 5 * 5 + 1
    out.push(("mining: efficiency V", p(&i, 1.5, true), "0.7556"));
    let mut i = base();
    i.tool = 1.0;
    i.efficiency = 26.0; // no bonus for a tool that does not apply
    out.push(("mining: efficiency needs a fast tool", p(&i, 1.5, false), "0.0067"));
    let mut i = base();
    i.haste = Some(1);
    out.push(("mining: haste II", p(&i, 1.5, true), "0.2489"));
    let mut i = base();
    i.haste = Some(0);
    i.conduit = Some(1);
    out.push(("mining: conduit beats haste I", p(&i, 1.5, true), "0.2489"));
    let mut i = base();
    i.fatigue = Some(2);
    out.push(("mining: fatigue III", p(&i, 1.5, true), "0.0048"));
    let mut i = base();
    i.fatigue = Some(3);
    out.push(("mining: fatigue IV", p(&i, 1.5, true), "0.0014"));
    let mut i = base();
    i.eye_in_water = true;
    let a = destroy_speed(&i);
    i.submerged = 1.0; // aqua affinity
    out.push(("mining: water and aqua affinity", format!("{:.2} {:.2}", a, destroy_speed(&i)), "1.60 8.00"));
    let mut i = base();
    i.on_ground = false;
    i.break_speed = 0.5;
    out.push(("mining: air and block_break_speed", format!("{:.2}", destroy_speed(&i)), "0.80"));
    out.push(("mining: no drops divisor, instant", format!("{:.4} {}", progress(8.0, 1.5, false), progress(1.0, 0.0, true)), "0.0533 1"));
    // A tool component in a patch: a tag rule with speed, an id rule that only sets drops.
    let mut c = Out::new();
    c.varint(2);
    c.varint(0).string("minecraft:mineable/pickaxe").bool(true).f32(12.0).bool(true).bool(true);
    c.varint(3).varint(5).varint(6).bool(false).bool(true).bool(false);
    c.f32(1.5).varint(1).bool(true);
    let mut o = Out::new();
    o.varint(1).varint(1).varint(TOOL).varint(c.b.len() as i32);
    o.b.extend_from_slice(&c.b);
    o.varint(4);
    let patch = from_patch(&o.b);
    let mut tags = BTreeMap::new();
    tags.insert(String::from("minecraft:mineable/pickaxe"), alloc::vec![1, 2]);
    let r = match &patch {
        Patch::Tool(t) => format!("{} {} {} {} {}", t.speed(1, &tags), t.speed(9, &tags), t.correct_for_drops(2, &tags), t.correct_for_drops(5, &tags), t.correct_for_drops(9, &tags)),
        other => format!("{:?}", other),
    };
    out.push(("mining: tool component in a patch", r, "12 1.5 true false false"));
    let mut o = Out::new();
    o.varint(0).varint(1).varint(TOOL);
    out.push(("mining: tool component removed", format!("{:?} {:?}", from_patch(&o.b), from_patch(&[0, 0])), "Removed Default"));
    out
}
