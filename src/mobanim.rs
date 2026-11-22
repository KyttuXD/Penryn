//! The model poses beyond the walk cycle, as vanilla's models set them (entities.rs calls
//! `walk`, `override_arm` and `post` for each part of a model, in that order):
//! - the arms: players' ArmPose (items, bows, crossbows, shields: HumanoidModel.poseRightArm
//!   and AnimationUtils.animateCrossbowHold/Charge), skeletons' aggressive arms and bows
//!   (SkeletonModel.setupAnim), zombies' (AnimationUtils.animateZombieArms), piglins' weapons
//!   and crossbows (PiglinModel.setupAnim), the arm bob every humanoid has (bobArms), and the
//!   swimming arms and legs (HumanoidModel.setupSwimAnimation);
//! - the legs: quadrupeds (QuadrupedModel), the spider's eight (SpiderModel), a villager's
//!   half-speed legs (VillagerModel), a chicken's and the elytra's wings (ChickenModel,
//!   ElytraModel), a piglin's ears (AbstractPiglinModel).
//! The per-entity state the models read (the chicken's flap, the elytra's ease, the swim
//! amount) is ticked by `tick`, from Entity::tick.

use crate::entities::Entity;
use crate::math;
use alloc::string::String;

const PI: f64 = math::PI;
const FALL_FLYING: i64 = 1;
const SWIMMING: i64 = 3;
const CROUCHING: i64 = 5;

/// The client-side animation state of an entity (vanilla's fields the models read).
#[derive(Clone, Copy)]
pub struct Anim {
    /// ChickenRenderState.flap and flapSpeed (old and current), and Chicken.flapping.
    pub flap: [f32; 2],
    pub flap_speed: [f32; 2],
    pub flapping: f32,
    /// LivingEntity.elytraAnimationState: the wing angles (old and current, x, y, z).
    pub elytra: [[f32; 3]; 2],
    /// LivingEntity.swimAmount (old and current).
    pub swim: [f32; 2],
}

impl Anim {
    pub const fn new() -> Anim {
        Anim { flap: [0.0; 2], flap_speed: [0.0; 2], flapping: 1.0, elytra: [[0.0; 3]; 2], swim: [0.0; 2] }
    }
}

/// ElytraAnimation.RESTING: the wings' angles when not gliding or crouching.
const ELYTRA_RESTING: [f32; 3] = [0.2617994, 0.0, -0.2617994];

/// One tick of the client-side state: Chicken.aiStep's flap, LivingEntity.elytraAnimationState
/// and updateSwimAmount. `on_ground` is judged from the last tick's vertical move.
pub fn tick(e: &mut Entity) {
    let v = [e.pos[0] - e.prev[0], e.pos[1] - e.prev[1], e.pos[2] - e.prev[2]];
    let on_ground = v[1].abs() < 0.001;
    let pose = e.d(6);
    let flying = pose == FALL_FLYING;
    let crouching = pose == CROUCHING;
    let mut a = e.anim;
    // Chicken.aiStep: the wings flap while it is in the air, and settle on the ground.
    a.flap[0] = a.flap[1];
    a.flap_speed[0] = a.flap_speed[1];
    let speed = a.flap_speed[1] + (if on_ground { -1.0 } else { 4.0 }) * 0.3;
    a.flap_speed[1] = speed.clamp(0.0, 1.0);
    if !on_ground && a.flapping < 1.0 {
        a.flapping = 1.0;
    }
    a.flapping *= 0.9;
    a.flap[1] += a.flapping * 2.0;
    // ElytraAnimation.tick: eased to the gliding, crouching or resting angles.
    a.elytra[0] = a.elytra[1];
    let target: [f32; 3] = if flying {
        let len = math::sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
        let ratio = if v[1] < 0.0 && len > 0.0 {
            let x = -v[1] / len;
            1.0 - (x * math::sqrt(x)) as f32
        } else {
            1.0
        };
        let pi = core::f32::consts::PI;
        [pi / 12.0 + (pi / 9.0 - pi / 12.0) * ratio, 0.0, -pi / 12.0 + (-pi / 2.0 + pi / 12.0) * ratio]
    } else if crouching {
        [core::f32::consts::PI * 2.0 / 9.0, 0.08726646, -core::f32::consts::PI / 4.0]
    } else {
        ELYTRA_RESTING
    };
    for i in 0..3 {
        a.elytra[1][i] += (target[i] - a.elytra[1][i]) * 0.3;
    }
    // LivingEntity.updateSwimAmount: 0.09 a tick toward the swimming pose.
    a.swim[0] = a.swim[1];
    a.swim[1] = if pose == SWIMMING { (a.swim[1] + 0.09).min(1.0) } else { (a.swim[1] - 0.09).max(0.0) };
    e.anim = a;
}

/// The arm poses (HumanoidModel.ArmPose) that set an arm's angles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm {
    Empty,
    Item,
    Block,
    Bow,
    Crossbow,
    Charge,
    /// A piglin's melee weapon held up (ATTACKING_WITH_MELEE_WEAPON).
    Weapon,
}

impl Arm {
    /// Two-handed poses (BOW_AND_ARROW, CROSSBOW_*) also set the other arm.
    fn two_handed(self) -> bool {
        matches!(self, Arm::Bow | Arm::Crossbow | Arm::Charge)
    }
}

/// The poses of both arms, by the renderers' rules (AvatarRenderer.getArmPose, the skeleton's
/// bow, the piglin's ArmPose).
#[derive(Clone, Copy)]
pub struct Hands {
    pub right: Arm,
    pub left: Arm,
    pub left_handed: bool,
    /// The main hand holds a bow, and an item at all.
    pub main_bow: bool,
    pub main_item: bool,
}

fn is_avatar(name: &str) -> bool {
    matches!(name, "player" | "mannequin")
}

fn is_skeleton(name: &str) -> bool {
    matches!(name, "skeleton" | "stray" | "bogged" | "wither_skeleton")
}

fn is_zombie(name: &str) -> bool {
    matches!(name, "zombie" | "husk" | "drowned" | "zombie_villager" | "giant" | "zombified_piglin")
}

fn is_piglin(name: &str) -> bool {
    matches!(name, "piglin" | "piglin_brute")
}

/// The piglins' ears (AbstractPiglinModel): the piglins and zombified ones.
fn has_ears(name: &str) -> bool {
    is_piglin(name) || name == "zombified_piglin"
}

/// Humanoid models with arms that bob (HumanoidModel.setupAnim's bobArms).
fn bobs(name: &str) -> bool {
    is_avatar(name) || is_zombie(name) || is_skeleton(name) || is_piglin(name)
}

fn melee(item: &str) -> bool {
    ["_sword", "_axe", "_pickaxe", "_shovel", "_hoe", "mace"].iter().any(|s| item.ends_with(s) || item == *s)
}

/// Whether the entity's left hand is its main one (Mob.isLeftHanded; a player's main hand 0).
pub fn left_handed(name: &str, e: &Entity) -> bool {
    if is_avatar(name) {
        e.data.get(&15) == Some(&0)
    } else {
        e.d(15) & 2 != 0
    }
}

/// The name of an item by its id (blocks.items), "" for none.
fn item_name(items: &[String], id: Option<(i32, i32)>) -> &str {
    match id {
        Some((i, _)) if i >= 0 => items.get(i as usize).map_or("", |s| s.as_str()),
        _ => "",
    }
}

pub fn hands(name: &str, e: &Entity, items: &[String]) -> Hands {
    let lh = left_handed(name, e);
    let main = item_name(items, e.equipment[0]);
    let off = item_name(items, e.equipment[1]);
    let aggressive = e.d(15) & 4 != 0;
    let mut h = Hands { right: Arm::Empty, left: Arm::Empty, left_handed: lh, main_bow: main == "bow", main_item: !main.is_empty() };
    // The arm a pose goes to: the main arm (the left when left-handed) or the off arm.
    let (mut main_pose, mut off_pose) = (Arm::Empty, Arm::Empty);
    if is_avatar(name) {
        // AvatarRenderer.getArmPose: the item in use is the one the living flags name (1 in use,
        // 2 the off hand's).
        let using = e.d(8) & 1 != 0;
        let off_used = e.d(8) & 2 != 0;
        let pose = |s: &str, used: bool| -> Arm {
            if s.is_empty() {
                Arm::Empty
            } else if using && used {
                match s {
                    "bow" => Arm::Bow,
                    "crossbow" => Arm::Charge,
                    "shield" => Arm::Block,
                    _ => Arm::Item,
                }
            } else {
                Arm::Item
            }
        };
        main_pose = pose(main, !off_used);
        off_pose = pose(off, off_used);
        // A two-handed main pose leaves the off arm an item's pose, or none.
        if main_pose.two_handed() {
            off_pose = if off.is_empty() { Arm::Empty } else { Arm::Item };
        }
    } else if is_skeleton(name) {
        // AbstractSkeletonRenderer.getArmPose: the bow aimed at a target.
        if aggressive && main == "bow" {
            main_pose = Arm::Bow;
        }
    } else if is_piglin(name) {
        // AbstractPiglin.getArmPose: a melee weapon up when fighting, a crossbow loaded or
        // charged.
        main_pose = if aggressive && melee(main) {
            Arm::Weapon
        } else if main == "crossbow" && e.d(8) & 1 != 0 {
            Arm::Charge
        } else if aggressive && (main == "crossbow" || off == "crossbow") {
            Arm::Crossbow
        } else {
            Arm::Empty
        };
    }
    if lh {
        h.left = main_pose;
        h.right = off_pose;
    } else {
        h.right = main_pose;
        h.left = off_pose;
    }
    h
}

/// Everything the poses read about one entity this frame.
pub struct Frame<'a> {
    pub name: &'a str,
    pub baby: bool,
    pub hands: Hands,
    pub fly: bool,
    pub crouch: bool,
    /// Angles of the head (radians): its yaw and pitch (-45 degrees gliding).
    pub hx: f64,
    pub hy: f64,
    /// ageInTicks.
    pub age: f64,
    /// WalkAnimationState.position (scaled for babies) and speed (up to 1).
    pub pos: f64,
    pub amount: f64,
    /// The swing's progress, when one is going on.
    pub attack: Option<f64>,
    /// The swinging arm is the right one.
    pub swing_right: bool,
    /// The entity uses an item (a bow drawn, a shield up): its arms are the item's.
    pub using: bool,
    /// Mob flag 4 (Mob.isAggressive).
    pub aggressive: bool,
    /// LivingEntity.getSwimAmount.
    pub swim: f64,
    /// The elytra wing angles this frame (x, y, z) and the chicken's flap angle.
    pub elytra: [f64; 3],
    pub wing: f64,
}

impl<'a> Frame<'a> {
    pub fn new(name: &'a str, e: &Entity, partial: f32, items: &[String], baby: bool, pos: f64, amount: f64, head: (f64, f64), attack: Option<f64>) -> Frame<'a> {
        let hands = hands(name, e, items);
        let fly = e.d(6) == FALL_FLYING;
        let lerp = |p: [f32; 2]| p[0] + (p[1] - p[0]) * partial;
        let flap = lerp(e.anim.flap) as f64;
        let speed = lerp(e.anim.flap_speed) as f64;
        let mut elytra = [0.0; 3];
        for (i, out) in elytra.iter_mut().enumerate() {
            let (o, n) = (e.anim.elytra[0][i], e.anim.elytra[1][i]);
            *out = (o + (n - o) * partial) as f64;
        }
        let swing_right = attack.is_some() && (e.swing_off == hands.left_handed);
        Frame {
            name,
            baby,
            hands,
            fly,
            crouch: e.d(6) == CROUCHING,
            hx: if fly { -PI / 4.0 } else { head.1 },
            hy: head.0,
            age: e.age as f64 + partial as f64,
            pos,
            amount,
            attack,
            swing_right,
            using: is_avatar(name) && e.d(8) & 1 != 0,
            aggressive: e.d(15) & 4 != 0,
            swim: if is_avatar(name) { (e.anim.swim[0] + (e.anim.swim[1] - e.anim.swim[0]) * partial) as f64 } else { 0.0 },
            elytra,
            wing: (math::sin(flap) + 1.0) * speed,
        }
    }

    /// The arm that holds the main hand's pose.
    fn main_arm(&self) -> Arm {
        if self.hands.left_handed { self.hands.left } else { self.hands.right }
    }

    /// A piglin's swing with its weapon (PiglinModel.setupAttackAnimation's swingWeaponDown,
    /// which runs before the humanoid attack in every case, so the body still turns).
    pub fn weapon_swing(&self) -> bool {
        is_piglin(self.name) && self.attack.is_some() && self.main_arm() == Arm::Weapon
    }
}

/// sin and cos of a swing angle, as the walk's cycle (`position * 0.6662`) reads them.
fn cos_s(s: f64, shift: f64) -> f64 {
    math::cos(s + shift)
}

/// AnimationUtils.animateCrossbowHold: the holding arm at the head's aim, the other across.
fn crossbow_hold(f: &Frame, right: bool, turn: &mut [f64; 3], this_right: bool) {
    let holding_right = right;
    let (hy, hx) = (f.hy, f.hx);
    if this_right == holding_right {
        turn[1] = (if holding_right { -0.3 } else { 0.3 }) as f64 + hy;
        turn[0] = -PI / 2.0 + hx + 0.1;
    } else {
        turn[1] = (if holding_right { 0.6 } else { -0.6 }) as f64 + hy;
        turn[0] = -1.5 + hx;
    }
}

/// AnimationUtils.animateCrossbowCharge: the holding arm levelled, the other drawn back. The
/// draw's time is not known here, so it sits half drawn.
fn crossbow_charge(right: bool, this_right: bool, turn: &mut [f64; 3]) {
    let alpha = 0.5;
    let holding_xr = -0.97079635;
    if this_right == right {
        turn[1] = if right { -0.8 } else { 0.8 };
        turn[0] = holding_xr;
    } else {
        let yr = 0.4 + (0.85 - 0.4) * alpha;
        turn[1] = yr * if right { 1.0 } else { -1.0 };
        turn[0] = holding_xr + (-PI / 2.0 - holding_xr) * alpha;
    }
}

/// HumanoidModel.poseRightArm / poseLeftArm for one arm: the pose of the arm itself, or of the
/// other arm when that one is two-handed (it sets both).
fn arm_pose(f: &Frame, right: bool, turn: &mut [f64; 3]) {
    let (own, other) = if right { (f.hands.right, f.hands.left) } else { (f.hands.left, f.hands.right) };
    let (pose, from_right) = if other.two_handed() {
        (other, !right)
    } else {
        (own, right)
    };
    let hx = f.hx;
    let hy = f.hy;
    match pose {
        Arm::Item if !other.two_handed() => {
            turn[0] = turn[0] * 0.5 - PI / 10.0;
            turn[1] = 0.0;
        }
        Arm::Block if !other.two_handed() => {
            // poseBlockingArm: lifted across the face.
            turn[0] = turn[0] * 0.5 - 0.9424779 + hx.clamp(-4.0 * PI / 9.0, 0.43633232);
            turn[1] = (if right { -30.0 } else { 30.0 }) * PI / 180.0 + hy.clamp(-PI / 6.0, PI / 6.0);
        }
        Arm::Bow => {
            // Both arms at the aim: the main arm's yaw is -0.1, the other's 0.1 and 0.4 apart.
            let main_right = from_right;
            turn[1] = if main_right {
                if right { -0.1 + hy } else { 0.1 + hy + 0.4 }
            } else if right {
                -0.1 + hy - 0.4
            } else {
                0.1 + hy
            };
            turn[0] = -PI / 2.0 + hx;
        }
        Arm::Crossbow => crossbow_hold(f, from_right, turn, right),
        Arm::Charge => crossbow_charge(from_right, right, turn),
        _ => {}
    }
}

/// The walk stage of a part: the limbs' swing, the head's turn, the arm poses, a chicken's
/// wings, the elytra's wings, a piglin's ears, a spider's legs. `pos` is the part's offset
/// (pixels), `turn` its x, y and z turns, both from the model's pose.
pub fn walk(f: &Frame, part: &str, pos: &mut [f64; 3], turn: &mut [f64; 3]) {
    let s = f.pos * 0.6662;
    let a = f.amount;
    match part {
        "head" => {
            turn[1] += f.hy;
            turn[0] += f.hx;
            return;
        }
        "right_arm" | "left_arm" => {
            let right = part == "right_arm";
            // HumanoidModel: the arms swing against the legs, the walk's amount (a zombie's are
            // then set by its override).
            turn[0] += cos_s(s, if right { PI } else { 0.0 }) * a;
            if is_avatar(f.name) || is_skeleton(f.name) || is_piglin(f.name) {
                arm_pose(f, right, turn);
            }
            return;
        }
        _ => {}
    }
    match f.name {
        "spider" | "cave_spider" => {
            // SpiderModel.setupAnim: the legs turn at the sides and step up and down, in
            // pairs a quarter cycle apart.
            let p = f.pos * 0.6662;
            let (sp, sa) = (p * 2.0, a);
            let swing = |shift: f64| -(math::cos(sp + shift) * 0.4) * sa;
            let step = |shift: f64| (math::sin(p + shift) * 0.4).abs() * sa;
            let (hind, mid_hind, mid_front, front) = (swing(0.0), swing(PI), swing(PI / 2.0), swing(3.0 * PI / 2.0));
            let (sh, smh, smf, sf) = (step(0.0), step(PI), step(PI / 2.0), step(3.0 * PI / 2.0));
            let (y, z, sign) = match part {
                "right_hind_leg" => (hind, sh, 1.0),
                "left_hind_leg" => (hind, sh, -1.0),
                "right_middle_hind_leg" => (mid_hind, smh, 1.0),
                "left_middle_hind_leg" => (mid_hind, smh, -1.0),
                "right_middle_front_leg" => (mid_front, smf, 1.0),
                "left_middle_front_leg" => (mid_front, smf, -1.0),
                "right_front_leg" => (front, sf, 1.0),
                "left_front_leg" => (front, sf, -1.0),
                _ => return,
            };
            turn[1] += y * sign;
            turn[2] += z * sign;
            return;
        }
        "chicken" => {
            match part {
                "right_wing" => {
                    turn[2] = f.wing;
                    return;
                }
                "left_wing" => {
                    turn[2] = -f.wing;
                    return;
                }
                _ => {}
            }
        }
        "villager" | "zombie_villager" if part == "right_leg" || part == "left_leg" => {
            // VillagerModel: the legs swing at half the amplitude.
            let shift = if part == "right_leg" { 0.0 } else { PI };
            turn[0] += cos_s(s, shift) * 1.4 * a * 0.5;
            return;
        }
        _ => {}
    }
    match part {
        // The elytra (ElytraModel): wings out from the shoulders, the right the mirror image.
        "left_wing" | "right_wing" if is_avatar(f.name) => {
            let left = part == "left_wing";
            let [x, y, z] = f.elytra;
            let (y, z) = if left { (y, z) } else { (-y, -z) };
            *turn = [x, y, z];
            pos[1] = if f.crouch { 3.0 } else { 0.0 };
            // The layer's translation: 0.125 blocks out.
            pos[2] += 2.0;
        }
        "right_leg" | "left_leg" => {
            let shift = if part == "right_leg" { 0.0 } else { PI };
            turn[0] += cos_s(s, shift) * 1.4 * a;
        }
        // QuadrupedModel: the hind leg on the right and the front on the left swing as the walk.
        "right_hind_leg" | "left_front_leg" => turn[0] += cos_s(s, 0.0) * 1.4 * a,
        "left_hind_leg" | "right_front_leg" => turn[0] += cos_s(s, PI) * 1.4 * a,
        // PiglinModel's ears (AbstractPiglinModel): flapping in the walk's rhythm.
        "right_ear" | "left_ear" if has_ears(f.name) => {
            let default = (if f.baby { 5.0 } else { 30.0 }) * PI / 180.0;
            let freq = f.age * 0.1 + f.pos * 0.5;
            let amp = 0.08 + f.amount * 0.4;
            if part == "left_ear" {
                turn[2] = -default - math::cos(freq * 1.2) * amp;
            } else {
                turn[2] = default + math::cos(freq) * amp;
            }
        }
        _ => {}
    }
}

/// Whether the part's arms are set by the piglin's weapon swing before the attack's own
/// animation (entities.rs applies the override first, then the attack's additive moves).
pub fn swing_before_attack(f: &Frame) -> bool {
    f.weapon_swing()
}

/// The absolute arm angles that override the attack: the zombies' arms
/// (AnimationUtils.animateZombieArms, after the attack), a skeleton's aggressive arms without
/// a bow (SkeletonModel.setupAnim, after the attack), a piglin's weapon swing (swingWeaponDown,
/// before the attack: entities.rs calls this first for it, see `swing_before_attack`).
pub fn override_arm(f: &Frame, part: &str, turn: &mut [f64; 3]) {
    let right = match part {
        "right_arm" => true,
        "left_arm" => false,
        _ => return,
    };
    let a = f.attack.unwrap_or(0.0);
    if is_zombie(f.name) {
        let raise = !f.baby || !f.hands.main_item;
        // animateZombieArms: the arms drop less far when no one is aggressive.
        let drop = if raise { -PI / if f.aggressive { 1.5 } else { 2.25 } } else { 0.0 };
        let sway = if raise { 1.0 } else { -1.0 } * math::sin(a * PI);
        let lift = math::sin((1.0 - (1.0 - a) * (1.0 - a)) * PI);
        let x = drop + sway * 1.2 - lift * 0.4;
        let y = 0.1 - sway * 0.6;
        turn[0] = x;
        turn[1] = if raise == right { -y } else { y };
        turn[2] = 0.0;
    } else if is_skeleton(f.name) && f.aggressive && !f.hands.main_bow {
        // SkeletonModel.setupAnim: both arms out in front, swinging with the attack.
        let s2 = math::sin(a * PI);
        let s = math::sin((1.0 - (1.0 - a) * (1.0 - a)) * PI);
        turn[2] = 0.0;
        turn[1] = if right { -(0.1 - s2 * 0.6) } else { 0.1 - s2 * 0.6 };
        turn[0] = -PI / 2.0 - (s2 * 1.2 - s * 0.4);
    } else if f.weapon_swing() {
        // AnimationUtils.swingWeaponDown: the weapon comes down, the main arm first.
        let s2 = math::sin(a * PI);
        let s = math::sin((1.0 - (1.0 - a) * (1.0 - a)) * PI);
        turn[2] = 0.0;
        turn[1] = if right { PI / 20.0 } else { -PI / 20.0 };
        // The main arm swings 2.2 and the other 1.2; the main arm starts raised, the other low.
        let main_right = !f.hands.left_handed;
        let is_main = right == main_right;
        let raised = -1.8849558 + math::cos(f.age * 0.09) * 0.15;
        let low = math::cos(f.age * 0.19) * 0.5;
        let (base, swing) = if is_main { (raised, 2.2) } else { (low, 1.2) };
        turn[0] = base + s2 * swing - s * 0.4;
        bob(f, right, turn);
    }
}

/// AnimationUtils.bobModelPart: the arm's idle sway (the left arm's the mirror image).
fn bob(f: &Frame, right: bool, turn: &mut [f64; 3]) {
    let scale = if right { 1.0 } else { -1.0 };
    turn[2] += scale * (math::cos(f.age * 0.09) * 0.05 + 0.05);
    turn[0] += scale * math::sin(f.age * 0.067) * 0.05;
}

/// The stage after the riding and crouching poses: the arm bob, a piglin's weapon held up, and
/// a swimmer's arms, legs and head (HumanoidModel.setupSwimAnimation).
pub fn post(f: &Frame, part: &str, turn: &mut [f64; 3]) {
    let right = part == "right_arm";
    if (part == "right_arm" || part == "left_arm") && bobs(f.name) {
        bob(f, right, turn);
        // PiglinModel.holdWeaponHigh: the main arm's raised after the bob.
        if is_piglin(f.name) && !f.weapon_swing() && f.main_arm() == Arm::Weapon && right == !f.hands.left_handed {
            turn[0] = -1.8;
        }
    }
    if is_avatar(f.name) && f.swim > 0.0 {
        swim(f, part, turn);
    }
}

/// HumanoidModel.setupSwimAnimation: the arms reach out and stroke, the legs kick, the head
/// lies to the water's pitch. `f.swim` is the swim amount.
fn swim(f: &Frame, part: &str, turn: &mut [f64; 3]) {
    let amt = f.swim;
    let lerp = |from: f64, to: f64, t: f64| from + (to - from) * t;
    if part == "head" {
        if !f.fly {
            turn[0] = lerp(turn[0], -PI / 4.0, amt);
        }
        return;
    }
    let sp = f.pos - 26.0 * math::floor(f.pos / 26.0);
    let quad = |x: f64| -65.0 * x + x * x;
    match part {
        "right_arm" | "left_arm" => {
            if f.using {
                return;
            }
            let right = part == "right_arm";
            // An arm that swings is not pulled into the stroke.
            let mine = if right { f.attack.is_some() && f.swing_right } else { f.attack.is_some() && !f.swing_right };
            let k = if mine { 0.0 } else { amt };
            if sp < 14.0 {
                turn[0] = lerp(turn[0], 0.0, k);
                turn[1] = lerp(turn[1], PI, k);
                let z = if right { PI - 1.8707964 * quad(sp) / quad(14.0) } else { PI + 1.8707964 * quad(sp) / quad(14.0) };
                turn[2] = lerp(turn[2], z, k);
            } else if sp < 22.0 {
                let t = (sp - 14.0) / 8.0;
                turn[0] = lerp(turn[0], PI / 2.0 * t, k);
                turn[1] = lerp(turn[1], PI, k);
                let z = if right { 1.2707963 + 1.8707964 * t } else { 5.012389 - 1.8707964 * t };
                turn[2] = lerp(turn[2], z, k);
            } else if sp < 26.0 {
                let t = (sp - 22.0) / 4.0;
                turn[0] = lerp(turn[0], PI / 2.0 - PI / 2.0 * t, k);
                turn[1] = lerp(turn[1], PI, k);
                turn[2] = lerp(turn[2], PI, k);
            }
        }
        "right_leg" => turn[0] = lerp(turn[0], 0.3 * math::cos(f.pos / 3.0), amt),
        "left_leg" => turn[0] = lerp(turn[0], 0.3 * math::cos(f.pos / 3.0 + PI), amt),
        _ => {}
    }
}

/// Checks of the poses above, against the vanilla maths.
pub fn selftest() -> alloc::vec::Vec<(&'static str, alloc::string::String, &'static str)> {
    use alloc::format;
    let mut v = alloc::vec::Vec::new();
    let items: alloc::vec::Vec<alloc::string::String> = ["stone", "bow", "iron_sword", "crossbow", "shield"].iter().map(|s| alloc::string::String::from(*s)).collect();
    // A skeleton aiming a bow: both arms at the head's aim (-90 degrees and the yaw's 0.1 apart).
    let mut sk = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    sk.equipment[0] = Some((1, 1));
    sk.data.insert(15, 4);
    let h = hands("skeleton", &sk, &items);
    let f = Frame::new("skeleton", &sk, 0.0, &items, false, 0.0, 0.0, (0.0, 0.0), None);
    let (mut p, mut r) = ([0.0; 3], [0.0; 3]);
    walk(&f, "right_arm", &mut p, &mut r);
    v.push(("skeleton bow arm", format!("{:.4} {:.4}", r[0], r[1]), "-1.5708 -0.1000"));
    let mut r = [0.0; 3];
    walk(&f, "left_arm", &mut p, &mut r);
    v.push(("skeleton bow across", format!("{:.4} {:.4}", r[0], r[1]), "-1.5708 0.5000"));
    v.push(("skeleton holds the bow", format!("{}", h.main_bow), "true"));
    // A zombie with its arms out (not aggressive, a swing's zero): -PI/2.25 and the 0.1 yaw.
    let mut z = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let zf = Frame::new("zombie", &z, 0.0, &items, false, 0.0, 0.0, (0.0, 0.0), None);
    let mut r = [0.0; 3];
    override_arm(&zf, "right_arm", &mut r);
    v.push(("zombie arm drop", format!("{:.4} {:.4}", r[0], r[1]), "-1.3963 -0.1000"));
    z.data.insert(15, 4);
    let zf = Frame::new("zombie", &z, 0.0, &items, false, 0.0, 0.0, (0.0, 0.0), None);
    let mut r = [0.0; 3];
    override_arm(&zf, "left_arm", &mut r);
    v.push(("aggressive zombie arm", format!("{:.4} {:.4}", r[0], r[1]), "-2.0944 0.1000"));
    // A piglin swinging its sword: swingWeaponDown sets the arms before the humanoid attack.
    let mut pg = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    pg.equipment[0] = Some((2, 1));
    pg.data.insert(15, 4);
    let pf = Frame::new("piglin", &pg, 0.0, &items, false, 0.0, 0.0, (0.0, 0.0), Some(0.0));
    v.push(("piglin weapon swing first", format!("{}", swing_before_attack(&pf)), "true"));
    // A spider's legs at rest of the cycle: the hind leg turns out by 0.4 at full speed.
    let sp = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    let spf = Frame::new("spider", &sp, 0.0, &items, false, 0.0, 1.0, (0.0, 0.0), None);
    let (mut p, mut r) = ([0.0; 3], [0.0; 3]);
    walk(&spf, "right_hind_leg", &mut p, &mut r);
    v.push(("spider hind leg turns", format!("{:.4}", r[1]), "-0.4000"));
    // A villager's legs swing at half the amplitude.
    let vf = Frame::new("villager", &sp, 0.0, &items, false, 0.0, 1.0, (0.0, 0.0), None);
    let mut r = [0.0; 3];
    walk(&vf, "right_leg", &mut p, &mut r);
    v.push(("villager leg swing", format!("{:.4}", r[0]), "0.7000"));
    // A chicken on the ground: the flap settles to nothing; in the air the wings spread.
    let mut ch = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    for _ in 0..10 {
        tick(&mut ch);
    }
    v.push(("chicken settles", format!("{:.3}", ch.anim.flap_speed[1]), "0.000"));
    for _ in 0..10 {
        ch.prev = ch.pos;
        ch.pos[1] -= 0.1;
        tick(&mut ch);
    }
    v.push(("chicken flies", format!("{:.3}", ch.anim.flap_speed[1]), "1.000"));
    // The elytra's wings ease to their rest in 30 ticks.
    let mut el = Entity::new(0, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
    for _ in 0..60 {
        tick(&mut el);
    }
    v.push(("elytra at rest", format!("{:.4}", el.anim.elytra[1][0]), "0.2618"));
    v
}
