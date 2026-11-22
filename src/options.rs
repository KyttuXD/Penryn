//! Vanilla's options: options.txt (vanilla's key names and value formats, kept in the data
//! folder) and the options screens reached from the pause menu's "Options..." button, laid out
//! as 26.3's OptionsScreen, OptionsSubScreen and OptionsList lay them out (HeaderAndFooterLayout
//! 61/33 and 33/33, 150-unit buttons, 310-unit rows of 25), with vanilla's button and slider
//! sprites. Options Penryn has no use for yet are still shown and kept, as vanilla keeps them.

use crate::gui::Gui;
use crate::lang;
use crate::math;
use crate::sys;
use crate::wire::Out;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Copy)]
pub enum Kind {
    /// A cycle button with ON / OFF.
    Bool,
    /// A slider over whole numbers: min, max, step.
    Int(i32, i32, i32),
    /// A slider over a range: min, max, step (0: continuous).
    Real(f64, f64, f64),
    /// A cycle button over named values: (as options.txt stores it, translation key).
    Cycle(&'static [(&'static str, &'static str)]),
}

pub struct Def {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: f64,
}

macro_rules! options {
    ($($id:ident: $key:literal, $label:literal, $kind:expr, $def:expr;)*) => {
        /// Every option Penryn knows, in the order of `DEFS`.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum O { $($id),* }
        pub const DEFS: &[Def] = &[$(Def { key: $key, label: $label, kind: $kind, default: $def }),*];
    };
}

const ON_OFF_HOLD: &[(&str, &str)] = &[("false", "options.key.hold"), ("true", "options.key.toggle")];
const NARRATOR: &[(&str, &str)] =
    &[("0", "options.narrator.off"), ("1", "options.narrator.all"), ("2", "options.narrator.chat"), ("3", "options.narrator.system")];
const UNIT: Kind = Kind::Real(0.0, 1.0, 0.0);

options! {
    Fov: "fov", "options.fov", Kind::Int(30, 110, 1), 70.0;
    // Music & Sounds (SoundSource order)
    Master: "soundCategory_master", "soundCategory.master", UNIT, 1.0;
    Music: "soundCategory_music", "soundCategory.music", UNIT, 1.0;
    Record: "soundCategory_record", "soundCategory.record", UNIT, 1.0;
    Weather: "soundCategory_weather", "soundCategory.weather", UNIT, 1.0;
    Block: "soundCategory_block", "soundCategory.block", UNIT, 1.0;
    Hostile: "soundCategory_hostile", "soundCategory.hostile", UNIT, 1.0;
    Neutral: "soundCategory_neutral", "soundCategory.neutral", UNIT, 1.0;
    Player: "soundCategory_player", "soundCategory.player", UNIT, 1.0;
    Ambient: "soundCategory_ambient", "soundCategory.ambient", UNIT, 1.0;
    Voice: "soundCategory_voice", "soundCategory.voice", UNIT, 1.0;
    Ui: "soundCategory_ui", "soundCategory.ui", UNIT, 1.0;
    ShowSubtitles: "showSubtitles", "options.showSubtitles", Kind::Bool, 0.0;
    DirectionalAudio: "directionalAudio", "options.directionalAudio", Kind::Bool, 0.0;
    MusicFrequency: "musicFrequency", "options.music_frequency", Kind::Cycle(&[
        ("\"DEFAULT\"", "options.music_frequency.default"), ("\"FREQUENT\"", "options.music_frequency.frequent"),
        ("\"CONSTANT\"", "options.music_frequency.constant")]), 0.0;
    MusicToast: "musicToast", "options.musicToast", Kind::Cycle(&[
        ("\"never\"", "options.musicToast.never"), ("\"pause\"", "options.musicToast.pauseMenu"),
        ("\"pause_and_toast\"", "options.musicToast.pauseMenuAndToast")]), 2.0;
    // Video Settings: display
    MaxFps: "maxFps", "options.framerateLimit", Kind::Int(10, 260, 10), 120.0;
    Vsync: "enableVsync", "options.vsync", Kind::Bool, 1.0;
    InactivityFps: "inactivityFpsLimit", "options.inactivityFpsLimit", Kind::Cycle(&[
        ("\"minimized\"", "options.inactivityFpsLimit.minimized"), ("\"afk\"", "options.inactivityFpsLimit.afk")]), 1.0;
    GuiScale: "guiScale", "options.guiScale", Kind::Int(0, 6, 1), 0.0;
    Fullscreen: "fullscreen", "options.fullscreen", Kind::Bool, 0.0;
    ExclusiveFullscreen: "exclusiveFullscreen", "options.exclusiveFullscreen", Kind::Bool, 0.0;
    Gamma: "gamma", "options.gamma", UNIT, 0.5;
    GraphicsApi: "preferredGraphicsBackend", "options.graphicsApi", Kind::Cycle(&[
        ("\"default\"", "options.graphicsApi.default"), ("\"opengl\"", "options.graphicsApi.opengl"),
        ("\"vulkan\"", "options.graphicsApi.vulkan")]), 0.0;
    // quality
    GraphicsPreset: "graphicsPreset", "options.graphics.preset", Kind::Cycle(&[
        ("\"fast\"", "options.graphics.fast"), ("\"fancy\"", "options.graphics.fancy"),
        ("\"fabulous\"", "options.graphics.fabulous"), ("\"custom\"", "options.graphics.custom")]), 1.0;
    BiomeBlend: "biomeBlendRadius", "options.biomeBlendRadius", Kind::Int(0, 7, 1), 2.0;
    RenderDistance: "renderDistance", "options.renderDistance", Kind::Int(2, 32, 1), 12.0;
    PrioritizeChunks: "prioritizeChunkUpdates", "options.prioritizeChunkUpdates", Kind::Cycle(&[
        ("0", "options.prioritizeChunkUpdates.none"), ("1", "options.prioritizeChunkUpdates.byPlayer"),
        ("2", "options.prioritizeChunkUpdates.nearby")]), 0.0;
    SimulationDistance: "simulationDistance", "options.simulationDistance", Kind::Int(5, 32, 1), 12.0;
    Ao: "ao", "options.ao", Kind::Bool, 1.0;
    Clouds: "renderClouds", "options.renderClouds", Kind::Cycle(&[
        ("\"false\"", "options.off"), ("\"fast\"", "options.clouds.fast"), ("\"true\"", "options.clouds.fancy")]), 2.0;
    Particles: "particles", "options.particles", Kind::Cycle(&[
        ("0", "options.particles.all"), ("1", "options.particles.decreased"), ("2", "options.particles.minimal")]), 0.0;
    Mipmap: "mipmapLevels", "options.mipmapLevels", Kind::Int(0, 4, 1), 4.0;
    EntityShadows: "entityShadows", "options.entityShadows", Kind::Bool, 1.0;
    EntityDistance: "entityDistanceScaling", "options.entityDistanceScaling", Kind::Real(0.5, 5.0, 0.25), 1.0;
    MenuBlur: "menuBackgroundBlurriness", "options.accessibility.menu_background_blurriness", Kind::Int(0, 10, 1), 5.0;
    CloudRange: "cloudRange", "options.renderCloudsDistance", Kind::Int(2, 128, 1), 128.0;
    CutoutLeaves: "cutoutLeaves", "options.cutoutLeaves", Kind::Bool, 1.0;
    ImprovedTransparency: "improvedTransparency", "options.improvedTransparency", Kind::Bool, 0.0;
    TextureFiltering: "textureFiltering", "options.textureFiltering", Kind::Cycle(&[
        ("0", "options.textureFiltering.none"), ("1", "options.textureFiltering.rgss"),
        ("2", "options.textureFiltering.anisotropic")]), 0.0;
    MaxAnisotropy: "maxAnisotropyBit", "options.maxAnisotropy", Kind::Int(1, 3, 1), 2.0;
    WeatherRadius: "weatherRadius", "options.weatherRadius", Kind::Int(3, 10, 1), 10.0;
    // preferences
    AutosaveIndicator: "showAutosaveIndicator", "options.autosaveIndicator", Kind::Bool, 1.0;
    Vignette: "vignette", "options.vignette", Kind::Bool, 1.0;
    AttackIndicator: "attackIndicator", "options.attackIndicator", Kind::Cycle(&[
        ("0", "options.off"), ("1", "options.attack.crosshair"), ("2", "options.attack.hotbar")]), 1.0;
    ChunkFade: "chunkSectionFadeInTime", "options.chunkFade", Kind::Real(0.0, 2.0, 0.05), 0.75;
    // Controls
    ToggleCrouch: "toggleCrouch", "key.sneak", Kind::Cycle(ON_OFF_HOLD), 0.0;
    ToggleSprint: "toggleSprint", "key.sprint", Kind::Cycle(ON_OFF_HOLD), 0.0;
    ToggleAttack: "toggleAttack", "key.attack", Kind::Cycle(ON_OFF_HOLD), 0.0;
    ToggleUse: "toggleUse", "key.use", Kind::Cycle(ON_OFF_HOLD), 0.0;
    AutoJump: "autoJump", "options.autoJump", Kind::Bool, 0.0;
    SprintWindow: "sprintWindow", "options.sprintWindow", Kind::Int(0, 10, 1), 7.0;
    QuitShortcuts: "quitShortcuts", "options.quitShortcuts", Kind::Bool, 1.0;
    OperatorItemsTab: "operatorItemsTab", "options.operatorItemsTab", Kind::Bool, 0.0;
    // Mouse Settings
    Sensitivity: "mouseSensitivity", "options.sensitivity", UNIT, 0.5;
    WheelSensitivity: "mouseWheelSensitivity", "options.mouseWheelSensitivity", Kind::Real(-2.0, 1.0, 0.01), 0.0;
    DiscreteScroll: "discrete_mouse_scroll", "options.discrete_mouse_scroll", Kind::Bool, 0.0;
    InvertX: "invertXMouse", "options.invertMouseX", Kind::Bool, 0.0;
    InvertY: "invertYMouse", "options.invertMouseY", Kind::Bool, 0.0;
    AllowCursor: "allowCursorChanges", "options.allowCursorChanges", Kind::Bool, 1.0;
    // Skin Customization (PlayerModelPart order: the ClientInformation bits)
    Cape: "modelPart_cape", "options.modelPart.cape", Kind::Bool, 1.0;
    Jacket: "modelPart_jacket", "options.modelPart.jacket", Kind::Bool, 1.0;
    LeftSleeve: "modelPart_left_sleeve", "options.modelPart.left_sleeve", Kind::Bool, 1.0;
    RightSleeve: "modelPart_right_sleeve", "options.modelPart.right_sleeve", Kind::Bool, 1.0;
    LeftPants: "modelPart_left_pants_leg", "options.modelPart.left_pants_leg", Kind::Bool, 1.0;
    RightPants: "modelPart_right_pants_leg", "options.modelPart.right_pants_leg", Kind::Bool, 1.0;
    Hat: "modelPart_hat", "options.modelPart.hat", Kind::Bool, 1.0;
    MainHand: "mainHand", "options.mainHand", Kind::Cycle(&[("\"left\"", "options.mainHand.left"), ("\"right\"", "options.mainHand.right")]), 1.0;
    // Chat Settings
    ChatVisibility: "chatVisibility", "options.chat.visibility", Kind::Cycle(&[
        ("0", "options.chat.visibility.full"), ("1", "options.chat.visibility.system"), ("2", "options.chat.visibility.hidden")]), 0.0;
    ChatColors: "chatColors", "options.chat.color", Kind::Bool, 1.0;
    ChatLinks: "chatLinks", "options.chat.links", Kind::Bool, 1.0;
    ChatLinksPrompt: "chatLinksPrompt", "options.chat.links.prompt", Kind::Bool, 1.0;
    ChatOpacity: "chatOpacity", "options.chat.opacity", UNIT, 1.0;
    TextBackground: "textBackgroundOpacity", "options.accessibility.text_background_opacity", UNIT, 0.5;
    ChatScale: "chatScale", "options.chat.scale", UNIT, 1.0;
    ChatLineSpacing: "chatLineSpacing", "options.chat.line_spacing", UNIT, 0.0;
    ChatDelay: "chatDelay", "options.chat.delay_instant", Kind::Real(0.0, 6.0, 0.1), 0.0;
    ChatWidth: "chatWidth", "options.chat.width", UNIT, 1.0;
    ChatHeightFocused: "chatHeightFocused", "options.chat.height.focused", UNIT, 1.0;
    ChatHeightUnfocused: "chatHeightUnfocused", "options.chat.height.unfocused", UNIT, 0.4375;
    Narrator: "narrator", "options.narrator", Kind::Cycle(NARRATOR), 0.0;
    AutoSuggest: "autoSuggestions", "options.autoSuggestCommands", Kind::Bool, 1.0;
    HideMatchedNames: "hideMatchedNames", "options.hideMatchedNames", Kind::Bool, 1.0;
    ReducedDebug: "reducedDebugInfo", "options.reducedDebugInfo", Kind::Bool, 0.0;
    OnlySecureChat: "onlyShowSecureChat", "options.onlyShowSecureChat", Kind::Bool, 0.0;
    ChatDrafts: "saveChatDrafts", "options.chat.drafts", Kind::Bool, 0.0;
    // Accessibility Settings
    HighContrast: "highContrast", "options.accessibility.high_contrast", Kind::Bool, 0.0;
    BackgroundForChat: "backgroundForChatOnly", "options.accessibility.text_background", Kind::Cycle(&[
        ("true", "options.accessibility.text_background.chat"), ("false", "options.accessibility.text_background.everywhere")]), 0.0;
    NotificationTime: "notificationDisplayTime", "options.notifications.display_time", Kind::Real(0.5, 10.0, 0.1), 1.0;
    BobView: "bobView", "options.viewBobbing", Kind::Bool, 1.0;
    ScreenEffect: "screenEffectScale", "options.screenEffectScale", UNIT, 1.0;
    FovEffect: "fovEffectScale", "options.fovEffectScale", UNIT, 1.0;
    DarknessEffect: "darknessEffectScale", "options.darknessEffectScale", UNIT, 1.0;
    DamageTilt: "damageTiltStrength", "options.damageTiltStrength", UNIT, 1.0;
    GlintSpeed: "glintSpeed", "options.glintSpeed", UNIT, 0.5;
    GlintStrength: "glintStrength", "options.glintStrength", UNIT, 0.75;
    HideLightning: "hideLightningFlashes", "options.hideLightningFlashes", Kind::Bool, 0.0;
    DarkMojang: "darkMojangStudiosBackground", "options.darkMojangStudiosBackgroundColor", Kind::Bool, 0.0;
    PanoramaSpeed: "panoramaScrollSpeed", "options.accessibility.panorama_speed", UNIT, 1.0;
    HideSplash: "hideSplashTexts", "options.hideSplashTexts", Kind::Bool, 0.0;
    NarratorHotkey: "narratorHotkey", "options.accessibility.narrator_hotkey", Kind::Bool, 1.0;
    RotateWithMinecart: "rotateWithMinecart", "options.rotateWithMinecart", Kind::Bool, 0.0;
    HighContrastOutline: "highContrastBlockOutline", "options.accessibility.high_contrast_block_outline", Kind::Bool, 0.0;
    // Online
    AllowServerListing: "allowServerListing", "options.allowServerListing", Kind::Bool, 1.0;
    RealmsNotifications: "realmsNotifications", "options.realmsNotifications", Kind::Bool, 1.0;
}

/// The sound categories' sliders, in SoundSource order (the mixer's categories).
pub const VOLUMES: [O; 11] =
    [O::Master, O::Music, O::Record, O::Weather, O::Block, O::Hostile, O::Neutral, O::Player, O::Ambient, O::Voice, O::Ui];
const SKIN: [O; 7] = [O::Cape, O::Jacket, O::LeftSleeve, O::RightSleeve, O::LeftPants, O::RightPants, O::Hat];

fn def(o: O) -> &'static Def {
    &DEFS[o as usize]
}

// ------------------------------------------------------------------------------ options.txt

/// The options, as options.txt has them. Lines Penryn does not know are kept as they are.
pub struct Settings {
    v: Vec<f64>,
    seen: Vec<bool>,
    other: Vec<(String, String)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { v: DEFS.iter().map(|d| d.default).collect(), seen: alloc::vec![false; DEFS.len()], other: Vec::new() }
    }
}

fn number(v: f64) -> String {
    let s = format!("{}", v);
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{}.0", s)
    }
}

fn round(v: f64) -> f64 {
    math::floor(v + 0.5)
}

impl Settings {
    pub fn path() -> String {
        format!("{}/options.txt", crate::data_dir())
    }

    /// options.txt from the data folder; defaults where it says nothing.
    pub fn load() -> Settings {
        let text = sys::read_file(&Self::path()).map(|d| String::from_utf8_lossy(&d).into_owned()).unwrap_or_default();
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((k, val)) = line.split_once(':') else { continue };
            match DEFS.iter().position(|d| d.key == k) {
                Some(i) => {
                    if let Some(x) = decode(&DEFS[i], val) {
                        s.v[i] = x;
                        s.seen[i] = true;
                    }
                }
                None => s.other.push((String::from(k), String::from(val))),
            }
        }
        s
    }

    pub fn text(&self) -> String {
        let mut out = String::from("version:4790\n");
        for (i, d) in DEFS.iter().enumerate() {
            out.push_str(d.key);
            out.push(':');
            out.push_str(&encode(d, self.v[i]));
            out.push('\n');
        }
        for (k, v) in self.other.iter().filter(|(k, _)| k != "version") {
            out.push_str(&format!("{}:{}\n", k, v));
        }
        out
    }

    pub fn save(&self) {
        if !sys::write_file_private(&Self::path(), self.text().as_bytes()) {
            crate::say!("cannot write {}", Self::path());
        }
    }

    pub fn get(&self, o: O) -> f64 {
        self.v[o as usize]
    }
    pub fn on(&self, o: O) -> bool {
        self.v[o as usize] != 0.0
    }
    pub fn int(&self, o: O) -> i32 {
        self.v[o as usize] as i32
    }
    /// Whether options.txt said anything about it (else the setting is a default).
    pub fn given(&self, o: O) -> bool {
        self.seen[o as usize]
    }
    pub fn set(&mut self, o: O, v: f64) {
        self.v[o as usize] = v;
    }

    /// The mixer's gain per category (SoundSource order).
    pub fn volumes(&self) -> [f32; 11] {
        let mut out = [1.0; 11];
        for (i, o) in VOLUMES.iter().enumerate() {
            out[i] = self.get(*o).clamp(0.0, 1.0) as f32;
        }
        out
    }

    /// Minecraft.turnPlayer: degrees turned per pixel of mouse movement.
    pub fn turn_per_pixel(&self) -> (f32, f32) {
        let d = self.get(O::Sensitivity) * 0.6 + 0.2;
        let k = (d * d * d * 8.0 * 0.15) as f32;
        (if self.on(O::InvertX) { -k } else { k }, if self.on(O::InvertY) { -k } else { k })
    }

    /// MusicManager.MusicFrequency.getNextSongDelay for the game's music (12000 to 24000 ticks).
    pub fn music_delay(&self, random: u32) -> i32 {
        let max = [24000, 12000, 0][self.int(O::MusicFrequency).clamp(0, 2) as usize];
        if max == 0 {
            return 100;
        }
        let (lo, hi) = (12000.min(max), 24000.min(max));
        lo + if hi > lo { (random % (hi - lo + 1) as u32) as i32 } else { 0 }
    }

    /// The GUI scale to use (0: automatic).
    pub fn gui_scale(&self) -> i32 {
        self.int(O::GuiScale)
    }

    /// Frames per second to hold to (None: unlimited).
    pub fn frame_limit(&self) -> Option<u32> {
        let f = self.int(O::MaxFps);
        if f >= 260 {
            None
        } else {
            Some(f.max(10) as u32)
        }
    }

    /// How dark the night overlay is, with the brightness option on top of it: Penryn has no
    /// light levels yet, so brightness lifts (or deepens) the overlay as vanilla's lightmap curve
    /// lifts dark light levels; at the default (50%) it is the overlay as it was.
    pub fn night_alpha(&self, alpha: f32) -> f32 {
        let lit = 1.0 - (alpha / 255.0).clamp(0.0, 1.0);
        let g = self.get(O::Gamma).clamp(0.0, 1.0) as f32;
        let bright = 1.0 - (1.0 - lit) * (1.0 - lit) * (1.0 - lit) * (1.0 - lit);
        let out = if g >= 0.5 { lit + (bright - lit) * (g - 0.5) * 2.0 } else { lit + (lit * lit - lit) * (0.5 - g) * 2.0 };
        ((1.0 - out) * 255.0).clamp(0.0, 255.0)
    }

    fn publish(&self) {
        let mut parts = 0u32;
        for (i, o) in SKIN.iter().enumerate() {
            if self.on(*o) {
                parts |= 1 << i;
            }
        }
        let v = (self.int(O::ChatVisibility).clamp(0, 2) as u32)
            | (self.on(O::ChatColors) as u32) << 2
            | parts << 3
            | (self.int(O::MainHand).clamp(0, 1) as u32) << 10
            | (self.on(O::AllowServerListing) as u32) << 11
            | (self.int(O::Particles).clamp(0, 2) as u32) << 12
            | 1 << 31;
        INFO.store(v, Ordering::Release);
    }
}

fn decode(d: &Def, val: &str) -> Option<f64> {
    let val = val.trim();
    match d.kind {
        Kind::Bool => match val {
            "true" => Some(1.0),
            "false" => Some(0.0),
            _ => None,
        },
        Kind::Int(lo, hi, _) => {
            if d.key == "fov" {
                // Stored as (fov - 70) / 40.
                let f: f64 = val.parse().ok()?;
                return Some(((f * 40.0 + 70.0) as i32).clamp(lo, hi) as f64);
            }
            let n: i32 = val.parse().ok().or_else(|| val.parse::<f64>().ok().map(|f| f as i32))?;
            Some(if d.key == "guiScale" { n.max(0) } else { n.clamp(lo, hi) } as f64)
        }
        Kind::Real(lo, hi, _) => {
            let f: f64 = val.parse().ok()?;
            if d.key == "mouseWheelSensitivity" {
                // Stored as the factor (0.01 to 10); the slider is its log10.
                return Some(log10(f.max(0.01)).clamp(lo, hi));
            }
            Some(f.clamp(lo, hi))
        }
        Kind::Cycle(values) => {
            let bare = val.trim_matches('"');
            values.iter().position(|(s, _)| *s == val || s.trim_matches('"') == bare).map(|i| i as f64)
        }
    }
}

fn encode(d: &Def, v: f64) -> String {
    match d.kind {
        Kind::Bool => String::from(if v != 0.0 { "true" } else { "false" }),
        Kind::Int(..) if d.key == "fov" => number((v - 70.0) / 40.0),
        Kind::Int(..) => format!("{}", v as i32),
        Kind::Real(..) if d.key == "mouseWheelSensitivity" => number(pow10(v)),
        Kind::Real(..) => number(v),
        Kind::Cycle(values) => String::from(values.get(v as usize).map_or(values[0].0, |x| x.0)),
    }
}

fn ln(x: f64) -> f64 {
    // x = m * 2^e, ln m by atanh's series.
    let (mut m, mut e) = (x, 0.0);
    while m > 2.0 {
        m /= 2.0;
        e += 1.0;
    }
    while m < 1.0 {
        m *= 2.0;
        e -= 1.0;
    }
    let t = (m - 1.0) / (m + 1.0);
    let (mut sum, mut p, mut k) = (0.0, t, 1.0);
    for _ in 0..30 {
        sum += p / k;
        p *= t * t;
        k += 2.0;
    }
    2.0 * sum + e * core::f64::consts::LN_2
}

fn log10(x: f64) -> f64 {
    ln(x) / core::f64::consts::LN_10
}

fn pow10(x: f64) -> f64 {
    // e^(x ln 10), by halving until the series is quick.
    let mut y = x * core::f64::consts::LN_10;
    let mut n = 0;
    while y.abs() > 0.5 {
        y /= 2.0;
        n += 1;
    }
    let (mut sum, mut term) = (1.0, 1.0);
    for i in 1..20 {
        term *= y / i as f64;
        sum += term;
    }
    for _ in 0..n {
        sum *= sum;
    }
    sum
}

// ------------------------------------------------------------------------------ ClientInformation

static INFO: AtomicU32 = AtomicU32::new(0);

/// ServerboundClientInformationPacket from the options (vanilla's defaults before any are read).
pub fn client_information(view: i8) -> Out {
    let mut v = INFO.load(Ordering::Acquire);
    if v & 1 << 31 == 0 {
        v = 1 << 2 | 0x7F << 3 | 1 << 10 | 1 << 11;
    }
    let mut o = Out::new();
    o.string("en_us")
        .u8(view as u8)
        .varint((v & 3) as i32)
        .bool(v & 1 << 2 != 0)
        .u8(((v >> 3) & 0x7F) as u8)
        .varint(((v >> 10) & 1) as i32)
        .bool(false)
        .bool(v & 1 << 11 != 0)
        .varint(((v >> 12) & 3) as i32);
    o
}

// ------------------------------------------------------------------------------ the screens

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Main,
    Online,
    Skin,
    Sounds,
    Video,
    Controls,
    Mouse,
    Chat,
    Accessibility,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Main => "options.title",
            Page::Online => "options.online.title",
            Page::Skin => "options.skinCustomisation.title",
            Page::Sounds => "options.sounds.title",
            Page::Video => "options.videoTitle",
            Page::Controls => "controls.title",
            Page::Mouse => "options.mouse_settings.title",
            Page::Chat => "options.chat.title",
            Page::Accessibility => "options.accessibility.title",
        }
    }

    pub fn by_name(s: &str) -> Option<Page> {
        Some(match s {
            "main" => Page::Main,
            "online" => Page::Online,
            "skin" => Page::Skin,
            "sounds" => Page::Sounds,
            "video" => Page::Video,
            "controls" => Page::Controls,
            "mouse" => Page::Mouse,
            "chat" => Page::Chat,
            "accessibility" => Page::Accessibility,
            _ => return None,
        })
    }
}

/// What a widget does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum W {
    Opt(O),
    /// A button opening another options screen.
    Open(Page, &'static str),
    /// A button Penryn has nothing behind yet: (label, value shown after it).
    Off(&'static str, Option<&'static str>),
    Done,
}

enum Row {
    Head(&'static str),
    Big(W),
    Two(W, Option<W>),
}

fn small(list: &[O]) -> Vec<Row> {
    list.chunks(2).map(|c| Row::Two(W::Opt(c[0]), c.get(1).map(|o| W::Opt(*o)))).collect()
}

/// The rows of a sub-screen, as its addOptions builds its OptionsList.
fn rows(p: Page) -> Vec<Row> {
    use O::*;
    let mut r = Vec::new();
    match p {
        Page::Main => {}
        Page::Online => {
            r.push(Row::Head("options.online.servers.header"));
            r.push(Row::Two(W::Opt(AllowServerListing), None));
            r.push(Row::Head("options.online.realms.header"));
            r.push(Row::Two(W::Opt(RealmsNotifications), None));
        }
        Page::Skin => {
            r.extend(small(&[Cape, Jacket, LeftSleeve, RightSleeve, LeftPants, RightPants, Hat, MainHand]));
        }
        Page::Sounds => {
            r.push(Row::Big(W::Opt(Master)));
            r.extend(small(&VOLUMES[1..]));
            r.push(Row::Big(W::Off("options.audioDevice", Some("options.audioDevice.default"))));
            r.extend(small(&[ShowSubtitles, DirectionalAudio]));
            r.extend(small(&[MusicFrequency, MusicToast]));
        }
        Page::Video => {
            r.push(Row::Head("options.video.display.header"));
            r.push(Row::Big(W::Off("options.fullscreen.resolution", Some("options.fullscreen.current"))));
            r.extend(small(&[MaxFps, Vsync, InactivityFps, GuiScale, Fullscreen, ExclusiveFullscreen, Gamma, GraphicsApi]));
            r.push(Row::Head("options.video.quality.header"));
            r.push(Row::Big(W::Opt(GraphicsPreset)));
            r.extend(small(&[
                BiomeBlend, RenderDistance, PrioritizeChunks, SimulationDistance, Ao, Clouds, Particles, Mipmap,
                EntityShadows, EntityDistance, MenuBlur, CloudRange, CutoutLeaves, ImprovedTransparency,
                TextureFiltering, MaxAnisotropy, WeatherRadius,
            ]));
            r.push(Row::Head("options.video.preferences.header"));
            r.extend(small(&[AutosaveIndicator, Vignette, AttackIndicator, ChunkFade]));
        }
        Page::Controls => {
            r.push(Row::Two(W::Open(Page::Mouse, "options.mouse_settings"), Some(W::Off("controls.keybinds", None))));
            r.extend(small(&[ToggleCrouch, ToggleSprint, ToggleAttack, ToggleUse, AutoJump, SprintWindow, QuitShortcuts, OperatorItemsTab]));
        }
        Page::Mouse => r.extend(small(&[Sensitivity, WheelSensitivity, DiscreteScroll, InvertX, InvertY, AllowCursor])),
        Page::Chat => r.extend(small(&[
            ChatVisibility, ChatColors, ChatLinks, ChatLinksPrompt, ChatOpacity, TextBackground, ChatScale, ChatLineSpacing,
            ChatDelay, ChatWidth, ChatHeightFocused, ChatHeightUnfocused, Narrator, AutoSuggest, HideMatchedNames,
            ReducedDebug, OnlySecureChat, ChatDrafts,
        ])),
        Page::Accessibility => r.extend(small(&[
            Narrator, ShowSubtitles, HighContrast, MenuBlur, TextBackground, BackgroundForChat, ChatOpacity,
            ChatLineSpacing, ChatDelay, NotificationTime, BobView, ScreenEffect, FovEffect, DarknessEffect, DamageTilt,
            GlintSpeed, GlintStrength, HideLightning, DarkMojang, PanoramaSpeed, HideSplash, NarratorHotkey,
            RotateWithMinecart, HighContrastOutline,
        ])),
    }
    r
}

/// A widget where the layout puts it.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub what: W,
}

impl Placed {
    fn hit(&self, mx: f32, my: f32) -> bool {
        mx >= self.x && mx < self.x + self.w && my >= self.y && my < self.y + 20.0
    }
}

pub struct Layout {
    pub widgets: Vec<Placed>,
    /// (text, x, y) drawn centred at x.
    pub texts: Vec<(String, f32, f32)>,
    /// The list's area and scrolling (sub-screens).
    pub list: Option<(f32, f32, f32, f32)>,
    pub content: f32,
}

const HEADER: f32 = 33.0;
const ROW: f32 = 25.0;

/// The options screens: which one is open (a stack, for Done and Escape), its scroll and the
/// slider being dragged. Changed options are listed in `changed` for the game to apply.
pub struct Menu {
    pub o: Settings,
    stack: Vec<Page>,
    scroll: f32,
    drag: Option<(O, f32, f32)>,
    /// The dragged slider's value, for the sliders vanilla applies only when let go.
    pending: Option<f64>,
    pub changed: Vec<O>,
    /// The automatic GUI scale for the window: the GUI scale slider's top.
    pub max_gui_scale: i32,
    /// Off in the self test, which must not write options.txt.
    pub saving: bool,
}

pub enum Ev {
    None,
    /// A button was pressed: play the click.
    Click,
    /// Done or Escape on the first screen: back to the pause menu.
    Close,
}

fn ceil(x: f32) -> f32 {
    -math::floor(-x as f64) as f32
}

fn fl(x: f32) -> f32 {
    math::floor(x as f64) as f32
}

impl Menu {
    pub fn new(o: Settings) -> Menu {
        o.publish();
        Menu { o, stack: Vec::new(), scroll: 0.0, drag: None, pending: None, changed: Vec::new(), max_gui_scale: 4, saving: true }
    }

    pub fn open(&mut self, p: Page) {
        if p == Page::Main {
            self.stack.clear();
        }
        if self.stack.is_empty() && p != Page::Main {
            self.stack.push(Page::Main);
        }
        self.stack.push(p);
        self.scroll = 0.0;
        self.drag = None;
    }

    /// Another screen took over (a container, death): save and forget the open screens.
    pub fn close(&mut self) {
        if !self.stack.is_empty() {
            self.stack.clear();
            self.drag = None;
            if self.saving {
                self.o.save();
            }
        }
    }

    pub fn page(&self) -> Page {
        self.stack.last().copied().unwrap_or(Page::Main)
    }

    /// Done, or Escape: back one screen (OptionsSubScreen.onClose), saving as vanilla does when
    /// a screen is removed. True when the options are closed.
    pub fn back(&mut self) -> bool {
        self.drag = None;
        self.stack.pop();
        self.scroll = 0.0;
        if self.saving {
            self.o.save();
        }
        self.stack.is_empty()
    }

    pub fn set(&mut self, o: O, v: f64) {
        if self.o.get(o) != v {
            self.o.set(o, v);
            self.o.publish();
            self.changed.push(o);
        }
    }

    /// The slider's values: lo + i * step for i in 0..=n (n 0: continuous).
    fn steps(&self, o: O) -> (f64, f64, f64, i64) {
        match def(o).kind {
            Kind::Int(lo, hi, step) => {
                let hi = if o == O::GuiScale { self.max_gui_scale.max(1) } else { hi };
                (lo as f64, hi as f64, step as f64, ((hi - lo) / step.max(1)) as i64)
            }
            Kind::Real(lo, hi, step) if step > 0.0 => (lo, hi, step, round((hi - lo) / step) as i64),
            Kind::Real(lo, hi, _) => (lo, hi, 0.0, 0),
            _ => (0.0, 1.0, 0.0, 0),
        }
    }

    /// The option's value, or while its slider is held, the value it will take when let go.
    fn shown(&self, o: O) -> f64 {
        match (self.drag, self.pending) {
            (Some((d, ..)), Some(v)) if d == o => v,
            _ => self.o.get(o),
        }
    }

    /// The slider's handle position (0..1) for a value (IntRangeBase.toSliderValue: the ends
    /// at the ends, the steps between them in the middle of their share).
    pub fn slider_pos_of(&self, o: O, v: f64) -> f64 {
        let (lo, hi, step, n) = self.steps(o);
        if n == 0 {
            return ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        }
        let i = round((v - lo) / step) as i64;
        if i <= 0 {
            0.0
        } else if i >= n {
            1.0
        } else {
            (i as f64 + 0.5) / (n + 1) as f64
        }
    }

    pub fn slider_pos(&self, o: O) -> f64 {
        self.slider_pos_of(o, self.shown(o))
    }

    /// The value at a slider position (IntRangeBase.fromSliderValue: each step gets an equal
    /// share of the slider).
    pub fn value_at(&self, o: O, pos: f64) -> f64 {
        let (lo, hi, step, n) = self.steps(o);
        let pos = pos.clamp(0.0, 1.0);
        if n == 0 {
            return lo + pos * (hi - lo);
        }
        let d = if pos >= 1.0 { 0.99999 } else { pos };
        let i = (math::floor(d * (n + 1) as f64) as i64).clamp(0, n);
        (lo + i as f64 * step).clamp(lo, hi)
    }

    /// Sliders vanilla applies only when let go (IntRange with applyValueImmediately false).
    fn on_release(o: O) -> bool {
        matches!(o, O::RenderDistance | O::SimulationDistance | O::BiomeBlend)
    }

    /// GUI Scale is a cycle button over 0 (automatic) to the largest scale the window allows
    /// (ClampingLazyMaxIntRange.createCycleButton).
    fn is_slider(o: O) -> bool {
        o != O::GuiScale && matches!(def(o).kind, Kind::Int(..) | Kind::Real(..))
    }

    /// The text on an option's button or slider, as its OptionInstance captions it.
    pub fn caption(&self, o: O) -> String {
        let d = def(o);
        let v = self.shown(o);
        let label = lang::get(d.label);
        let generic = |value: String| lang::format("options.generic_value", &[label.clone(), value]);
        let percent = |x: f64| lang::format("options.percent_value", &[label.clone(), format!("{}", (x * 100.0) as i32)]);
        let off = || lang::get("options.off");
        let n = v as i32;
        match o {
            O::Fov => generic(match n {
                70 => lang::get("options.fov.min"),
                110 => lang::get("options.fov.max"),
                _ => format!("{}", n),
            }),
            O::RenderDistance | O::SimulationDistance | O::CloudRange | O::WeatherRadius => {
                generic(lang::format("options.chunks", &[format!("{}", n)]))
            }
            O::MaxFps => generic(if n >= 260 { lang::get("options.framerateLimit.max") } else { lang::format("options.framerate", &[format!("{}", n)]) }),
            O::GuiScale => generic(if n == 0 { lang::get("options.guiScale.auto") } else { format!("{}", n) }),
            O::BiomeBlend => generic(if n == 0 { off() } else { lang::get(&format!("options.biomeBlendRadius.{}", n * 2 + 1)) }),
            O::Mipmap | O::MenuBlur | O::SprintWindow => generic(if n == 0 { off() } else { format!("{}", n) }),
            O::MaxAnisotropy => generic(format!("{}x", 1 << n.clamp(0, 4))),
            O::Sensitivity => {
                if v == 0.0 {
                    generic(lang::get("options.sensitivity.min"))
                } else if v == 1.0 {
                    generic(lang::get("options.sensitivity.max"))
                } else {
                    percent(v * 2.0)
                }
            }
            O::Gamma => {
                let p = (v * 100.0) as i32;
                generic(match p {
                    0 => lang::get("options.gamma.min"),
                    50 => lang::get("options.gamma.default"),
                    100 => lang::get("options.gamma.max"),
                    _ => format!("{}", p),
                })
            }
            O::WheelSensitivity => generic(format!("{:.2}", pow10(v))),
            O::ChatDelay => {
                if v <= 0.0 {
                    lang::get("options.chat.delay_none")
                } else {
                    lang::format("options.chat.delay", &[format!("{:.1}", v)])
                }
            }
            O::ChatWidth => lang::format("options.pixel_value", &[label.clone(), format!("{}", (v * 280.0 + 40.0) as i32)]),
            O::ChatHeightFocused | O::ChatHeightUnfocused => {
                lang::format("options.pixel_value", &[label.clone(), format!("{}", (v * 160.0 + 20.0) as i32)])
            }
            O::NotificationTime => generic(format!("{:.1}x", v)),
            O::ChunkFade => {
                if v <= 0.0 {
                    lang::get("options.chunkFade.none")
                } else {
                    lang::format("options.chunkFade.seconds", &[format!("{:.2}", v)])
                }
            }
            _ if VOLUMES.contains(&o) => {
                if v == 0.0 {
                    generic(off())
                } else {
                    percent(v)
                }
            }
            _ => match d.kind {
                Kind::Bool => generic(lang::get(if v != 0.0 { "options.on" } else { "options.off" })),
                Kind::Cycle(values) => generic(lang::get(values.get(n as usize).map_or(values[0].1, |x| x.1))),
                Kind::Int(..) => generic(format!("{}", n)),
                Kind::Real(..) => percent(v),
            },
        }
    }

    /// Where everything on the open screen is, for a GUI of `gw` x `gh` units.
    pub fn layout(&self, gw: f32, gh: f32) -> Layout {
        let (w, h) = (ceil(gw), ceil(gh));
        let cx = fl(w / 2.0);
        let mut l = Layout { widgets: Vec::new(), texts: Vec::new(), list: None, content: 0.0 };
        let page = self.page();
        l.texts.push((lang::get(page.title()), cx, 12.0));
        l.widgets.push(Placed { x: fl((w - 200.0) / 2.0), y: h - HEADER + 6.0, w: 200.0, what: W::Done });
        if page == Page::Main {
            // OptionsScreen: the FOV slider and Online... in the header (title 9, gap 8, row 20,
            // centred in 61), then the ten screens in a grid of 158 x 24 cells.
            let x0 = fl((w - 308.0) / 2.0);
            l.widgets.push(Placed { x: x0, y: 29.0, w: 150.0, what: W::Opt(O::Fov) });
            l.widgets.push(Placed { x: x0 + 158.0, y: 29.0, w: 150.0, what: W::Open(Page::Online, "options.online") });
            let grid = [
                W::Open(Page::Skin, "options.skinCustomisation"),
                W::Open(Page::Sounds, "options.sounds"),
                W::Open(Page::Video, "options.video"),
                W::Open(Page::Controls, "options.controls"),
                W::Off("options.language", None),
                W::Open(Page::Chat, "options.chat"),
                W::Off("options.resourcepack", None),
                W::Open(Page::Accessibility, "options.accessibility"),
                W::Off("options.telemetry", None),
                W::Off("options.credits_and_attribution", None),
            ];
            let gx = fl((w - 316.0) / 2.0);
            let gy = 91.0f32.min(h - HEADER - 120.0);
            for (i, what) in grid.iter().enumerate() {
                let (c, r) = ((i % 2) as f32, (i / 2) as f32);
                l.widgets.push(Placed { x: gx + 4.0 + c * 158.0, y: gy + r * 24.0, w: 150.0, what: *what });
            }
            return l;
        }
        // OptionsSubScreen: an OptionsList between the 33-unit header and footer.
        let (top, bottom) = (HEADER, h - HEADER);
        let left = fl(w / 2.0) - 155.0;
        let mut y = top + 2.0 - self.scroll;
        let mut first = true;
        let mut content = 4.0;
        for row in rows(page) {
            let height = match row {
                Row::Head(_) => {
                    if first {
                        18.0
                    } else {
                        36.0
                    }
                }
                _ => ROW,
            };
            let cy = y + 2.0;
            let inside = cy >= top && cy + 20.0 <= bottom;
            match row {
                Row::Head(key) => {
                    let pad = if first { 0.0 } else { 18.0 };
                    if cy + pad >= top && cy + pad + 9.0 <= bottom {
                        l.texts.push((format!("\u{a7}l{}", lang::get(key)), cx, cy + pad + 4.0));
                    }
                }
                Row::Big(what) if inside => l.widgets.push(Placed { x: left, y: cy, w: 310.0, what }),
                Row::Two(a, b) if inside => {
                    l.widgets.push(Placed { x: left, y: cy, w: 150.0, what: a });
                    if let Some(b) = b {
                        l.widgets.push(Placed { x: left + 160.0, y: cy, w: 150.0, what: b });
                    }
                }
                _ => {}
            }
            first = false;
            y += height;
            content += height;
        }
        l.list = Some((0.0, top, w, bottom));
        l.content = content;
        l
    }

    fn max_scroll(&self, gw: f32, gh: f32) -> f32 {
        let l = self.layout(gw, gh);
        match l.list {
            Some((_, top, _, bottom)) => (l.content - (bottom - top)).max(0.0),
            None => 0.0,
        }
    }

    /// The mouse wheel over a list (AbstractSelectionList: half a row a notch).
    pub fn wheel(&mut self, gw: f32, gh: f32, dy: i32) {
        let max = self.max_scroll(gw, gh);
        self.scroll = (self.scroll - dy as f32 * ROW / 2.0).clamp(0.0, max);
    }

    /// A left (or, with `back`, shift-) click.
    pub fn press(&mut self, gw: f32, gh: f32, mx: f32, my: f32, back: bool) -> Ev {
        let l = self.layout(gw, gh);
        let Some(p) = l.widgets.iter().find(|p| p.hit(mx, my)).copied() else { return Ev::None };
        match p.what {
            W::Done => {
                if self.back() {
                    Ev::Close
                } else {
                    Ev::Click
                }
            }
            W::Open(page, _) => {
                self.open(page);
                Ev::Click
            }
            W::Off(..) => Ev::None,
            W::Opt(O::GuiScale) => {
                let n = self.max_gui_scale.max(1) + 1;
                let v = (self.o.int(O::GuiScale).clamp(0, n - 1) + if back { -1 } else { 1 }).rem_euclid(n);
                self.set(O::GuiScale, v as f64);
                Ev::Click
            }
            W::Opt(o) => match def(o).kind {
                Kind::Bool => {
                    let v = if self.o.on(o) { 0.0 } else { 1.0 };
                    self.set(o, v);
                    Ev::Click
                }
                Kind::Cycle(values) => {
                    let n = values.len() as i32;
                    let v = (self.o.int(o) + if back { -1 } else { 1 }).rem_euclid(n);
                    self.set(o, v as f64);
                    Ev::Click
                }
                _ => {
                    self.drag = Some((o, p.x, p.w));
                    self.pending = None;
                    self.drag_to(mx);
                    Ev::None
                }
            },
        }
    }

    /// The mouse moved with a slider held (AbstractSliderButton.setValueFromMouse).
    pub fn drag_to(&mut self, mx: f32) {
        if let Some((o, x, w)) = self.drag {
            let pos = ((mx - (x + 4.0)) / (w - 8.0)) as f64;
            let v = self.value_at(o, pos);
            if Self::on_release(o) {
                self.pending = Some(v);
            } else {
                self.set(o, v);
            }
        }
    }

    /// The button came up: a slider let go plays the click (AbstractSliderButton.onRelease).
    pub fn release(&mut self) -> Ev {
        if let Some((o, ..)) = self.drag.take() {
            if let Some(v) = self.pending.take() {
                self.set(o, v);
            }
            Ev::Click
        } else {
            Ev::None
        }
    }

    pub fn draw(&self, gui: &mut Gui, mx: f32, my: f32) {
        let (w, h) = (gui.width, gui.height);
        // Screen.renderMenuBackground in a world (no blur here), the list's background and its
        // header and footer separators (the inworld_* textures: flat greys).
        gui.fill(0.0, 0.0, w, h, 0x4000_0000);
        let l = self.layout(w, h);
        if let Some((x0, top, x1, bottom)) = l.list {
            gui.fill(x0, top, x1, bottom, 0x7000_0000);
            gui.fill(x0, top - 2.0, x1, top - 1.0, 0x33FF_FFFF);
            gui.fill(x0, top - 1.0, x1, top, 0xBF00_0000);
            gui.fill(x0, bottom, x1, bottom + 1.0, 0xBF00_0000);
            gui.fill(x0, bottom + 1.0, x1, bottom + 2.0, 0x33FF_FFFF);
            let view = bottom - top;
            if l.content > view {
                let sx = fl(w / 2.0) + 155.0 + 8.0;
                let max = l.content - view;
                let sh = (view * view / l.content).clamp(32.0, view - 8.0);
                let sy = top + fl(self.scroll * (view - sh) / max);
                gui.blit_nine("widget/scroller_background", sx, top, 6.0, view);
                gui.blit_nine("widget/scroller", sx, sy, 6.0, sh);
            }
        }
        for (t, x, y) in l.texts.iter() {
            let tw = gui.text_width(t);
            gui.text(t, fl(x - tw / 2.0), *y, crate::screens::WHITE);
        }
        for p in l.widgets.iter() {
            let hover = p.hit(mx, my) && self.drag.is_none();
            match p.what {
                W::Opt(o) if Self::is_slider(o) => {
                    let held = self.drag.map_or(false, |d| d.0 == o);
                    gui.blit_nine("widget/slider", p.x, p.y, p.w, 20.0);
                    let hx = p.x + fl((self.slider_pos(o) * (p.w as f64 - 8.0)) as f32);
                    gui.blit_nine(if hover || held { "widget/slider_handle_highlighted" } else { "widget/slider_handle" }, hx, p.y, 8.0, 20.0);
                    label(gui, &self.caption(o), p, true);
                }
                W::Opt(o) => button(gui, &self.caption(o), p, true, hover),
                W::Open(_, key) => button(gui, &lang::get(key), p, true, hover),
                W::Done => button(gui, &lang::get("gui.done"), p, true, hover),
                W::Off(key, value) => {
                    let t = match value {
                        Some(v) => lang::format("options.generic_value", &[lang::get(key), lang::get(v)]),
                        None => lang::get(key),
                    };
                    button(gui, &t, p, false, false);
                }
            }
        }
    }
}

/// AbstractButton: the sprite for its state, the label centred (grey when inactive).
fn button(gui: &mut Gui, text: &str, p: &Placed, active: bool, hover: bool) {
    let sprite = if !active {
        "widget/button_disabled"
    } else if hover {
        "widget/button_highlighted"
    } else {
        "widget/button"
    };
    gui.blit_nine(sprite, p.x, p.y, p.w, 20.0);
    label(gui, text, p, active);
}

/// AbstractWidget.renderScrollingString: centred, or, when it does not fit inside the 2-unit
/// margins, sliding back and forth (only the whole letters inside are drawn: no clipping here).
fn label(gui: &mut Gui, text: &str, p: &Placed, active: bool) {
    let color = if active { 0xFFFF_FFFF } else { 0xFFA0_A0A0 };
    let tw = gui.text_width(text);
    let room = p.w - 4.0;
    if tw <= room {
        gui.text(text, fl(p.x + (p.w - tw) / 2.0), p.y + 6.0, color);
        return;
    }
    let over = (tw - room) as f64;
    let period = (over * 0.5).max(3.0);
    let t = math::sin(core::f64::consts::FRAC_PI_2 * math::cos(core::f64::consts::TAU * sys::now() / period)) / 2.0 + 0.5;
    let shift = fl((t * over) as f32);
    let mut x = p.x + 2.0 - shift;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let s = ch.encode_utf8(&mut buf);
        let cw = gui.text_width(s);
        if x >= p.x + 2.0 - 0.01 && x + cw <= p.x + p.w - 2.0 + 0.01 {
            gui.text(s, x, p.y + 6.0, color);
        }
        x += cw;
    }
}

/// The largest GUI scale the window allows (Window.calculateScale with 0, as Gui.begin does).
pub fn auto_gui_scale(w: i32, h: i32) -> i32 {
    let mut s = 1;
    while s < 6 && w / (s + 1) >= 320 && h / (s + 1) >= 240 {
        s += 1;
    }
    s
}

/// AbstractClientPlayer.getFieldOfViewModifier: flying and the movement speed (sprinting adds
/// 30%) widen the view, scaled by the FOV Effects option.
pub fn fov_modifier(flying: bool, move_speed: f64, sprinting: bool, effect: f64) -> f32 {
    let mut f = 1.0f64;
    if flying {
        f *= 1.1;
    }
    let speed = move_speed * if sprinting { 1.3 } else { 1.0 };
    f *= (speed / 0.1 + 1.0) / 2.0;
    (1.0 + (f - 1.0) * effect.clamp(0.0, 1.0)) as f32
}

/// Checks for `penryn selftest`: options.txt round trip, captions, slider steps, layout.
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut check = |what: &'static str, got: String, want: &'static str| out.push((what, got, want));
    let s = Settings::parse("fov:1.0\nrenderDistance:6\nsoundCategory_music:0.25\nmainHand:\"left\"\nbobView:false\nmaxFps:260\nlastServer:example.org\nmouseWheelSensitivity:1.0\n");
    check("options.txt fov", format!("{}", s.int(O::Fov)), "110");
    check("options.txt view", format!("{} {}", s.int(O::RenderDistance), s.given(O::RenderDistance)), "6 true");
    check("options.txt music", format!("{}", s.get(O::Music)), "0.25");
    check("options.txt hand", format!("{} {}", s.int(O::MainHand), s.on(O::BobView)), "0 false");
    let text = s.text();
    let again = Settings::parse(&text);
    let same = DEFS.iter().enumerate().all(|(i, _)| (again.v[i] - s.v[i]).abs() < 1e-9);
    check("options.txt round trip", format!("{} {}", same, text.contains("lastServer:example.org\n")), "true true");
    check("options.txt lines", format!("{}", text.lines().filter(|l| l.starts_with("fov:") || l.starts_with("mainHand:")).collect::<Vec<_>>().join(" ")), "fov:1.0 mainHand:\"left\"");
    check("options.txt wheel", format!("{:.3}", pow10(again.get(O::WheelSensitivity))), "1.000");
    let mut m = Menu::new(Settings::default());
    m.saving = false;
    check("slider fps steps", format!("{} {}", m.value_at(O::MaxFps, 0.5), m.value_at(O::MaxFps, 1.0)), "140 260");
    check("slider view", format!("{}", m.value_at(O::RenderDistance, 0.33)), "12");
    m.max_gui_scale = 3;
    check("slider gui scale", format!("{}", m.value_at(O::GuiScale, 0.7)), "2");
    m.max_gui_scale = 3;
    m.set(O::GuiScale, 3.0);
    m.open(Page::Video);
    let gl = m.layout(480.0, 270.0);
    let gs = gl.widgets.iter().find(|p| p.what == W::Opt(O::GuiScale)).copied();
    let cycled = gs.map(|p| {
        m.press(480.0, 270.0, p.x + 1.0, p.y + 1.0, false);
        m.o.int(O::GuiScale)
    });
    check("gui scale cycles to auto", format!("{:?}", cycled), "Some(0)");
    m.changed.clear();
    m.scroll = 120.0;
    let rd = m.layout(480.0, 270.0).widgets.iter().find(|p| p.what == W::Opt(O::RenderDistance)).copied();
    let applied = rd.map(|p| {
        m.press(480.0, 270.0, p.x + 4.0, p.y + 1.0, false);
        let during = (m.o.int(O::RenderDistance), m.changed.len());
        m.release();
        (during, m.o.int(O::RenderDistance))
    });
    check("render distance applies on release", format!("{:?}", applied), "Some(((12, 0), 2))");
    m.set(O::RenderDistance, 12.0);
    check("slider handle for a step", format!("{:.4}", m.slider_pos(O::RenderDistance)), "0.3387");
    check("turn per pixel", format!("{:.3}", Settings::default().turn_per_pixel().0), "0.150");
    check("night overlay at default brightness", format!("{:.1}", Settings::default().night_alpha(140.0)), "140.0");
    let fov = fov_modifier(false, 0.1, true, 1.0);
    check("sprint fov", format!("{:.3}", fov), "1.150");
    // OptionsScreen at a 480 x 270 GUI: the FOV slider, the grid and Done where vanilla puts them.
    m.open(Page::Main);
    let l = m.layout(480.0, 270.0);
    let at = |w: W| l.widgets.iter().find(|p| p.what == w).map(|p| format!("{} {}", p.x, p.y)).unwrap_or_default();
    check("options screen FOV", at(W::Opt(O::Fov)), "86 29");
    check("options screen sounds", at(W::Open(Page::Sounds, "options.sounds")), "244 91");
    check("options screen done", at(W::Done), "140 243");
    m.open(Page::Sounds);
    let l = m.layout(480.0, 270.0);
    let at = |w: W| l.widgets.iter().find(|p| p.what == w).map(|p| format!("{} {}", p.x, p.y)).unwrap_or_default();
    check("sounds master", at(W::Opt(O::Master)), "85 37");
    check("sounds music", at(W::Opt(O::Music)), "85 62");
    check("sounds records", at(W::Opt(O::Record)), "245 62");
    let before = m.page();
    m.back();
    check("back to options", format!("{:?} {:?}", before, m.page()), "Sounds Main");
    let info = client_information(8);
    let hex: String = info.b.iter().map(|b| format!("{:02x}", b)).collect();
    check("client information", hex, "05656e5f75730800017f01000100");
    out
}
