//! The game window: input, the 20 Hz tick (movement, digging, placing), and drawing.
//! Controls are vanilla's defaults (see README.md).

use crate::blocks::{self, Blocks};
use crate::client::{Account, Game};
use crate::math;
use crate::math::FloorExt;
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
const K_D: usize = 7;
const K_E: usize = 8;
const K_Q: usize = 20;
const K_R: usize = 21;
const K_S: usize = 22;
const K_T: usize = 23;
const K_W: usize = 26;
const K_1: usize = 30;
const K_RETURN: usize = 40;
const K_ESC: usize = 41;
const K_BACKSPACE: usize = 42;
const K_SPACE: usize = 44;
const K_SLASH: usize = 56;
const K_F3: usize = 60;
const K_F11: usize = 68;
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
    Dead,
    /// The options screens (options.rs), from the pause menu.
    Options,
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
    chat_line: String,
    /// The key that opened the chat also arrives as text: drop it.
    skip_text: Option<(char, u64)>,
    debug: bool,
    fullscreen: bool,
    mover: Mover,
    prev_pos: [f64; 3],
    last_input: u8,
    sprinting: bool,
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
    /// PENRYN_TEST_INPUT: held keys for tests, e.g. "w" to walk forward.
    test_keys: String,
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
    /// The arm's swing: ticks into it (-1: none), as vanilla's 6-tick swing.
    swing_ticks: i32,
    /// The hand's lowering while a new item comes up: 1 lowered .. 0 raised, and last tick's.
    equip: f32,
    prev_equip: f32,
    /// View bobbing: distance walked and bob amount (and last tick's).
    walk_dist: f32,
    prev_walk_dist: f32,
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
}

impl App {
    fn eye_height(&self) -> f64 {
        if self.keys[K_LSHIFT] && self.screen == Screen::Play {
            physics::SNEAK_EYE
        } else {
            physics::EYE
        }
    }

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

    /// Shows `lines` until the window is closed or Escape is pressed.
    fn wait_on(&mut self, lines: &[&str]) {
        loop {
            while let Some(e) = self.v.poll() {
                let t = e.u32_at(0);
                if t == sdl::EV_QUIT || (t == sdl::EV_KEYDOWN && e.i32_at(16) as usize == K_ESC) {
                    return;
                }
            }
            self.message(lines);
            unsafe { (self.v.sdl.SDL_Delay)(30) };
        }
    }
}

pub fn run(host: &str, port: u16, name: &str, account: Option<&Account>, blocks: Blocks) -> Result<(), String> {
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
    unsafe { (v.sdl.SDL_GL_SetSwapInterval)(settings.on(O::Vsync) as i32) };
    let view: i32 = settings.int(O::RenderDistance);
    say!("OpenGL: {} ({})", v.gl_string(sdl::RENDERER), v.gl_string(sdl::VERSION));
    let mut app = App {
        v,
        r: Renderer::new(),
        keys: [false; 512],
        screen: Screen::Play,
        chat_line: String::new(),
        skip_text: None,
        debug: false,
        fullscreen,
        mover: Mover::new([0.0; 3]),
        prev_pos: [0.0; 3],
        last_input: 0,
        sprinting: false,
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
            (p, sys::now_ms() + after * 1000)
        }),
        test_keys: sys::env("PENRYN_TEST_INPUT").unwrap_or_default(),
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
        swing_ticks: -1,
        equip: 0.0,
        prev_equip: 0.0,
        walk_dist: 0.0,
        prev_walk_dist: 0.0,
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
    };
    let atlas = sys::read_file(&format!("{}/atlas.bin", crate::data_dir())).ok_or("cannot read atlas.bin (see README.md)")?;
    app.r.load_atlas(&app.v.gl, &atlas)?;
    drop(atlas);
    let gui = sys::read_file(&format!("{}/gui.bin", crate::data_dir())).ok_or("cannot read gui.bin (see README.md)")?;
    app.gui = Some(Gui::load(&app.v.gl, &gui, app.r.atlas, app.r.atlas_size)?);
    drop(gui);
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
    if let Some(d) = sys::read_file(&format!("{}/entities.bin", crate::data_dir())) {
        app.entities = Some(crate::entities::EntityTable::load(&app.v.gl, &d)?);
    }
    let target = format!("Joining {}:{} ...", host, port);
    app.message(&[&target]);
    let mut g = match Game::join(host, port, name, account, blocks, view as i8) {
        Ok(g) => g,
        Err(e) => {
            app.wait_on(&["Could not join the server", &e, "", "Esc to quit"]);
            return Err(e);
        }
    };
    app.v.grab_mouse(true);
    let result = app.main_loop(&mut g);
    if let Err(e) = &result {
        app.v.grab_mouse(false);
        let reason = g.disconnected.clone().unwrap_or_else(|| e.clone());
        app.wait_on(&["Disconnected", &reason, "", "Esc to quit"]);
    }
    result
}

impl App {
    fn main_loop(&mut self, g: &mut Game) -> Result<(), String> {
        let mut next_tick = sys::now_ms();
        loop {
            // ---- events
            let mut quit = false;
            while let Some(e) = self.v.poll() {
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
            // ---- network
            g.pump()?;
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
            self.draw(g, partial);
            // The Max Framerate option (vanilla's FramerateLimitTracker), on top of vsync.
            if let Some(fps) = self.menu.o.frame_limit() {
                let due = self.frame_shown + 1000 / fps as u64;
                let t = sys::now_ms();
                if t < due {
                    sys::sleep_ms((due - t) as u32);
                }
            }
            self.frame_shown = sys::now_ms();
            if let Some(shot) = self.test_shot.as_ref() {
                if now >= shot.1 {
                    let (w, h) = self.v.size();
                    let png = sdl::screenshot_png(&self.v.gl, w, h);
                    sys::write_file_private(&shot.0, &png);
                    say!("screenshot: {}", shot.0);
                    return Ok(());
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
    fn teleported(&mut self, g: &Game) -> bool {
        self.mover.reset([g.player.x, g.player.y, g.player.z]);
        self.prev_pos = self.mover.pos;
        true
    }

    fn event(&mut self, g: &mut Game, e: &sdl::Event) -> Result<bool, String> {
        let t = e.u32_at(0);
        match t {
            sdl::EV_QUIT => return Ok(true),
            sdl::EV_WINDOW => {
                if e.0[12] == sdl::WINEV_FOCUS_LOST && self.screen == Screen::Play {
                    self.set_screen(Screen::Pause);
                }
            }
            sdl::EV_KEYDOWN | sdl::EV_KEYUP => {
                let sc = e.i32_at(16).clamp(0, 511) as usize;
                let down = t == sdl::EV_KEYDOWN;
                let repeat = e.0[13] != 0;
                self.keys[sc] = down;
                if down && !repeat {
                    return self.key(g, sc);
                }
                if down && repeat && self.screen == Screen::Chat && sc == K_BACKSPACE {
                    self.chat_line.pop();
                }
            }
            sdl::EV_TEXTINPUT => {
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
                    if self.chat_line.chars().count() + s.chars().count() <= 256 {
                        self.chat_line.push_str(&s);
                    }
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
                } else if self.screen == Screen::Options {
                    self.menu.drag_to(self.mouse.0);
                }
            }
            sdl::EV_MOUSEDOWN | sdl::EV_MOUSEUP => {
                let down = t == sdl::EV_MOUSEDOWN;
                let button = e.0[16];
                if self.screen != Screen::Play {
                    if down {
                        return self.screen_click(g, button);
                    }
                    if self.screen == Screen::Options && button == 1 {
                        if let crate::options::Ev::Click = self.menu.release() {
                            self.ui_click();
                        }
                    }
                    return Ok(false);
                }
                if down && (button == 1 || button == 3) {
                    if let Some((id, _, at)) = self.pick_entity(g) {
                        if button == 1 {
                            g.attack(id)?;
                        } else {
                            let sneak = self.keys[K_LSHIFT] || self.keys[K_RSHIFT];
                            g.interact(id, at, sneak)?;
                            g.punch()?;
                        }
                        return Ok(false);
                    }
                }
                if button == 1 {
                    self.left_down = down;
                    if down {
                        self.punch_timer = 0;
                    } else if let Some(m) = self.mining.take() {
                        g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], 0)?;
                    }
                } else if button == 3 {
                    self.right_down = down;
                    if down {
                        self.use_delay = 0;
                    } else if self.using.take().is_some() {
                        g.dig(RELEASE_USE_ITEM, 0, 0, 0, 0)?;
                    }
                }
            }
            sdl::EV_MOUSEWHEEL => {
                if let (Screen::Options, Some(gui)) = (&self.screen, self.gui.as_ref()) {
                    self.menu.wheel(gui.width, gui.height, e.i32_at(20));
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
            Screen::Pause if button == 1 => {
                let b = screens::pause_buttons(gui);
                if b[0].hit(mx, my) {
                    self.ui_click();
                    self.set_screen(Screen::Play);
                } else if b[1].hit(mx, my) {
                    self.ui_click();
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
                    crate::options::Ev::None => {}
                }
            }
            Screen::Dead if button == 1 => {
                let b = screens::death_buttons(gui);
                if b[0].hit(mx, my) {
                    self.ui_click();
                    g.respawn()?;
                } else if b[1].hit(mx, my) {
                    self.ui_click();
                    return Ok(true);
                }
            }
            Screen::Inventory if button == 1 || button == 3 => {
                let win = screens::window(g);
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

    fn close_inventory(&mut self, g: &mut Game) -> Result<(), String> {
        let id = g.container.as_ref().map_or(0, |c| c.id);
        g.close_container(id)?;
        self.container_screen = false;
        self.set_screen(Screen::Play);
        Ok(())
    }

    fn set_screen(&mut self, s: Screen) {
        if self.screen == Screen::Options && s != Screen::Options {
            self.menu.close();
        }
        self.v.grab_mouse(s == Screen::Play);
        self.v.text_input(s == Screen::Chat);
        if s != Screen::Play {
            self.left_down = false;
            self.right_down = false;
        }
        self.screen = s;
    }

    fn key(&mut self, g: &mut Game, sc: usize) -> Result<bool, String> {
        match self.screen {
            Screen::Chat => match sc {
                K_ESC => self.set_screen(Screen::Play),
                K_RETURN => {
                    let line = core::mem::take(&mut self.chat_line);
                    let line = String::from(line.trim());
                    if !line.is_empty() {
                        g.say(&line)?;
                    }
                    self.set_screen(Screen::Play);
                }
                K_BACKSPACE => {
                    self.chat_line.pop();
                }
                _ => {}
            },
            Screen::Options => {
                if sc == K_ESC && self.menu.back() {
                    self.set_screen(Screen::Pause);
                }
            }
            Screen::Pause => match sc {
                K_ESC => self.set_screen(Screen::Play),
                K_Q => return Ok(true),
                _ => {}
            },
            Screen::Inventory => {
                if sc == K_ESC || sc == K_E {
                    self.close_inventory(g)?;
                } else if let Some(gui) = self.gui.as_ref() {
                    // Over a slot: 1-9 swap with the hotbar, Q throws (Ctrl+Q the stack).
                    let win = screens::window(g);
                    if let Some(slot) = screens::slot_at(gui, &win, self.mouse.0, self.mouse.1) {
                        if (K_1..K_1 + 9).contains(&sc) {
                            g.click(win.container_id, slot, (sc - K_1) as i8, 2)?;
                        } else if sc == K_Q {
                            g.click(win.container_id, slot, if self.keys[K_LCTRL] { 1 } else { 0 }, 4)?;
                        }
                    }
                }
            }
            Screen::Dead => {
                if sc == K_R && !self.respawn_sent {
                    self.ui_click();
                    self.respawn_sent = true;
                    g.respawn()?;
                }
            }
            Screen::Play => match sc {
                K_ESC => self.set_screen(Screen::Pause),
                K_T => {
                    self.set_screen(Screen::Chat);
                    self.skip_text = Some(('t', sys::now_ms()));
                }
                K_SLASH => {
                    self.set_screen(Screen::Chat);
                    self.chat_line = String::from("/");
                    self.skip_text = Some(('/', sys::now_ms()));
                }
                K_E => self.set_screen(Screen::Inventory),
                K_F3 => self.debug = !self.debug,
                K_F11 => {
                    let f = !self.fullscreen;
                    self.menu.set(crate::options::O::Fullscreen, f as i32 as f64);
                    self.menu.o.save();
                }
                K_SPACE => {
                    let now = sys::now_ms();
                    if g.may_fly && now - self.last_space < 300 {
                        let f = !g.flying;
                        g.set_flying(f)?;
                        self.last_space = 0;
                    } else {
                        self.last_space = now;
                    }
                }
                K_W => {
                    let now = sys::now_ms();
                    if now - self.last_w < 300 {
                        self.double_w = true;
                    }
                    self.last_w = now;
                }
                x if (K_1..K_1 + 9).contains(&x) => {
                    g.select_slot((x - K_1) as i16)?;
                }
                _ => {}
            },
        }
        Ok(false)
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
        if g.dead && self.screen != Screen::Dead {
            self.respawn_sent = false;
            self.set_screen(Screen::Dead);
        } else if !g.dead && self.screen == Screen::Dead {
            self.set_screen(Screen::Play);
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
        for (c, sc) in [('w', K_W), ('a', K_A), ('s', K_S), ('d', K_D), (' ', K_SPACE), ('^', K_LCTRL), ('v', K_LSHIFT)] {
            if self.test_keys.contains(c) {
                self.keys[sc] = true;
            }
        }
        if self.test_keys.contains('3') {
            self.debug = true;
        }
        if self.test_keys.contains('A') && g.loaded_sent && sys::now_ms() / 50 % 10 == 0 {
            if let Some((id, d, _)) = self.pick_entity(g) {
                say!("attacking entity {} at {:.2}", id, d);
                g.attack(id)?;
            }
        }
        if self.test_keys.contains('I') && self.screen == Screen::Play && g.loaded_sent {
            self.set_screen(Screen::Inventory);
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
                }
            }
        }
        // click=X:Y (GUI units): a left click on the open screen, one each half second.
        if self.screen != Screen::Play && self.test_keys.contains("click=") {
            self.test_ticks += 1;
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
            }
        }
        if self.test_keys.contains('L') {
            self.left_down = true;
        }
        if self.test_keys.contains('R') && self.screen == Screen::Play {
            self.right_down = true;
        }
        for part in self.test_keys.split(',') {
            if let Some(v) = part.strip_prefix("pitch=").and_then(|v| v.parse::<f32>().ok()) {
                g.player.pitch = v;
            }
            if let Some(v) = part.strip_prefix("yaw=").and_then(|v| v.parse::<f32>().ok()) {
                g.player.yaw = v;
            }
        }
        let play = self.screen == Screen::Play && !g.dead;
        let k = |i: usize| play && self.keys[i];
        let mut inp = Input {
            forward: (k(K_W) as i32 - k(K_S) as i32) as f64 * 0.98,
            strafe: (k(K_A) as i32 - k(K_D) as i32) as f64 * 0.98,
            jump: k(K_SPACE),
            sneak: k(K_LSHIFT) || k(K_RSHIFT),
            shift: k(K_LSHIFT) || k(K_RSHIFT),
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
        let want_sprint = (k(K_LCTRL) || self.double_w) && inp.forward > 0.0 && !inp.sneak && (g.food > 6 || g.may_fly);
        if !k(K_W) {
            self.double_w = false;
        }
        let sprint = want_sprint && !(self.mover.horizontal_collision && self.mover.on_ground && !self.sprinting);
        if sprint != self.sprinting {
            self.sprinting = sprint;
            g.command(if sprint { 1 } else { 2 })?;
        }
        inp.sprint = self.sprinting;
        if inp.sneak && !g.flying {
            inp.forward *= 0.3;
            inp.strafe *= 0.3;
        }
        if self.using.is_some() {
            inp.forward *= 0.2;
            inp.strafe *= 0.2;
            inp.sprint = false;
        }
        let flags = (k(K_W) as u8) | (k(K_S) as u8) << 1 | (k(K_A) as u8) << 2 | (k(K_D) as u8) << 3
            | (inp.jump as u8) << 4 | (shift as u8) << 5 | (self.sprinting as u8) << 6;
        if flags != self.last_input {
            self.last_input = flags;
            g.send_input(flags)?;
        }
        if g.world.loaded(math::ifloor(self.mover.pos[0]), math::ifloor(self.mover.pos[2])) && g.loaded_sent {
            let fluid = self.fluid_at(g);
            self.mover.tick(&g.world, &g.blocks, &inp, g.player.yaw, g.flying, fluid);
            // LocalPlayer.aiStep: landing ends flight (except in spectator mode).
            if g.flying && self.mover.on_ground && g.game_mode != 3 {
                g.set_flying(false)?;
            }
        }
        g.player.x = self.mover.pos[0];
        g.player.y = self.mover.pos[1];
        g.player.z = self.mover.pos[2];
        // Walking (Player.aiStep's bob and walkDist), the swing and the equip animation.
        let (dx, dz) = (self.mover.pos[0] - self.prev_pos[0], self.mover.pos[2] - self.prev_pos[2]);
        let speed = math::sqrt(dx * dx + dz * dz) as f32;
        self.prev_walk_dist = self.walk_dist;
        self.walk_dist += speed * 0.6;
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
            self.using = None;
            g.dig(RELEASE_USE_ITEM, 0, 0, 0, 0)?;
        }
        if !(self.left_down && play) {
            if let Some(m) = self.mining.take() {
                g.dig(DIG_ABORT, m.pos[0], m.pos[1], m.pos[2], 0)?;
            }
        }
        if self.left_down && play {
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
        let held_name = g.held().and_then(|(i, _)| g.blocks.items.get(i as usize)).cloned().unwrap_or_default();
        if self.right_down && play && self.use_delay == 0 && self.using.is_none() {
            self.use_delay = 4;
            let usable = use_duration(&held_name);
            // Minecraft.startUseItem: the block first (useItemOn), then the item (useItem)
            // when the block does nothing with it.
            let block_used = match hit {
                Some(h) => {
                    let c = [
                        (h.point[0] - h.pos[0] as f64) as f32,
                        (h.point[1] - h.pos[1] as f64) as f32,
                        (h.point[2] - h.pos[2] as f64) as f32,
                    ];
                    g.place(h.pos[0], h.pos[1], h.pos[2], h.face, c, false)?;
                    let name = g.blocks.of_state(g.world.get(h.pos[0], h.pos[1], h.pos[2])).name.clone();
                    usable.is_none() || interactive(&name)
                }
                None => false,
            };
            if block_used {
                g.punch()?;
            } else {
                g.use_item()?;
                if let Some(d) = usable {
                    self.using = Some((0, d));
                }
            }
        }
        g.tick()
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

    fn draw(&mut self, g: &mut Game, partial: f64) {
        let (w, h) = self.v.size();
        let lerp = |a: f64, b: f64| a + (b - a) * partial;
        let eye = [
            lerp(self.prev_pos[0], self.mover.pos[0]),
            lerp(self.prev_pos[1], self.mover.pos[1]) + self.eye_height(),
            lerp(self.prev_pos[2], self.mover.pos[2]),
        ];
        self.r.update(&self.v.gl, &mut g.world, &g.blocks, eye, 6);
        // GameRenderer.getFov: the FOV option times the modifier, narrower under water.
        {
            use crate::options::O;
            let o = &self.menu.o;
            let m = self.prev_fov_mod + (self.fov_mod - self.prev_fov_mod) * partial as f32;
            let in_water = g.blocks.kind(g.world.get(math::ifloor(eye[0]), math::ifloor(eye[1]), math::ifloor(eye[2]))) == blocks::WATER;
            let water = if in_water { 1.0 + (6.0 / 7.0 - 1.0) * o.get(O::FovEffect) as f32 } else { 1.0 };
            self.r.fov = o.int(O::Fov) as f32 * m * water;
            self.menu.max_gui_scale = crate::options::auto_gui_scale(w, h);
        }
        let clouds_on = self.menu.o.int(crate::options::O::Clouds) != 0;
        // Daylight: the sun's height over the day (dayTime 0 = sunrise, 6000 noon).
        let t = (g.day_time.rem_euclid(24000)) as f64 / 24000.0;
        let sun = math::cos((t - 0.25) * 2.0 * math::PI);
        let day = ((sun * 2.0 + 0.5).clamp(0.0, 1.0)) as f32;
        let dim = g.world.min_y >= 0 && g.world.height == 256;
        let sky = if dim { [0.2, 0.05, 0.05] } else { [0.47 * day + 0.02, 0.65 * day + 0.02, 1.0 * day + 0.06] };
        let ents = self.entities.as_ref();
        let gl = &self.v.gl;
        let own = g.entity_id;
        let pf = partial as f32;
        let entities = &g.entities;
        let icons = self.gui.as_ref().map(|gui| &gui.icons[..]).unwrap_or(&[]);
        let (atlas, atlas_size) = (self.r.atlas, self.r.atlas_size);
        let blocks_ref = &g.blocks;
        let gui_ref = self.gui.as_ref();
        let sky_ref = &self.sky;
        let (day_time, game_time) = (g.day_time, g.game_time as f64 + partial);
        // Clouds reach further than the terrain (vanilla: the cloud range option, 128 by default).
        let radius = (((self.view as f64 + 1.0) * 16.0).max(128.0) * 1.5).min(self.menu.o.int(crate::options::O::CloudRange) as f64 * 16.0);
        self.r.draw_world(
            gl,
            &g.world,
            eye,
            g.player.yaw,
            g.player.pitch,
            w,
            h,
            self.view,
            sky,
            &mut |view| {
                if let Some(t) = ents {
                    t.draw(gl, view, eye, entities, pf, own);
                }
                crate::entities::draw_items(gl, view, eye, entities, pf, icons, blocks_ref, atlas, atlas_size);
                if let (Some(gui), false, true) = (gui_ref, dim, clouds_on) {
                    sky_ref.draw_clouds(gl, view, gui, eye, game_time, day, radius);
                }
            },
            &mut |view| {
                if let (Some(gui), false) = (gui_ref, dim) {
                    sky_ref.draw_celestial(gl, view, gui, day_time, day);
                }
            },
        );
        let hit = if self.screen == Screen::Play { self.pick(g) } else { None };
        if let Some(hh) = hit {
            let s = g.world.get(hh.pos[0], hh.pos[1], hh.pos[2]);
            let boxes: Vec<[u8; 6]> = g.blocks.shape(s).to_vec();
            // Drawn at the block's offset: the same as moving the eye the other way.
            let o = Self::outline_offset(&g.blocks, s, hh.pos);
            let eye_o = [eye[0] - o[0], eye[1], eye[2] - o[2]];
            self.r.draw_outline(&self.v.gl, eye_o, g.player.yaw, g.player.pitch, hh.pos, &boxes);
            if let Some(m) = self.mining.as_ref().filter(|m| m.pos == hh.pos) {
                let stage = ((m.progress * 10.0) as usize).min(9);
                self.r.draw_cracks(&self.v.gl, eye_o, g.player.yaw, g.player.pitch, hh.pos, &boxes, g.blocks.crack_uv[stage]);
            }
        }
        // ---- the hand
        if let Some(gui) = self.gui.as_ref() {
            let swing = if self.swing_ticks >= 0 { ((self.swing_ticks as f64 + partial) / 6.0) as f32 } else { 0.0 };
            let equip = self.prev_equip + (self.equip - self.prev_equip) * partial as f32;
            let walk = self.prev_walk_dist + (self.walk_dist - self.prev_walk_dist) * partial as f32;
            let bob = self.prev_bob + (self.bob - self.prev_bob) * partial as f32;
            let skin = crate::entities::EntityTable::skin_index(&g.uuid);
            self.hand.draw(
                &self.v.gl,
                w,
                h,
                g.held().map(|h| h.0),
                gui,
                &g.blocks,
                self.r.atlas,
                self.r.atlas_size,
                &self.r.atlas_alpha,
                self.entities.as_ref(),
                skin,
                swing,
                equip,
                if self.menu.o.on(crate::options::O::BobView) { (-walk, bob) } else { (0.0, 0.0) },
            );
        }
        // ---- 2D: vanilla's HUD and screens
        let Some(mut gui) = self.gui.take() else {
            self.v.swap();
            return;
        };
        gui.begin(w, h);
        let (gw, gh) = (gui.width, gui.height);
        if !dim && day < 1.0 {
            let a = self.menu.o.night_alpha((1.0 - day) * 140.0);
            gui.fill(0.0, 0.0, gw, gh, ((a as u32) << 24) | 0x00_0014);
        }
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
        screens::hud(&mut gui, g, self.screen == Screen::Play && !self.debug, held_name);
        let now = sys::now_ms();
        if self.screen != Screen::Inventory && self.screen != Screen::Dead {
            screens::chat(&mut gui, g, self.screen == Screen::Chat, now, &self.chat_line, (now / 300) % 2 == 0);
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
            Screen::Pause => screens::draw_pause(&mut gui, mx, my),
            Screen::Options => self.menu.draw(&mut gui, mx, my),
            Screen::Inventory => {
                let win = screens::window(g);
                screens::draw_window(&mut gui, g, &win, mx, my);
            }
            Screen::Dead => {
                let msg = g.chat.last().map(|c| c.0.clone()).unwrap_or_default();
                screens::draw_death(&mut gui, mx, my, &msg);
            }
            _ => {}
        }
        gui.flush(&self.v.gl, w, h);
        self.gui = Some(gui);
        self.v.swap();
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
