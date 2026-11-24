//! What servers put on the screen besides the world, as vanilla's client keeps it: the tab
//! list (ClientboundPlayerInfoUpdatePacket, the header and footer), the scoreboard (objectives,
//! scores with their number formats, display slots), teams (prefixes, suffixes, colours, name
//! tag visibility), boss bars (LerpingBossEvent), titles and the action bar (Hud.setTitle,
//! setOverlayMessage), the world border (WorldBorder with its tick-based lerp), server links,
//! cookies, transfers, the death message (ClientboundPlayerCombatKillPacket), the camera
//! (ClientboundSetCameraPacket) and the death-screen game rules. Drawing is in serverhud.rs.

use crate::client::Game;
use crate::conn::{Conn, Packet};
use crate::nbt;
use crate::wire::{In, Out};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// Play packets (horizon-protocol ids.rs, 26.3).
pub const C_BOSS_EVENT: i32 = 9;
pub const C_CLEAR_TITLES: i32 = 14;
pub const C_COOKIE_REQUEST: i32 = 21;
pub const C_GAME_EVENT: i32 = 39;
pub const C_INITIALIZE_BORDER: i32 = 44;
pub const C_LOGIN: i32 = 50;
pub const C_PLAYER_COMBAT_KILL: i32 = 69;
pub const C_RESET_SCORE: i32 = 80;
pub const C_SET_ACTION_BAR_TEXT: i32 = 89;
pub const C_SET_BORDER_CENTER: i32 = 90;
pub const C_SET_BORDER_LERP_SIZE: i32 = 91;
pub const C_SET_BORDER_SIZE: i32 = 92;
pub const C_SET_BORDER_WARNING_DELAY: i32 = 93;
pub const C_SET_BORDER_WARNING_DISTANCE: i32 = 94;
pub const C_SET_CAMERA: i32 = 95;
pub const C_SET_DISPLAY_OBJECTIVE: i32 = 100;
pub const C_SET_ENTITY_DATA: i32 = 101;
pub const C_SET_OBJECTIVE: i32 = 108;
pub const C_SET_PLAYER_TEAM: i32 = 111;
pub const C_SET_SCORE: i32 = 112;
pub const C_SET_SUBTITLE_TEXT: i32 = 114;
pub const C_SET_TITLE_TEXT: i32 = 116;
pub const C_SET_TITLES_ANIMATION: i32 = 117;
pub const C_STORE_COOKIE: i32 = 122;
pub const C_SYSTEM_CHAT: i32 = 124;
pub const C_TAB_LIST: i32 = 125;
pub const C_TRANSFER: i32 = 132;
pub const C_SERVER_LINKS: i32 = 140;
pub const S_COOKIE_RESPONSE: i32 = 21;
// Configuration packets.
const CC_STORE_COOKIE: i32 = 11;
const CC_TRANSFER: i32 = 12;
const CC_UPDATE_TAGS: i32 = 14;
const CC_SERVER_LINKS: i32 = 17;
pub const C_UPDATE_TAGS: i32 = 137;
pub const CS_CUSTOM_PAYLOAD: i32 = 2;

/// What outlives one connection: the cookies (kept through a transfer, as vanilla's
/// TransferState), the server links (sent in the configuration, used in play) and a transfer
/// the server asked for.
#[derive(Default, Clone)]
pub struct Common {
    pub cookies: Vec<(String, Vec<u8>)>,
    /// (label, url), only http and https links (Util.parseAndValidateUntrustedUri).
    pub links: Vec<(String, String)>,
    pub transfer: Option<(String, u16)>,
    /// The block tags (ClientboundUpdateTagsPacket), for can_break and can_place_on.
    pub block_tags: BTreeMap<String, Vec<i32>>,
}

impl Common {
    fn cookie(&self, key: &str) -> Option<&Vec<u8>> {
        self.cookies.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn store(&mut self, key: String, v: Vec<u8>) {
        self.cookies.retain(|(k, _)| *k != key);
        // Vanilla keeps every cookie; a bound against a server filling memory.
        if self.cookies.len() < 256 {
            self.cookies.push((key, v));
        }
    }
    /// The answer to a cookie request: the key, then the payload when one is stored.
    fn answer(&self, key: &str) -> Out {
        let mut o = Out::new();
        o.string(key);
        match self.cookie(key) {
            Some(v) => {
                o.bool(true).varint(v.len() as i32).bytes(v);
            }
            None => {
                o.bool(false);
            }
        }
        o
    }
}

/// A login-phase cookie request (ServerboundCookieResponsePacket of the login protocol).
pub fn login_cookie(c: &mut Conn, r: &mut In, common: &Common) -> Result<(), String> {
    let key = r.string().unwrap_or_default();
    c.send(crate::ids::login::S_COOKIE_RESPONSE, &common.answer(&key))
}

/// The brand vanilla announces as soon as the login is finished (ClientBrandRetriever).
pub fn send_brand(c: &mut Conn) -> Result<(), String> {
    let mut o = Out::new();
    o.string("minecraft:brand").string("vanilla");
    c.send(CS_CUSTOM_PAYLOAD, &o)
}

/// A configuration packet the session loop does not handle itself. `Err("transfer")` when the
/// server sends the player elsewhere.
pub fn config_packet(c: &mut Conn, p: &Packet, common: &mut Common) -> Result<bool, String> {
    let mut r = p.reader();
    match p.id {
        crate::ids::config::C_COOKIE_REQUEST => {
            let key = r.string().unwrap_or_default();
            c.send(crate::ids::config::S_COOKIE_RESPONSE, &common.answer(&key))?;
        }
        CC_STORE_COOKIE => store_cookie(&mut r, common),
        CC_SERVER_LINKS => common.links = server_links(&mut r),
        CC_UPDATE_TAGS => {
            update_tags(&mut r, common);
            return Ok(false);
        }
        CC_TRANSFER => {
            if let Some(t) = transfer(&mut r) {
                common.transfer = Some(t);
                return Err(String::from("transfer"));
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// ClientboundUpdateTagsPacket: per registry its tags and their ids; the blocks' are kept.
fn update_tags(r: &mut In, common: &mut Common) -> Option<()> {
    let n = r.varint()?;
    for _ in 0..n.clamp(0, 1024) {
        let reg = r.string()?;
        let k = r.varint()?;
        for _ in 0..k.clamp(0, 65536) {
            let tag = r.string()?;
            let m = r.varint()?;
            if m < 0 || m as usize > r.left() {
                return None;
            }
            let mut ids = Vec::with_capacity(m as usize);
            for _ in 0..m {
                ids.push(r.varint()?);
            }
            if reg == "minecraft:block" {
                common.block_tags.insert(tag, ids);
            }
        }
    }
    Some(())
}

fn store_cookie(r: &mut In, common: &mut Common) {
    let Some(key) = r.string() else { return };
    let n = r.varint().unwrap_or(-1);
    // ByteBufCodecs.byteArray(5120).
    if (0..=5120).contains(&n) {
        if let Some(b) = r.take(n as usize) {
            common.store(key, b.to_vec());
        }
    }
}

fn transfer(r: &mut In) -> Option<(String, u16)> {
    let host = r.string()?;
    let port = r.varint()?;
    if host.is_empty() || !(1..=65535).contains(&port) {
        return None;
    }
    Some((host, port as u16))
}

/// ServerLinks.UntrustedEntry list: a known type (its translation) or a label, then the link.
fn server_links(r: &mut In) -> Vec<(String, String)> {
    const KNOWN: [&str; 10] =
        ["report_bug", "community_guidelines", "support", "status", "feedback", "community", "website", "forums", "news", "announcements"];
    let mut out = Vec::new();
    let n = r.varint().unwrap_or(0);
    for _ in 0..n.clamp(0, 256) {
        let label = match r.bool() {
            Some(true) => {
                let id = r.varint().unwrap_or(0);
                crate::lang::get(&format!("known_server_link.{}", KNOWN.get(id as usize).copied().unwrap_or(KNOWN[0])))
            }
            Some(false) => nbt::text(r),
            None => break,
        };
        let Some(link) = r.string() else { break };
        let l = link.to_ascii_lowercase();
        if l.starts_with("http://") || l.starts_with("https://") {
            out.push((label, link));
        }
    }
    out
}

// ------------------------------------------------------------------ the state

/// One entry of the tab list (PlayerInfo).
#[derive(Clone)]
pub struct TabPlayer {
    pub name: String,
    pub game_mode: i32,
    pub listed: bool,
    pub latency: i32,
    /// The display name component, as text with formatting codes.
    pub display: Option<String>,
    pub order: i32,
    pub hat: bool,
}

#[derive(Clone)]
pub struct Team {
    pub display: String,
    pub prefix: String,
    pub suffix: String,
    /// Team.Visibility: 0 always, 1 never, 2 hide for other teams, 3 hide for own team.
    pub visibility: i32,
    /// Team.CollisionRule: 0 always, 1 never, 2 push other teams, 3 push own team.
    pub collision: i32,
    /// TeamColor (ChatFormatting 0 black .. 15 white).
    pub color: Option<i32>,
    /// 1 friendly fire, 2 see friendly invisibles.
    pub options: i8,
    pub players: Vec<String>,
}

/// NumberFormat: how a score is written.
#[derive(Clone, PartialEq, Debug)]
pub enum NumFmt {
    Blank,
    /// A style: its colour as a formatting code.
    Styled(String),
    /// A fixed component's text.
    Fixed(String),
}

impl NumFmt {
    pub fn format(&self, v: i32) -> String {
        match self {
            NumFmt::Blank => String::new(),
            NumFmt::Styled(code) => format!("{}{}", code, v),
            NumFmt::Fixed(t) => t.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Objective {
    pub display: String,
    /// ObjectiveCriteria.RenderType HEARTS (else INTEGER).
    pub hearts: bool,
    pub format: Option<NumFmt>,
}

#[derive(Clone)]
pub struct Score {
    pub value: i32,
    pub display: Option<String>,
    pub format: Option<NumFmt>,
}

/// LerpingBossEvent.
#[derive(Clone)]
pub struct Boss {
    pub id: [u8; 16],
    pub name: String,
    /// The progress it lerps from, to, and when the target was set (ms).
    pub from: f32,
    pub progress: f32,
    pub set_at: u64,
    /// BossBarColor: pink, blue, red, green, yellow, purple, white.
    pub color: i32,
    /// BossBarOverlay: progress, notched 6, 10, 12, 20.
    pub overlay: i32,
    /// 1 darken the sky, 2 play boss music, 4 create world fog.
    pub flags: u8,
}

impl Boss {
    /// LerpingBossEvent.getProgress: 100 ms from the last value to the new one.
    pub fn shown_progress(&self, now: u64) -> f32 {
        let t = (now.saturating_sub(self.set_at) as f32 / 100.0).clamp(0.0, 1.0);
        self.from + (self.progress - self.from) * t
    }
}

/// WorldBorder: centre, size lerping over ticks, warnings.
#[derive(Clone)]
pub struct Border {
    pub center: [f64; 2],
    /// The size being lerped from and to, and the ticks left of the lerp out of its length.
    pub from: f64,
    pub to: f64,
    pub left: i64,
    pub duration: i64,
    /// This and last tick's size (drawn between them).
    pub size: f64,
    pub prev_size: f64,
    pub absolute_max: i32,
    pub warning_blocks: i32,
    pub warning_time: i32,
}

impl Default for Border {
    fn default() -> Border {
        Border {
            center: [0.0; 2],
            from: 5.9999968e7,
            to: 5.9999968e7,
            left: 0,
            duration: 0,
            size: 5.9999968e7,
            prev_size: 5.9999968e7,
            absolute_max: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        }
    }
}

impl Border {
    fn calc(&self) -> f64 {
        if self.left > 0 && self.duration > 0 {
            let t = 1.0 - self.left as f64 / self.duration as f64;
            self.from + (self.to - self.from) * t
        } else {
            self.to
        }
    }
    fn set_size(&mut self, s: f64) {
        self.from = s;
        self.to = s;
        self.left = 0;
        self.duration = 0;
        self.size = s;
        self.prev_size = s;
    }
    fn lerp(&mut self, from: f64, to: f64, ticks: i64) {
        if from == to || ticks <= 0 {
            return self.set_size(to);
        }
        self.from = from;
        self.to = to;
        self.duration = ticks;
        self.left = ticks;
        self.size = self.calc();
        self.prev_size = self.size;
    }
    /// WorldBorder.tick: the moving extent's update.
    pub fn tick(&mut self) {
        self.prev_size = self.size;
        if self.left > 0 {
            self.left -= 1;
            self.size = self.calc();
            if self.left <= 0 {
                self.set_size(self.to);
            }
        }
    }
    pub fn size_at(&self, partial: f64) -> f64 {
        self.prev_size + (self.size - self.prev_size) * partial
    }
    /// min x, min z, max x, max z, as getMinX(partial) and the others (clamped to the absolute
    /// maximum).
    pub fn bounds(&self, partial: f64) -> [f64; 4] {
        let h = self.size_at(partial) / 2.0;
        let m = self.absolute_max as f64;
        [
            (self.center[0] - h).clamp(-m, m),
            (self.center[1] - h).clamp(-m, m),
            (self.center[0] + h).clamp(-m, m),
            (self.center[1] + h).clamp(-m, m),
        ]
    }
    /// WorldBorder.getDistanceToBorder.
    pub fn distance(&self, x: f64, z: f64) -> f64 {
        let b = self.bounds(1.0);
        let d = (z - b[1]).min(b[3] - z);
        let e = (x - b[0]).min(b[2] - x);
        d.min(e)
    }
    /// Blocks per tick the border moves (getLerpSpeed), 0 when still.
    pub fn speed(&self) -> f64 {
        if self.left > 0 && self.duration > 0 {
            (self.from - self.to).abs() / self.duration as f64
        } else {
            0.0
        }
    }
    pub fn moving(&self) -> i32 {
        if self.left > 0 {
            if self.to < self.from {
                -1
            } else {
                1
            }
        } else {
            0
        }
    }
}

pub struct ServerUi {
    pub common: Common,
    pub players: BTreeMap<[u8; 16], TabPlayer>,
    pub header: Option<String>,
    pub footer: Option<String>,
    pub teams: BTreeMap<String, Team>,
    /// Which team each player (score holder) is in.
    pub team_of: BTreeMap<String, String>,
    pub objectives: BTreeMap<String, Objective>,
    /// Scores by holder, then objective.
    pub scores: BTreeMap<String, BTreeMap<String, Score>>,
    /// DisplaySlot: 0 list, 1 sidebar, 2 below name, 3.. the team colours' sidebars.
    pub display: [Option<String>; 19],
    pub bosses: Vec<Boss>,
    /// GameRenderer.darkenWorldAmount, this and last tick.
    pub darken: f32,
    pub prev_darken: f32,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub title_time: i32,
    pub fade_in: i32,
    pub stay: i32,
    pub fade_out: i32,
    /// The action bar: text, ticks left, animated colour (the record's "Now Playing").
    pub action: Option<String>,
    pub action_time: i32,
    pub action_animate: bool,
    pub border: Border,
    /// ClientboundPlayerCombatKillPacket's message for the death screen.
    pub death_message: Option<String>,
    /// The login's showDeathScreen inverted, and game event 11.
    pub immediate_respawn: bool,
    pub hardcore: bool,
    /// The entity the view is attached to (spectating), None for the player.
    pub camera: Option<i32>,
    /// Player.DATA_SCORE_ID of our own player.
    pub score: i32,
    /// The connection is encrypted (online mode): vanilla's tab list shows faces only then.
    pub online: bool,
    /// Gui ticks (the hearts' blinking in the tab list).
    pub ticks: u32,
    /// PlayerTabOverlay.healthStates: last value, displayed value, blink until.
    pub health_states: BTreeMap<[u8; 16], (i32, i32, u32)>,
    /// Bumped when anything a name tag shows changes.
    pub version: u32,
    /// Player names as name tags show them (team prefix, colour, suffix), for entities.rs.
    pub tag_names: BTreeMap<[u8; 16], String>,
    /// The below-name objective's line per player.
    pub below_names: BTreeMap<[u8; 16], String>,
    tags_version: u32,
}

impl ServerUi {
    /// A new listener (reconfiguration, login): the tab list, teams, scoreboard, boss bars,
    /// titles, border and name-tag caches start empty. What outlives the connection
    /// (`common`) and what the connection itself says (`online`) stay. The login packet's
    /// hardcore and immediate_respawn are read after this reset (Game::handle resets first).
    /// `version` only ever grows, so a cache keyed on it never meets an old key again.
    pub fn reset_session(&mut self) {
        let common = core::mem::take(&mut self.common);
        let (online, ticks, version) = (self.online, self.ticks, self.version);
        *self = ServerUi::new(common);
        self.online = online;
        self.ticks = ticks;
        self.version = version.wrapping_add(1);
    }

    pub fn new(mut common: Common) -> ServerUi {
        // Tests: PENRYN_TEST_LINKS="label|url;label|url" stands in for the server's links.
        if let (true, Some(t)) = (common.links.is_empty(), crate::sys::env("PENRYN_TEST_LINKS")) {
            common.links = t.split(';').filter_map(|l| l.split_once('|')).map(|(a, b)| (String::from(a), String::from(b))).collect();
        }
        ServerUi {
            common,
            players: BTreeMap::new(),
            header: None,
            footer: None,
            teams: BTreeMap::new(),
            team_of: BTreeMap::new(),
            objectives: BTreeMap::new(),
            scores: BTreeMap::new(),
            display: Default::default(),
            bosses: Vec::new(),
            darken: 0.0,
            prev_darken: 0.0,
            title: None,
            subtitle: None,
            title_time: 0,
            fade_in: 10,
            stay: 70,
            fade_out: 20,
            action: None,
            action_time: 0,
            action_animate: false,
            border: Border::default(),
            death_message: None,
            immediate_respawn: false,
            hardcore: false,
            camera: None,
            score: 0,
            online: false,
            ticks: 0,
            health_states: BTreeMap::new(),
            version: 0,
            tag_names: BTreeMap::new(),
            below_names: BTreeMap::new(),
            tags_version: u32::MAX,
        }
    }

    /// Hud.tick and the parts of GameRenderer.tick and ClientLevel.tick that are ours.
    pub fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.action_time > 0 {
            self.action_time -= 1;
            if self.action_time == 0 {
                self.action = None;
            }
        }
        if self.title_time > 0 {
            self.title_time -= 1;
            if self.title_time <= 0 {
                self.title = None;
                self.subtitle = None;
            }
        }
        self.prev_darken = self.darken;
        if self.bosses.iter().any(|b| b.flags & 1 != 0) {
            self.darken = (self.darken + 0.05).min(1.0);
        } else if self.darken > 0.0 {
            self.darken = (self.darken - 0.0125).max(0.0);
        }
        self.border.tick();
    }

    pub fn darken_at(&self, partial: f32) -> f32 {
        self.prev_darken + (self.darken - self.prev_darken) * partial
    }

    /// BossHealthOverlay.shouldCreateWorldFog.
    pub fn boss_fog(&self) -> bool {
        self.bosses.iter().any(|b| b.flags & 4 != 0)
    }

    pub fn team_of_player(&self, name: &str) -> Option<&Team> {
        self.teams.get(self.team_of.get(name)?)
    }

    /// PlayerTeam.formatNameForTeam: the prefix, the name in the team's colour, the suffix.
    pub fn format_name(&self, holder: &str, name: &str) -> String {
        match self.team_of_player(holder) {
            Some(t) => {
                let c = t.color.map(colour_code).unwrap_or_default();
                format!("{}\u{a7}r{}{}\u{a7}r{}\u{a7}r", t.prefix, c, name, t.suffix)
            }
            None => String::from(name),
        }
    }

    /// The objective in a display slot.
    pub fn shown(&self, slot: usize) -> Option<(&String, &Objective)> {
        let name = self.display.get(slot)?.as_ref()?;
        self.objectives.get_key_value(name)
    }

    /// Hud.extractScoreboardSidebar: the team colour's sidebar first, then the sidebar.
    pub fn sidebar(&self, own_name: &str) -> Option<(&String, &Objective)> {
        if let Some(c) = self.team_of_player(own_name).and_then(|t| t.color) {
            if let Some(o) = self.shown(3 + c.clamp(0, 15) as usize) {
                return Some(o);
            }
        }
        self.shown(1)
    }

    pub fn score_of(&self, holder: &str, objective: &str) -> Option<&Score> {
        self.scores.get(holder)?.get(objective)
    }

    /// The name tag text of a player: None when the team hides it from us
    /// (Team.Visibility and LivingEntity.shouldShowName).
    pub fn tag_visible(&self, holder: &str, own: &str) -> bool {
        let Some(t) = self.team_of_player(holder) else { return true };
        let mine = self.team_of.get(own);
        let same = mine.is_some() && mine == self.team_of.get(holder);
        // EntityRenderer.shouldShowName: a viewer in no team sees both "hide for" kinds.
        match t.visibility {
            1 => false,
            2 => mine.is_none() || same,
            3 => !same,
            _ => true,
        }
    }

    /// Rebuilds the name tags' names when teams or players changed.
    pub fn refresh_tags(&mut self, own: &str) {
        if self.tags_version == self.version {
            return;
        }
        self.tags_version = self.version;
        let mut m = BTreeMap::new();
        let mut below = BTreeMap::new();
        for (u, p) in self.players.iter() {
            if let Some(b) = self.below_name(&p.name) {
                below.insert(*u, b);
            }
            // A player's name tag is its team-formatted name (Player.getDisplayName), hidden
            // as the team's visibility says.
            if p.name != own && !self.tag_visible(&p.name, own) {
                m.insert(*u, String::new());
                continue;
            }
            m.insert(*u, self.format_name(&p.name, &p.name));
        }
        self.tag_names = m;
        self.below_names = below;
    }

    /// The below-name objective's line for a player (AvatarRenderer: score, a space, the
    /// objective's name).
    pub fn below_name(&self, holder: &str) -> Option<String> {
        let (name, o) = self.shown(2)?;
        let s = self.score_of(holder, name);
        let f = s.and_then(|s| s.format.clone()).or_else(|| o.format.clone()).unwrap_or(NumFmt::Styled(String::new()));
        Some(format!("{}\u{a7}r {}", f.format(s.map_or(0, |s| s.value)), o.display))
    }

    fn title_reset(&mut self) {
        self.fade_in = 10;
        self.stay = 70;
        self.fade_out = 20;
    }
}

/// A ChatFormatting colour's code ("§c" for red...).
pub fn colour_code(c: i32) -> String {
    format!("\u{a7}{}", char::from_digit(c.clamp(0, 15) as u32, 16).unwrap_or('f'))
}

/// A Style compound (NumberFormat styled): its colour as a formatting code.
fn style_code(r: &mut In) -> Option<String> {
    let t = r.u8()?;
    if t != 10 {
        nbt::skip_payload(r, t)?;
        return Some(String::new());
    }
    let mut code = String::new();
    loop {
        let ct = r.u8()?;
        if ct == 0 {
            break;
        }
        let n = r.u16()? as usize;
        let name = String::from_utf8_lossy(r.take(n)?).into_owned();
        if name == "color" && ct == 8 {
            let n = r.u16()? as usize;
            let v = String::from_utf8_lossy(r.take(n)?).into_owned();
            code = named_colour(&v).unwrap_or_default();
        } else {
            nbt::skip_payload(r, ct)?;
        }
    }
    Some(code)
}

/// A colour name or "#RRGGBB" as a formatting code.
pub fn named_colour(name: &str) -> Option<String> {
    const NAMES: [&str; 16] = [
        "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green", "aqua",
        "red", "light_purple", "yellow", "white",
    ];
    if let Some(i) = NAMES.iter().position(|n| *n == name) {
        return Some(colour_code(i as i32));
    }
    if name.len() == 7 && name.starts_with('#') {
        return Some(format!("\u{a7}{}", name));
    }
    None
}

/// NumberFormatTypes.STREAM_CODEC: 0 blank, 1 styled, 2 fixed.
fn number_format(r: &mut In) -> Option<NumFmt> {
    Some(match r.varint()? {
        0 => NumFmt::Blank,
        1 => NumFmt::Styled(style_code(r)?),
        2 => NumFmt::Fixed(nbt::text(r)),
        _ => return None,
    })
}

fn opt_number_format(r: &mut In) -> Option<Option<NumFmt>> {
    Some(if r.bool()? { Some(number_format(r)?) } else { None })
}

/// A component that is empty when it says nothing (the tab list's header and footer).
fn non_empty(s: String) -> Option<String> {
    let mut plain = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\u{a7}' {
            if it.next() == Some('#') {
                for _ in 0..6 {
                    it.next();
                }
            }
            continue;
        }
        plain.push(c);
    }
    if plain.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// The play packets that are ours. `Ok(true)` when the packet was taken; `Ok(false)` lets
/// the session handle it too (the login, game events, system chat that is not an overlay).
pub fn handle(g: &mut Game, p: &Packet) -> Result<bool, String> {
    let mut r = p.reader();
    let ui = &mut g.ui;
    match p.id {
        crate::ids::play::C_PLAYER_INFO_UPDATE => {
            crate::skins::player_info(&mut p.reader(), g.uuid);
            crate::mobs::player_info(&mut p.reader(), &mut g.player_names);
            player_info(&mut r, ui);
            ui.version = ui.version.wrapping_add(1);
        }
        crate::ids::play::C_PLAYER_INFO_REMOVE => {
            crate::skins::player_remove(&mut p.reader());
            crate::mobs::player_info_remove(&mut p.reader(), &mut g.player_names);
            let n = r.varint().unwrap_or(0);
            for _ in 0..n.clamp(0, 4096) {
                let Some(u) = r.uuid() else { break };
                ui.players.remove(&u);
                ui.health_states.remove(&u);
            }
            ui.version = ui.version.wrapping_add(1);
        }
        C_TAB_LIST => {
            ui.header = non_empty(nbt::text(&mut r));
            ui.footer = non_empty(nbt::text(&mut r));
        }
        C_SET_OBJECTIVE => {
            let Some(name) = r.string() else { return Ok(true) };
            match r.u8().map(|m| m as i8) {
                Some(0) | Some(2) => {
                    let display = nbt::text(&mut r);
                    let hearts = r.varint() == Some(1);
                    let format = opt_number_format(&mut r).flatten();
                    ui.objectives.insert(name, Objective { display, hearts, format });
                }
                Some(1) => {
                    ui.objectives.remove(&name);
                    for s in ui.scores.values_mut() {
                        s.remove(&name);
                    }
                    for d in ui.display.iter_mut() {
                        if d.as_deref() == Some(name.as_str()) {
                            *d = None;
                        }
                    }
                }
                _ => {}
            }
            ui.version = ui.version.wrapping_add(1);
        }
        C_SET_DISPLAY_OBJECTIVE => {
            let slot = r.varint().unwrap_or(-1);
            let name = r.string().unwrap_or_default();
            if (0..19).contains(&slot) {
                ui.display[slot as usize] = if name.is_empty() { None } else { Some(name) };
            }
            ui.version = ui.version.wrapping_add(1);
        }
        C_SET_SCORE => {
            let (Some(owner), Some(obj), Some(value)) = (r.string(), r.string(), r.varint()) else { return Ok(true) };
            let display = match r.bool() {
                Some(true) => Some(nbt::text(&mut r)),
                _ => None,
            };
            let format = opt_number_format(&mut r).flatten();
            ui.scores.entry(owner).or_default().insert(obj, Score { value, display, format });
            ui.version = ui.version.wrapping_add(1);
        }
        C_RESET_SCORE => {
            let owner = r.string().unwrap_or_default();
            let obj = if r.bool() == Some(true) { r.string() } else { None };
            match obj {
                Some(o) => {
                    if let Some(s) = ui.scores.get_mut(&owner) {
                        s.remove(&o);
                    }
                }
                None => {
                    ui.scores.remove(&owner);
                }
            }
            ui.version = ui.version.wrapping_add(1);
        }
        C_SET_PLAYER_TEAM => {
            team(&mut r, ui);
            ui.version = ui.version.wrapping_add(1);
        }
        C_BOSS_EVENT => boss(&mut r, ui),
        C_SET_TITLE_TEXT => {
            ui.title = Some(nbt::text(&mut r));
            ui.title_time = ui.fade_in + ui.stay + ui.fade_out;
        }
        C_SET_SUBTITLE_TEXT => ui.subtitle = Some(nbt::text(&mut r)),
        C_SET_TITLES_ANIMATION => {
            let (a, b, c) = (r.i32().unwrap_or(10), r.i32().unwrap_or(70), r.i32().unwrap_or(20));
            // Hud.setTimes: a title on screen restarts with the new times.
            // Gui.setTimes: a negative time keeps the old one.
            if a >= 0 {
                ui.fade_in = a;
            }
            if b >= 0 {
                ui.stay = b;
            }
            if c >= 0 {
                ui.fade_out = c;
            }
            if ui.title_time > 0 {
                ui.title_time = ui.fade_in + ui.stay + ui.fade_out;
            }
        }
        C_CLEAR_TITLES => {
            let reset = r.bool().unwrap_or(false);
            ui.title = None;
            ui.subtitle = None;
            ui.title_time = 0;
            if reset {
                ui.title_reset();
            }
        }
        C_SET_ACTION_BAR_TEXT => {
            ui.action = Some(nbt::text(&mut r));
            ui.action_time = 60;
            ui.action_animate = false;
        }
        C_SYSTEM_CHAT => {
            let t = nbt::text(&mut r);
            if r.bool() != Some(true) {
                return Ok(false);
            }
            ui.action = Some(t);
            ui.action_time = 60;
            ui.action_animate = false;
        }
        C_INITIALIZE_BORDER => {
            let (Some(x), Some(z), Some(old), Some(new)) = (r.f64(), r.f64(), r.f64(), r.f64()) else { return Ok(true) };
            let t = r.varlong().unwrap_or(0);
            let b = &mut ui.border;
            b.center = [x, z];
            b.absolute_max = r.varint().unwrap_or(29_999_984);
            b.warning_blocks = r.varint().unwrap_or(5);
            b.warning_time = r.varint().unwrap_or(15);
            if t > 0 {
                b.lerp(old, new, t);
            } else {
                b.set_size(new);
            }
        }
        C_SET_BORDER_CENTER => {
            if let (Some(x), Some(z)) = (r.f64(), r.f64()) {
                ui.border.center = [x, z];
            }
        }
        C_SET_BORDER_LERP_SIZE => {
            if let (Some(a), Some(b)) = (r.f64(), r.f64()) {
                let t = r.varlong().unwrap_or(0);
                ui.border.lerp(a, b, t);
            }
        }
        C_SET_BORDER_SIZE => {
            if let Some(s) = r.f64() {
                ui.border.set_size(s);
            }
        }
        C_SET_BORDER_WARNING_DELAY => ui.border.warning_time = r.varint().unwrap_or(15),
        C_SET_BORDER_WARNING_DISTANCE => ui.border.warning_blocks = r.varint().unwrap_or(5),
        C_SET_CAMERA => {
            let id = r.varint().unwrap_or(g.entity_id);
            g.ui.camera = if id == g.entity_id || !g.entities.contains_key(&id) { None } else { Some(id) };
        }
        C_PLAYER_COMBAT_KILL => {
            let id = r.varint().unwrap_or(-1);
            if id == g.entity_id {
                g.ui.death_message = Some(nbt::text(&mut r));
                // A death arrives before the health packet: the screen opens on it.
                g.dead = true;
            }
        }
        C_COOKIE_REQUEST => {
            let key = r.string().unwrap_or_default();
            let o = g.ui.common.answer(&key);
            g.conn.send(S_COOKIE_RESPONSE, &o)?;
        }
        C_STORE_COOKIE => store_cookie(&mut r, &mut ui.common),
        C_SERVER_LINKS => ui.common.links = server_links(&mut r),
        C_UPDATE_TAGS => {
            update_tags(&mut r, &mut ui.common);
            return Ok(false);
        }
        C_TRANSFER => {
            if let Some(t) = transfer(&mut r) {
                g.ui.common.transfer = Some(t.clone());
                g.disconnected = Some(crate::lang::get("disconnect.transfer"));
                return Err(String::from("transfer"));
            }
        }
        C_LOGIN => {
            // The fields ahead of the spawn info: hardcore, the levels, max players, view and
            // simulation distances, reduced debug info, showDeathScreen.
            ui.hardcore = r.i32().and_then(|_| r.bool()).unwrap_or(false);
            let n = r.varint().unwrap_or(0);
            for _ in 0..n.clamp(0, 1024) {
                r.string();
            }
            let _ = (r.varint(), r.varint(), r.varint(), r.bool());
            ui.immediate_respawn = r.bool() == Some(false);
            ui.camera = None;
            return Ok(false);
        }
        C_GAME_EVENT => {
            // 11: the immediate_respawn game rule.
            if r.u8() == Some(11) {
                ui.immediate_respawn = r.f32() == Some(1.0);
            }
            return Ok(false);
        }
        C_SET_ENTITY_DATA => {
            let id = r.varint().unwrap_or(0);
            if id != g.entity_id {
                return Ok(false);
            }
            // Our own player's data: the score (Player.DATA_SCORE_ID) for the death screen.
            let mut e = crate::entities::Entity::new(-1, g.uuid, [0.0; 3], 0.0, 0.0, 0.0);
            crate::client::entity_data(&mut r, &mut e);
            if let Some(v) = e.data.get(&18) {
                g.ui.score = *v as i32;
            }
            return Ok(false);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// ClientboundPlayerInfoUpdatePacket: the actions present, then each entry's fields for them.
fn player_info(r: &mut In, ui: &mut ServerUi) -> Option<()> {
    let actions = r.u8()?;
    let n = r.varint()?;
    for _ in 0..n.clamp(0, 4096) {
        let uuid = r.uuid()?;
        if actions & 1 != 0 {
            let name = r.string()?;
            for _ in 0..r.varint()?.clamp(0, 16) {
                r.string()?;
                r.string()?;
                if r.bool()? {
                    r.string()?;
                }
            }
            ui.players.insert(uuid, TabPlayer { name, game_mode: 0, listed: false, latency: 0, display: None, order: 0, hat: true });
        }
        // An entry for a player not added before is read and dropped (vanilla logs it).
        let mut scratch = TabPlayer { name: String::new(), game_mode: 0, listed: false, latency: 0, display: None, order: 0, hat: true };
        let e = match ui.players.get_mut(&uuid) {
            Some(e) => e,
            None => &mut scratch,
        };
        if actions & 2 != 0 && r.bool()? {
            r.take(16 + 8)?;
            let k = r.varint()?;
            r.take(k.max(0) as usize)?;
            let s = r.varint()?;
            r.take(s.max(0) as usize)?;
        }
        if actions & 4 != 0 {
            e.game_mode = r.varint()?;
        }
        if actions & 8 != 0 {
            e.listed = r.bool()?;
        }
        if actions & 16 != 0 {
            e.latency = r.varint()?;
        }
        if actions & 32 != 0 {
            e.display = if r.bool()? { Some(nbt::text(r)) } else { None };
        }
        if actions & 64 != 0 {
            e.order = r.varint()?;
        }
        if actions & 128 != 0 {
            e.hat = r.bool()?;
        }
    }
    Some(())
}

/// ClientboundSetPlayerTeamPacket: 0 add, 1 remove, 2 change, 3 join, 4 leave.
fn team(r: &mut In, ui: &mut ServerUi) -> Option<()> {
    let name = r.string()?;
    let method = r.u8()? as i8;
    if method == 1 {
        if let Some(t) = ui.teams.remove(&name) {
            for p in t.players {
                if ui.team_of.get(&p) == Some(&name) {
                    ui.team_of.remove(&p);
                }
            }
        }
        return Some(());
    }
    if method == 0 || method == 2 {
        let display = nbt::text(r);
        let prefix = nbt::text(r);
        let suffix = nbt::text(r);
        let visibility = r.varint()?;
        let collision = r.varint()?;
        let color = if r.bool()? { Some(r.varint()?) } else { None };
        let options = r.u8()? as i8;
        let t = ui.teams.entry(name.clone()).or_insert_with(|| Team {
            display: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            visibility: 0,
            collision: 0,
            color: None,
            options: 0,
            players: Vec::new(),
        });
        t.display = display;
        t.prefix = prefix;
        t.suffix = suffix;
        t.visibility = visibility;
        t.collision = collision;
        t.color = color.filter(|c| (0..16).contains(c));
        t.options = options;
    }
    if matches!(method, 0 | 3 | 4) {
        let n = r.varint()?;
        for _ in 0..n.clamp(0, 65536) {
            let p = r.string()?;
            if method == 4 {
                if ui.team_of.get(&p) == Some(&name) {
                    ui.team_of.remove(&p);
                }
                if let Some(t) = ui.teams.get_mut(&name) {
                    t.players.retain(|x| *x != p);
                }
            } else if ui.teams.contains_key(&name) {
                // Scoreboard.addPlayerToTeam: out of the old team first.
                if let Some(old) = ui.team_of.insert(p.clone(), name.clone()) {
                    if let Some(t) = ui.teams.get_mut(&old) {
                        t.players.retain(|x| *x != p);
                    }
                }
                if let Some(t) = ui.teams.get_mut(&name) {
                    t.players.push(p);
                }
            }
        }
    }
    Some(())
}

/// ClientboundBossEventPacket: the bar's UUID, then 0 add, 1 remove, 2 progress, 3 name,
/// 4 style, 5 flags.
fn boss(r: &mut In, ui: &mut ServerUi) {
    let (Some(id), Some(op)) = (r.uuid(), r.varint()) else { return };
    let now = crate::sys::now_ms();
    if op == 0 {
        let name = nbt::text(r);
        let progress = r.f32().unwrap_or(1.0);
        let color = r.varint().unwrap_or(0);
        let overlay = r.varint().unwrap_or(0);
        let flags = r.u8().unwrap_or(0);
        ui.bosses.retain(|b| b.id != id);
        ui.bosses.push(Boss { id, name, from: progress, progress, set_at: now, color: color.clamp(0, 6), overlay: overlay.clamp(0, 4), flags });
        return;
    }
    if op == 1 {
        ui.bosses.retain(|b| b.id != id);
        return;
    }
    let Some(b) = ui.bosses.iter_mut().find(|b| b.id == id) else { return };
    match op {
        2 => {
            let p = r.f32().unwrap_or(b.progress);
            b.from = b.shown_progress(now);
            b.progress = p;
            b.set_at = now;
        }
        3 => b.name = nbt::text(r),
        4 => {
            b.color = r.varint().unwrap_or(0).clamp(0, 6);
            b.overlay = r.varint().unwrap_or(0).clamp(0, 4);
        }
        5 => b.flags = r.u8().unwrap_or(0),
        _ => {}
    }
}

// ------------------------------------------------------------------ self test

fn nbt_string(o: &mut Out, s: &str) {
    o.u8(8).u16(s.len() as u16).bytes(s.as_bytes());
}

/// Checks of the packets' effects (penryn selftest; no server needed).
pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut ui = ServerUi::new(Common::default());
    // A team with a prefix and colour, a player in it.
    let mut o = Out::new();
    o.string("red").u8(0);
    nbt_string(&mut o, "Red Team");
    nbt_string(&mut o, "[R] ");
    nbt_string(&mut o, "");
    o.varint(0).varint(0).bool(true).varint(12).u8(0);
    o.varint(2).string("Alice").string("Bob");
    team(&mut In::new(&o.b), &mut ui);
    out.push(("team name format", ui.format_name("Alice", "Alice"), "[R] \u{a7}r\u{a7}cAlice\u{a7}r\u{a7}r"));
    // Bob leaves.
    let mut o = Out::new();
    o.string("red").u8(4).varint(1).string("Bob");
    team(&mut In::new(&o.b), &mut ui);
    out.push(("team leave", format!("{}", ui.team_of.contains_key("Bob")), "false"));
    // An objective on the sidebar, scores sorted as Hud.displayScoreboardSidebar sorts them.
    let mut sb = |owner: &str, v: i32| {
        ui.scores.entry(String::from(owner)).or_default().insert(String::from("kills"), Score { value: v, display: None, format: None });
    };
    sb("Alice", 3);
    sb("bob", 7);
    sb("#hidden", 9);
    sb("Carl", 3);
    ui.objectives.insert(String::from("kills"), Objective { display: String::from("Kills"), hearts: false, format: None });
    ui.display[1] = Some(String::from("kills"));
    let rows: Vec<String> = crate::serverhud::sidebar_rows(&ui, "kills").iter().map(|(n, s)| format!("{}={}", n, s)).collect();
    out.push(("sidebar order", rows.join(","), "bob=\u{a7}c7,[R] \u{a7}r\u{a7}cAlice\u{a7}r\u{a7}r=\u{a7}c3,Carl=\u{a7}c3"));
    // The team colour's own sidebar wins for its members.
    ui.objectives.insert(String::from("redonly"), Objective { display: String::from("Red"), hearts: false, format: Some(NumFmt::Blank) });
    ui.display[3 + 12] = Some(String::from("redonly"));
    out.push(("team sidebar slot", ui.sidebar("Alice").map(|o| o.0.clone()).unwrap_or_default(), "redonly"));
    out.push(("plain sidebar slot", ui.sidebar("Carl").map(|o| o.0.clone()).unwrap_or_default(), "kills"));
    // Name tag visibility: a viewer in no team sees "hide for other teams" and "hide for own team".
    let vis = |ui: &mut ServerUi, v: i32| {
        ui.teams.get_mut("red").map(|t| t.visibility = v);
        format!("{} {}", ui.tag_visible("Alice", "Carl"), ui.tag_visible("Alice", "Alice"))
    };
    out.push(("name tag hide for other teams", vis(&mut ui, 2), "true true"));
    out.push(("name tag hide for own team", vis(&mut ui, 3), "true false"));
    vis(&mut ui, 0);
    // Number formats.
    let mut o = Out::new();
    o.varint(1).u8(10).u8(8).u16(5).bytes(b"color").u16(4).bytes(b"gold").u8(0);
    out.push(("styled number format", number_format(&mut In::new(&o.b)).map(|f| f.format(42)).unwrap_or_default(), "\u{a7}642"));
    let mut o = Out::new();
    o.varint(2);
    nbt_string(&mut o, "MAX");
    out.push(("fixed number format", number_format(&mut In::new(&o.b)).map(|f| f.format(42)).unwrap_or_default(), "MAX"));
    // Boss bars: add, progress lerps over 100 ms, remove.
    let id = [7u8; 16];
    let mut o = Out::new();
    o.bytes(&id).varint(0);
    nbt_string(&mut o, "Wither");
    o.f32(1.0).varint(5).varint(2).u8(5);
    boss(&mut In::new(&o.b), &mut ui);
    let mut o = Out::new();
    o.bytes(&id).varint(2).f32(0.5);
    boss(&mut In::new(&o.b), &mut ui);
    let b = &ui.bosses[0];
    out.push((
        "boss bar",
        format!("{} {} {} {:.2} {:.2} {}", b.name, b.color, b.overlay, b.shown_progress(b.set_at + 50), b.shown_progress(b.set_at + 500), ui.boss_fog()),
        "Wither 5 2 0.75 0.50 true",
    ));
    ui.tick();
    out.push(("boss darkens the sky", format!("{:.2}", ui.darken), "0.05"));
    // Titles: times, then the title runs 1 + 2 + 3 ticks.
    ui.fade_in = 1;
    ui.stay = 2;
    ui.fade_out = 3;
    ui.title = Some(String::from("Hi"));
    ui.title_time = 6;
    for _ in 0..5 {
        ui.tick();
    }
    let still = ui.title.is_some();
    ui.tick();
    out.push(("title runs its times", format!("{} {}", still, ui.title.is_some()), "true false"));
    // The world border: 100 blocks shrinking to 50 over 10 ticks.
    let mut bd = Border::default();
    bd.lerp(100.0, 50.0, 10);
    for _ in 0..5 {
        bd.tick();
    }
    out.push(("border lerp", format!("{:.1} {:.1} {}", bd.size, bd.distance(10.0, 0.0), bd.moving()), "75.0 27.5 -1"));
    for _ in 0..6 {
        bd.tick();
    }
    out.push(("border lerp ends", format!("{:.1} {}", bd.size, bd.moving()), "50.0 0"));
    // Tab list entries: add with a name, game mode, listed, latency, display name.
    let mut o = Out::new();
    o.u8(1 | 4 | 8 | 16 | 32).varint(1).bytes(&[1u8; 16]).string("Dave").varint(0).varint(3).bool(true).varint(250).bool(true);
    nbt_string(&mut o, "Davey");
    player_info(&mut In::new(&o.b), &mut ui);
    let p = ui.players.get(&[1u8; 16]);
    out.push((
        "tab list entry",
        p.map(|p| format!("{} {} {} {} {:?}", p.name, p.game_mode, p.listed, p.latency, p.display)).unwrap_or_default(),
        "Dave 3 true 250 Some(\"Davey\")",
    ));
    // Server links: a known type and a custom label; not-web links are dropped.
    let mut o = Out::new();
    o.varint(3).bool(true).varint(6).string("https://example.net").bool(false);
    nbt_string(&mut o, "Shop");
    o.string("https://shop.example.net").bool(false);
    nbt_string(&mut o, "Bad");
    o.string("file:///etc/passwd");
    let links = server_links(&mut In::new(&o.b));
    out.push(("server links", format!("{} {}", links.len(), links.get(1).map(|l| l.0.as_str()).unwrap_or("")), "2 Shop"));
    // Cookies are answered with what was stored.
    let mut c = Common::default();
    let mut o = Out::new();
    o.string("minecraft:token").varint(3).bytes(b"abc");
    store_cookie(&mut In::new(&o.b), &mut c);
    let a = c.answer("minecraft:token").b;
    out.push(("cookie answer", format!("{:?}", &a[a.len() - 5..]), "[1, 3, 97, 98, 99]"));
    // A new listener (reconfiguration): tab list, teams, border and sidebar are gone, the
    // cookies and the connection's online flag stay.
    let mut ui = ServerUi::new(c);
    ui.online = true;
    let mut o = Out::new();
    o.u8(1 | 8).varint(1).bytes(&[2u8; 16]).string("Eve").varint(0).bool(true);
    player_info(&mut In::new(&o.b), &mut ui);
    let mut o = Out::new();
    o.string("blue").u8(0);
    nbt_string(&mut o, "Blue");
    nbt_string(&mut o, "");
    nbt_string(&mut o, "");
    o.varint(0).varint(0).bool(true).varint(9).u8(0);
    o.varint(1).string("Eve");
    team(&mut In::new(&o.b), &mut ui);
    ui.display[1] = Some(String::from("side"));
    ui.border.size = 5.0;
    ui.title = Some(String::from("hi"));
    ui.version = 7;
    ui.reset_session();
    out.push(("reset session keeps the version growing", format!("{}", ui.version), "8"));
    out.push((
        "reset session",
        format!("{} {} {} {} {} {} {} {}", ui.players.len(), ui.teams.len() + ui.team_of.len(), ui.display[1].is_none(), ui.border.size > 5.0, ui.title.is_none(), ui.bosses.len(), ui.online, ui.common.cookies.len()),
        "0 0 true true true 0 true 1",
    ));
    out
}
