//! The game window: input, the 20 Hz tick (movement, digging, placing), and drawing.
//! Controls are vanilla's defaults (see README.md).

use crate::blocks::{self, Blocks};
use crate::client::{Account, Game, Join};
use crate::keybinds::{Key, B};
use crate::math;
use crate::physics::{self, Input, Mover};
use crate::gui::Gui;
use crate::render::Renderer;
use crate::screens;
use crate::sdl::{self, Video};
use crate::sys;
use crate::say;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// SDL scancodes
const K_A: usize = 4;
const K_Q: usize = 20;
const K_R: usize = 21;
const K_ESC: usize = 41;
const K_BACKSPACE: usize = 42;
const K_F3: usize = 60;
const K_LCTRL: usize = 224;
const K_LSHIFT: usize = 225;
const K_RSHIFT: usize = 229;

/// ServerboundPlayerActionPacket.Action in 26.3 (1 is "change destroy direction").
const RELEASE_USE_ITEM: i32 = 6;
const DIG_START: i32 = 0;
const DIG_ABORT: i32 = 2;
const DIG_STOP: i32 = 3;

/// A block the crosshair is on.
#[derive(Clone, Copy)]
struct Hit {
    pos: [i32; 3],
    /// Direction id: 0 down, 1 up, 2 north, 3 south, 4 west, 5 east.
    face: i32,
    point: [f64; 3],
}

struct Mining {
    pos: [i32; 3],
    progress: f64,
    ticks: u32,
}

#[derive(PartialEq)]
enum Screen {
    Play,
    Chat,
    Pause,
    /// The player's inventory, or a container the server opened.
    Inventory,
    /// The creative inventory (creative.rs).
    Creative,
    Dead,
    /// The sign editor the server opened (signs.rs).
    Sign,
    /// The options screens (options.rs), from the pause menu.
    Options,
    /// The server's links (serverhud.rs), from the pause menu.
    Links,
    /// A book being read or written (books.rs).
    Book,
}

/// Items used over time while the button is held (eaten, drunk, drawn, raised), and for how
/// many ticks (0: until released).
fn use_duration(name: &str) -> Option<u32> {
    const FOOD: &[&str] = &[
        "apple", "bread", "carrot", "potato", "baked_potato", "poisonous_potato", "beetroot", "melon_slice", "cookie",
        "pumpkin_pie", "golden_apple", "enchanted_golden_apple", "golden_carrot", "sweet_berries", "glow_berries",
        "chorus_fruit", "rotten_flesh", "spider_eye", "pufferfish", "tropical_fish", "beef", "porkchop", "chicken",
        "mutton", "rabbit", "cod", "salmon", "cooked_beef", "cooked_porkchop", "cooked_chicken", "cooked_mutton",
        "cooked_rabbit", "cooked_cod", "cooked_salmon", "mushroom_stew", "rabbit_stew", "beetroot_soup", "suspicious_stew",
    ];
    if FOOD.contains(&name) {
        return Some(32);
    }
    match name {
        "dried_kelp" => Some(16),
        "potion" | "milk_bucket" | "honey_bottle" | "ominous_bottle" => Some(32),
        "bow" | "crossbow" | "shield" | "trident" | "spyglass" | "goat_horn" | "brush" => Some(0),
        _ => None,
    }
}

/// Items whose use (Item.use) does something without a block: they end a right click even when
/// they use no time (throwables, buckets, rods, boats, maps, armour that equips, spawn eggs...).
fn use_acts(name: &str) -> bool {
    use_duration(name).is_some()
        || matches!(
            name,
            "ender_pearl" | "snowball" | "egg" | "blue_egg" | "brown_egg" | "wind_charge" | "experience_bottle" | "splash_potion"
                | "lingering_potion" | "fishing_rod" | "ender_eye" | "writable_book" | "written_book" | "filled_map" | "map"
                | "glass_bottle" | "elytra" | "lily_pad" | "frogspawn" | "knowledge_book"
        )
        || name.ends_with("bucket")
        || name.ends_with("_boat")
        || name.ends_with("_raft")
        || name.ends_with("_spawn_egg")
        || ["_helmet", "_chestplate", "_leggings", "_boots"].iter().any(|w| name.ends_with(w))
}

/// What the client predicts Item.useOn does to the block aimed at (a consumed result: no
/// UseItem follows): block items place, tools and utensils act on the blocks they work on.
fn use_on_consumes(block_item: bool, item: &str, block: &str) -> bool {
    let has = |ws: &[&str]| ws.iter().any(|w| block.contains(w));
    if item.ends_with("_axe") {
        return !block.starts_with("stripped_") && has(&["log", "wood", "stem", "hyphae", "copper", "bamboo_block"]);
    }
    if item.ends_with("_shovel") {
        return has(&["grass_block", "dirt", "podzol", "mycelium", "campfire"]);
    }
    if item.ends_with("_hoe") {
        return has(&["grass_block", "dirt", "podzol", "mycelium", "moss_block", "rooted_dirt"]);
    }
    if item.ends_with("minecart") {
        return block.contains("rail");
    }
    if item.ends_with("_dye") || item == "ink_sac" || item == "glow_ink_sac" {
        return block.contains("sign");
    }
    if item.starts_with("music_disc_") {
        return block.contains("jukebox");
    }
    match item {
        "flint_and_steel" | "fire_charge" | "bone_meal" | "armor_stand" | "painting" | "item_frame" | "glow_item_frame" => return true,
        "end_crystal" => return has(&["obsidian", "bedrock"]),
        "lead" => return has(&["fence", "wall"]),
        "ender_eye" => return block.contains("end_portal_frame"),
        "honeycomb" => return has(&["copper", "sign"]),
        "shears" => return has(&["pumpkin", "beehive", "bee_nest"]),
        "brush" => return block.contains("suspicious"),
        "lily_pad" | "frogspawn" => return false,
        _ => {}
    }
    if item.ends_with("_spawn_egg") {
        return true;
    }
    if block.contains("cauldron") && (item.ends_with("bucket") || item.ends_with("potion") || item == "glass_bottle") {
        return true;
    }
    // A block item places itself (BlockItem.useOn).
    block_item
}

/// InBedChatScreen's Leave Bed button: 200 wide, 40 above the bottom.
fn leave_bed_button(gui: &Gui) -> screens::Button {
    let key = "multiplayer.stopSleeping";
    let t = crate::lang::get(key);
    screens::Button::new(crate::math::floor((gui.width - 200.0) as f64 / 2.0) as f32, gui.height - 40.0, 200.0, if t == key { String::from("Leave Bed") } else { t })
}

/// Blocks a right click uses rather than letting the held item act (doors, containers...).
fn interactive(block: &str) -> bool {
    ["door", "trapdoor", "gate", "button", "lever", "chest", "barrel", "furnace", "smoker", "crafting_table", "anvil",
        "bed", "bell", "hopper", "dispenser", "dropper", "shulker_box", "enchanting_table", "brewing_stand", "loom",
        "cartography_table", "grindstone", "stonecutter", "smithing_table", "lectern", "repeater", "comparator",
        "note_block", "jukebox", "crafter", "beacon", "cake", "composter", "respawn_anchor"]
        .iter()
        .any(|w| block.contains(w))
}

fn tool_of(name: &str) -> (u8, f64, u8) {
    // (mineable bit, speed, tier bit for incorrect_for_*), from the item's name.
    let kind = if name.ends_with("_pickaxe") {
        1
    } else if name.ends_with("_axe") {
        2
    } else if name.ends_with("_shovel") {
        4
    } else if name.ends_with("_hoe") {
        8
    } else if name.ends_with("_sword") {
        16
    } else {
        0
    };
    if kind == 0 {
        return (0, 1.0, 0);
    }
    let (speed, tier) = if name.starts_with("wooden_") {
        (2.0, 1)
    } else if name.starts_with("stone_") {
        (4.0, 2)
    } else if name.starts_with("copper_") {
        (5.0, 4)
    } else if name.starts_with("iron_") {
        (6.0, 8)
    } else if name.starts_with("golden_") {
        (12.0, 16)
    } else if name.starts_with("diamond_") {
        (8.0, 32)
    } else if name.starts_with("netherite_") {
        (9.0, 64)
    } else {
        (1.0, 0)
    };
    (kind, speed, tier)
}

pub struct App {
    v: Video,
    r: Renderer,
    keys: [bool; 512],
    screen: Screen,
    /// The chat screen: input, suggestions, clicks (chatui.rs).
    chat_ui: crate::chatui::ChatUi,
    /// The key that opened the chat also arrives as text: drop it.
    skip_text: Option<(char, u64)>,
    debug: bool,
    fullscreen: bool,
    mover: Mover,
    prev_pos: [f64; 3],
    last_input: u8,
    sprinting: bool,
    /// The local player's pose (Entity.DATA_POSE): crouching, swimming, or standing.
    own_pose: i64,
    /// Simple Voice Chat (Options > Voice Chat): off until the player turns it on.
    voice: crate::voicechat::client::Voice,
    /// The client information (skin parts...) sent in play this session, once.
    info_sent: bool,
    own_in_water: bool,
    mining: Option<Mining>,
    destroy_delay: i32,
    use_delay: i32,
    punch_timer: i32,
    left_down: bool,
    right_down: bool,
    /// An item in use (eating, a drawn bow): ticks so far, and how long it lasts (0: until released).
    using: Option<(u32, u32)>,
    last_space: u64,
    last_w: u64,
    double_w: bool,
    fps: u32,
    frames: u32,
    /// PENRYN_SHOT=path and PENRYN_SHOT_AFTER=seconds: save the picture then and quit (tests).
    test_shot: Option<(String, u64)>,
    /// PENRYN_SHOT_INGAME=SECONDS: the shot is taken that long after the first frame of the game itself (the menus,
    /// the pack screens and the loading never take it), instead of PENRYN_SHOT_AFTER seconds after the start.
    shot_ingame: Option<u64>,
    /// PENRYN_TEST_INPUT: held keys for tests, e.g. "w" to walk forward.
    test_keys: String,
    /// The in-bed screen (InBedChatScreen) is the chat screen with a Leave Bed button; after
    /// leaving it stays closed until the server stands the player up.
    in_bed: bool,
    bed_left: bool,
    fps_at: u64,
    /// PENRYN_PERF: print the frame rate, the slowest frame and sound decoding time each second.
    perf: bool,
    frame_at: u64,
    worst_frame: u64,
    view: i32,
    gui: Option<Gui>,
    entities: Option<crate::entities::EntityTable>,
    hand: crate::hand::Hand,
    audio: Option<crate::audio::Audio>,
    /// Footsteps: distance walked (moveDist) and the next step's mark.
    move_dist: f32,
    next_step: f32,
    /// Ticks until the next song (MusicManager's delay).
    music_delay: i32,
    /// R on the death screen: one respawn request until the server answers.
    respawn_sent: bool,
    sky: crate::sky::Sky,
    weather: crate::weather::Weather,
    /// The arm's swing: ticks into it (-1: none), as vanilla's 6-tick swing.
    swing_ticks: i32,
    /// The hand's lowering while a new item comes up: 1 lowered .. 0 raised, and last tick's.
    equip: f32,
    prev_equip: f32,
    /// View bobbing: distance walked and bob amount (and last tick's).
    walk_dist: f32,
    prev_walk_dist: f32,
    /// The local player's body yaw (previous, current): it lags the head (Mob.tickHeadTurn).
    own_body: Option<(f32, f32)>,
    bob: f32,
    prev_bob: f32,
    held_item: Option<i32>,
    /// The mouse in GUI units, while a screen is open.
    mouse: (f32, f32),
    /// Ticks left showing the held item's name (vanilla's toolHighlightTimer).
    held_ticks: i32,
    last_held: (i32, Option<(i32, i32)>),
    /// The open inventory screen is a container the server opened.
    container_screen: bool,
    /// options.txt and the options screens.
    menu: crate::options::Menu,
    /// GameRenderer's fovModifier (sprinting, flying), and last tick's.
    fov_mod: f32,
    prev_fov_mod: f32,
    /// When the last frame was shown (the Max Framerate option).
    frame_shown: u64,
    /// Ticks since a test's screen opened (PENRYN_TEST_INPUT click=X:Y, one each half second).
    test_ticks: usize,
    particles: Option<crate::particles::Particles>,
    creative: crate::creative::Screen,
    /// The pause menu's Disconnect (or the death screen's): back to the menus, not out.
    to_title: bool,
    /// Since when the world is being waited for (LevelLoadingScreen).
    loading_from: Option<u64>,
    /// The open book (Screen::Book).
    book: Option<crate::books::Book>,
    /// The vehicle the player rides (riding.rs).
    ride: crate::riding::Rider,
    /// Swimming, crawling, gliding, riptide (special.rs).
    special: crate::special::Special,
    /// The mouse buttons held (GLFW numbering), and the sneak and sprint toggles (Options).
    mouse_held: [bool; 5],
    sneak_toggle: bool,
    sprint_toggle: bool,
    /// F5: 0 first person, 1 third person behind the player, 2 in front (vanilla's CameraType).
    perspective: u8,
    /// F1 (Toggle GUI): the HUD is hidden.
    hide_hud: bool,
    /// F2 (Take Screenshot): the next frame is saved.
    take_shot: bool,
}

impl App {
    fn eye_height(&self) -> f64 {
        let sneak = self.sneaking();
        if let Some(e) = self.ride.eye(sneak) {
            e
        } else if let Some(e) = self.special.eye() {
            e
        } else if sneak {
            physics::SNEAK_EYE
        } else {
            physics::EYE
        }
    }

    /// Whether a binding is held: its key down, or its mouse button.
    fn held(&self, b: B) -> bool {
        match self.menu.keys.get(b) {
            Key::Kb(sc) => self.keys[(sc as usize).min(511)],
            Key::Mouse(m) => self.mouse_held.get(m as usize).copied().unwrap_or(false),
            Key::Unbound => false,
        }
    }

    /// The character a binding's key types, when it is a keyboard key.
    fn bound_char(&self, b: B) -> Option<char> {
        match self.menu.keys.get(b) {
            Key::Kb(sc) => crate::keybinds::kb_char(sc),
            _ => None,
        }
    }

    /// Sneaking: the sneak binding held, or toggled on (Options' toggleCrouch).
    fn sneaking(&self) -> bool {
        self.screen == Screen::Play && if self.menu.o.on(crate::options::O::ToggleCrouch) { self.sneak_toggle } else { self.held(B::Sneak) }
    }

    /// The player's own model for third person: the server never sends the player itself, so
    /// it is made here from the player's state (pose from the server's flags, the held item,
    /// the walk swing), as LocalPlayer is drawn by vanilla.
    fn own_model(&self, g: &Game, kind: i32) -> crate::entities::Entity {
        let mut e = crate::entities::Entity::new(kind, g.uuid, self.mover.pos, g.player.yaw, g.player.pitch, g.player.yaw);
        e.prev = self.prev_pos;
        let (prev_body, body) = self.own_body.unwrap_or((g.player.yaw, g.player.yaw));
        e.body_yaw = body;
        e.prev_body_yaw = prev_body;
        e.on_ground = self.mover.on_ground;
        e.limb_pos = self.walk_dist;
        e.limb_speed = ((self.walk_dist - self.prev_walk_dist) * 4.0).clamp(0.0, 1.0);
        // Entity flags: the server's word (gliding, swimming...), with the sneak and sprint keys.
        let mut flags = g.riding.own_flags.unwrap_or(0) as i64;
        if self.sneaking() {
            flags |= 0x02;
        }
        if self.sprinting {
            flags |= 0x08;
        }
        if self.own_in_water && self.sprinting && !g.flying {
            flags |= 0x10; // the swimming flag (Entity.FLAG_SWIMMING) the pose reads
        }
        e.data.insert(0, flags);
        e.data.insert(6, self.own_pose);
        e.equipment[0] = g.held();
        e
    }

    /// The camera (GameRenderer's camera setup): the player's eye and angles in first person.
    /// In third person it is 4 blocks back along the view (or in front, looking back at the
    /// player, for F5's second view), stopped short of a solid block.
    fn camera(&self, g: &Game, eye: [f64; 3]) -> ([f64; 3], f32, f32) {
        if self.perspective == 0 {
            return (eye, g.player.yaw, g.player.pitch);
        }
        let front = self.perspective == 2;
        let (yaw, pitch) = if front { (g.player.yaw + 180.0, -g.player.pitch) } else { (g.player.yaw, g.player.pitch) };
        let (y, p) = (math::to_rad(yaw as f64), math::to_rad(pitch as f64));
        let dir = [-math::sin(y) * math::cos(p), -math::sin(p), math::cos(y) * math::cos(p)];
        let mut dist = 4.0;
        let mut t = 0.1;
        while t <= 4.0 {
            let at = [eye[0] - dir[0] * t, eye[1] - dir[1] * t, eye[2] - dir[2] * t];
            let s = g.world.get(math::ifloor(at[0]), math::ifloor(at[1]), math::ifloor(at[2]));
            if s != 0 && g.blocks.collides(s) {
                dist = (t - 0.2).max(0.0);
                break;
            }
            t += 0.1;
        }
        ([eye[0] - dir[0] * dist, eye[1] - dir[1] * dist, eye[2] - dir[2] * dist], yaw, pitch)
    }

    /// A bound key or mouse button went down or up in the game: Attack and Use (held, or
    /// toggled), Pick Block, and the sneak and sprint toggles. True when nothing else acts on it.
    fn bind_edge(&mut self, g: &mut Game, b: B, down: bool) -> Result<bool, String> {
        use crate::options::O;
        match b {
            B::Attack | B::Use => {
                if down {
                    if let Some((id, _, at)) = self.pick_entity(g) {
                        if b == B::Attack {
                            g.attack(id)?;
                        } else {
                            let sneak = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                            g.interact(id, at, sneak)?;
                            g.punch()?;
                        }
                        return Ok(true);
                    }
                }
                let toggle = if b == B::Attack { O::ToggleAttack } else { O::ToggleUse };
                let on = if self.menu.o.on(toggle) { if down { !self.held_flag(b) } else { self.held_flag(b) } } else { down };
                if b == B::Attack {
                    if on && !self.left_down {
                        self.punch_timer = 0;
                    }
                    self.left_down = on;
                } else {
                    if on && !self.right_down {
                        self.use_delay = 0;
                    }
                    self.right_down = on;
                }
                Ok(true)
            }
            B::PickItem => {
                if down {
                    let data = self.keys[K_LCTRL];
                    self.pick_block(g, data)?;
                }
                Ok(true)
            }
            B::Sneak => {
                if down && self.menu.o.on(O::ToggleCrouch) {
                    self.sneak_toggle = !self.sneak_toggle;
                }
                Ok(false)
            }
            B::Sprint => {
                if down && self.menu.o.on(O::ToggleSprint) {
                    self.sprint_toggle = !self.sprint_toggle;
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// The state Attack or Use is in (for a toggle).
    fn held_flag(&self, b: B) -> bool {
        if b == B::Attack { self.left_down } else { self.right_down }
    }

    /// Pick Block (vanilla's Minecraft.pickBlock): the block's item to the selected hotbar slot
    /// when the inventory has it; in creative mode, asked of the server when it does not
    /// (with Ctrl, its NBT too).
    fn pick_block(&mut self, g: &mut Game, include_data: bool) -> Result<(), String> {
        let Some(hit) = self.pick(g) else { return Ok(()) };
        let s = g.world.get(hit.pos[0], hit.pos[1], hit.pos[2]);
        let name = g.blocks.of_state(s).name.clone();
        let Some(item) = g.blocks.items.iter().position(|n| *n == name).map(|i| i as i32) else { return Ok(()) };
        if let Some(slot) = (0usize..9).find(|i| g.inventory[36 + i].map_or(false, |(id, _)| id == item)) {
            return g.select_slot(slot as i16);
        }
        if g.game_mode == 1 {
            let p = hit.pos;
            let pos = ((p[0] as i64 & 0x3FF_FFFF) << 38) | ((p[2] as i64 & 0x3FF_FFFF) << 12) | (p[1] as i64 & 0xFFF);
            let mut o = crate::wire::Out::new();
            o.i64(pos).bool(include_data);
            return g.conn.send(crate::ids::play::S_PICK_ITEM_FROM_BLOCK, &o);
        }
        // Survival: a stack in the main inventory swaps into the selected hotbar slot.
        if let Some(slot) = (9usize..36).find(|i| g.inventory[*i].map_or(false, |(id, _)| id == item)) {
            let sel = g.selected as i8;
            g.click(0, slot as i16, sel, 2)?;
        }
        Ok(())
    }

    /// Screenshot (F2 by default): the frame as a PNG in screenshots/, named by the time as
    /// vanilla's Screenshot.getFile does (2026-11-22_11.20.33.png, _1 for a second one).
    fn save_screenshot(&mut self, g: &mut Game) {
        let (w, h) = self.v.size();
        let png = sdl::screenshot_png(&self.v.gl, w, h);
        let dir = format!("{}/screenshots", crate::data_dir());
        sys::make_dir(&dir);
        let base = format!("{}/{}", dir, sys::local_stamp());
        let mut path = format!("{}.png", base);
        let mut n = 1;
        while sys::read_head(&path, 1).is_some() {
            path = format!("{}_{}.png", base, n);
            n += 1;
        }
        if sys::write_file_readable(&path, &png) {
            say!("screenshot: {}", path);
            // Vanilla says where the picture went, in chat (the file name only).
            let name = format!("{}", path.rsplit('/').next().unwrap_or(&path));
            g.add_chat(format!("Saved screenshot as {}", name));
        } else {
            say!("cannot save screenshot {}", path);
        }
    }
}

/// The window: the title screen and the menus (title.rs), then each game joined from them. With
/// `server` Penryn connects straight away, as `penryn HOST` always has.
pub fn run(server: Option<String>, name: &str, account: Option<Account>, relay: Option<(String, u16)>) -> Result<(), String> {
    // options.txt first; settings.txt's VIEW and FULLSCREEN (PENRYN_VIEW, PENRYN_FULLSCREEN)
    // stand in for what it does not say yet.
    let mut settings = crate::options::Settings::load();
    use crate::options::O;
    if !settings.given(O::RenderDistance) {
        let v: i32 = sys::env("PENRYN_VIEW").and_then(|s| s.parse().ok()).unwrap_or(8).clamp(2, 16);
        settings.set(O::RenderDistance, v as f64);
    }
    if !settings.given(O::Fullscreen) && sys::env("PENRYN_FULLSCREEN").is_some() {
        settings.set(O::Fullscreen, 1.0);
    }
    let fullscreen = settings.on(O::Fullscreen);
    let v = Video::open("Penryn", 1280, 720, fullscreen)?;
    let swap = unsafe { (v.sdl.SDL_GL_SetSwapInterval)(settings.on(O::Vsync) as i32) };
    say!("vsync {} (SDL: {})", if settings.on(O::Vsync) { "on" } else { "off" }, swap);
    let view: i32 = settings.int(O::RenderDistance);
    say!("OpenGL: {} ({})", v.gl_string(sdl::RENDERER), v.gl_string(sdl::VERSION));
    crate::perf::init(&v);
    let mut app = App {
        v,
        r: Renderer::new(),
        keys: [false; 512],
        screen: Screen::Play,
        chat_ui: crate::chatui::ChatUi::new(),
        skip_text: None,
        debug: false,
        fullscreen,
        mover: Mover::new([0.0; 3]),
        prev_pos: [0.0; 3],
        last_input: 0,
        sprinting: false,
        own_pose: 0,
        voice: crate::voicechat::client::Voice::new(),
        info_sent: false,
        own_in_water: false,
        in_bed: false,
        bed_left: false,
        mining: None,
        destroy_delay: 0,
        use_delay: 0,
        punch_timer: 0,
        left_down: false,
        right_down: false,
        using: None,
        last_space: 0,
        last_w: 0,
        double_w: false,
        fps: 0,
        frames: 0,
        test_shot: sys::env("PENRYN_SHOT").map(|p| {
            let after: u64 = sys::env("PENRYN_SHOT_AFTER").and_then(|s| s.parse().ok()).unwrap_or(8);
            // Waiting for the game: no deadline until its first frame.
            let ingame = sys::env("PENRYN_SHOT_INGAME").is_some();
            (p, if ingame { u64::MAX } else { sys::now_ms() + after * 1000 })
        }),
        shot_ingame: sys::env("PENRYN_SHOT_INGAME").and_then(|s| s.parse().ok()),
        test_keys: crate::title::game_test_keys(&sys::env("PENRYN_TEST_INPUT").unwrap_or_default()),
        fps_at: sys::now_ms(),
        perf: sys::env("PENRYN_PERF").is_some(),
        frame_at: sys::now_ms(),
        worst_frame: 0,
        view,
        gui: None,
        entities: None,
        hand: crate::hand::Hand::new(),
        audio: None,
        move_dist: 0.0,
        next_step: 1.0,
        music_delay: 100,
        respawn_sent: false,
        sky: crate::sky::Sky::new(),
        weather: crate::weather::Weather::new(),
        swing_ticks: -1,
        equip: 0.0,
        prev_equip: 0.0,
        walk_dist: 0.0,
        prev_walk_dist: 0.0,
        own_body: None,
        bob: 0.0,
        prev_bob: 0.0,
        held_item: None,
        mouse: (0.0, 0.0),
        held_ticks: 0,
        last_held: (-1, None),
        container_screen: false,
        menu: crate::options::Menu::new(settings),
        fov_mod: 1.0,
        prev_fov_mod: 1.0,
        frame_shown: 0,
        test_ticks: 0,
        particles: None,
        creative: crate::creative::Screen::new(),
        to_title: false,
        loading_from: None,
        book: None,
        ride: crate::riding::Rider::new(),
        special: crate::special::Special::new(),
        mouse_held: [false; 5],
        sneak_toggle: false,
        sprint_toggle: false,
        perspective: 0,
        hide_hud: false,
        take_shot: false,
    };
    let atlas = sys::read_file(&crate::packs::path("atlas.bin")).ok_or("cannot read atlas.bin (see README.md)")?;
    app.r.load_atlas(&app.v.gl, &atlas)?;
    drop(atlas);
    // Particle sprites are optional: without particles.bin only block particles are drawn.
    let pd = sys::read_file(&crate::packs::path("particles.bin"));
    app.particles = Some(crate::particles::Particles::load(&app.v.gl, pd.as_deref(), app.r.atlas_size));
    drop(pd);
    let gui = sys::read_file(&crate::packs::path("gui.bin")).ok_or("cannot read gui.bin (see README.md)")?;
    app.gui = Some(Gui::load(&app.v.gl, &gui, app.r.atlas, app.r.atlas_size)?);
    drop(gui);
    if let Some(g) = app.gui.as_mut() {
        g.overlays = crate::itemmodels::read();
    }
    // The other container screens, maps and item frames (containers.bin, optional).
    crate::menus::load(&app.v.gl);
    // Weather is optional: without weather.bin no rain or snow falls (the sky still greys).
    if let Some(d) = sys::read_file(&crate::packs::path("weather.bin")) {
        if app.weather.parse(&d, Some(&app.v.gl)).is_none() {
            say!("weather.bin is of another version: no rain or snow");
        }
    }
    if let Some(gui) = app.gui.as_mut() {
        gui.fixed_scale = app.menu.o.gui_scale();
    }
    // Sound is optional: without sounds.bin (or an audio device) the game plays silent.
    app.audio = crate::audio::Audio::open(&app.v.sdl, &format!("{}/sounds.bin", crate::data_dir()));
    if app.audio.is_none() {
        say!("no sound (sounds.bin or the audio device is missing)");
    }
    if let Some(a) = app.audio.as_mut() {
        a.set_volumes(app.menu.o.volumes());
    }
    // Entity models are optional: without entities.bin the world still plays, without mobs drawn.
    if let Some(d) = sys::read_file(&crate::packs::path("entities.bin")) {
        app.entities = Some(crate::entities::EntityTable::load(&app.v.gl, &d)?);
    }
    let mut front = crate::title::Front::new(account, relay, name, &sys::env("PENRYN_TEST_INPUT").unwrap_or_default());
    if let Some(address) = server {
        front.connect(&address, false);
    }
    loop {
        let next = {
            let Some(gui) = app.gui.as_mut() else { return Err(String::from("no GUI")) };
            let mut ui = crate::title::Ui { v: &app.v, gui, menu: &mut app.menu, audio: app.audio.as_mut(), shot: &app.test_shot };
            front.run(&mut ui)
        };
        match next {
            crate::title::Next::Quit => return Ok(()),
            crate::title::Next::Packs => {
                // Options > Resource Packs: the selection screen holds the window, then the list is kept.
                let local = crate::packs::parse_option(app.menu.o.other("resourcePacks").unwrap_or(""));
                if let Some(new) = crate::packs::selection_screen(&mut app, &local) {
                    app.menu.o.set_other("resourcePacks", &crate::packs::option_text(&new));
                    app.menu.o.save();
                }
            }
            crate::title::Next::Join(login, address, back_to_list) => {
                if !play_session(&mut app, &mut front, login, address, back_to_list)? {
                    return Ok(());
                }
            }
        }
    }
}

/// A game from its login to its end, through the transfers the server makes (ClientboundTransferPacket,
/// keeping the cookies). Ok(false): the window was closed.
fn play_session(app: &mut App, front: &mut crate::title::Front, mut login: crate::client::Login, mut address: String, back_to_list: bool) -> Result<bool, String> {
    let (who_name, who_account) = (login.name.clone(), login.account.clone());
    loop {
        let mut g = match app.join(login, &address) {
            Ok(g) => {
                say!("playing on {}", address);
                g
            }
            Err(e) if e == crate::packs::WINDOW_CLOSED => return Ok(false),
            Err(e) => {
                front.join_failed(back_to_list, e);
                return Ok(true);
            }
        };
        app.new_session();
        app.v.grab_mouse(true);
        let result = app.main_loop(&mut g);
        app.v.grab_mouse(false);
        // The meshing threads read the block table: they finish before the game's goes.
        app.r.cancel_meshing();
        // Back to the player's own packs (DownloadedPackSource.cleanupAfterDisconnect).
        {
            let crate::client::Game { packs, world, blocks, .. } = &mut g;
            packs.leave(app, Some(world), blocks);
        }
        if let Some((h, p)) = g.ui.common.transfer.take() {
            // The old connection goes first, then the new one comes (Connection.disconnect).
            let common = core::mem::take(&mut g.ui.common);
            drop(g);
            address = format!("{}:{}", h, p);
            match Game::login(&h, p, &who_name, who_account.as_ref(), common, true) {
                Ok(l) => login = l,
                Err(e) => {
                    front.join_failed(back_to_list, e);
                    return Ok(true);
                }
            }
            continue;
        }
        if app.to_title {
            front.after_game(None);
            return Ok(true);
        }
        return match result {
            Ok(()) => Ok(false),
            Err(e) => {
                let reason = g.disconnected.clone().unwrap_or(e);
                crate::warn!("disconnected: {}", reason);
                front.after_game(Some(reason));
                Ok(true)
            }
        };
    }
}

impl App {
    fn message(&mut self, lines: &[&str]) {
        let (w, h) = self.v.size();
        let gl = &self.v.gl;
        unsafe {
            (gl.Viewport)(0, 0, w, h);
            (gl.ClearColor)(0.06, 0.06, 0.06, 1.0);
            (gl.Clear)(sdl::COLOR_BUFFER_BIT | sdl::DEPTH_BUFFER_BIT);
        }
        if let Some(gui) = self.gui.as_mut() {
            gui.begin(w, h);
            let (cx, cy) = (gui.width / 2.0, gui.height / 2.0);
            for (i, l) in lines.iter().enumerate() {
                gui.text_centered(l, cx, cy - 20.0 + i as f32 * 12.0, if i == 0 { screens::WHITE } else { 0xFFA0_A0A0 });
            }
            gui.flush(&self.v.gl, w, h);
        }
        self.v.swap();
    }

    /// The second half of a join, on the window's thread: the player's own resource packs
    /// (options.txt resourcePacks, baked by the relay), then the configuration, where the server's
    /// packs are asked for and loaded. A transfer during the configuration joins the server it names.
    fn join(&mut self, mut login: crate::client::Login, address: &str) -> Result<Game, String> {
        use crate::options::O;
        let mut address = String::from(address);
        loop {
            let (name, account) = (login.name.clone(), login.account.clone());
            let mut blocks = crate::load_blocks()?;
            let local = crate::packs::parse_option(self.menu.o.other("resourcePacks").unwrap_or(""));
            if !local.is_empty() {
                if let Err(e) = crate::packs::wait_stack(&local, &[], self, None, &mut blocks) {
                    say!("could not load the resource packs: {}", e);
                }
            }
            self.audio.as_mut().map(|a| a.set_overlay(crate::packs::sound_overlay().as_deref()));
            let view = self.menu.o.int(O::RenderDistance) as i8;
            let target = format!("Joining {} ...", address);
            self.message(&[&target]);
            match Game::configure(login, blocks, view, local, self) {
                Ok(g) => return Ok(g),
                Err(Join::Failed(e)) => return Err(e),
                Err(Join::Transfer(h, p, common)) => {
                    say!("transferred to {}:{}", h, p);
                    address = format!("{}:{}", h, p);
                    login = Game::login(&h, p, &name, account.as_ref(), common, true)?;
                }
            }
        }
    }

    /// A new game: what the last one left behind goes.
    fn new_session(&mut self) {
        use crate::options::O;
        self.voice = crate::voicechat::client::Voice::new();
        self.info_sent = false;
        self.r.forget_world(&self.v.gl);
        self.set_screen(Screen::Play);
        self.keys = [false; 512];
        self.chat_ui = crate::chatui::ChatUi::new();
        self.book = None;
        self.ride = crate::riding::Rider::new();
        self.special = crate::special::Special::new();
        self.mouse_held = [false; 5];
        self.sneak_toggle = false;
        self.sprint_toggle = false;
        self.perspective = 0;
        self.hide_hud = false;
        self.take_shot = false;
        self.mover = Mover::new([0.0; 3]);
        self.prev_pos = [0.0; 3];
        self.sprinting = false;
        self.mining = None;
        self.using = None;
        self.respawn_sent = false;
        self.swing_ticks = -1;
        self.held_item = None;
        self.held_ticks = 0;
        self.last_held = (-1, None);
        self.container_screen = false;
        self.creative = crate::creative::Screen::new();
        self.to_title = false;
        self.loading_from = None;
        self.view = self.menu.o.int(O::RenderDistance);
        self.fullscreen = self.menu.o.on(O::Fullscreen);
    }
}

impl App {
    fn main_loop(&mut self, g: &mut Game) -> Result<(), String> {
        let mut next_tick = sys::now_ms();
        loop {
            // ---- events
            let mut quit = false;
            self.v.pump();
            while let Some(e) = self.v.next_event() {
                if self.event(g, &e)? {
                    quit = true;
                }
            }
            if quit {
                return Ok(());
            }
            if !self.menu.changed.is_empty() {
                self.apply_options(g)?;
            }
            crate::perf::lap(crate::perf::S::Events);
            // ---- network
            g.pump()?;
            if g.reconfigure {
                g.reconfigure_now(self)?;
            }
            // Resource packs: the confirm screen holds the window until it is answered.
            g.pack_step(self)?;
            if g.packs.prompt.is_some() {
                while g.packs.prompt.is_some() {
                    g.pump()?;
                    g.pack_step(self)?;
                }
                self.v.grab_mouse(self.screen == Screen::Play);
            }
            self.particle_effects(g);
            crate::perf::lap(crate::perf::S::Net);
            // ---- sound
            if let Some(a) = self.audio.as_mut() {
                let eye = [self.mover.pos[0], self.mover.pos[1] + 1.62, self.mover.pos[2]];
                a.set_listener(eye, g.player.yaw);
                for snd in g.sounds.drain(..) {
                    match snd.sound {
                        crate::client::SoundRef::Id(id) => a.play(id, snd.pos, snd.volume, snd.pitch, snd.source),
                        crate::client::SoundRef::Name(n) => a.play_named(&n, snd.pos, snd.volume, snd.pitch, snd.source),
                    }
                }
                a.feed_music();
            } else {
                g.sounds.clear();
            }
            // ---- voice chat: off unless the player turned it on
            {
                use crate::options::O;
                let cfg = crate::voicechat::client::Config {
                    enabled: self.menu.o.on(O::VoiceChat),
                    push_to_talk: self.menu.o.int(O::VoiceMic) == 0,
                    threshold_db: self.menu.o.int(O::VoiceThreshold),
                    volume: (self.menu.o.get(O::VoiceVolume) * 2.0) as f32,
                    talk_held: self.screen == Screen::Play && self.held(B::VoiceTalk),
                };
                let me = crate::voicechat::client::Listener { pos: [self.mover.pos[0], self.mover.pos[1] + 1.62, self.mover.pos[2]], yaw: g.player.yaw };
                self.voice.update(sys::now_ms(), g, self.audio.as_mut(), &self.v, &cfg, &me);
            }
            crate::perf::lap(crate::perf::S::Sound);
            // ---- ticks
            let now = sys::now_ms();
            if now > next_tick + 1000 {
                next_tick = now; // far behind (a stall): do not run a second of ticks at once
            }
            while now >= next_tick {
                self.tick(g)?;
                next_tick += 50;
            }
            let partial = 1.0 - ((next_tick - now) as f64 / 50.0).clamp(0.0, 1.0);
            crate::perf::lap(crate::perf::S::Logic);
            self.draw(g, partial);
            // The Max Framerate option (vanilla's FramerateLimitTracker), on top of vsync.
            if let Some(fps) = self.menu.o.frame_limit() {
                let due = self.frame_shown + 1000 / fps as u64;
                let t = sys::now_ms();
                if t < due {
                    sys::sleep_ms((due - t) as u32);
                }
            }
            crate::perf::lap(crate::perf::S::Sleep);
            self.frame_shown = sys::now_ms();
            if self.take_shot {
                self.take_shot = false;
                self.save_screenshot(g);
            }
            if let (Some(secs), Some(shot)) = (self.shot_ingame, self.test_shot.as_mut()) {
                // Only once the player is in the world (the pack screens are drawn by this loop too).
                if shot.1 == u64::MAX && g.loaded_sent && self.screen == Screen::Play {
                    shot.1 = now + secs * 1000;
                }
            }
            if let Some(shot) = self.test_shot.as_ref() {
                // PENRYN_TEST_INPUT "m": wait for a piston in motion.
                if now >= shot.1 && (!self.test_keys.contains('m') || g.world.moving.in_motion()) {
                    let (w, h) = self.v.size();
                    let png = sdl::screenshot_png(&self.v.gl, w, h);
                    sys::write_file_private(&shot.0, &png);
                    say!("screenshot: {}", shot.0);
                    // Not back to the menus: they would take their own shot of the next screen over this one.
                    unsafe { sys::exit(0) };
                }
            }
            self.frames += 1;
            let ft = now.saturating_sub(self.frame_at);
            self.frame_at = now;
            self.worst_frame = self.worst_frame.max(ft);
            if now >= self.fps_at + 1000 {
                if self.perf {
                    let a = self.audio.as_mut().map_or(0.0, |a| core::mem::replace(&mut a.busy_ms, 0.0));
                    let p = self.mover.pos;
                    say!(
                        "perf: {} fps, slowest frame {} ms, sound decoding {:.0} ms, at {:.2} {:.2} {:.2}{}{}",
                        self.frames,
                        self.worst_frame,
                        a,
                        p[0],
                        p[1],
                        p[2],
                        if self.mover.on_ground { " ground" } else { "" },
                        if self.mover.horizontal_collision { " wall" } else { "" }
                    );
                    if self.special.pose != 0 || self.special.swimming || self.special.fall_flying {
                        say!("perf: pose {}, swimming {}, gliding {}, in water {}", self.special.pose, self.special.swimming, self.special.fall_flying, self.special.in_water);
                    }
                    say!("perf: {}, {} sections, {}k vertices", crate::perf::report(self.frames), self.r.sections_drawn, self.r.vertices_drawn / 1000);
                    if self.mover.horizontal_collision {
                        // What is around the player at feet and head height.
                        let (bx, by, bz) = (math::ifloor(p[0]), math::ifloor(p[1]), math::ifloor(p[2]));
                        let mut near = String::new();
                        for dy in 0..2 {
                            for dz in -1..=1 {
                                for dx in -1..=1 {
                                    let st = g.world.get(bx + dx, by + dy, bz + dz);
                                    if st != 0 && g.blocks.collides(st) {
                                        near.push_str(&format!(" [{} {} {}]{}", dx, dy, dz, g.blocks.of_state(st).name));
                                    }
                                }
                            }
                        }
                        say!("perf: yaw {:.1}, vel {:.3} {:.3} {:.3}, solid around:{}", g.player.yaw, self.mover.vel[0], self.mover.vel[1], self.mover.vel[2], near);
                    }
                }
                self.worst_frame = 0;
                self.fps = self.frames;
                self.frames = 0;
                self.fps_at = now;
            }
        }
    }

    /// Where a block's outline sits: moved by its offset when vanilla's getShape moves it
/// (flowers, bamboo, dripstone; not grass).
fn outline_offset(b: &blocks::Blocks, s: u16, p: [i32; 3]) -> [f64; 3] {
    if b.of_state(s).offset & blocks::OFF_OUTLINE != 0 {
        b.offset_at(s, p[0], p[2])
    } else {
        [0.0; 3]
    }
}

/// A position packet moved the player: take it over.
    fn teleported(&mut self, g: &mut Game) -> bool {
        let to = [g.player.x, g.player.y, g.player.z];
        let jump = (0..3).map(|i| (to[i] - self.prev_pos[i]) * (to[i] - self.prev_pos[i])).sum::<f64>();
        // A long jump (a teleport): every mesh is rebuilt, as F3+A does, so none stays stale.
        if jump > 64.0 * 64.0 {
            self.r.reload(&self.v.gl, &mut g.world);
        }
        self.mover.reset(to);
        self.prev_pos = self.mover.pos;
        true
    }

    /// F3+A: rebuild every chunk mesh (vanilla's debug reload).
    fn reload_chunks(&mut self, g: &mut Game) {
        self.r.reload(&self.v.gl, &mut g.world);
    }

    fn event(&mut self, g: &mut Game, e: &sdl::Event) -> Result<bool, String> {
        let t = e.u32_at(0);
        if self.screen == Screen::Creative && t != sdl::EV_QUIT && t != sdl::EV_WINDOW && t != sdl::EV_MOUSEMOTION {
            return self.creative_event(g, e);
        }
        match t {
            sdl::EV_QUIT => return Ok(true),
            sdl::EV_WINDOW => {
                if e.0[12] == sdl::WINEV_FOCUS_LOST && self.screen == Screen::Play && self.test_shot.is_none() {
                    self.set_screen(Screen::Pause);
                }
            }
            sdl::EV_KEYDOWN | sdl::EV_KEYUP => {
                let sc = e.i32_at(16).clamp(0, 511) as usize;
                let down = t == sdl::EV_KEYDOWN;
                let repeat = e.0[13] != 0;
                self.keys[sc] = down;
                // Controls > Key Binds: the next key is the binding (Escape unbinds it).
                if self.menu.keys.listening.is_some() {
                    if down && !repeat {
                        let key = if sc == K_ESC { Key::Unbound } else { Key::Kb(sc as u16) };
                        crate::keybinds::bind(&mut self.menu, key);
                    }
                    return Ok(false);
                }
                if self.screen == Screen::Play && !repeat {
                    if let Some(b) = self.menu.keys.find_kb(sc) {
                        if self.bind_edge(g, b, down)? {
                            return Ok(false);
                        }
                    }
                }
                if down && !repeat {
                    return self.key(g, sc);
                }
                if down && repeat && self.screen == Screen::Chat {
                    if let crate::chatui::Act::Close = self.chat_ui.key(g, sc, &self.keys, &self.v)? {
                        self.set_screen(Screen::Play);
                    }
                }
                if down && repeat && self.screen == Screen::Sign && sc != K_ESC {
                    if let Some(gui) = self.gui.as_ref().filter(|_| !matches!(sc, 4 | 6 | 25 | 27)) {
                        crate::signs::key(g, gui, sc, &self.keys, &self.v)?;
                    }
                }
                if down && repeat && self.screen == Screen::Book && matches!(sc, 40 | 76 | 79 | 80 | 81 | 82) {
                    return self.key(g, sc);
                }
                if down && repeat && sc == K_BACKSPACE && (self.screen == Screen::Inventory || self.screen == Screen::Book) {
                    return self.key(g, sc);
                }
            }
            sdl::EV_TEXTINPUT => {
                if let (Screen::Sign, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    let n = e.0[12..44].iter().position(|b| *b == 0).unwrap_or(32);
                    crate::signs::type_text(g, gui, &String::from_utf8_lossy(&e.0[12..12 + n]));
                }
                if self.screen == Screen::Inventory || self.screen == Screen::Book {
                    let n = e.0[12..44].iter().position(|b| *b == 0).unwrap_or(32);
                    let s = String::from_utf8_lossy(&e.0[12..12 + n]).into_owned();
                    if self.screen == Screen::Inventory {
                        crate::menus::text(g, &s)?;
                    } else if let (Some(gui), Some(b)) = (self.gui.as_ref(), self.book.as_mut()) {
                        crate::books::text(gui, b, &s);
                    }
                }
                if self.screen == Screen::Chat {
                    let mut n = 0;
                    while n < 32 && e.0[12 + n] != 0 {
                        n += 1;
                    }
                    let s = String::from_utf8_lossy(&e.0[12..12 + n]).into_owned();
                    if let Some((c, at)) = self.skip_text.take() {
                        if sys::now_ms() < at + 150 && s.chars().next().map(|x| x.to_ascii_lowercase()) == Some(c) {
                            return Ok(false);
                        }
                    }
                    self.chat_ui.type_text(g, &s);
                }
            }
            sdl::EV_MOUSEMOTION => {
                if let Some(gui) = self.gui.as_ref() {
                    self.mouse = (e.i32_at(20) as f32 / gui.scale, e.i32_at(24) as f32 / gui.scale);
                }
                if self.screen == Screen::Play {
                    let (dx, dy) = (e.i32_at(28) as f32, e.i32_at(32) as f32);
                    let (kx, ky) = self.menu.o.turn_per_pixel();
                    g.player.yaw += dx * kx;
                    g.player.pitch = (g.player.pitch + dy * ky).clamp(-90.0, 90.0);
                } else if let (Screen::Chat, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    self.chat_ui.drag(gui, self.mouse.0, self.mouse_held[0]);
                } else if self.screen == Screen::Options {
                    self.menu.drag_to(self.mouse.0);
                } else if let (Screen::Inventory, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    crate::menus::drag(gui, g, self.mouse.0, self.mouse.1);
                } else if let (Screen::Sign, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    crate::signs::drag(g, gui, self.mouse.0, self.mouse_held.first().copied().unwrap_or(false));
                } else if let (Screen::Book, Some(gui), Some(b)) = (&self.screen, self.gui.as_ref(), self.book.as_mut()) {
                    crate::books::drag(gui, b, self.mouse.0, self.mouse.1, self.mouse_held.first().copied().unwrap_or(false));
                }
            }
            sdl::EV_MOUSEDOWN | sdl::EV_MOUSEUP => {
                let down = t == sdl::EV_MOUSEDOWN;
                let button = e.0[16];
                let glfw = crate::keybinds::glfw_button(button);
                if let Some(h) = self.mouse_held.get_mut(glfw as usize) {
                    *h = down;
                }
                if self.menu.keys.listening.is_some() {
                    if down {
                        crate::keybinds::bind(&mut self.menu, Key::Mouse(glfw));
                    }
                    return Ok(false);
                }
                if self.screen != Screen::Play {
                    if down {
                        return self.screen_click(g, button);
                    }
                    crate::menus::release(g);
                    if self.screen == Screen::Options && button == 1 {
                        if let crate::options::Ev::Click = self.menu.release() {
                            self.ui_click();
                        }
                    }
                    return Ok(false);
                }
                if let Some(b) = self.menu.keys.find_mouse(glfw) {
                    self.bind_edge(g, b, down)?;
                }
            }
            sdl::EV_MOUSEWHEEL => {
                if self.screen == Screen::Chat {
                    let shift = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                    self.chat_ui.wheel(g, e.i32_at(20), shift);
                } else if let (Screen::Options, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    self.menu.wheel(gui.width, gui.height, e.i32_at(20));
                } else if self.screen == Screen::Links {
                    crate::serverhud::links_wheel(e.i32_at(20));
                } else if let (Screen::Inventory, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    crate::menus::wheel(gui, g, e.i32_at(20));
                } else if self.screen == Screen::Play {
                    let y = e.i32_at(20);
                    if y != 0 {
                        let s = (g.selected - y.signum()).rem_euclid(9);
                        g.select_slot(s as i16)?;
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }

    /// A click on a screen: buttons, or slots as vanilla's AbstractContainerScreen handles them.
    fn screen_click(&mut self, g: &mut Game, button: u8) -> Result<bool, String> {
        let Some(gui) = self.gui.as_ref() else { return Ok(false) };
        let (mx, my) = self.mouse;
        match self.screen {
            Screen::Chat => {
                if self.in_bed && leave_bed_button(gui).hit(mx, my) {
                    self.leave_bed(g)?;
                    self.set_screen(Screen::Play);
                    return Ok(false);
                }
                let shift = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                if let crate::chatui::Act::Close = self.chat_ui.click(g, gui, mx, my, button, shift, &self.v)? {
                    self.set_screen(Screen::Play);
                }
            }
            Screen::Pause if button == 1 => {
                let links = !g.ui.common.links.is_empty();
                let b = screens::pause_buttons(gui, links);
                if links && crate::serverhud::links_button(gui).hit(mx, my) {
                    self.ui_click();
                    crate::serverhud::open_links();
                    self.set_screen(Screen::Links);
                    return Ok(false);
                }
                if b[0].hit(mx, my) {
                    self.ui_click();
                    self.set_screen(Screen::Play);
                } else if b[1].hit(mx, my) {
                    self.ui_click();
                    self.to_title = true;
                    return Ok(true);
                } else if b[2].hit(mx, my) {
                    self.ui_click();
                    self.menu.open(crate::options::Page::Main);
                    self.set_screen(Screen::Options);
                }
            }
            Screen::Options if button == 1 => {
                let shift = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                match self.menu.press(gui.width, gui.height, mx, my, shift) {
                    crate::options::Ev::Click => self.ui_click(),
                    crate::options::Ev::Close => {
                        self.ui_click();
                        self.set_screen(Screen::Pause);
                    }
                    crate::options::Ev::Packs => {
                        self.ui_click();
                        let local = g.packs.local.clone();
                        if let Some(new) = crate::packs::selection_screen(self, &local) {
                            self.menu.o.set_other("resourcePacks", &crate::packs::option_text(&new));
                            self.menu.o.save();
                            g.packs.local = new;
                        }
                    }
                    crate::options::Ev::None => {}
                }
            }
            Screen::Links if button == 1 => match crate::serverhud::links_click(gui, g, mx, my) {
                crate::serverhud::LinkAct::Click => self.ui_click(),
                crate::serverhud::LinkAct::Close => {
                    self.ui_click();
                    self.set_screen(Screen::Pause);
                }
                crate::serverhud::LinkAct::None => {}
            },
            Screen::Dead if button == 1 => {
                let b = crate::serverhud::death_buttons(gui, &g.ui);
                if b[0].hit(mx, my) {
                    self.ui_click();
                    g.respawn()?;
                } else if b[1].hit(mx, my) {
                    self.ui_click();
                    self.to_title = true;
                    return Ok(true);
                }
            }
            Screen::Sign if button == 1 => {
                if crate::signs::done_button(gui).hit(mx, my) {
                    self.ui_click();
                    crate::signs::close(g)?;
                    self.set_screen(Screen::Play);
                } else {
                    crate::signs::click(g, gui, mx, my);
                }
            }
            Screen::Book if button == 1 => {
                if let Some(b) = self.book.as_mut() {
                    if crate::books::click(gui, g, b, mx, my)? {
                        self.book = None;
                        self.set_screen(Screen::Play);
                    }
                }
            }
            Screen::Inventory if button == 1 || button == 3 => {
                let win = screens::window(g);
                if crate::menus::click(gui, g, &win, mx, my, button)? {
                    return Ok(false);
                }
                let shift = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                let b = if button == 1 { 0 } else { 1 };
                match screens::slot_at(gui, &win, mx, my) {
                    Some(slot) => g.click(win.container_id, slot, b, if shift { 1 } else { 0 })?,
                    None if !screens::inside(gui, &win, mx, my) && g.carried.is_some() => {
                        g.click(win.container_id, -999, b, 0)?; // drop what is on the cursor
                    }
                    None => {}
                }
            }
            _ => {}
        }
        Ok(false)
    }

    /// E: the creative inventory in creative mode, else the survival one.
    fn open_inventory(&mut self, g: &mut Game, key: char) -> Result<(), String> {
        // LocalPlayer.sendOpenInventory: riding something with an inventory screen, the server
        // opens it (PlayerCommand OPEN_INVENTORY, action 5).
        if self.ride_has_inventory(g) {
            return g.command(5);
        }
        self.open_inventory_screen(g, key);
        Ok(())
    }

    /// HasCustomInventoryScreen: horses and kin, camels, nautiluses, chest boats.
    fn ride_has_inventory(&self, g: &Game) -> bool {
        let (Some(t), Some(v)) = (self.entities.as_ref(), g.riding.vehicle_of.get(&g.entity_id)) else { return false };
        let Some(e) = g.entities.get(v) else { return false };
        let name = t.types.get(e.kind as usize).map_or("", |x| x.name.as_str());
        name.ends_with("_chest_boat") || name.ends_with("_chest_raft") || name.ends_with("nautilus") || {
            use crate::riding::Kind;
            matches!(crate::riding::kind_of(name), Kind::Horse | Kind::Camel | Kind::Llama)
        }
    }

    fn open_inventory_screen(&mut self, g: &Game, key: char) {
        if g.game_mode == 1 {
            if let Some(d) = self.gui.as_ref().and_then(|x| x.creative.as_ref()) {
                self.creative.open(d, g, key);
                self.set_screen(Screen::Creative);
                return;
            }
        }
        self.set_screen(Screen::Inventory);
    }

    /// Input while the creative inventory is open.
    fn creative_event(&mut self, g: &mut Game, e: &sdl::Event) -> Result<bool, String> {
        use crate::creative::Action;
        let Some(gui) = self.gui.as_ref() else { return Ok(false) };
        let Some(d) = gui.creative.as_ref() else { return Ok(false) };
        let t = e.u32_at(0);
        let shift = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
        let act = match t {
            sdl::EV_KEYDOWN | sdl::EV_KEYUP => {
                let sc = e.i32_at(16).clamp(0, 511) as usize;
                self.keys[sc] = t == sdl::EV_KEYDOWN;
                if t == sdl::EV_KEYUP {
                    return Ok(false);
                }
                self.creative.key(d, gui, g, sc, e.0[13] != 0, &self.keys, self.mouse, &self.v)?
            }
            sdl::EV_TEXTINPUT => {
                let n = e.0[12..44].iter().position(|c| *c == 0).unwrap_or(32);
                self.creative.text(d, g, &String::from_utf8_lossy(&e.0[12..12 + n]));
                Action::None
            }
            sdl::EV_MOUSEDOWN | sdl::EV_MOUSEUP => {
                self.creative.mouse(d, gui, g, e.0[16], t == sdl::EV_MOUSEDOWN, shift, self.mouse.0, self.mouse.1)?;
                Action::None
            }
            sdl::EV_MOUSEWHEEL => {
                self.creative.wheel(d, e.i32_at(20));
                Action::None
            }
            _ => Action::None,
        };
        self.creative_action(g, act)?;
        Ok(false)
    }

    fn creative_action(&mut self, g: &mut Game, act: crate::creative::Action) -> Result<(), String> {
        match act {
            crate::creative::Action::Close => self.close_inventory(g)?,
            crate::creative::Action::Survival => self.set_screen(Screen::Inventory),
            crate::creative::Action::None => {}
        }
        Ok(())
    }

    fn close_inventory(&mut self, g: &mut Game) -> Result<(), String> {
        let id = g.container.as_ref().map_or(0, |c| c.id);
        g.close_container(id)?;
        self.container_screen = false;
        self.set_screen(Screen::Play);
        Ok(())
    }

    /// InBedChatScreen.sendWakeUp: PlayerCommand STOP_SLEEPING (action 0), once.
    fn leave_bed(&mut self, g: &mut Game) -> Result<(), String> {
        if self.in_bed {
            self.in_bed = false;
            self.bed_left = true;
            g.command(0)?;
        }
        Ok(())
    }

    fn set_screen(&mut self, s: Screen) {
        if self.screen == Screen::Options && s != Screen::Options {
            self.menu.close();
        }
        self.v.grab_mouse(s == Screen::Play);
        self.v.text_input(s == Screen::Chat || s == Screen::Sign || s == Screen::Creative || s == Screen::Inventory || s == Screen::Book);
        if s != Screen::Play {
            self.left_down = false;
            self.right_down = false;
        }
        self.screen = s;
    }

    fn key(&mut self, g: &mut Game, sc: usize) -> Result<bool, String> {
        match self.screen {
            Screen::Chat => {
                if let crate::chatui::Act::Close = self.chat_ui.key(g, sc, &self.keys, &self.v)? {
                    self.leave_bed(g)?;
                    self.set_screen(Screen::Play);
                }
            }
            Screen::Sign => {
                if let Some(gui) = self.gui.as_ref() {
                    if crate::signs::key(g, gui, sc, &self.keys, &self.v)? {
                        self.set_screen(Screen::Play);
                    }
                }
            }
            Screen::Options => {
                if sc == K_ESC && self.menu.back() {
                    self.set_screen(Screen::Pause);
                }
            }
            Screen::Links => {
                if sc == K_ESC {
                    if let crate::serverhud::LinkAct::Close = crate::serverhud::links_escape() {
                        self.set_screen(Screen::Pause);
                    }
                }
            }
            Screen::Pause => match sc {
                K_ESC => self.set_screen(Screen::Play),
                K_Q => return Ok(true),
                _ => {}
            },
            Screen::Book => {
                if let (Some(b), Some(gui)) = (self.book.as_mut(), self.gui.as_ref()) {
                    if crate::books::key(g, gui, b, sc, &self.keys, &self.v)? {
                        self.book = None;
                        self.set_screen(Screen::Play);
                    }
                }
            }
            Screen::Inventory => {
                if crate::menus::key(g, sc, &self.keys, &self.v)? {
                    return Ok(false);
                }
                let bound = self.menu.keys.find_kb(sc);
                if sc == K_ESC || bound == Some(B::Inventory) {
                    self.close_inventory(g)?;
                } else if let Some(gui) = self.gui.as_ref() {
                    // Over a slot: the hotbar keys swap with it, Drop throws (Ctrl the stack),
                    // Swap Item With Off Hand swaps with the off hand (slot 40).
                    let win = screens::window(g);
                    if let Some(slot) = screens::slot_at(gui, &win, self.mouse.0, self.mouse.1) {
                        if let Some(i) = bound.and_then(|b| b.hotbar_slot()) {
                            g.click(win.container_id, slot, i as i8, 2)?;
                        } else if bound == Some(B::Drop) {
                            g.click(win.container_id, slot, if self.keys[K_LCTRL] { 1 } else { 0 }, 4)?;
                        } else if bound == Some(B::SwapOffhand) {
                            g.click(win.container_id, slot, 40, 2)?;
                        }
                    }
                }
            }
            Screen::Creative => {} // creative_event has it
            Screen::Dead => {
                if sc == K_R && !self.respawn_sent {
                    self.ui_click();
                    self.respawn_sent = true;
                    g.respawn()?;
                }
            }
            Screen::Play => {
                if sc == K_ESC {
                    self.set_screen(Screen::Pause);
                } else if sc == K_F3 {
                    self.debug = !self.debug;
                } else if sc == K_A && self.keys[K_F3] {
                    self.reload_chunks(g);
                } else if let Some(b) = self.menu.keys.find_kb(sc) {
                    self.play_key(g, b)?;
                }
            }
        }
        Ok(false)
    }

    /// A bound key pressed in the game (Minecraft.handleKeybinds, once per press).
    fn play_key(&mut self, g: &mut Game, b: B) -> Result<(), String> {
        if let Some(slot) = b.hotbar_slot() {
            // Creative: Save Hotbar Activator (C) with a number saves the hotbar, Load (X) loads it.
            let d = self.gui.as_ref().and_then(|x| x.creative.as_ref()).filter(|_| g.game_mode == 1);
            match d {
                Some(d) if self.held(B::SaveToolbar) || self.held(B::LoadToolbar) => {
                    self.creative.hotbar_load_or_save(d, g, slot, self.held(B::LoadToolbar))?
                }
                _ => g.select_slot(slot as i16)?,
            }
            return Ok(());
        }
        match b {
            // The key that opens the chat also arrives as text: its character is dropped.
            B::Chat => {
                self.chat_ui.open(g, "");
                self.set_screen(Screen::Chat);
                self.skip_text = self.bound_char(B::Chat).map(|c| (c, sys::now_ms()));
            }
            B::Command => {
                self.chat_ui.open(g, "/");
                self.set_screen(Screen::Chat);
                self.skip_text = self.bound_char(B::Command).map(|c| (c, sys::now_ms()));
            }
            B::Inventory => self.open_inventory(g, 'e')?,
            B::Drop => {
                if g.held().is_some() {
                    // Drop Selected Item; with Ctrl the whole stack (ServerboundPlayerActionPacket).
                    let all = self.keys[K_LCTRL];
                    g.dig(if all { 4 } else { 5 }, 0, 0, 0, 0)?;
                }
            }
            B::SwapOffhand => g.dig(7, 0, 0, 0, 0)?,
            B::Fullscreen => {
                let f = !self.fullscreen;
                self.menu.set(crate::options::O::Fullscreen, f as i32 as f64);
                self.menu.o.save();
            }
            B::ToggleGui => self.hide_hud = !self.hide_hud,
            B::Perspective => self.perspective = (self.perspective + 1) % 3,
            B::Screenshot => self.take_shot = true,
            B::Jump => {
                // A double tap of jump flies in creative (and spectator) mode.
                let now = sys::now_ms();
                if g.may_fly && now - self.last_space < 300 {
                    let f = !g.flying;
                    g.set_flying(f)?;
                    self.last_space = 0;
                } else {
                    self.last_space = now;
                }
            }
            B::Forward => {
                // A double tap of forward starts a sprint.
                let now = sys::now_ms();
                if now - self.last_w < 300 {
                    self.double_w = true;
                }
                self.last_w = now;
            }
            _ => {}
        }
        Ok(())
    }

    /// Puts changed options into effect (OptionInstance's value-update listeners).
    fn apply_options(&mut self, g: &mut Game) -> Result<(), String> {
        use crate::options::{O, VOLUMES};
        let mut info = false;
        for o in core::mem::take(&mut self.menu.changed) {
            let opts = &self.menu.o;
            match o {
                O::RenderDistance => {
                    self.view = opts.int(o);
                    info = true;
                }
                O::Fullscreen | O::ExclusiveFullscreen => {
                    self.fullscreen = opts.on(O::Fullscreen);
                    let mode = if !self.fullscreen {
                        0
                    } else if opts.on(O::ExclusiveFullscreen) {
                        sdl::WINDOW_FULLSCREEN
                    } else {
                        sdl::WINDOW_FULLSCREEN_DESKTOP
                    };
                    unsafe { (self.v.sdl.SDL_SetWindowFullscreen)(self.v.window, mode) };
                }
                O::Vsync => unsafe {
                    (self.v.sdl.SDL_GL_SetSwapInterval)(opts.on(o) as i32);
                },
                O::GuiScale => {
                    if let Some(gui) = self.gui.as_mut() {
                        gui.fixed_scale = opts.gui_scale();
                    }
                }
                O::Cape | O::Jacket | O::LeftSleeve | O::RightSleeve | O::LeftPants | O::RightPants | O::Hat | O::MainHand
                | O::ChatVisibility | O::ChatColors | O::AllowServerListing | O::Particles => info = true,
                _ if VOLUMES.contains(&o) => {
                    let v = opts.volumes();
                    if let Some(a) = self.audio.as_mut() {
                        a.set_volumes(v);
                    }
                }
                _ => {}
            }
        }
        if info {
            g.conn.send(crate::ids::play::S_CLIENT_INFORMATION, &crate::options::client_information(self.view as i8))?;
        }
        Ok(())
    }

    fn fluid_at(&self, g: &Game) -> u8 {
        let p = self.mover.pos;
        let k = g.blocks.kind(g.world.get(math::ifloor(p[0]), math::ifloor(p[1] + 0.4), math::ifloor(p[2])));
        if k == blocks::WATER || k == blocks::LAVA {
            k
        } else {
            0
        }
    }

    fn tick(&mut self, g: &mut Game) -> Result<(), String> {
        g.ui.tick();
        let own = g.name.clone();
        g.ui.refresh_tags(&own);
        let eye = [self.mover.pos[0], self.mover.pos[1] + self.eye_height(), self.mover.pos[2]];
        self.weather.tick(g, eye);
        self.r.lightmap.tick();
        crate::menus::tick(g)?;
        // A written book the server opened (ClientboundOpenBook).
        if let Some(hand) = g.menus.open_book.take() {
            if let Some(b) = crate::books::open_view(g, hand) {
                self.book = Some(b);
                self.set_screen(Screen::Book);
            }
        }
        // Screens follow what the server does: containers it opens or closes, death, respawn.
        if g.container.is_some() && !self.container_screen {
            self.container_screen = true;
            self.set_screen(Screen::Inventory);
        } else if g.container.is_none() && self.container_screen {
            self.container_screen = false;
            if self.screen == Screen::Inventory {
                self.set_screen(Screen::Play);
            }
        }
        // The server opened a sign's editor (after placing one, or using one); typing for tests.
        if let Some((pos, front)) = g.block_entities.open_request.take() {
            if crate::signs::open_editor(g, pos, front) {
                self.set_screen(Screen::Sign);
            }
        }
        if self.screen == Screen::Sign {
            if !crate::signs::editor_valid(g) {
                crate::signs::close(g)?;
                self.set_screen(Screen::Play);
            } else if let (true, Some(gui)) = (self.test_keys.contains('N') || self.test_keys.contains('M'), self.gui.as_ref()) {
                if crate::signs::test_type(g, gui, self.test_keys.contains('N'))? {
                    self.test_keys = self.test_keys.replace('R', "");
                    self.set_screen(Screen::Play);
                }
            }
        }
        // Tests: X respawns straight away.
        if g.dead && self.test_keys.contains('X') && !self.respawn_sent {
            self.respawn_sent = true;
            g.respawn()?;
        }
        if !g.dead {
            g.ui.death_message = None;
            // Alive again: the next death may respawn too (immediate_respawn, tests).
            self.respawn_sent = false;
        }
        // The immediate_respawn game rule: no death screen (LocalPlayer.respawn straight away).
        if g.dead && g.ui.immediate_respawn {
            if !self.respawn_sent {
                self.respawn_sent = true;
                g.respawn()?;
            }
        } else if g.dead && self.screen != Screen::Dead {
            self.respawn_sent = false;
            self.set_screen(Screen::Dead);
        } else if !g.dead && self.screen == Screen::Dead {
            self.set_screen(Screen::Play);
        }
        // Another screen took the sign editor's place (death, a container): its text still goes
        // to the server (AbstractSignEditScreen.removed).
        if self.screen != Screen::Sign && g.block_entities.editor.is_some() {
            crate::signs::close(g)?;
        }
        // PENRYN_TEST_INPUT "!": respawn from the death screen.
        if self.test_keys.contains('!') && self.screen == Screen::Dead && !self.respawn_sent {
            self.respawn_sent = true;
            g.respawn()?;
        }
        // The held item's name shows for 2 s when it changes.
        let held = (g.selected, g.held());
        if held != self.last_held {
            if held.1.is_some() && (held.0 != self.last_held.0 || held.1.map(|h| h.0) != self.last_held.1.map(|h| h.0)) {
                self.held_ticks = 40;
            }
            self.last_held = held;
        } else if self.held_ticks > 0 {
            self.held_ticks -= 1;
        }
        if !g.positioned {
            return Ok(());
        }
        // A teleport from the server: the next tick's position is the server's.
        if (g.player.x - self.mover.pos[0]).abs() > 1e-9 || (g.player.y - self.mover.pos[1]).abs() > 1e-9 || (g.player.z - self.mover.pos[2]).abs() > 1e-9 {
            self.teleported(g);
        }
        self.prev_pos = self.mover.pos;
        // The default keys (test input presses them, whatever they are bound to).
        for (c, sc) in [('w', 26), ('a', 4), ('s', 22), ('d', 7), (' ', 44), ('^', K_LCTRL), ('v', K_LSHIFT)] {
            if self.test_keys.contains(c) {
                self.keys[sc] = true;
            }
        }
        if self.test_keys.contains('3') {
            self.debug = true;
        }
        // PENRYN_TEST_SAY, PENRYN_TEST_CHAT: the chat screen and chat lines (chatui.rs).
        if self.chat_ui.test(g, &mut self.mouse, self.gui.as_ref(), &self.v)? && self.screen == Screen::Play {
            self.set_screen(Screen::Chat);
        }
        // G: use (right click) the entity under the crosshair, sneaking with v (tests).
        if self.test_keys.contains('G') && self.screen == Screen::Play && g.loaded_sent && sys::now_ms() / 50 % 20 == 0 {
            if let Some((id, _, at)) = self.pick_entity(g) {
                say!("using entity {} (type {}, sneaking {})", id, g.entities.get(&id).map_or(-1, |e| e.kind), self.keys[K_LSHIFT]);
                g.interact(id, at, self.keys[K_LSHIFT])?;
            }
        }
        if self.test_keys.contains('A') && g.loaded_sent && sys::now_ms() / 50 % 10 == 0 {
            if let Some((id, d, _)) = self.pick_entity(g) {
                say!("attacking entity {} at {:.2}", id, d);
                g.attack(id)?;
            }
        }
        if self.screen == Screen::Creative {
            let act = match self.gui.as_ref().and_then(|x| x.creative.as_ref().map(|d| (x, d))) {
                Some((gui, d)) => self.creative.tick(d, gui, g, &mut self.mouse)?,
                None => crate::creative::Action::None,
            };
            self.creative_action(g, act)?;
        } else if self.screen == Screen::Inventory && !self.container_screen && g.game_mode == 1 && self.gui.as_ref().map_or(false, |x| x.creative.is_some()) {
            // InventoryScreen.containerTick: in creative it becomes the creative screen.
            self.open_inventory_screen(g, '\0');
        }
        if self.test_keys.contains('I') && self.screen == Screen::Play && g.loaded_sent && g.menus.ticks >= self.test_keys.split(',').find_map(|t| t.strip_prefix("rt=")).and_then(|v| v.parse().ok()).unwrap_or(0) {
            self.open_inventory(g, 'i')?;
            if let Some(gui) = self.gui.as_ref() {
                self.mouse = (gui.width / 2.0 - 52.0, gui.height / 2.0 + 60.0);
            }
        }
        // opt=PAGE (pause, main, sounds, video, ...): open that screen once the world is in.
        if let Some(p) = self.test_keys.split(',').find_map(|t| t.strip_prefix("opt=")) {
            if self.screen == Screen::Play && g.loaded_sent && self.test_ticks == 0 {
                self.test_ticks = 1;
                if let Some(page) = crate::options::Page::by_name(p) {
                    self.menu.open(page);
                    self.set_screen(Screen::Options);
                } else if p == "pause" {
                    self.set_screen(Screen::Pause);
                } else if p == "links" {
                    crate::serverhud::open_links();
                    self.set_screen(Screen::Links);
                }
            }
        }
        // click=X:Y (GUI units): a left click on the open screen, one each half second.
        if self.screen != Screen::Play && self.test_keys.contains("click=") {
            self.test_ticks += 1;
            // type=TEXT: typed into the open screen after the first click; ptr=X:Y: where the
            // mouse rests after the last one (menus.rs, books.rs).
            if self.test_ticks == 5 {
                if let Some(t) = self.test_keys.split(',').find_map(|t| t.strip_prefix("type=")).map(String::from) {
                    if self.screen == Screen::Inventory {
                        crate::menus::text(g, &t)?;
                    } else if let (Some(gui), Some(b)) = (self.gui.as_ref(), self.book.as_mut()) {
                        crate::books::text(gui, b, &t);
                    }
                }
            }
            if self.test_ticks % 10 == 1 {
                let clicks: Vec<String> = self.test_keys.split(',').filter_map(|t| t.strip_prefix("click=")).map(String::from).collect();
                if let Some((x, y)) = clicks.get(self.test_ticks / 10 - 1).and_then(|c| c.split_once(':')) {
                    self.mouse = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                    say!("test click at {} {}", x, y);
                    if self.screen_click(g, 1)? {
                        return Err(String::from("left by a test click"));
                    }
                    if let crate::options::Ev::Click = self.menu.release() {
                        self.ui_click();
                    }
                }
                if self.test_ticks / 10 == clicks.len() + 1 && self.test_ticks % 10 == 1 {
                    if let Some((x, y)) = self.test_keys.split(',').find_map(|t| t.strip_prefix("ptr=")).and_then(|c| c.split_once(':')) {
                        self.mouse = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                    }
                }
            }
        }
        if self.test_keys.contains('L') {
            self.left_down = true;
        }
        // rt=TICKS: R starts that many ticks after joining (menus.rs counts them).
        let rt: u32 = self.test_keys.split(',').find_map(|t| t.strip_prefix("rt=")).and_then(|v| v.parse().ok()).unwrap_or(0);
        if self.test_keys.contains('R') && self.screen == Screen::Play && g.menus.ticks >= rt {
            self.right_down = true;
        }
        for part in self.test_keys.split(',') {
            if let Some(v) = part.strip_prefix("pitch=").and_then(|v| v.parse::<f32>().ok()) {
                g.player.pitch = v;
            }
            if let Some(v) = part.strip_prefix("yaw=").and_then(|v| v.parse::<f32>().ok()) {
                g.player.yaw = v;
            }
            // Tests: the camera (F5's 0, 1 or 2) and the HUD (F1's hidden or not).
            if let Some(v) = part.strip_prefix("persp=").and_then(|v| v.parse::<u8>().ok()) {
                self.perspective = v % 3;
            }
            if let Some(v) = part.strip_prefix("hud=").and_then(|v| v.parse::<u8>().ok()) {
                self.hide_hud = v == 0;
            }
        }
        let play = self.screen == Screen::Play && !g.dead;
        let (kf, kb, kl, kr, kj) = (
            play && self.held(B::Forward),
            play && self.held(B::Back),
            play && self.held(B::Left),
            play && self.held(B::Right),
            play && self.held(B::Jump),
        );
        let sneak_key = play && self.sneaking();
        let mut inp = Input {
            forward: (kf as i32 - kb as i32) as f64 * 0.98,
            strafe: (kl as i32 - kr as i32) as f64 * 0.98,
            jump: kj,
            sneak: sneak_key,
            shift: sneak_key,
            sprint: false,
            // Inventory slot 8: the feet.
            boots: g.inventory[8].and_then(|(i, _)| g.blocks.items.get(i as usize)).map_or(false, |n| n == "leather_boots"),
            move_speed: g.move_speed,
            fly_speed: g.fly_speed,
            jump_strength: g.jump_strength,
        };
        // LocalPlayer: keep crouching where standing up does not fit (a 1.5-high gap).
        let shift = inp.sneak;
        if !shift && !g.flying && g.world.loaded(math::ifloor(self.mover.pos[0]), math::ifloor(self.mover.pos[2])) {
            let p = self.mover.pos;
            if !crate::physics::fits(&g.world, &g.blocks, p, crate::physics::HEIGHT)
                && crate::physics::fits(&g.world, &g.blocks, p, crate::physics::SNEAK_HEIGHT)
            {
                inp.sneak = true;
            }
        }
        // Sprinting: Ctrl or a double tap of W, forward only, with food above 6.
        let sprint_key = play && if self.menu.o.on(crate::options::O::ToggleSprint) { self.sprint_toggle } else { self.held(B::Sprint) };
        let want_sprint = (sprint_key || self.double_w) && inp.forward > 0.0 && !inp.sneak && (g.food > 6 || g.may_fly);
        if !kf {
            self.double_w = false;
        }
        let sprint = want_sprint && !(self.mover.horizontal_collision && self.mover.on_ground && !self.sprinting) && self.special.sprint_allowed(self.sprinting, kf);
        if sprint != self.sprinting {
            self.sprinting = sprint;
            g.command(if sprint { 1 } else { 2 })?;
        }
        inp.sprint = self.sprinting;
        // PENRYN_TEST_INPUT "J": jump held 10 ticks out of every 40 (a horse's jump bar, an
        // elytra's start).
        let pulse = self.test_keys.contains('J') && g.game_time % 40 < 10;
        // Swimming, the pose's box, crawling, a glide starting (special.rs).
        let on_vehicle = g.riding.vehicle_of.contains_key(&g.entity_id);
        self.special.before(g, &mut self.mover, &mut inp, kj || pulse, shift, on_vehicle)?;
        if inp.sneak && !g.flying {
            inp.forward *= 0.3;
            inp.strafe *= 0.3;
        }
        if self.using.is_some() {
            inp.forward *= 0.2;
            inp.strafe *= 0.2;
            inp.sprint = false;
        }
        let flags = (kf as u8) | (kb as u8) << 1 | (kl as u8) << 2 | (kr as u8) << 3
            | (inp.jump as u8) << 4 | (shift as u8) << 5 | (self.sprinting as u8) << 6;
        if flags != self.last_input {
            self.last_input = flags;
            g.send_input(flags)?;
        }
        let keys = crate::riding::Keys { forward: kf, back: kb, left: kl, right: kr, jump: kj || pulse, sprint: self.sprinting };
        let riding = match self.entities.as_ref() {
            Some(t) => self.ride.tick(g, t, keys, &mut self.mover)?,
            None => false,
        };
        if riding {
            // A passenger does not move itself (Entity.rideTick).
        } else if g.world.loaded(math::ifloor(self.mover.pos[0]), math::ifloor(self.mover.pos[2])) && g.loaded_sent {
            let fluid = self.fluid_at(g);
            self.mover.effects = g.effects.movement();
            if g.game_mode == crate::gamemode::SPECTATOR {
                crate::gamemode::spectator_tick(&mut self.mover, &inp, g.player.yaw);
            } else {
                let rocket = self.entities.as_ref().and_then(|t| t.types.iter().position(|x| x.name == "firework_rocket")).map_or(-1, |i| i as i32);
                if !self.special.travel(g, &mut self.mover, &inp, fluid, rocket) {
                    self.mover.tick(&g.world, &g.blocks, &inp, g.player.yaw, g.flying, fluid);
                    // Player.updatePlayerPose: crouching on the ground when sneaking, swimming
                    // while sprinting in water.
                    if !self.info_sent {
                        // The skin parts and the rest of the client information, in play too: the
                        // server keeps what the player last sent for other players' models.
                        self.info_sent = true;
                        g.conn.send(crate::ids::play::S_CLIENT_INFORMATION, &crate::options::client_information(self.view as i8))?;
                    }
                    self.own_in_water = fluid == crate::blocks::WATER;
                    self.own_pose = if self.sneaking() && self.mover.on_ground && !g.flying {
                        crate::poses::CROUCHING
                    } else if self.own_in_water && self.sprinting && !g.flying {
                        crate::poses::SWIMMING
                    } else {
                        0
                    };
                }
                crate::serverhud::collide(&mut self.mover, self.prev_pos, &g.ui);
            }
            // LocalPlayer.aiStep: landing ends flight (except in spectator mode).
            if g.flying && self.mover.on_ground && g.game_mode != 3 {
                g.set_flying(false)?;
            }
        }
        // Block entities tick after the player: moving pistons and shulker lids push it.
        if !riding {
            crate::moving::tick(&mut g.world, &g.blocks, &mut self.mover, inp.sneak, g.flying);
        }
        g.effects.tick();
        g.player.x = self.mover.pos[0];
        g.player.y = self.mover.pos[1];
        g.player.z = self.mover.pos[2];
        // Walking (Player.aiStep's bob and walkDist), the swing and the equip animation.
        let (dx, dz) = (self.mover.pos[0] - self.prev_pos[0], self.mover.pos[2] - self.prev_pos[2]);
        let speed = math::sqrt(dx * dx + dz * dz) as f32;
        self.prev_walk_dist = self.walk_dist;
        self.walk_dist += speed * 0.6;
        // The body turns to the way the player walks (or stays), 30% of the way a tick; the head
        // may turn 50 degrees from it at most, then the body follows (Mob.tickHeadTurn).
        let yaw = g.player.yaw;
        let (_, mut body) = self.own_body.unwrap_or((yaw, yaw));
        let old = body;
        let mut turn_to = body;
        if speed > 0.0005 {
            let walk = (math::atan2(dz as f64, dx as f64) * (180.0 / math::PI)) as f32 - 90.0;
            let diff = (crate::entities::wrap_deg(yaw) - walk).abs();
            turn_to = if diff > 95.0 && diff < 265.0 { walk - 180.0 } else { walk };
        }
        body += crate::entities::wrap_deg(turn_to - body) * 0.3;
        let head_diff = crate::entities::wrap_deg(yaw - body);
        if head_diff.abs() > 50.0 {
            body += head_diff - head_diff.signum() * 50.0;
        }
        self.own_body = Some((old, body));
        // Entity.move: a step sound each block walked on the ground (moveDist > nextStep).
        if self.mover.on_ground && !g.flying {
            self.move_dist += speed * 0.6;
            if self.move_dist > self.next_step {
                self.next_step = self.move_dist as i32 as f32 + 1.0;
                let p = self.mover.pos;
                let (bx, by, bz) = (math::ifloor(p[0]), math::ifloor(p[1] - 0.2), math::ifloor(p[2]));
                let s = g.world.get(bx, by, bz);
                if s != 0 {
                    self.block_sound(g, s, 1, [p[0], p[1], p[2]], 0.15, 1.0);
                }
            }
        }
        // MusicManager: the game's music, then a pause of 10 to 20 minutes.
        if let Some(a) = self.audio.as_mut() {
            if !a.music_playing() {
                self.music_delay -= 1;
                if self.music_delay <= 0 {
                    let ev = if g.game_mode == 1 { "music.creative" } else { "music.game" };
                    a.play_named(ev, None, 1.0, 1.0, crate::audio::MUSIC);
                    self.music_delay = self.menu.o.music_delay(sys::now_ms() as u32);
                }
            }
        }
        // GameRenderer.tickFov: halfway to the player's FOV modifier each tick.
        self.prev_fov_mod = self.fov_mod;
        let target = crate::options::fov_modifier(g.flying, g.move_speed, self.sprinting, self.menu.o.get(crate::options::O::FovEffect));
        self.fov_mod = (self.fov_mod + (target - self.fov_mod) * 0.5).clamp(0.1, 1.5);
        self.prev_bob = self.bob;
        let target = if self.mover.on_ground && !g.flying { speed.min(0.1) } else { 0.0 };
        self.bob += (target - self.bob) * 0.4;
        if g.swung {
            g.swung = false;
            if self.swing_ticks < 0 || self.swing_ticks >= 3 {
                self.swing_ticks = 0;
            }
        } else if self.swing_ticks >= 0 {
            self.swing_ticks += 1;
            if self.swing_ticks >= 6 {
                self.swing_ticks = -1;
            }
        }
        self.prev_equip = self.equip;
        let held_now = g.held().map(|h| h.0);
        if held_now != self.held_item {
            self.held_item = held_now;
            self.equip = 1.0;
        } else {
            self.equip = (self.equip - 0.4).max(0.0);
        }
        g.player.on_ground = self.mover.on_ground;
        // ---- the in-bed screen (Minecraft.tick opens it while the player sleeps)
        if !g.sleeping {
            self.bed_left = false;
            if self.in_bed {
                self.in_bed = false;
                if self.screen == Screen::Chat {
                    self.set_screen(Screen::Play);
                }
            }
        } else if !self.in_bed && !self.bed_left && self.screen == Screen::Play {
            self.chat_ui.open(g, "");
            self.in_bed = true;
            self.set_screen(Screen::Chat);
        }
        // ---- hands
        if self.destroy_delay > 0 {
            self.destroy_delay -= 1;
        }
        if self.use_delay > 0 {
            self.use_delay -= 1;
        }
        let hit = if play { self.pick(g) } else { None };
        // Minecraft.handleKeybinds: a use ends as soon as the use key is up (a screen opening
        // releases every key, and its mouse-up never reaches the game).
        if self.using.is_some() && (!self.right_down || !play) {
            // A riptide trident throws the player (TridentItem.releaseUsing).
            if let Some((t, _)) = self.using {
                self.special.release_use(g, &mut self.mover, t, g.rain_level > 0.2);
            }
            self.using = None;
            g.dig(RELEASE_USE_ITEM, 0, 0, 0, 0)?;
        }
        if !(self.left_down && play) {
            if let Some(m) = self.mining.take() {
                g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], 0)?;
            }
        }
        if self.left_down && play && g.game_mode != crate::gamemode::SPECTATOR {
            self.dig_tick(g, hit)?;
        }
        // Using an item: food finishes on its own, then starts again while the button is held.
        if let Some((t, d)) = self.using.as_mut() {
            *t += 1;
            if *d > 0 && *t >= *d {
                self.using = None;
                self.use_delay = 4;
            }
        }
        if self.right_down && play && self.use_delay == 0 && self.using.is_none() {
            self.use_delay = 4;
            let sneak = self.sneaking();
            let bname = hit.map(|h| g.blocks.of_state(g.world.get(h.pos[0], h.pos[1], h.pos[2])).name.clone());
            // Minecraft.startUseItem: each hand in turn, the main hand first. A hand tries the
            // block (useItemOn) and, when that passes, the item (useItem); a hand whose action
            // is consumed ends the loop, a pass goes on to the off hand.
            for hand in 0..2 {
                let stack = g.hand_item(hand);
                let name = stack.and_then(|(i, _)| g.blocks.items.get(i as usize)).cloned().unwrap_or_default();
                if let (Some(h), Some(bn)) = (hit, bname.as_deref()) {
                    let c = [
                        (h.point[0] - h.pos[0] as f64) as f32,
                        (h.point[1] - h.pos[1] as f64) as f32,
                        (h.point[2] - h.pos[2] as f64) as f32,
                    ];
                    g.place_with(hand, h.pos[0], h.pos[1], h.pos[2], h.face, c, false)?;
                    // The result the client predicts: the block's own use (skipped when sneaking
                    // with something in hand), then what the item does to the block.
                    let consumed = (!(sneak && stack.is_some()) && interactive(bn)) || (stack.is_some() && use_on_consumes(g.blocks.blocks.iter().any(|b| b.name == name), &name, bn));
                    if consumed {
                        g.punch()?;
                        if name == "brush" {
                            self.using = Some((0, 0));
                        }
                        break;
                    }
                }
                if stack.is_none() || g.game_mode == crate::gamemode::SPECTATOR {
                    continue;
                }
                g.use_item_with(hand)?;
                let usable = use_duration(&name);
                if let Some(d) = usable {
                    self.using = Some((0, d));
                }
                // WritableBookItem.use: the client opens its own edit screen.
                if hand == 0 {
                    if let Some(b) = crate::books::open_edit(g) {
                        self.book = Some(b);
                        self.set_screen(Screen::Book);
                    }
                }
                if usable.is_some() {
                    break;
                }
                if use_acts(&name) {
                    g.punch()?;
                    break;
                }
            }
        }
        self.particle_tick(g, play, hit);
        g.tick()?;
        if let Some(t) = self.entities.as_ref() {
            self.ride.after_tick(g, t);
        }
        Ok(())
    }

    /// What the server sent for the particle engine (the explosion's push goes to the player).
    fn particle_effects(&mut self, g: &mut Game) {
        let Some(pt) = self.particles.as_mut() else {
            g.particle_events.clear();
            return;
        };
        let eye = [self.mover.pos[0], self.mover.pos[1] + physics::EYE, self.mover.pos[2]];
        // Each entity type's hitbox (EntityType.dimensions), for death poofs.
        let sizes: Vec<(f32, f32)> = self.entities.as_ref().map_or(Vec::new(), |t| t.types.iter().map(|ty| ty.size).collect());
        if let Some(k) = pt.handle(g, self.gui.as_ref(), &sizes, eye) {
            for a in 0..3 {
                self.mover.vel[a] += k[a];
            }
        }
    }

    /// The particles' tick: digging cracks on the face being hit, sprinting and eating.
    fn particle_tick(&mut self, g: &Game, play: bool, hit: Option<Hit>) {
        let in_fluid = self.fluid_at(g) != 0;
        let eye_y = self.eye_height();
        let Some(pt) = self.particles.as_mut() else { return };
        if let (true, Some(h)) = (self.left_down && play, hit) {
            pt.crack(&g.world, &g.blocks, h.pos, h.face);
        }
        let eating = match (self.using, g.held()) {
            (Some((t, d)), Some((item, _))) if d > 0 => Some((item, t, d)),
            _ => None,
        };
        let fx = crate::particles::PlayerFx {
            pos: self.mover.pos,
            vel: self.mover.vel,
            eye: [self.mover.pos[0], self.mover.pos[1] + eye_y, self.mover.pos[2]],
            yaw: g.player.yaw,
            pitch: g.player.pitch,
            sprinting: self.sprinting && !g.flying,
            on_ground: self.mover.on_ground,
            in_fluid,
            eating,
        };
        pt.tick(g, self.gui.as_ref(), &fx);
    }

    fn dig_tick(&mut self, g: &mut Game, hit: Option<Hit>) -> Result<(), String> {
        // continueDestroyBlock: the block's hit sound every 4 ticks, (volume + 1) / 8 at pitch x 0.5.
        if let Some(m) = self.mining.as_ref() {
            if m.ticks % 4 == 0 && hit.map_or(false, |h| h.pos == m.pos) {
                let p = m.pos;
                let s = g.world.get(p[0], p[1], p[2]);
                let v = g.blocks.of_state(s).sound_volume;
                self.block_sound(g, s, 3, [p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5], (v + 1.0) / 8.0 / v.max(0.01), 0.5);
            }
        }
        let Some(h) = hit else {
            if let Some(m) = self.mining.take() {
                g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], 0)?;
            }
            return Ok(());
        };
        if self.punch_timer <= 0 {
            g.punch()?;
            self.punch_timer = 3;
        }
        self.punch_timer -= 1;
        if self.destroy_delay > 0 {
            return Ok(());
        }
        let face = h.face as u8;
        // Player.blockActionRestricted: adventure mode breaks only what can_break allows.
        if !crate::tooltip::may_break(g, g.world.get(h.pos[0], h.pos[1], h.pos[2])) {
            if let Some(m) = self.mining.take() {
                g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], face)?;
            }
            return Ok(());
        }
        let creative = g.game_mode == 1;
        if creative {
            let held = g.held().and_then(|(i, _)| g.blocks.items.get(i as usize)).cloned().unwrap_or_default();
            if held.ends_with("_sword") || held == "trident" || held == "mace" {
                return Ok(()); // vanilla: these cannot break blocks in creative
            }
            g.dig(DIG_START, h.pos[0], h.pos[1], h.pos[2], face)?;
            self.break_locally(g, h.pos);
            self.destroy_delay = 5;
            return Ok(());
        }
        let per_tick = self.dig_speed(g, h.pos);
        match &mut self.mining {
            Some(m) if m.pos == h.pos => {
                m.progress += per_tick;
                m.ticks += 1;
                if m.progress >= 1.0 {
                    let p = m.pos;
                    self.mining = None;
                    g.dig(DIG_STOP, p[0], p[1], p[2], face)?;
                    self.break_locally(g, p);
                    self.destroy_delay = 5;
                }
            }
            other => {
                if let Some(m) = other.take() {
                    g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], face)?;
                }
                g.dig(DIG_START, h.pos[0], h.pos[1], h.pos[2], face)?;
                if per_tick >= 1.0 {
                    self.break_locally(g, h.pos); // instant (grass, flowers, torches...)
                } else {
                    self.mining = Some(Mining { pos: h.pos, progress: per_tick, ticks: 0 });
                }
            }
        }
        Ok(())
    }

    /// BlockState.getDestroyProgress for the held item (no enchantments or effects).
    fn dig_speed(&self, g: &Game, pos: [i32; 3]) -> f64 {
        let s = g.world.get(pos[0], pos[1], pos[2]);
        let st = g.blocks.state(s);
        if st.hardness == 65535 {
            return 0.0;
        }
        let hardness = st.hardness as f64 / 100.0;
        if hardness <= 0.0 {
            return 1.0;
        }
        let blk = g.blocks.of_state(s);
        let held = g.held().and_then(|(i, _)| g.blocks.items.get(i as usize)).map(|s| s.as_str()).unwrap_or("");
        let (kind, mut speed, tier) = tool_of(held);
        let right_tool = kind != 0 && blk.mineable & kind != 0;
        if !right_tool {
            speed = 1.0;
        }
        if held == "shears" && (blk.name.ends_with("leaves") || blk.name == "cobweb" || blk.name.ends_with("_wool")) {
            speed = if blk.name.ends_with("_wool") { 5.0 } else { 15.0 };
        }
        if kind == 16 && blk.name == "cobweb" {
            speed = 15.0;
        }
        let harvest = st.flags & blocks::NEEDS_TOOL == 0
            || (right_tool && blk.incorrect & tier == 0)
            || (held == "shears" && blk.name == "cobweb");
        if self.fluid_at(g) == blocks::WATER {
            speed /= 5.0;
        }
        if !self.mover.on_ground && !g.flying {
            speed /= 5.0;
        }
        speed / hardness / if harvest { 30.0 } else { 100.0 }
    }

    /// A block's sound (`which`: 0 break, 1 step, 2 place, 3 hit, 4 fall) at a block.
    fn block_sound(&mut self, g: &Game, state: u16, which: usize, pos: [f64; 3], volume: f32, pitch: f32) {
        let blk = g.blocks.of_state(state);
        let id = blk.sounds[which];
        if id == 0xFFFF {
            return;
        }
        let (v, p) = (blk.sound_volume, blk.sound_pitch);
        if let Some(a) = self.audio.as_mut() {
            a.play(id as u32, Some(pos), v * volume, p * pitch, if which == 1 { 7 } else { 4 });
        }
    }

    fn ui_click(&mut self) {
        if let Some(a) = self.audio.as_mut() {
            a.play_named("ui.button.click", None, 0.25, 1.0, 10);
        }
    }

    fn break_locally(&mut self, g: &mut Game, p: [i32; 3]) {
        // MultiPlayerGameMode.destroyBlock: the break sound, (volume + 1) / 2 at pitch x 0.8.
        let state = g.world.get(p[0], p[1], p[2]);
        let blk = g.blocks.of_state(state);
        let (v, pi, id) = (blk.sound_volume, blk.sound_pitch, blk.sounds[0]);
        if id != 0xFFFF {
            if let Some(a) = self.audio.as_mut() {
                a.play(id as u32, Some([p[0] as f64 + 0.5, p[1] as f64 + 0.5, p[2] as f64 + 0.5]), (v + 1.0) / 2.0, pi * 0.8, 4);
            }
        }
        if sys::env("PENRYN_DEBUG").is_some() {
            let s = g.world.get(p[0], p[1], p[2]);
            say!("broke {} at {} {} {} (on ground {})", g.blocks.of_state(s).name, p[0], p[1], p[2], self.mover.on_ground);
        }
        if let Some(pt) = self.particles.as_mut() {
            pt.destroy(&g.world, &g.blocks, p, state);
        }
        let b = &g.blocks;
        g.world.set(p[0], p[1], p[2], 0, &|s| b.kind(s) == blocks::AIR);
    }

    /// The entity under the crosshair (vanilla's entity reach, 3), when no block is nearer.
    fn pick_entity(&self, g: &Game) -> Option<(i32, f64, [f64; 3])> {
        let table = self.entities.as_ref()?;
        let eye = [self.mover.pos[0], self.mover.pos[1] + self.eye_height(), self.mover.pos[2]];
        let (yr, pr) = (math::to_rad(g.player.yaw as f64), math::to_rad(g.player.pitch as f64));
        let d = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let reach = if g.game_mode == 1 { 5.0 } else { 3.0 };
        let hit = table.pick(&g.entities, eye, d, reach, g.entity_id)?;
        if let Some(b) = self.pick(g) {
            let bd = [b.point[0] - eye[0], b.point[1] - eye[1], b.point[2] - eye[2]];
            if math::sqrt(bd[0] * bd[0] + bd[1] * bd[1] + bd[2] * bd[2]) < hit.1 {
                return None;
            }
        }
        Some(hit)
    }

    /// The block under the crosshair, by the block shapes (vanilla's reach: 4.5, creative 5).
    fn pick(&self, g: &Game) -> Option<Hit> {
        let reach = if g.game_mode == 1 { 5.0 } else { 4.5 };
        let eye = [self.mover.pos[0], self.mover.pos[1] + self.eye_height(), self.mover.pos[2]];
        let (yr, pr) = (math::to_rad(g.player.yaw as f64), math::to_rad(g.player.pitch as f64));
        let d = [-math::sin(yr) * math::cos(pr), -math::sin(pr), math::cos(yr) * math::cos(pr)];
        let mut cell = [math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2])];
        let step = [d[0].signum() as i32, d[1].signum() as i32, d[2].signum() as i32];
        let inv = |v: f64| if v.abs() < 1e-12 { 1e30 } else { 1.0 / v.abs() };
        let mut tmax = [0.0; 3];
        let tdelta = [inv(d[0]), inv(d[1]), inv(d[2])];
        for a in 0..3 {
            let b = if d[a] > 0.0 { cell[a] as f64 + 1.0 - eye[a] } else { eye[a] - cell[a] as f64 };
            tmax[a] = b * tdelta[a];
        }
        for _ in 0..64 {
            let s = g.world.get(cell[0], cell[1], cell[2]);
            let kind = g.blocks.kind(s);
            if s != 0 && kind != blocks::AIR && kind != blocks::WATER && kind != blocks::LAVA {
                let mut best: Option<(f64, i32)> = None;
                let o = Self::outline_offset(&g.blocks, s, cell);
                for bx in g.blocks.shape(s) {
                    let lo = [cell[0] as f64 + bx[0] as f64 / 16.0 + o[0], cell[1] as f64 + bx[1] as f64 / 16.0, cell[2] as f64 + bx[2] as f64 / 16.0 + o[2]];
                    let hi = [cell[0] as f64 + bx[3] as f64 / 16.0 + o[0], cell[1] as f64 + bx[4] as f64 / 16.0, cell[2] as f64 + bx[5] as f64 / 16.0 + o[2]];
                    if let Some((t, f)) = ray_box(eye, d, lo, hi) {
                        if best.map_or(true, |(bt, _)| t < bt) {
                            best = Some((t, f));
                        }
                    }
                }
                if let Some((t, face)) = best {
                    if t <= reach {
                        return Some(Hit { pos: cell, face, point: [eye[0] + d[0] * t, eye[1] + d[1] * t, eye[2] + d[2] * t] });
                    }
                    return None;
                }
            }
            let a = if tmax[0] < tmax[1] { if tmax[0] < tmax[2] { 0 } else { 2 } } else if tmax[1] < tmax[2] { 1 } else { 2 };
            if tmax[a] > reach {
                return None;
            }
            cell[a] += step[a];
            tmax[a] += tdelta[a];
        }
        None
    }

    /// A frame, from the camera entity's eyes while spectating one (ClientboundSetCameraPacket).
    fn draw(&mut self, g: &mut Game, partial: f64) {
        // LevelLoadingScreen until the client is in its world (at most 30 s).
        if !g.loaded_sent {
            let now = sys::now_ms();
            let since = *self.loading_from.get_or_insert(now);
            if let (true, Some(gui)) = (now < since + 30000, self.gui.as_mut()) {
                use crate::options::O;
                crate::title::draw_loading(&self.v, gui, self.menu.o.get(O::PanoramaSpeed), self.menu.o.int(O::MenuBlur));
                return;
            }
        } else {
            self.loading_from = None;
        }
        let cam = g.ui.camera.and_then(|id| g.entities.get(&id)).map(|e| {
            let player = self.entities.as_ref().map_or(true, |t| t.types.get(e.kind as usize).map_or(true, |ty| ty.name == "player"));
            let eye = if player { physics::EYE } else { self.entities.as_ref().and_then(|t| t.types.get(e.kind as usize)).map_or(1.62, |t| t.size.1 as f64 * 0.85) };
            (e.prev, e.pos, e.yaw, e.pitch, eye)
        });
        let Some((prev, pos, yaw, pitch, eye)) = cam else { return self.draw_view(g, partial) };
        let saved = (self.prev_pos, self.mover.pos, g.player.yaw, g.player.pitch);
        let lift = eye - self.eye_height();
        self.prev_pos = [prev[0], prev[1] + lift, prev[2]];
        self.mover.pos = [pos[0], pos[1] + lift, pos[2]];
        (g.player.yaw, g.player.pitch) = (yaw, pitch);
        self.draw_view(g, partial);
        (self.prev_pos, self.mover.pos, g.player.yaw, g.player.pitch) = saved;
    }

    fn draw_view(&mut self, g: &mut Game, partial: f64) {
        let (w, h) = self.v.size();
        let lerp = |a: f64, b: f64| a + (b - a) * partial;
        let mut eye = [
            lerp(self.prev_pos[0], self.mover.pos[0]),
            lerp(self.prev_pos[1], self.mover.pos[1]) + self.eye_height(),
            lerp(self.prev_pos[2], self.mover.pos[2]),
        ];
        // Riding: the camera sits on the seat as the vehicle is drawn.
        if let Some(c) = self.entities.as_ref().and_then(|t| self.ride.camera(g, t, partial)) {
            eye = [c[0], c[1] + self.eye_height(), c[2]];
        }
        // The camera: the eye and angles the world is drawn from (F5 moves them); the cave
        // culling walks from the same eye.
        let (eye, cam_yaw, cam_pitch) = self.camera(g, eye);
        self.r.update(&self.v.gl, &mut g.world, &g.blocks, eye, 6);
        crate::perf::lap(crate::perf::S::Mesh);
        // GameRenderer.getFov: the FOV option times the modifier, narrower under water.
        {
            use crate::options::O;
            let o = &self.menu.o;
            let m = self.prev_fov_mod + (self.fov_mod - self.prev_fov_mod) * partial as f32;
            let in_water = g.blocks.kind(g.world.get(math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2]))) == blocks::WATER;
            let water = if in_water { 1.0 + (6.0 / 7.0 - 1.0) * o.get(O::FovEffect) as f32 } else { 1.0 };
            self.r.fov = o.int(O::Fov) as f32 * m * water;
            self.r.cutout_leaves = o.on(O::CutoutLeaves);
            self.menu.max_gui_scale = crate::options::auto_gui_scale(w, h);
        }
        let clouds_on = self.menu.o.int(crate::options::O::Clouds) != 0;
        // Daylight: the sun's height over the day (dayTime 0 = sunrise, 6000 noon).
        let t = (g.day_time.rem_euclid(24000)) as f64 / 24000.0;
        let sun = math::cos((t - 0.25) * 2.0 * math::PI);
        let day = ((sun * 2.0 + 0.5).clamp(0.0, 1.0)) as f32;
        let dim = g.world.min_y >= 0 && g.world.height == 256;
        let sky = if dim { [0.2, 0.05, 0.05] } else { [0.47 * day + 0.02, 0.65 * day + 0.02, 1.0 * day + 0.06] };
        // The weather greys the sky and fog, dims the daylight and pulls the fog in.
        let (rain, thunder) = if dim { (0.0, 0.0) } else { self.weather.levels(g) };
        let sky = if dim { sky } else { self.weather.sky_colour(sky, rain, thunder) };
        // The weather dims the sky light (the rain columns' light); the clouds and stars have
        // their own weather blends (sky.rs) on the clear day's values.
        let rain_light = self.weather.daylight(day, rain, thunder);
        (self.sky.rain, self.sky.thunder) = (rain, thunder);
        self.r.rain_fog = if dim { 0.0 } else { self.weather.fog };
        // The light map: the dimension's light attributes at the day clock, the weather, the
        // brightness option and the effects.
        {
            use crate::options::O;
            let pf = partial as f32;
            let nv = if g.effects.get(crate::effects::NIGHT_VISION).is_some() { g.effects.night_vision(pf) } else { 0.0 };
            let params = crate::lightmap::Params {
                dimension: g.world.light_env,
                day_time: g.day_time as f64 + g.clock_rate as f64 * partial,
                rain,
                thunder,
                flicker: self.r.lightmap.flicker(),
                gamma: self.menu.o.get(O::Gamma) as f32,
                darkness_blend: g.effects.darkness_blend(pf),
                darkness_option: self.menu.o.get(O::DarknessEffect) as f32,
                tick: g.game_time as f64 - partial,
                night_vision: nv,
                boss: g.ui.darken_at(pf),
            };
            self.r.lightmap.update(&self.v.gl, &params.input());
        }
        let mut world_view = [0f32; 16];
        let world_view_ref = &mut world_view;
        // Blindness and darkness: a near, dark fog and no sky.
        let fog = g.effects.fog((self.view as f32 + 1.0) * 16.0, partial as f32);
        let sky = g.effects.sky(sky, (self.view as f32 + 1.0) * 16.0, partial as f32);
        // Boss bars: world fog (as the nether's) and the darkened sky (FogRenderer).
        let far = (self.view as f32 + 1.0) * 16.0;
        let fog = fog.or_else(|| g.ui.boss_fog().then(|| crate::effects::Fog { start: far * 0.05, end: far.min(192.0) * 0.5, darkness: 0.0 }));
        let dk = 1.0 - 0.3 * g.ui.darken_at(partial as f32);
        let sky = [sky[0] * dk, sky[1] * dk, sky[2] * dk];
        g.maps.upload(&self.v.gl);
        let maps_ref = &g.maps;
        let ents = self.entities.as_ref();
        let gl = &self.v.gl;
        let own = if self.perspective == 0 { g.ui.camera.unwrap_or(g.entity_id) } else { -1 };
        let pf = partial as f32;
        let entities = &g.entities;
        let icons = self.gui.as_ref().map(|gui| &gui.icons[..]).unwrap_or(&[]);
        let overlays = self.gui.as_ref().map(|gui| &gui.overlays[..]).unwrap_or(&[]);
        let (atlas, atlas_size) = (self.r.atlas, self.r.atlas_size);
        let blocks_ref = &g.blocks;
        let block_entities = &g.block_entities;
        let world_ref = &g.world;
        // How things are lit: the world's light through a copy of the light map's table.
        let lm_table = self.r.lightmap.cpu;
        let lit = crate::lightmap::Lit { world: world_ref, table: &lm_table, tex: self.r.lightmap.tex };
        let lit = &lit;
        let gui_ref = self.gui.as_ref();
        let sky_ref = &self.sky;
        let ent_cx = crate::entities::DrawCx {
            variants: &g.variants,
            names: &g.ui.tag_names,
            below: &g.ui.below_names,
            game_time: g.game_time,
            icons,
            blocks: &g.blocks,
            atlas,
            atlas_size,
            gui: gui_ref,
            yaw: cam_yaw,
            pitch: cam_pitch,
            reach: if g.game_mode == 1 { 5.0 } else { 3.0 },
            tan_y: {
                let half = math::to_rad(self.r.fov as f64 / 2.0);
                (math::sin(half) / math::cos(half)) as f32
            },
            aspect: {
                let (vw, vh) = self.v.size();
                vw as f32 / vh as f32
            },
            lit,
        };
        let (day_time, game_time) = (g.day_time, g.game_time as f64 + partial);
        // Clouds reach further than the terrain (vanilla: the cloud range option, 128 by default).
        let radius = (((self.view as f64 + 1.0) * 16.0).max(128.0) * 1.5).min(self.menu.o.int(crate::options::O::CloudRange) as f64 * 16.0);
        crate::skins::frame(gl);
        // Third person: the player's own model, drawn with the others (never in spectator mode).
        let mut mine: alloc::collections::BTreeMap<i32, crate::entities::Entity> = alloc::collections::BTreeMap::new();
        if let (Some(t), 1..=2, false, false) = (ents, self.perspective, g.game_mode == crate::gamemode::SPECTATOR, g.dead) {
            mine.insert(g.entity_id, self.own_model(g, t.player_type));
        }
        let mut parts = self.particles.as_mut();
        self.r.draw_world(
            gl,
            &g.world,
            eye,
            cam_yaw,
            cam_pitch,
            w,
            h,
            self.view,
            sky,
            &mut |view| {
                *world_view_ref = *view;
                if let Some(t) = ents {
                    t.draw(gl, view, eye, entities, pf, own, &ent_cx);
                    crate::perf::lap(crate::perf::S::Entities);
                    crate::skins::draw(gl, view, eye, entities, pf, own, t, &ent_cx.blocks.items, lit);
                    crate::perf::lap(crate::perf::S::EntSkins);
                    if !mine.is_empty() {
                        t.draw(gl, view, eye, &mine, pf, -1, &ent_cx);
                        crate::skins::draw(gl, view, eye, &mine, pf, -1, t, &ent_cx.blocks.items, lit);
                    }
                }
                // Display entities (display.rs) are not dropped items.
                let skip_kinds = ents.map_or([-1, -1], |t| crate::display::kinds(t));
                crate::entities::draw_items(gl, view, eye, entities, pf, icons, overlays, blocks_ref, atlas, atlas_size, lit, &skip_kinds);
                if let Some(t) = ents {
                    crate::display::draw(gl, view, eye, entities, pf, t, &ent_cx, overlays);
                }
                crate::maps::draw_frames(gl, view, eye, entities, maps_ref, icons, overlays, blocks_ref, atlas, atlas_size, lit);
                crate::perf::lap(crate::perf::S::EntItems);
                if let Some(gui) = gui_ref {
                    crate::signs::draw_world(gl, view, eye, block_entities, world_ref, blocks_ref, gui, lit);
                }
                crate::moving::draw(gl, view, eye, world_ref, blocks_ref, atlas, pf, lit);
                crate::perf::lap(crate::perf::S::BlockEntities);
                if let (Some(gui), false, true, None) = (gui_ref, dim, clouds_on, fog) {
                    sky_ref.draw_clouds(gl, view, gui, eye, game_time, day, radius);
                }
                crate::perf::lap(crate::perf::S::Clouds);
                if let Some(pt) = parts.as_deref_mut() {
                    pt.draw(gl, view, eye, pf, atlas, lit);
                }
                crate::perf::lap(crate::perf::S::Particles);
            },
            &mut |view| {
                if let Some(f) = fog.as_ref() {
                    crate::effects::apply_fog(gl, f);
                } else if let (Some(gui), false) = (gui_ref, dim) {
                    sky_ref.draw_celestial(gl, view, gui, day_time, day);
                }
            },
        );
        // Rain, snow and lightning, over the water (vanilla: after the translucent terrain).
        self.weather.draw(&self.v.gl, &world_view, &g.world, &g.blocks, eye, pf, rain, 0.2 + 0.8 * rain_light);
        // The world border's wall (WorldBorderRenderer), after the weather.
        crate::serverhud::draw_wall(&self.v.gl, &world_view, eye, &g.ui, partial, self.view, (self.view as f64 + 1.0) * 16.0);
        crate::perf::lap(crate::perf::S::Weather);
        let hit = if self.screen == Screen::Play { self.pick(g) } else { None };
        if let Some(hh) = hit {
            let s = g.world.get(hh.pos[0], hh.pos[1], hh.pos[2]);
            let boxes: Vec<[u8; 6]> = g.blocks.shape(s).to_vec();
            // Drawn at the block's offset: the same as moving the eye the other way.
            let o = Self::outline_offset(&g.blocks, s, hh.pos);
            let eye_o = [eye[0] - o[0], eye[1], eye[2] - o[2]];
            self.r.draw_outline(&self.v.gl, eye_o, cam_yaw, cam_pitch, hh.pos, &boxes);
            if let Some(m) = self.mining.as_ref().filter(|m| m.pos == hh.pos) {
                let stage = ((m.progress * 10.0) as usize).min(9);
                self.r.draw_cracks(&self.v.gl, eye_o, cam_yaw, cam_pitch, hh.pos, &boxes, g.blocks.crack_uv[stage]);
            }
        }
        // ---- the hand (not a spectator's; third person hides it)
        if let (Some(gui), 0) = (self.gui.as_ref().filter(|_| g.game_mode != crate::gamemode::SPECTATOR), self.perspective) {
            let swing = if self.swing_ticks >= 0 { ((self.swing_ticks as f64 + partial) / 6.0) as f32 } else { 0.0 };
            let equip = self.prev_equip + (self.equip - self.prev_equip) * partial as f32;
            let walk = self.prev_walk_dist + (self.walk_dist - self.prev_walk_dist) * partial as f32;
            let bob = self.prev_bob + (self.bob - self.prev_bob) * partial as f32;
            let skin = crate::entities::EntityTable::skin_index(&g.uuid);
            // A held map (ItemInHandRenderer.renderTwoHandedMap / renderOneHandedMap).
            let map = g.held().filter(|h| crate::menus::item_name(g, h.0) == "filled_map").map(|_| crate::maps::map_id(&g.creative.patches[36 + g.selected as usize]));
            // ItemInHandRenderer: lit by the light at the player's eye.
            let hand_light = lit.at(eye);
            let bob_v = if self.menu.o.on(crate::options::O::BobView) { (-walk, bob) } else { (0.0, 0.0) };
            if !crate::maps::draw_hand(&self.v.gl, w, h, &g.maps, map, g.inventory[45].is_none(), g.player.pitch, swing, equip, bob_v, hand_light) {
            self.hand.draw(
                &self.v.gl,
                w,
                h,
                g.held().map(|h| h.0),
                &g.creative.patches[36 + g.selected as usize],
                gui,
                &g.blocks,
                self.r.atlas,
                self.r.atlas_size,
                &self.r.atlas_alpha,
                self.entities.as_ref(),
                skin,
                swing,
                equip,
                bob_v,
                hand_light,
            );
            }
        }
        crate::perf::lap(crate::perf::S::Hand);
        // ---- 2D: vanilla's HUD and screens
        let Some(mut gui) = self.gui.take() else {
            self.v.swap();
            crate::perf::lap(crate::perf::S::Swap);
            return;
        };
        crate::serverhud::vignette(&self.v.gl, w, h, &g.ui, eye[0], eye[2]);
        gui.begin(w, h);
        let (gw, gh) = (gui.width, gui.height);
        let eye_block = g.blocks.kind(g.world.get(math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2])));
        if eye_block == blocks::WATER {
            gui.fill(0.0, 0.0, gw, gh, 0x6E14_32A0);
        } else if eye_block == blocks::LAVA {
            gui.fill(0.0, 0.0, gw, gh, 0xC8C8_4600);
        }
        let held_name = if self.held_ticks > 0 {
            g.held().map(|(item, _)| {
                let id = g.blocks.items.get(item as usize).cloned().unwrap_or_default();
                (crate::lang::item_name(&id), (self.held_ticks * 256 / 10).clamp(0, 255) as u8)
            })
        } else {
            None
        };
        // Spectators have no hotbar, health or crosshair (Hud: the spectator GUI instead).
        // Toggle GUI (F1) hides the HUD: crosshair, hotbar, health, the held item's name, effects, the server's HUD.
        if g.game_mode != crate::gamemode::SPECTATOR && !self.hide_hud {
            screens::hud(&mut gui, g, self.screen == Screen::Play && !self.debug, held_name);
        }
        if !self.hide_hud {
            g.effects.draw_hud(&self.v.gl, &mut gui, self.screen == Screen::Inventory && g.container.is_none());
            crate::serverhud::hud(&mut gui, &self.v.gl, g, partial as f32);
        }
        let show_chat = self.screen != Screen::Inventory && self.screen != Screen::Creative && self.screen != Screen::Dead && self.screen != Screen::Book;
        crate::chatui::draw(&mut gui, g, &mut self.chat_ui, self.screen == Screen::Chat, show_chat, partial as f32, self.mouse.0, self.mouse.1);
        if self.in_bed && self.screen == Screen::Chat {
            leave_bed_button(&gui).draw(&mut gui, self.mouse.0, self.mouse.1);
        }
        // The player list while Tab is held (PENRYN_TEST_INPUT "P" holds it).
        if self.screen == Screen::Play && (self.held(B::PlayerList) || self.test_keys.contains('P')) {
            crate::serverhud::tab_list(&mut gui, &self.v.gl, g);
        }
        if self.debug {
            let p = self.mover.pos;
            let facing = ["south (Towards positive Z)", "west (Towards negative X)", "north (Towards negative Z)", "east (Towards positive X)"]
                [math::wrap(g.player.yaw as f64 / 90.0 + 0.5, 4.0) as usize % 4];
            let target = hit.map(|hh| {
                let s = g.world.get(hh.pos[0], hh.pos[1], hh.pos[2]);
                format!("Targeted Block: {}, {}, {}  minecraft:{}", hh.pos[0], hh.pos[1], hh.pos[2], g.blocks.of_state(s).name)
            });
            let lines = [
                format!("Minecraft {} (Penryn)", crate::ids::VERSION_NAME),
                format!("{} fps", self.fps),
                format!("C: {}/{} sections, {}k vertices, {} to mesh", self.r.sections_drawn, g.world.columns.len() * (g.world.height / 16) as usize, self.r.vertices_drawn / 1000, g.world.dirty.len()),
                String::new(),
                format!("XYZ: {:.3} / {:.5} / {:.3}", p[0], p[1], p[2]),
                format!("Block: {} {} {}", math::ifloor(p[0]), math::ifloor(p[1]), math::ifloor(p[2])),
                format!("Chunk: {} {} {} in {} {} {}", math::ifloor(p[0]) & 15, math::ifloor(p[1]) & 15, math::ifloor(p[2]) & 15, math::ifloor(p[0]) >> 4, math::ifloor(p[1]) >> 4, math::ifloor(p[2]) >> 4),
                format!("Facing: {} ({:.1} / {:.1})", facing, math::wrap(g.player.yaw as f64 + 180.0, 360.0) - 180.0, g.player.pitch),
                format!("Day {} time {}", g.day_time / 24000, g.day_time.rem_euclid(24000)),
                String::new(),
                target.unwrap_or_default(),
                String::new(),
                self.v.gl_string(sdl::RENDERER),
            ];
            screens::debug(&mut gui, &lines);
        }
        let (mx, my) = self.mouse;
        match self.screen {
            Screen::Pause => {
                let links = (!g.ui.common.links.is_empty()).then(|| crate::serverhud::links_button(&gui));
                screens::draw_pause(&mut gui, mx, my, links.as_ref())
            }
            Screen::Links => crate::serverhud::draw_links(&mut gui, g, mx, my),
            Screen::Options => self.menu.draw(&mut gui, mx, my),
            Screen::Inventory => {
                let win = screens::window(g);
                screens::draw_window(&mut gui, g, &win, mx, my);
                if g.container.is_none() {
                    let (left, top) = screens::window_origin(&gui, &win);
                    if let Some(tip) = g.effects.draw_inventory(&self.v.gl, &mut gui, left, top, win.w, mx, my) {
                        screens::tooltip(&mut gui, &tip, mx, my);
                    }
                }
            }
            Screen::Sign => crate::signs::draw_editor(&mut gui, &self.v.gl, w, h, g, mx, my),
            Screen::Book => {
                if let Some(b) = self.book.as_ref() {
                    crate::books::draw(&mut gui, b, mx, my, (sys::now_ms() / 300) % 2 == 0);
                }
            }
            Screen::Creative => {
                if let Some(d) = gui.creative.take() {
                    self.creative.draw(&d, &mut gui, g, mx, my);
                    gui.creative = Some(d);
                }
            }
            Screen::Dead => {
                let msg = g.chat.last().map(|c| c.0.clone()).unwrap_or_default();
                crate::serverhud::draw_death(&mut gui, g, &msg, mx, my);
            }
            _ => {}
        }
        gui.flush(&self.v.gl, w, h);
        self.gui = Some(gui);
        crate::perf::lap(crate::perf::S::Gui);
        self.v.swap();
        crate::perf::lap(crate::perf::S::Swap);
    }
}

impl crate::packs::Host for App {
    fn ui_frame(&mut self, draw: &mut dyn FnMut(&mut Gui, f32, f32), input: &mut Vec<crate::packs::Input>) -> bool {
        use crate::packs::Input;
        self.v.grab_mouse(false);
        while let Some(e) = self.v.poll() {
            let scale = self.gui.as_ref().map_or(1.0, |g| g.scale);
            match e.u32_at(0) {
                sdl::EV_QUIT => return false,
                sdl::EV_KEYDOWN if e.i32_at(16) as usize == K_ESC => input.push(Input::Escape),
                sdl::EV_MOUSEMOTION => self.mouse = (e.i32_at(20) as f32 / scale, e.i32_at(24) as f32 / scale),
                sdl::EV_MOUSEDOWN if e.0[16] == 1 => {
                    self.mouse = (e.i32_at(20) as f32 / scale, e.i32_at(24) as f32 / scale);
                    input.push(Input::Click(self.mouse.0, self.mouse.1));
                }
                _ => {}
            }
        }
        let (w, h) = self.v.size();
        unsafe {
            (self.v.gl.Viewport)(0, 0, w, h);
            (self.v.gl.ClearColor)(0.06, 0.06, 0.06, 1.0);
            (self.v.gl.Clear)(sdl::COLOR_BUFFER_BIT | sdl::DEPTH_BUFFER_BIT);
        }
        if let Some(mut gui) = self.gui.take() {
            gui.begin(w, h);
            draw(&mut gui, self.mouse.0, self.mouse.1);
            gui.flush(&self.v.gl, w, h);
            self.gui = Some(gui);
        }
        // PENRYN_SHOT also catches these screens (tests).
        if let Some(shot) = self.test_shot.as_ref() {
            if sys::now_ms() >= shot.1 {
                let png = sdl::screenshot_png(&self.v.gl, w, h);
                sys::write_file_private(&shot.0, &png);
                say!("screenshot: {}", shot.0);
                unsafe { sys::exit(0) };
            }
        }
        self.v.swap();
        true
    }

    fn reload(&mut self, names: &[String], world: Option<&mut crate::world::World>, blocks: &mut Blocks) {
        use crate::packs::path;
        // The meshing threads read the block table, which a reload replaces.
        self.r.cancel_meshing();
        let has = |n: &str| names.iter().any(|x| x == n);
        let gl = &self.v.gl;
        if has("blocks.bin") {
            match sys::read_file(&path("blocks.bin")).and_then(|d| Blocks::parse(&d)) {
                Some(b) => *blocks = b,
                None => say!("{} is damaged", path("blocks.bin")),
            }
        }
        if has("atlas.bin") {
            if let Some(d) = sys::read_file(&path("atlas.bin")) {
                unsafe { (gl.DeleteTextures)(1, &self.r.atlas) };
                if let Err(e) = self.r.load_atlas(gl, &d) {
                    say!("{}", e);
                }
            }
        }
        if has("atlas.bin") || has("particles.bin") {
            if let Some(old) = self.particles.as_ref().map(|p| p.sheet()).filter(|&t| t != 0) {
                unsafe { (gl.DeleteTextures)(1, &old) };
            }
            let pd = sys::read_file(&path("particles.bin"));
            self.particles = Some(crate::particles::Particles::load(gl, pd.as_deref(), self.r.atlas_size));
        }
        if has("atlas.bin") || has("gui.bin") || has("items.bin") {
            if let Some(d) = sys::read_file(&path("gui.bin")) {
                match Gui::load(gl, &d, self.r.atlas, self.r.atlas_size) {
                    Ok(mut new) => {
                        new.overlays = crate::itemmodels::read();
                        if let Some(old) = self.gui.take() {
                            new.fixed_scale = old.fixed_scale;
                            unsafe { (gl.DeleteTextures)(1, &old.texture()) };
                        }
                        self.gui = Some(new);
                    }
                    Err(e) => say!("{}", e),
                }
            }
        }
        if has("weather.bin") {
            if let Some(d) = sys::read_file(&path("weather.bin")) {
                let _ = self.weather.parse(&d, Some(gl));
            }
        }
        if has("entities.bin") {
            if let Some(d) = sys::read_file(&path("entities.bin")) {
                match crate::entities::EntityTable::load(gl, &d) {
                    Ok(t) => {
                        if let Some(old) = self.entities.as_ref().map(|e| e.texture()).filter(|&t| t != 0) {
                            unsafe { (gl.DeleteTextures)(1, &old) };
                        }
                        self.entities = Some(t)
                    }
                    Err(e) => say!("{}", e),
                }
            }
        }
        if has("packsounds.bin") {
            if let Some(a) = self.audio.as_mut() {
                a.set_overlay(crate::packs::sound_overlay().as_deref());
            }
        }
        if let Some(w) = world {
            if has("blocks.bin") || has("atlas.bin") {
                crate::packs::remesh(w, blocks);
            }
        }
    }
}

/// Where a ray from `o` along `d` enters the box, and the face (direction id) it enters by.
fn ray_box(o: [f64; 3], d: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> Option<(f64, i32)> {
    let mut tmin: f64 = -1e30;
    let mut tmax: f64 = 1e30;
    let mut face = 0;
    // faces by axis: (negative side, positive side)
    let faces = [(4, 5), (0, 1), (2, 3)];
    for a in 0..3 {
        if d[a].abs() < 1e-12 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = ((lo[a] - o[a]) / d[a], (hi[a] - o[a]) / d[a]);
        let mut f = faces[a].0;
        if t1 > t2 {
            core::mem::swap(&mut t1, &mut t2);
            f = faces[a].1;
        }
        if t1 > tmin {
            tmin = t1;
            face = f;
        }
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    if tmax < 0.0 {
        return None;
    }
    Some((tmin.max(0.0), face))
}

/// Checks: the right click's predicted results (which hand-loop packets follow) and the
/// mounts that have an inventory screen.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut v = Vec::new();
    let yn = |b: bool| if b { "yes" } else { "no" };
    // On the ground (a grass block): a bucket, a pearl and a sword do not act on it, so UseItem follows.
    v.push(("water bucket on ground sends UseItem", yn(!use_on_consumes(false, "water_bucket", "grass_block")).into(), "yes"));
    v.push(("ender pearl on ground sends UseItem", yn(!use_on_consumes(false, "ender_pearl", "stone")).into(), "yes"));
    v.push(("snowball in air acts", yn(use_acts("snowball")).into(), "yes"));
    v.push(("bucket in air acts", yn(use_acts("lava_bucket")).into(), "yes"));
    v.push(("sword use passes to the off hand", yn(!use_acts("iron_sword")).into(), "yes"));
    v.push(("shield blocks", yn(use_duration("shield") == Some(0)).into(), "yes"));
    v.push(("block item places", yn(use_on_consumes(true, "torch", "stone")).into(), "yes"));
    v.push(("axe on stone passes (shield follows)", yn(!use_on_consumes(false, "iron_axe", "stone")).into(), "yes"));
    v.push(("axe on log acts", yn(use_on_consumes(false, "iron_axe", "oak_log")).into(), "yes"));
    v.push(("door is interactive", yn(interactive("oak_door")).into(), "yes"));
    v.push(("lily pad is used, not placed on a block", yn(!use_on_consumes(true, "lily_pad", "water")).into(), "yes"));
    v
}
