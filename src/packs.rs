//! Resource packs: the server's (ClientboundResourcePackPush / Pop in configuration and play)
//! and the player's own (Options > Resource Packs), as vanilla's DownloadedPackSource,
//! ServerPackManager and PackSelectionScreen.
//!
//! Penryn cannot fetch a pack itself (servers' packs are often on HTTPS, which Mac OS X 10.6
//! cannot do) nor read zips, JSON and PNGs quickly on a 2008 machine, so the relay
//! (tools/relay.py) downloads the pack, checks its SHA-1 and bakes the whole pack stack into
//! Penryn's game files (make_assets.py --packs). Penryn fetches only the files the stack
//! changes, keeps them beside the game's own as `pack-<name>` and reloads what they hold
//! (world meshes, atlas, GUI and font, language, items, entities, particles, sounds) without
//! reconnecting; back to the game's own files when the packs go.
//!
//! The statuses go to the server exactly as vanilla's PackLoadFeedback sends them: ACCEPTED,
//! DOWNLOADED, then SUCCESSFULLY_LOADED (or FAILED_DOWNLOAD, INVALID_URL, FAILED_RELOAD,
//! DECLINED, DISCARDED).

use crate::blocks::Blocks;
use crate::conn::Conn;
use crate::gui::Gui;
use crate::screens::{self, Button, WHITE};
use crate::sys;
use crate::wire::{In, Out};
use crate::world::World;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

/// ServerboundResourcePackPacket.Action.
pub const SUCCESSFULLY_LOADED: i32 = 0;
pub const DECLINED: i32 = 1;
pub const FAILED_DOWNLOAD: i32 = 2;
pub const ACCEPTED: i32 = 3;
pub const DOWNLOADED: i32 = 4;
pub const INVALID_URL: i32 = 5;
pub const FAILED_RELOAD: i32 = 6;
pub const DISCARDED: i32 = 7;

/// The game files a pack stack can change (relay.py's PACK_FILES).
pub const FILES: [&str; 10] =
    ["blocks.bin", "atlas.bin", "gui.bin", "entities.bin", "weather.bin", "signs.bin", "particles.bin", "effects.bin", "packsounds.bin", "items.bin"];

// ------------------------------------------------------------------ the files in use

static ACTIVE: AtomicPtr<Vec<String>> = AtomicPtr::new(core::ptr::null_mut());
static GENERATION: AtomicU32 = AtomicU32::new(0);

fn active() -> Vec<String> {
    let p = ACTIVE.load(Ordering::Acquire);
    if p.is_null() {
        Vec::new()
    } else {
        unsafe { &*p }.clone()
    }
}

fn set_active(v: Vec<String>) {
    // The old list is leaked on purpose: a reader may still hold it (a few bytes per reload).
    ACTIVE.store(Box::into_raw(Box::new(v)), Ordering::Release);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Where game file `name` is read from: the pack stack's copy when it changes it.
pub fn path(name: &str) -> String {
    if active().iter().any(|n| n == name) {
        format!("{}/pack-{}", crate::data_dir(), name)
    } else {
        format!("{}/{}", crate::data_dir(), name)
    }
}

/// Bumped at every reload: modules that load a file lazily load it again.
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Acquire)
}

/// The sound overlay of the pack stack, if it has sounds.
pub fn sound_overlay() -> Option<String> {
    if active().iter().any(|n| n == "packsounds.bin") {
        Some(path("packsounds.bin"))
    } else {
        None
    }
}

// ------------------------------------------------------------------ what the window does for us

/// What the screens here get from the window.
pub enum Input {
    Click(f32, f32),
    Escape,
}

/// What Host::ui_frame reports when the window is closed while a prompt waits.
pub const WINDOW_CLOSED: &str = "the window was closed";

pub trait Host {
    /// One frame of a screen drawn by `draw` (GUI units, the mouse position), over the dark
    /// background; the clicks and keys since the last frame go into `input`. False when the
    /// window is being closed.
    fn ui_frame(&mut self, draw: &mut dyn FnMut(&mut Gui, f32, f32), input: &mut Vec<Input>) -> bool;
    /// Loads game files `names` again (from `path`) and everything drawn with them.
    fn reload(&mut self, names: &[String], world: Option<&mut World>, blocks: &mut Blocks);
    /// No window: prompts are answered without asking (PENRYN_PACK_ANSWER, else yes).
    fn headless(&self) -> bool {
        false
    }
}

/// Without a window (penryn join): prompts answered by PENRYN_PACK_ANSWER (yes by default),
/// files fetched but nothing to draw.
pub struct NoWindow;

impl Host for NoWindow {
    fn headless(&self) -> bool {
        true
    }
    fn ui_frame(&mut self, _draw: &mut dyn FnMut(&mut Gui, f32, f32), _input: &mut Vec<Input>) -> bool {
        sys::sleep_ms(20);
        true
    }
    fn reload(&mut self, names: &[String], _world: Option<&mut World>, blocks: &mut Blocks) {
        if names.iter().any(|n| n == "blocks.bin") {
            if let Some(b) = sys::read_file(&path("blocks.bin")).and_then(|d| Blocks::parse(&d)) {
                *blocks = b;
            }
        }
    }
}

/// Marks every loaded section for meshing again (new block models or textures).
pub fn remesh(world: &mut World, blocks: &Blocks) {
    let n = world.height / 16;
    let keys: Vec<(i32, i32)> = world.columns.keys().copied().collect();
    for (x, z) in keys {
        for sy in 0..n {
            world.dirty.insert((x, sy, z));
        }
    }
    world.epoch += 1;
    let names = world.biome_names.clone();
    if !names.is_empty() {
        world.set_biomes(&names, blocks);
    }
}

// ------------------------------------------------------------------ servers.dat

/// The server's resource pack setting, as the server list keeps it (ServerData.ServerPackStatus):
/// Enabled, Disabled or Prompt.
pub type Setting = crate::servers::Pack;

/// The server's entry in the server list: its setting (Prompt when it is not listed).
pub fn setting_of(host: &str, port: u16) -> Setting {
    crate::servers::List::load()
        .servers
        .iter()
        .find(|s| same_server(&s.ip, host, port))
        .map_or(Setting::Prompt, |s| s.pack)
}

fn same_server(ip: &str, host: &str, port: u16) -> bool {
    crate::servers::address(ip).is_some_and(|(h, p)| h.eq_ignore_ascii_case(host) && p == port)
}

/// ServerList.saveSingleServer: the answer kept in the server's entry, when it has one.
pub fn save_setting(host: &str, port: u16, s: Setting) {
    let mut list = crate::servers::List::load();
    let mut changed = false;
    for sv in list.servers.iter_mut().filter(|sv| same_server(&sv.ip, host, port)) {
        sv.pack = s;
        changed = true;
    }
    if changed {
        list.save();
    }
}

// ------------------------------------------------------------------ the relay

static RELAY: AtomicPtr<(String, u16)> = AtomicPtr::new(core::ptr::null_mut());

pub fn set_relay(r: Option<(String, u16)>) {
    if let Some(r) = r {
        RELAY.store(Box::into_raw(Box::new(r)), Ordering::Release);
    }
}

fn relay() -> Option<(String, u16)> {
    let p = RELAY.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { &*p }.clone())
    }
}

/// One poll of a relay job: Ok(None) while it works.
fn relay_job(path: &str, body: &str) -> Result<Option<String>, String> {
    let r = relay().ok_or("no relay")?;
    let resp = crate::http::post(&r.0, r.1, path, body.as_bytes(), 15000)?;
    match resp.status {
        202 => Ok(None),
        200 => Ok(Some(resp.text())),
        _ => Err(String::from(resp.text().trim())),
    }
}

/// Vanilla's Pack.Compatibility as the relay names it: compatible, made for a newer or an older
/// version, or unknown (no format, or broken).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Compat {
    Ok,
    New,
    Old,
    Unknown,
}

/// The local packs the relay has (DATA/resourcepacks): (id, compatibility, description).
pub fn list_local() -> Result<Vec<(String, Compat, String)>, String> {
    let r = relay().ok_or("no relay")?;
    let resp = crate::http::get(&r.0, r.1, "/pack/list", 15000)?;
    if resp.status != 200 {
        return Err(String::from(resp.text().trim()));
    }
    let mut out = Vec::new();
    for l in resp.text().lines() {
        let mut f = l.splitn(3, '\t');
        let (Some(id), Some(c), d) = (f.next(), f.next(), f.next()) else { continue };
        let c = match c {
            "1" => Compat::Ok,
            "new" => Compat::New,
            "old" => Compat::Old,
            _ => Compat::Unknown,
        };
        out.push((String::from(id), c, String::from(d.unwrap_or(""))));
    }
    Ok(out)
}

// ------------------------------------------------------------------ the pack manager

#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptStatus {
    Pending,
    Allowed,
    Declined,
}

/// ServerPackManager.RemovalReason, with the answer it gives the server.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Removal {
    DownloadFailed,
    ActivationFailed,
    Declined,
    Discarded,
    ServerRemoved,
    ServerReplaced,
}

impl Removal {
    fn response(self) -> Option<i32> {
        match self {
            Removal::DownloadFailed => Some(FAILED_DOWNLOAD),
            Removal::ActivationFailed => Some(FAILED_RELOAD),
            Removal::Declined => Some(DECLINED),
            Removal::Discarded => Some(DISCARDED),
            Removal::ServerRemoved | Removal::ServerReplaced => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Download {
    Requested,
    Pending,
    /// In the relay's cache: its SHA-1.
    Done(String),
    Failed,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Activation {
    Inactive,
    Pending,
    Active,
}

#[derive(Debug)]
struct ServerPack {
    id: [u8; 16],
    url: String,
    hash: String,
    accepted: bool,
    download: Download,
    activation: Activation,
    removed: Option<Removal>,
}

/// A pack the confirm screen asks about.
#[derive(Clone)]
pub struct Request {
    pub id: [u8; 16],
    pub url: String,
    pub hash: String,
}

/// ClientCommonPacketListenerImpl.PackConfirmScreen: the requests it answers for.
pub struct Prompt {
    pub requests: Vec<Request>,
    pub required: bool,
    pub message: Option<String>,
    shown_at: u64,
}

/// A reload being prepared: the stack (pack references, bottom to top) and the packs it
/// turns on.
struct Reload {
    refs: Vec<String>,
    activating: Vec<[u8; 16]>,
}

pub struct Packs {
    packs: Vec<ServerPack>,
    status: PromptStatus,
    pub setting: Setting,
    server: (String, u16),
    pub prompt: Option<Prompt>,
    out: Vec<([u8; 16], i32)>,
    /// Set when the player declined a required pack: the reason to leave with.
    pub disconnect: Option<String>,
    player: (String, String),
    /// The player's own packs (options.txt resourcePacks, without "vanilla"), bottom to top.
    pub local: Vec<String>,
    /// The stack in use, bottom to top.
    applied: Vec<String>,
    reload: Option<Reload>,
    /// The stack whose last reload failed.
    failed: Option<Vec<String>>,
    next_poll: u64,
    /// Feedback sent, for tests (PENRYN_DEBUG prints them).
    pub sent: Vec<i32>,
}

fn uuid_hex(u: &[u8; 16]) -> String {
    let mut s = String::new();
    for b in u {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

impl Packs {
    /// For a connection to `host:port` (DownloadedPackSource.configureForServerControl).
    pub fn new(host: &str, port: u16, player: &str, uuid: &[u8; 16], local: Vec<String>) -> Packs {
        let setting = setting_of(host, port);
        let status = match setting {
            Setting::Enabled => PromptStatus::Allowed,
            Setting::Disabled => PromptStatus::Declined,
            Setting::Prompt => PromptStatus::Pending,
        };
        Packs {
            packs: Vec::new(),
            status,
            setting,
            server: (String::from(host), port),
            prompt: None,
            out: Vec::new(),
            disconnect: None,
            player: (String::from(player), uuid_hex(uuid)),
            applied: local.clone(),
            local,
            reload: None,
            failed: None,
            next_poll: 0,
            sent: Vec::new(),
        }
    }

    fn feedback(&mut self, id: [u8; 16], action: i32) {
        self.out.push((id, action));
    }

    /// Sends the statuses waiting (`packet`: the state's ServerboundResourcePackPacket id).
    pub fn flush(&mut self, c: &mut Conn, packet: i32) -> Result<(), String> {
        for (id, action) in core::mem::take(&mut self.out) {
            let mut o = Out::new();
            o.bytes(&id).varint(action);
            c.send(packet, &o)?;
            self.sent.push(action);
            if sys::env("PENRYN_DEBUG").is_some() {
                crate::say!("resource pack {}: status {}", uuid_hex(&id), action);
            }
        }
        Ok(())
    }

    /// ClientboundResourcePackPushPacket (handleResourcePackPush).
    pub fn on_push(&mut self, r: &mut In) {
        let id = r.uuid().unwrap_or([0; 16]);
        let url = r.string().unwrap_or_default();
        let hash = r.string().unwrap_or_default();
        let required = r.bool().unwrap_or(false);
        let message = if r.bool() == Some(true) { Some(crate::nbt::text(r)) } else { None };
        if !SELFTEST.load(core::sync::atomic::Ordering::Relaxed) {
            crate::say!("the server offers a resource pack{}: {}", if required { " (required)" } else { "" }, url);
        }
        // parseResourcePackUrl: http and https only.
        let scheme = url.split("://").next().unwrap_or("");
        if !url.contains("://") || !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) || url.len() <= scheme.len() + 3 {
            self.feedback(id, INVALID_URL);
            return;
        }
        let req = Request { id, url, hash };
        if self.setting == Setting::Prompt || (required && self.setting == Setting::Disabled) {
            // addOrUpdatePackPrompt: one screen for every request so far.
            match self.prompt.as_mut() {
                // PackConfirmScreen.update: a new screen with every request so far, and the
                // latest push's required flag and prompt.
                Some(p) => {
                    p.requests.push(req);
                    p.required = required;
                    p.message = message;
                }
                None => self.prompt = Some(Prompt { requests: alloc::vec![req], required, message, shown_at: sys::now_ms() }),
            }
        } else {
            self.push_pack(req);
        }
    }

    /// ClientboundResourcePackPopPacket: one pack, or all of them.
    pub fn on_pop(&mut self, r: &mut In) {
        if r.bool() == Some(true) {
            let id = r.uuid().unwrap_or([0; 16]);
            if let Some(p) = self.packs.iter_mut().find(|p| p.removed.is_none() && p.id == id) {
                p.removed.get_or_insert(Removal::ServerRemoved);
            }
        } else {
            for p in self.packs.iter_mut() {
                p.removed.get_or_insert(Removal::ServerRemoved);
            }
        }
    }

    /// ServerPackManager.pushPack.
    fn push_pack(&mut self, req: Request) {
        if self.status == PromptStatus::Declined {
            self.feedback(req.id, DECLINED);
            return;
        }
        for p in self.packs.iter_mut() {
            if p.id == req.id {
                p.removed.get_or_insert(Removal::ServerReplaced);
            }
        }
        let accepted = self.status == PromptStatus::Allowed;
        if accepted {
            self.feedback(req.id, ACCEPTED);
        }
        self.packs.push(ServerPack {
            id: req.id,
            url: req.url,
            hash: req.hash,
            accepted,
            download: Download::Requested,
            activation: Activation::Inactive,
            removed: None,
        });
    }

    /// The confirm screen's answer (PackConfirmScreen's callback).
    pub fn answer(&mut self, yes: bool) {
        let Some(p) = self.prompt.take() else { return };
        if yes {
            self.setting = Setting::Enabled;
            // allowServerPacks
            self.status = PromptStatus::Allowed;
            let mut acc = Vec::new();
            for sp in self.packs.iter_mut() {
                if !sp.accepted && sp.removed.is_none() {
                    sp.accepted = true;
                    acc.push(sp.id);
                }
            }
            for id in acc {
                self.feedback(id, ACCEPTED);
            }
        } else {
            // rejectServerPacks
            self.status = PromptStatus::Declined;
            for sp in self.packs.iter_mut() {
                if !sp.accepted {
                    sp.removed.get_or_insert(Removal::Declined);
                }
            }
            if p.required {
                self.disconnect = Some(crate::lang::get("multiplayer.requiredTexturePrompt.disconnect"));
                return;
            }
            self.setting = Setting::Disabled;
        }
        for r in p.requests {
            self.push_pack(r);
        }
        save_setting(&self.server.0, self.server.1, self.setting);
    }

    /// ServerPackManager.tick: downloads, then the reload once nothing is downloading, then the
    /// removed packs' answers. Polls the relay at most every quarter second.
    pub fn tick(&mut self, host: &mut dyn Host, world: Option<&mut World>, blocks: &mut Blocks) {
        let now = sys::now_ms();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + 250;
        if !self.update_downloads() {
            self.trigger_reload(host, world, blocks);
        }
        self.cleanup();
    }

    /// True while a download is still going.
    fn update_downloads(&mut self) -> bool {
        let mut busy = false;
        let mut failed = false;
        let mut finished = Vec::new();
        let (name, uuid) = self.player.clone();
        for p in self.packs.iter_mut() {
            if p.removed.is_some() || !p.accepted {
                continue;
            }
            match p.download {
                Download::Done(_) | Download::Failed => continue,
                Download::Requested => p.download = Download::Pending,
                Download::Pending => {}
            }
            let body = format!("{}\n{}\n{}\n{}\n", p.url, p.hash, name, uuid);
            match relay_job("/pack/download", &body) {
                Ok(None) => busy = true,
                Ok(Some(sha)) => {
                    let sha = String::from(sha.trim());
                    crate::say!("resource pack downloaded ({})", sha);
                    p.download = Download::Done(sha);
                    finished.push(p.id);
                }
                Err(e) => {
                    if !SELFTEST.load(core::sync::atomic::Ordering::Relaxed) {
                        crate::warn!("could not download the resource pack {}: {}", p.url, e);
                    }
                    p.download = Download::Failed;
                    failed = true;
                }
            }
        }
        if failed {
            // onDownload with failures: every pack not in use goes (setRemovalReasonIfNotSet),
            // the failed ones as DOWNLOAD_FAILED, the others as DISCARDED.
            for p in self.packs.iter_mut() {
                if p.activation != Activation::Active && p.removed.is_none() {
                    p.removed = Some(if p.download == Download::Failed { Removal::DownloadFailed } else { Removal::Discarded });
                }
            }
        }
        for id in finished {
            if self.packs.iter().any(|p| p.id == id && p.removed.is_none()) {
                self.feedback(id, DOWNLOADED);
            }
        }
        busy
    }

    /// The stack wanted, the packs it loads (vanilla's packsToLoad: the active ones that stay
    /// too) and whether one of them is not active yet.
    fn wanted(&self) -> (Vec<String>, Vec<[u8; 16]>, bool) {
        let mut refs = self.local.clone();
        let mut load = Vec::new();
        let mut new = false;
        for p in self.packs.iter() {
            if let (true, None, Download::Done(sha)) = (p.accepted, p.removed, &p.download) {
                refs.push(format!("sha1:{}", sha));
                load.push(p.id);
                new |= p.activation == Activation::Inactive;
            }
        }
        (refs, load, new)
    }

    /// triggerReloadIfNeeded's first half: the reload to make, if one is needed.
    fn begin_reload(&mut self) -> bool {
        if self.reload.is_some() {
            return true;
        }
        let (refs, activating, new) = self.wanted();
        let leaving = self.packs.iter().any(|p| p.activation == Activation::Active && p.removed.is_some());
        if !new && !leaving && (refs == self.applied || Some(&refs) == self.failed.as_ref()) {
            return false;
        }
        for p in self.packs.iter_mut() {
            if activating.contains(&p.id) && p.activation != Activation::Active {
                p.activation = Activation::Pending;
            }
        }
        self.reload = Some(Reload { refs, activating });
        true
    }

    fn trigger_reload(&mut self, host: &mut dyn Host, world: Option<&mut World>, blocks: &mut Blocks) {
        if !self.begin_reload() {
            return;
        }
        let refs = self.reload.as_ref().map(|r| r.refs.clone()).unwrap_or_default();
        let result = if refs == self.applied {
            Some(Ok(()))
        } else {
            match apply_stack(&refs, &self.applied, host, world, blocks) {
                Ok(true) => Some(Ok(())),
                Ok(false) => None, // the relay is still baking
                Err(e) => Some(Err(e)),
            }
        };
        if let Some(result) = result {
            self.finish_reload(result);
        }
    }

    /// The reload's onSuccess / onFailure.
    fn finish_reload(&mut self, result: Result<(), String>) {
        let r = self.reload.take().unwrap_or(Reload { refs: Vec::new(), activating: Vec::new() });
        match result {
            Ok(()) => {
                // onSuccess: every pack loaded is active and answered APPLIED (again, for the
                // ones that were active already), the unloaded ones are inactive.
                self.applied = r.refs;
                self.failed = None;
                for p in self.packs.iter_mut() {
                    if r.activating.contains(&p.id) {
                        p.activation = Activation::Active;
                    } else if p.removed.is_some() {
                        p.activation = Activation::Inactive;
                    }
                }
                for id in r.activating {
                    if self.packs.iter().any(|p| p.id == id && p.removed.is_none()) {
                        self.feedback(id, SUCCESSFULLY_LOADED);
                    }
                }
            }
            Err(e) => {
                // onFailure: the packs being loaded failed, the inactive ones are discarded; the
                // game's files were not replaced, so the active ones stay.
                crate::warn!("could not load the resource packs: {}", e);
                for p in self.packs.iter_mut() {
                    if p.activation == Activation::Pending {
                        p.activation = Activation::Inactive;
                        p.removed.get_or_insert(Removal::ActivationFailed);
                    } else if p.activation == Activation::Inactive {
                        p.removed.get_or_insert(Removal::Discarded);
                    } else if p.removed.is_some() {
                        p.activation = Activation::Inactive;
                    }
                }
                // Not tried again every tick: only when the stack wanted changes.
                self.failed = Some(r.refs);
            }
        }
    }

    /// cleanupRemovedPacks: removed packs that are not in use go, with their answer.
    fn cleanup(&mut self) {
        let mut answers = Vec::new();
        self.packs.retain(|p| {
            if p.activation != Activation::Inactive || p.removed.is_none() {
                return true;
            }
            if let Some(a) = p.removed.and_then(|r| r.response()) {
                answers.push((p.id, a));
            }
            false
        });
        for (id, a) in answers {
            self.feedback(id, a);
        }
    }

    /// Leaving the server (cleanupAfterDisconnect): back to the player's own packs.
    pub fn leave(&mut self, host: &mut dyn Host, world: Option<&mut World>, blocks: &mut Blocks) {
        self.packs.clear();
        self.prompt = None;
        self.reload = None;
        self.failed = None;
        if self.applied != self.local {
            let local = self.local.clone();
            if wait_stack(&local, &self.applied, host, world, blocks).is_ok() {
                self.applied = local;
            }
        }
    }

    /// One step of the pack work: the statuses, a frame of the confirm screen while it is
    /// open (PENRYN_PACK_ANSWER=yes|no answers it after 1.5 s, for tests), the downloads and
    /// reloads. The caller keeps the connection answered between steps. Err: leave the server.
    pub fn step(&mut self, c: &mut Conn, packet: i32, host: &mut dyn Host, world: Option<&mut World>, blocks: &mut Blocks) -> Result<(), String> {
        self.flush(c, packet)?;
        if let Some(p) = self.prompt.as_ref() {
            let due = sys::now_ms() > p.shown_at + 1500;
            let auto = sys::env("PENRYN_PACK_ANSWER").or_else(|| if host.headless() { Some(String::from("yes")) } else { None });
            match auto {
                Some(a) if due || host.headless() => self.answer(a != "no"),
                _ => {
                    let mut input = Vec::new();
                    if !host.ui_frame(&mut |gui, mx, my| draw_prompt(gui, p, mx, my), &mut input) {
                        return Err(String::from(WINDOW_CLOSED));
                    }
                    let mut clicked = None;
                    for ev in input.drain(..) {
                        if let Input::Click(mx, my) = ev {
                            clicked = clicked.or(prompt_hit(host_gui_size(), mx, my));
                        }
                    }
                    if let Some(yes) = clicked {
                        self.answer(yes);
                    }
                }
            }
            self.flush(c, packet)?;
        }
        if let Some(reason) = self.disconnect.clone() {
            return Err(reason);
        }
        if self.prompt.is_none() {
            self.tick(host, world, blocks);
        }
        self.flush(c, packet)
    }

    /// Work is going on (a download, a bake, the prompt): the configuration waits for it.
    pub fn busy(&self) -> bool {
        self.prompt.is_some() || self.reload.is_some() || self.packs.iter().any(|p| p.removed.is_none() && p.accepted && p.activation != Activation::Active)
    }
}

/// The GUI size of the last frame drawn (for hit tests outside a draw).
static GUI_SIZE: AtomicU32 = AtomicU32::new(0);

fn host_gui_size() -> (f32, f32) {
    let v = GUI_SIZE.load(Ordering::Relaxed);
    ((v >> 16) as f32, (v & 0xFFFF) as f32)
}

fn note_size(gui: &Gui) {
    GUI_SIZE.store(((gui.width as u32) << 16) | (gui.height as u32 & 0xFFFF), Ordering::Relaxed);
}

// ------------------------------------------------------------------ fetching a stack

/// Bakes `refs` on the relay and loads it: Ok(false) while the relay works on it.
fn apply_stack(refs: &[String], old: &[String], host: &mut dyn Host, world: Option<&mut World>, blocks: &mut Blocks) -> Result<bool, String> {
    let _ = old;
    let answer = if refs.is_empty() {
        String::new()
    } else {
        match relay_job("/pack/bake", &refs.join("\n"))? {
            None => return Ok(false),
            Some(a) => a,
        }
    };
    let mut lines = answer.lines();
    let key = String::from(lines.next().unwrap_or("").trim());
    let mut names: Vec<String> = Vec::new();
    for l in lines {
        if let Some(n) = l.split(' ').next().filter(|n| FILES.contains(n)) {
            names.push(String::from(n));
        }
    }
    let marker = format!("{}/pack-stack.txt", crate::data_dir());
    let have = sys::read_file(&marker).map(|d| String::from_utf8_lossy(&d).into_owned()).unwrap_or_default();
    // Drawn once before the fetch: vanilla's reload overlay.
    let mut no_input = Vec::new();
    host.ui_frame(&mut |gui, _, _| {
        gui.text_centered(&crate::lang::get("resourcepack.downloading"), gui.width / 2.0, gui.height / 2.0 - 4.0, WHITE);
    }, &mut no_input);
    if !key.is_empty() && have.trim() != key {
        let r = relay().ok_or("no relay")?;
        // Every file is fetched beside the ones in use first: a failed fetch leaves the stack
        // in use whole.
        let dir = crate::data_dir();
        for n in names.iter() {
            let resp = crate::http::get(&r.0, r.1, &format!("/pack/files/{}/{}", key, n), 600000)?;
            if resp.status != 200 {
                return Err(format!("the relay has no {} for the packs", n));
            }
            if !sys::write_file_private(&format!("{}/pack-{}.part", dir, n), &resp.body) {
                return Err(format!("cannot write pack-{}", n));
            }
        }
        sys::write_file_private(&marker, b"");
        for n in names.iter() {
            if !sys::rename_file(&format!("{}/pack-{}.part", dir, n), &format!("{}/pack-{}", dir, n)) {
                return Err(format!("cannot write pack-{}", n));
            }
        }
        sys::write_file_private(&marker, key.as_bytes());
    }
    let before = active();
    let mut changed: Vec<String> = before.clone();
    for n in names.iter() {
        if !changed.contains(n) {
            changed.push(n.clone());
        }
    }
    set_active(names);
    crate::say!("resource packs: {} ({} game files changed)", if refs.is_empty() { String::from("none") } else { refs.join(", ") }, active().len());
    if !changed.is_empty() {
        host.reload(&changed, world, blocks);
    }
    Ok(true)
}

/// `apply_stack` until it is done (at startup and on leaving).
pub fn wait_stack(refs: &[String], old: &[String], host: &mut dyn Host, mut world: Option<&mut World>, blocks: &mut Blocks) -> Result<(), String> {
    let end = sys::now_ms() + 300_000;
    loop {
        if apply_stack(refs, old, host, world.as_deref_mut(), blocks)? {
            return Ok(());
        }
        if sys::now_ms() > end {
            return Err(String::from("the relay took too long to make the packs' files"));
        }
        let mut input = Vec::new();
        host.ui_frame(&mut |gui, _, _| {
            gui.text_centered(&crate::lang::get("resourcepack.downloading"), gui.width / 2.0, gui.height / 2.0 - 4.0, WHITE);
        }, &mut input);
        sys::sleep_ms(200);
    }
}

/// options.txt's resourcePacks (["vanilla","file/x"]): the player's own packs, bottom to top.
pub fn parse_option(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut esc = false;
    for ch in v.chars() {
        if in_str {
            if esc {
                cur.push(ch);
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
                out.push(core::mem::take(&mut cur));
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            in_str = true;
        }
    }
    out.retain(|s| s.starts_with("file/"));
    out
}

pub fn option_text(local: &[String]) -> String {
    let mut s = String::from("[\"vanilla\"");
    for l in local {
        s.push_str(",\"");
        for ch in l.chars() {
            if ch == '"' || ch == '\\' {
                s.push('\\');
            }
            s.push(ch);
        }
        s.push('"');
    }
    s.push(']');
    s
}

// ------------------------------------------------------------------ the confirm screen

/// ConfirmScreen's lines: the title, then the message (line 2, and the server's prompt).
fn prompt_lines(gui: &Gui, p: &Prompt) -> (String, Vec<(String, u32)>) {
    let title = crate::lang::get(if p.required { "multiplayer.requiredTexturePrompt.line1" } else { "multiplayer.texturePrompt.line1" });
    let line2 = crate::lang::get(if p.required { "multiplayer.requiredTexturePrompt.line2" } else { "multiplayer.texturePrompt.line2" });
    let width = gui.width - 50.0;
    let mut lines: Vec<(String, u32)> = Vec::new();
    let colour = if p.required { 0xFFFF_FF55 } else { WHITE };
    let text = match &p.message {
        // preparePackPrompt: multiplayer.texturePrompt.serverPrompt ("%s\n\nMessage from server:\n%s")
        Some(m) => crate::lang::format("multiplayer.texturePrompt.serverPrompt", &[line2.clone(), m.clone()]),
        None => line2.clone(),
    };
    let mut first = true;
    for para in text.split('\n') {
        // The required line 2 is yellow and bold (bold is drawn as plain here).
        let c = if first && para.starts_with(line2.split('\n').next().unwrap_or("")) { colour } else { WHITE };
        first = false;
        if para.is_empty() {
            lines.push((String::new(), c));
            continue;
        }
        for l in screens::wrap(gui, para, width) {
            lines.push((l, c));
        }
    }
    lines.truncate(15);
    (title, lines)
}

/// ConfirmScreen's layout: title, message and the button row in a column (8 apart, the
/// buttons 16 further), centred on the screen. (title y, message y, buttons y).
fn prompt_layout(h: f32, lines: usize) -> (f32, f32, f32) {
    let total = 9.0 + 8.0 + 9.0 * lines as f32 + 8.0 + 16.0 + 20.0;
    let top = crate::math::floor(((h - total) / 2.0) as f64) as f32;
    (top, top + 17.0, top + 17.0 + 9.0 * lines as f32 + 24.0)
}

fn prompt_buttons(w: f32, by: f32, required: bool) -> [Button; 2] {
    let x = crate::math::floor(((w - 304.0) / 2.0) as f64) as f32;
    let (yes, no) = if required { ("gui.proceed", "menu.disconnect") } else { ("gui.yes", "gui.no") };
    [Button::new(x, by, 150.0, crate::lang::get(yes)), Button::new(x + 154.0, by, 150.0, crate::lang::get(no))]
}

pub fn draw_prompt(gui: &mut Gui, p: &Prompt, mx: f32, my: f32) {
    note_size(gui);
    let (title, lines) = prompt_lines(gui, p);
    let (ty, my0, by) = prompt_layout(gui.height, lines.len());
    let cx = gui.width / 2.0;
    gui.text_centered(&title, cx, ty, WHITE);
    for (i, (l, c)) in lines.iter().enumerate() {
        gui.text_centered(l, cx, my0 + i as f32 * 9.0, *c);
    }
    for b in prompt_buttons(gui.width, by, p.required).iter() {
        b.draw(gui, mx, my);
    }
    PROMPT_ROWS.store(lines.len() as u32 | if p.required { 1 << 16 } else { 0 }, Ordering::Relaxed);
}

static PROMPT_ROWS: AtomicU32 = AtomicU32::new(0);

/// The button under a click on the last prompt drawn: Some(true) yes / proceed.
fn prompt_hit(size: (f32, f32), mx: f32, my: f32) -> Option<bool> {
    let v = PROMPT_ROWS.load(Ordering::Relaxed);
    let (_, _, by) = prompt_layout(size.1, (v & 0xFFFF) as usize);
    let b = prompt_buttons(size.0, by, v >> 16 != 0);
    if b[0].hit(mx, my) {
        Some(true)
    } else if b[1].hit(mx, my) {
        Some(false)
    } else {
        None
    }
}

// ------------------------------------------------------------------ Options > Resource Packs

/// PackSelectionScreen: the available packs on the left, the selected ones on the right (the
/// top of the list is the top of the stack; "Default" always at the bottom). A click moves a
/// pack across; a click on a selected pack's left or right half of its arrows column moves it
/// up or down. Done applies the change. Returns the new list when it changed.
pub fn selection_screen(host: &mut dyn Host, local: &[String]) -> Option<Vec<String>> {
    let found = list_local();
    let all: Vec<(String, Compat, String)> = match &found {
        Ok(v) => v.clone(),
        Err(_) => Vec::new(),
    };
    let mut selected: Vec<String> = local.iter().filter(|l| all.iter().any(|a| &a.0 == *l)).cloned().collect();
    let error = found.err();
    let mut input = Vec::new();
    let mut done = false;
    // An incompatible pack waits for the player's yes: (its id, why).
    let mut confirm: Option<(String, Compat)> = None;
    let auto = sys::env("PENRYN_TEST_PACKS");
    // PENRYN_TEST_INPUT's pclick=X:Y steps: clicks on this screen, one each half second.
    let mut clicks: Vec<(f32, f32)> = sys::env("PENRYN_TEST_INPUT")
        .unwrap_or_default()
        .split(',')
        .filter_map(|t| t.strip_prefix("pclick=")?.split_once(':').map(|(x, y)| (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0))))
        .collect();
    clicks.reverse();
    let started = sys::now_ms();
    let mut next_click = started + 1500;
    loop {
        if let Some(a) = auto.as_deref() {
            if sys::now_ms() > started + 1500 {
                selected = a.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
                done = true;
            }
        }
        if done {
            break;
        }
        let sel = selected.clone();
        let asked = confirm.as_ref().map(|c| c.1);
        let open = host.ui_frame(
            &mut |gui, mx, my| {
                draw_selection(gui, &all, &sel, error.as_deref(), mx, my);
                if let Some(k) = asked {
                    draw_pack_confirm(gui, k, mx, my);
                }
            },
            &mut input,
        );
        if !open {
            return None;
        }
        let (w, h) = host_gui_size();
        if sys::now_ms() >= next_click {
            if let Some((x, y)) = clicks.pop() {
                input.push(Input::Click(x, y));
                next_click = sys::now_ms() + 700;
            }
        }
        for ev in input.drain(..) {
            match ev {
                Input::Escape => {
                    if confirm.is_some() {
                        confirm = None;
                    } else {
                        done = true;
                    }
                }
                Input::Click(mx, my) => {
                    if let Some((id, _)) = confirm.clone() {
                        match prompt_hit((w, h), mx, my) {
                            Some(true) => {
                                selected.push(id);
                                confirm = None;
                            }
                            Some(false) => confirm = None,
                            None => {}
                        }
                        continue;
                    }
                    let l = selection_layout(w, h);
                    if done_button(w, h).hit(mx, my) {
                        done = true;
                        continue;
                    }
                    let available: Vec<&(String, Compat, String)> = all.iter().filter(|a| !selected.contains(&a.0)).collect();
                    if let Some(i) = l.row(mx, my, l.left) {
                        if let Some(a) = available.get(i) {
                            if a.1 == Compat::Ok {
                                selected.push(a.0.clone()); // to the top
                            } else {
                                confirm = Some((a.0.clone(), a.1));
                            }
                        }
                    } else if let Some(i) = l.row(mx, my, l.right) {
                        // Displayed top first.
                        let n = selected.len();
                        if i < n {
                            let idx = n - 1 - i;
                            let x_in = mx - l.right;
                            if x_in < 16.0 && idx + 1 < n {
                                selected.swap(idx, idx + 1); // up
                            } else if x_in >= 16.0 && x_in < 32.0 && idx > 0 {
                                selected.swap(idx, idx - 1); // down
                            } else {
                                selected.remove(idx);
                            }
                        }
                    }
                }
            }
        }
    }
    if selected.as_slice() == local {
        None
    } else {
        Some(selected)
    }
}

struct SelLayout {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

const ROW: f32 = 36.0;
const LIST_W: f32 = 200.0;

impl SelLayout {
    fn row(&self, mx: f32, my: f32, x: f32) -> Option<usize> {
        if mx < x || mx >= x + LIST_W || my < self.top + 18.0 || my >= self.bottom {
            return None;
        }
        Some(((my - self.top - 18.0) / ROW) as usize)
    }
}

fn selection_layout(w: f32, h: f32) -> SelLayout {
    let cx = crate::math::floor((w / 2.0) as f64) as f32;
    SelLayout { left: cx - 4.0 - LIST_W, right: cx + 4.0, top: 33.0, bottom: h - 33.0 }
}

fn done_button(w: f32, h: f32) -> Button {
    Button::new(crate::math::floor((w / 2.0 + 4.0) as f64) as f32, h - 27.0, 150.0, crate::lang::get("gui.done"))
}

fn folder_button(w: f32, h: f32) -> Button {
    Button::new(crate::math::floor((w / 2.0 - 154.0) as f64) as f32, h - 27.0, 150.0, crate::lang::get("pack.openFolder")).off()
}

fn pack_name(id: &str) -> &str {
    id.strip_prefix("file/").unwrap_or(id)
}

fn draw_entry(gui: &mut Gui, x: f32, y: f32, name: &str, desc: &str, kind: Compat, hover: bool, arrows: bool) {
    let compatible = kind == Compat::Ok;
    if hover {
        gui.fill(x, y, x + LIST_W, y + ROW, 0x40FF_FFFF);
    }
    // The pack's icon place (pack.png is not drawn yet).
    gui.fill(x + 2.0, y + 2.0, x + 34.0, y + 34.0, 0xFF30_3030);
    let tx = x + 36.0;
    let name = if gui.text_width(name) > LIST_W - 40.0 { screens::wrap(gui, name, LIST_W - 46.0).into_iter().next().unwrap_or_default() + "..." } else { String::from(name) };
    gui.text(&name, tx, y + 3.0, WHITE);
    let mut lines = screens::wrap(gui, if compatible { desc } else { "" }, LIST_W - 40.0);
    if !compatible {
        // PackCompatibility's reason: made for a newer or an older version, or broken.
        let key = match kind {
            Compat::New => "pack.incompatible.new",
            Compat::Old => "pack.incompatible.old",
            _ => "pack.incompatible.unknown",
        };
        lines = alloc::vec![crate::lang::get(key)];
    }
    for (i, l) in lines.iter().take(2).enumerate() {
        gui.text(l, tx, y + 14.0 + i as f32 * 9.0, if compatible { 0xFF80_8080 } else { 0xFFFF_5555 });
    }
    if arrows && hover {
        gui.text("^", x + 6.0, y + 14.0, WHITE);
        gui.text("v", x + 22.0, y + 14.0, WHITE);
    }
}

/// ConfirmScreen before an incompatible pack is taken (vanilla's PackSelectionScreen): the
/// question, the reason, Yes and No, over the dimmed selection.
fn draw_pack_confirm(gui: &mut Gui, kind: Compat, mx: f32, my: f32) {
    note_size(gui);
    let reason = match kind {
        Compat::New => "pack.incompatible.confirm.new",
        Compat::Old => "pack.incompatible.confirm.old",
        _ => "pack.incompatible.confirm.unknown",
    };
    let title = crate::lang::get("pack.incompatible.confirm.title");
    let mut lines: Vec<String> = screens::wrap(gui, &crate::lang::get(reason), gui.width - 50.0);
    lines.truncate(15);
    let (w, h) = (gui.width, gui.height);
    gui.fill(0.0, 0.0, w, h, 0xC000_0000);
    let (ty, my0, by) = prompt_layout(h, lines.len());
    gui.text_centered(&title, w / 2.0, ty, WHITE);
    for (i, l) in lines.iter().enumerate() {
        gui.text_centered(l, w / 2.0, my0 + i as f32 * 9.0, WHITE);
    }
    for b in prompt_buttons(w, by, false).iter() {
        b.draw(gui, mx, my);
    }
    PROMPT_ROWS.store(lines.len() as u32, Ordering::Relaxed);
}

fn draw_selection(gui: &mut Gui, all: &[(String, Compat, String)], selected: &[String], error: Option<&str>, mx: f32, my: f32) {
    note_size(gui);
    let (w, h) = (gui.width, gui.height);
    let l = selection_layout(w, h);
    gui.text_centered(&crate::lang::get("resourcePack.title"), w / 2.0, 8.0, WHITE);
    gui.text_centered(&crate::lang::get("pack.dropInfo"), w / 2.0, 20.0, 0xFF80_8080);
    gui.fill(l.left, l.top, l.left + LIST_W, l.bottom, 0x8000_0000);
    gui.fill(l.right, l.top, l.right + LIST_W, l.bottom, 0x8000_0000);
    gui.text_centered(&crate::lang::get("pack.available.title"), l.left + LIST_W / 2.0, l.top + 4.0, WHITE);
    gui.text_centered(&crate::lang::get("pack.selected.title"), l.right + LIST_W / 2.0, l.top + 4.0, WHITE);
    let mut y = l.top + 18.0;
    for a in all.iter().filter(|a| !selected.contains(&a.0)) {
        if y + ROW > l.bottom {
            break;
        }
        let hover = mx >= l.left && mx < l.left + LIST_W && my >= y && my < y + ROW;
        draw_entry(gui, l.left, y, pack_name(&a.0), &a.2, a.1, hover, false);
        y += ROW;
    }
    if let Some(e) = error {
        for (i, line) in screens::wrap(gui, e, LIST_W - 8.0).iter().enumerate() {
            gui.text(line, l.left + 4.0, l.top + 18.0 + i as f32 * 9.0, 0xFFFF_5555);
        }
    }
    let mut y = l.top + 18.0;
    for s in selected.iter().rev() {
        if y + ROW > l.bottom {
            break;
        }
        let a = all.iter().find(|a| &a.0 == s);
        let hover = mx >= l.right && mx < l.right + LIST_W && my >= y && my < y + ROW;
        draw_entry(gui, l.right, y, pack_name(s), a.map_or("", |a| a.2.as_str()), a.map_or(Compat::Ok, |a| a.1), hover, true);
        y += ROW;
    }
    if y + ROW <= l.bottom {
        draw_entry(gui, l.right, y, &crate::lang::get("resourcePack.vanilla.name"), &crate::lang::get("resourcePack.vanilla.description"), Compat::Ok, false, false);
    }
    folder_button(w, h).draw(gui, mx, my);
    done_button(w, h).draw(gui, mx, my);
}

// ------------------------------------------------------------------ tests

/// Set while the selftest runs: its pushes are not reported.
static SELFTEST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    SELFTEST.store(true, core::sync::atomic::Ordering::Relaxed);
    let mut out = Vec::new();
    let mut check = |what: &'static str, got: String, want: &'static str| out.push((what, got, want));
    // A push (play state): uuid, url, hash, required, no prompt.
    let push = |url: &str, required: bool| {
        let mut o = Out::new();
        o.bytes(&[7u8; 16]).string(url).string("0123456789abcdef0123456789abcdef01234567").bool(required).bool(false);
        o.b
    };
    let mk = |s: Setting| {
        let mut p = Packs::new("127.0.0.1", 1, "P", &[0; 16], Vec::new());
        p.setting = s;
        p.status = match s {
            Setting::Enabled => PromptStatus::Allowed,
            Setting::Disabled => PromptStatus::Declined,
            Setting::Prompt => PromptStatus::Pending,
        };
        p
    };
    let statuses = |p: &Packs| p.out.iter().map(|x| format!("{}", x.1)).collect::<Vec<_>>().join(" ");
    let mut p = mk(Setting::Prompt);
    let b = push("ftp://x/p.zip", false);
    p.on_push(&mut In::new(&b));
    check("pack push: not http is INVALID_URL", statuses(&p), "5");
    let mut p = mk(Setting::Prompt);
    let b = push("http://127.0.0.1:1/p.zip", false);
    p.on_push(&mut In::new(&b));
    check("pack push, setting prompt: the confirm screen", format!("{} {}", p.prompt.is_some(), statuses(&p)), "true ");
    p.answer(true);
    check("pack prompt accepted: ACCEPTED, setting enabled", format!("{} {:?}", statuses(&p), p.setting), "3 Enabled");
    let mut p = mk(Setting::Prompt);
    p.on_push(&mut In::new(&b));
    p.answer(false);
    p.cleanup();
    check("pack prompt declined: DECLINED, setting disabled", format!("{} {:?}", statuses(&p), p.setting), "1 Disabled");
    let mut p = mk(Setting::Disabled);
    let br = push("http://127.0.0.1:1/p.zip", true);
    p.on_push(&mut In::new(&br));
    check("required pack, setting disabled: still asks", format!("{}", p.prompt.is_some()), "true");
    p.answer(false);
    check("required pack declined: disconnect", format!("{}", p.disconnect.is_some()), "true");
    let mut p = mk(Setting::Disabled);
    p.on_push(&mut In::new(&b));
    check("pack, setting disabled: DECLINED without asking", format!("{} {}", p.prompt.is_some(), statuses(&p)), "false 1");
    let mut p = mk(Setting::Enabled);
    p.on_push(&mut In::new(&b));
    check("pack, setting enabled: ACCEPTED", statuses(&p), "3");
    // The download fails (no relay at port 1): FAILED_DOWNLOAD.
    p.out.clear();
    p.packs[0].download = Download::Pending;
    p.packs[0].url = String::from("http://127.0.0.1:1/p.zip");
    let busy = {
        let saved = RELAY.swap(core::ptr::null_mut(), Ordering::AcqRel);
        let b = p.update_downloads();
        RELAY.store(saved, Ordering::Release);
        b
    };
    p.cleanup();
    check("pack download fails: FAILED_DOWNLOAD", format!("{} {}", busy, statuses(&p)), "false 2");
    // Pop of a pack that is not loaded: no answer (SERVER_REMOVED).
    let mut p = mk(Setting::Enabled);
    p.on_push(&mut In::new(&b));
    p.out.clear();
    let mut o = Out::new();
    o.bool(true).bytes(&[7u8; 16]);
    p.on_pop(&mut In::new(&o.b));
    p.cleanup();
    check("pack popped: removed, no answer", format!("{} [{}]", p.packs.len(), statuses(&p)), "0 []");
    // A second push while the screen is open: the screen takes the latest required flag.
    let mut p = mk(Setting::Prompt);
    p.on_push(&mut In::new(&br));
    p.on_push(&mut In::new(&b));
    check("pack prompt updated: 2 requests, latest not required", format!("{} {}", p.prompt.as_ref().map_or(0, |q| q.requests.len()), p.prompt.as_ref().map_or(true, |q| q.required)), "2 false");
    // A reload answers SUCCESSFULLY_LOADED for every pack it loads, the active ones too.
    let mut p = mk(Setting::Enabled);
    let b2 = {
        let mut o = Out::new();
        o.bytes(&[8u8; 16]).string("http://127.0.0.1:1/q.zip").string("").bool(false).bool(false);
        o.b
    };
    p.on_push(&mut In::new(&b));
    p.on_push(&mut In::new(&b2));
    p.packs[0].download = Download::Done(String::from("a"));
    p.packs[0].activation = Activation::Active;
    p.packs[1].download = Download::Done(String::from("b"));
    p.applied = alloc::vec![String::from("sha1:a"), String::from("sha1:b")];
    p.out.clear();
    p.begin_reload();
    p.finish_reload(Ok(()));
    check("pack reload: APPLIED for the active pack too", format!("{} {:?}", statuses(&p), p.packs[1].activation), "0 0 Active");
    // options.txt resourcePacks.
    let l = parse_option("[\"vanilla\",\"file/Local Test\",\"file/a\\\"b.zip\"]");
    check("resourcePacks option", format!("{:?}", l), "[\"file/Local Test\", \"file/a\\\"b.zip\"]");
    check("resourcePacks option written", option_text(&l), "[\"vanilla\",\"file/Local Test\",\"file/a\\\"b.zip\"]");
    check("server list match", format!("{} {}", same_server("example.org:25601", "Example.org", 25601), same_server("example.org", "example.org", 25601)), "true false");
    SELFTEST.store(false, core::sync::atomic::Ordering::Relaxed);
    out
}
