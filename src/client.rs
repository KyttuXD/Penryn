//! A Minecraft 26.3 session: status ping, login (offline, or online through the relay),
//! configuration, and the play packets penryn understands.

use crate::blocks::Blocks;
use crate::conn::{Conn, Packet};
use crate::crypto;
use crate::ids::{self, config, login, play};
use crate::nbt;
use crate::wire::{uuid_text, In, Out};
use crate::world::World;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// How far the join on its thread got, for ConnectScreen (title.rs): 0 connecting, 1 logging
/// in (the session server), 2 encrypting, 3 joining the world.
pub static JOIN_STAGE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn stage(s: u8) {
    JOIN_STAGE.store(s, core::sync::atomic::Ordering::Relaxed);
}

/// A signed-in Minecraft account (from the relay).
#[derive(Clone)]
pub struct Account {
    pub name: String,
    /// 32 hex digits, no dashes.
    pub uuid: String,
    pub token: String,
    /// Where the relay listens, to make the session-server call for online-mode servers.
    pub relay: (String, u16),
}

pub struct Player {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub vy: f64,
}

pub struct Game {
    pub conn: Conn,
    pub world: World,
    pub blocks: Blocks,
    pub player: Player,
    pub name: String,
    pub uuid: [u8; 16],
    pub entity_id: i32,
    pub game_mode: i32,
    pub may_fly: bool,
    pub flying: bool,
    pub health: f32,
    pub food: i32,
    /// The server's address (the voice chat's UDP target when the server names none).
    pub server_host: String,
    /// Voice chat payloads that came in (channel, bytes), kept only while voice chat is on.
    pub voice_in: Vec<(String, Vec<u8>)>,
    pub voice_wanted: bool,
    /// The armor and max_health attributes, and the air supply (entity data 1; 300 is full).
    pub armor: f64,
    pub max_health: f64,
    pub air: i32,
    pub day_time: i64,
    /// Chat lines and when they arrived (ms).
    pub chat: Vec<(String, u64)>,
    /// Set by the server's position packets until we have confirmed a teleport.
    pub positioned: bool,
    pub loaded_sent: bool,
    pub sequence: i32,
    pub dead: bool,
    pub disconnected: Option<String>,
    pub chunks_received: u32,
    /// The player's inventory menu (container 0): 0 result, 1-4 crafting, 5-8 armour,
    /// 9-35 main, 36-44 hotbar, 45 offhand. (item id, count).
    pub inventory: [Option<(i32, i32)>; 46],
    pub selected: i32,
    /// The inventory menu's state id (the server checks clicks against it).
    pub inventory_state: i32,
    /// The item on the mouse cursor in a container screen.
    pub carried: Option<(i32, i32)>,
    /// A container the server opened (chest, furnace...): its window.
    pub container: Option<Container>,
    pub xp_progress: f32,
    pub xp_level: i32,
    /// How fast the day clock runs (0 when the daylight cycle is off).
    pub clock_rate: f32,
    pub entities: alloc::collections::BTreeMap<i32, crate::entities::Entity>,
    /// Set when the arm swings (the hand's swing animation starts).
    pub swung: bool,
    /// The local player's pose is sleeping (the server's entity data): the in-bed screen shows.
    pub sleeping: bool,
    /// The level's game time in ticks (clouds drift with it).
    pub game_time: i64,
    /// Sounds the server asked for, waiting for the window to play them.
    pub sounds: Vec<Sound>,
    /// A block this client placed, until the server confirms it (then its place sound).
    pub placing: Option<([i32; 3], u64)>,
    /// The player's movement_speed attribute without sprinting, jump_strength, and the
    /// abilities' flying speed.
    pub move_speed: f64,
    pub jump_strength: f64,
    /// The mining attributes: mining_efficiency (0), block_break_speed (1), submerged_mining_speed (0.2).
    pub mining_attrs: [f64; 3],
    pub fly_speed: f32,
    /// The rain and thunder levels (ClientboundGameEventPacket 1, 2, 7 and 8).
    pub rain_level: f32,
    pub thunder_level: f32,
    /// Block entities' data (signs' text) and the sign editor the server opened.
    pub block_entities: crate::signs::BlockEntities,
    /// The player's status effects.
    pub effects: crate::effects::Effects,
    /// Particles, explosions and events for the particle engine, until the window takes them.
    pub particle_events: Vec<crate::particles::Effect>,
    /// Creative mode: the slots' components, the registries they name, the permission level.
    pub creative: crate::creative::Sync,
    /// The synced variant registries (mob textures), and the tab list's player names.
    pub variants: crate::mobs::Variants,
    pub player_names: alloc::collections::BTreeMap<[u8; 16], String>,
    /// The server's resource packs (packs.rs).
    pub packs: crate::packs::Packs,
    /// The server started a configuration phase: the window runs it (reconfigure).
    pub reconfigure: bool,
    /// Chat: the log, secure chat, the command tree, titles (talk.rs).
    pub talk: crate::talk::Talk,
    /// The tab list, scoreboard, teams, boss bars, titles, world border... (serverui.rs).
    pub ui: crate::serverui::ServerUi,
    /// The container screens' own state (menus.rs) and the maps the server sent (maps.rs).
    pub menus: crate::menus::State,
    pub maps: crate::maps::Maps,
    /// Who rides what, vehicles' attributes, the vehicle this client moves (riding.rs).
    pub riding: crate::riding::Links,
}

/// A sound to play: a sound-event registry id or a name, where (None: no position), volume,
/// pitch, and its SoundSource (category).
pub enum SoundRef {
    Id(u32),
    Name(String),
}

pub struct Sound {
    pub sound: SoundRef,
    pub pos: Option<[f64; 3]>,
    pub volume: f32,
    pub pitch: f32,
    pub source: usize,
}

pub struct Container {
    pub id: i32,
    /// The menu type's registry id (generic_9x3 is 2, ...).
    pub menu: i32,
    pub title: String,
    pub slots: Vec<Option<(i32, i32)>>,
    /// Each slot's component patch (creative::read_stack's form; empty when unknown).
    pub patches: Vec<Vec<u8>>,
    pub state: i32,
}

/// Drops every entity and the riding links with them (a new level): no ghost may remain.
pub fn clear_entities(entities: &mut alloc::collections::BTreeMap<i32, crate::entities::Entity>, riding: &mut crate::riding::Links) {
    entities.clear();
    riding.clear();
}

/// Pseudo entity events for particles.rs (real ones are 0-127 bytes the server sends; these are
/// above that range): the animate packet's crits.
pub const CRIT_EVENT: u8 = 200;
pub const MAGIC_CRIT_EVENT: u8 = 201;

pub fn uuid_bytes(hex: &str) -> [u8; 16] {
    let mut u = [0u8; 16];
    let h: Vec<u8> = hex.bytes().filter(|c| *c != b'-').collect();
    for i in 0..16 {
        let d = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        if h.len() >= 32 {
            u[i] = d(h[2 * i]) << 4 | d(h[2 * i + 1]);
        }
    }
    u
}

fn handshake(c: &mut Conn, host: &str, port: u16, intent: i32) -> Result<(), String> {
    let mut o = Out::new();
    o.varint(ids::PROTOCOL).string(host).u16(port).varint(intent);
    c.send(0, &o)
}

/// The server-list ping: (description and version, players online/max, round trip ms).
/// The handshake names host and port; the connection goes to dial_host and dial_port (an SRV
/// record, srv.rs), which are the same for a plain address.
pub fn status(host: &str, port: u16, dial_host: &str, dial_port: u16) -> Result<String, String> {
    let mut c = Conn::open(dial_host, dial_port)?;
    handshake(&mut c, host, port, 1)?;
    c.send(0, &Out::new())?;
    let p = c.recv(5000)?.ok_or("no status answer")?;
    let json = p.reader().string().ok_or("bad status answer")?;
    let t0 = crate::sys::now_ms();
    let mut o = Out::new();
    o.i64(t0 as i64);
    c.send(1, &o)?;
    let _ = c.recv(5000)?;
    Ok(format!("{}\nping: {} ms", json, crate::sys::now_ms() - t0))
}

/// Reads a whole frame or fails; for the handshake phases.
fn next(c: &mut Conn) -> Result<Packet, String> {
    c.recv(30000)?.ok_or_else(|| String::from("the server stopped answering"))
}

fn random_bytes(n: usize) -> Vec<u8> {
    // /dev/urandom exists on Mac OS X 10.6 (getentropy does not).
    let fd = crate::sys::read_urandom(n);
    fd.unwrap_or_else(|| {
        // Fallback: time-seeded xorshift (only reached if /dev/urandom is unreadable).
        let mut s = (crate::sys::now() * 1e6) as u64 | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    })
}

/// A login that got through to configuration (Game::login); Game::configure finishes the join
/// on the thread that owns the window, where the resource packs can ask and reload.
pub struct Login {
    pub conn: Conn,
    pub host: String,
    pub port: u16,
    pub name: String,
    pub uuid: [u8; 16],
    /// What a transfer keeps: the cookies (serverui.rs).
    pub common: crate::serverui::Common,
    pub account: Option<Account>,
}

/// Why a join stopped: an error, or a transfer to another server (its address, and what it keeps).
pub enum Join {
    Failed(String),
    Transfer(String, u16, crate::serverui::Common),
}

impl From<String> for Join {
    fn from(e: String) -> Join {
        Join::Failed(e)
    }
}

impl Game {
    /// PENRYN_DEBUG: what light a chunk's packet brought (the first few, and the player's).
    fn debug_light(&self, x: i32, z: i32) {
        if crate::sys::env("PENRYN_DEBUG").is_none() {
            return;
        }
        let pl = &self.player;
        let (px, py, pz) = (crate::math::ifloor(pl.x), crate::math::ifloor(pl.y), crate::math::ifloor(pl.z));
        let here = px >> 4 == x && pz >> 4 == z;
        if self.chunks_received > 3 && !here {
            return;
        }
        if let Some([sl, sn, bl, bn]) = self.world.light_stats(x, z) {
            crate::say!("light chunk {} {}: sky in {} sections ({} varying), block in {} ({} varying)", x, z, sl, sn, bl, bn);
        }
        if here {
            // A column through the player: sky and block light from 3 above to 6 below.
            let mut s = alloc::string::String::new();
            for dy in (-6..=3).rev() {
                let (sk, bk) = self.world.light_at(px, py + dy, pz);
                s += &alloc::format!(" y{}:{}/{}", py + dy, sk, bk);
            }
            crate::say!("light at player column (sky/block):{}", s);
        }
    }

    /// Connects and logs in (the connect screen's stages). Runs on the join's thread. `common` is
    /// what a transfer keeps (the cookies); `transferred` says so in the handshake (ClientIntent.TRANSFER).
    pub fn login(host: &str, port: u16, name: &str, account: Option<&Account>, common: crate::serverui::Common, transferred: bool) -> Result<Login, String> {
        Game::login_dialing(host, port, host, port, name, account, common, transferred)
    }

    /// `login` for a server whose connect address differs (an SRV record, srv.rs): the handshake
    /// and the Login name host and port, the connection goes to dial_host and dial_port.
    pub fn login_dialing(host: &str, port: u16, dial_host: &str, dial_port: u16, name: &str, account: Option<&Account>, mut common: crate::serverui::Common, transferred: bool) -> Result<Login, String> {
        crate::talk::set_account(account);
        let mut c = Conn::open(dial_host, dial_port)?;
        handshake(&mut c, host, port, if transferred { 3 } else { 2 })?;
        let (name, uuid) = match account {
            Some(a) => (a.name.clone(), uuid_bytes(&a.uuid)),
            None => (String::from(name), crate::md5::offline_uuid(name)),
        };
        let mut o = Out::new();
        o.string(&name).bytes(&uuid);
        c.send(login::S_HELLO, &o)?;
        let mut final_uuid = uuid;
        // ---- login
        loop {
            let p = next(&mut c)?;
            let mut r = p.reader();
            match p.id {
                login::C_DISCONNECT => {
                    let s = r.string().unwrap_or_default();
                    return Err(format!("the server refused the login: {}", json_text(&s)));
                }
                login::C_HELLO => {
                    let server_id = r.string().ok_or("bad encryption request")?;
                    let n = r.varint().ok_or("bad key")? as usize;
                    let key = r.take(n).ok_or("bad key")?.to_vec();
                    let n = r.varint().ok_or("bad token")? as usize;
                    let token = r.take(n).ok_or("bad token")?.to_vec();
                    let authenticate = r.bool().unwrap_or(true);
                    let rsa = crypto::RsaKey::from_spki(&key).ok_or("unreadable server key")?;
                    let s = random_bytes(16);
                    let mut secret = [0u8; 16];
                    secret.copy_from_slice(&s);
                    stage(1);
                    if authenticate {
                        let a = account.ok_or(
                            "this is an online-mode server: sign in with a Microsoft account first (penryn login)",
                        )?;
                        let hash = crypto::server_hash(server_id.as_bytes(), &secret, &key);
                        let body = format!("{}\n{}\n{}\n", a.token, a.uuid, hash);
                        let resp = crate::http::post(&a.relay.0, a.relay.1, "/join", body.as_bytes(), 20000)
                            .map_err(|e| format!("cannot reach the relay to join: {}", e))?;
                        if resp.status != 204 && resp.status != 200 {
                            return Err(format!("Mojang refused the join ({}): {}", resp.status, resp.text()));
                        }
                    }
                    stage(2);
                    let mut pad = random_bytes(512).into_iter().cycle();
                    let mut rnd = || pad.next().unwrap_or(7);
                    let es = rsa.encrypt(&secret, &mut rnd).ok_or("encryption failed")?;
                    let et = rsa.encrypt(&token, &mut rnd).ok_or("encryption failed")?;
                    let mut o = Out::new();
                    o.varint(es.len() as i32).bytes(&es).varint(et.len() as i32).bytes(&et);
                    c.send(login::S_KEY, &o)?;
                    c.cipher = Some(crypto::Cfb8Pair::new(&secret));
                }
                login::C_COMPRESSION => {
                    c.threshold = Some(r.varint().ok_or("bad compression")?);
                }
                login::C_CUSTOM_QUERY => {
                    let id = r.varint().unwrap_or(0);
                    let mut o = Out::new();
                    o.varint(id).bool(false);
                    c.send(login::S_CUSTOM_QUERY_ANSWER, &o)?;
                }
                login::C_COOKIE_REQUEST => crate::serverui::login_cookie(&mut c, &mut r, &mut common)?,
                login::C_FINISHED => {
                    let u = r.uuid().unwrap_or([0; 16]);
                    final_uuid = u;
                    let n = r.string().unwrap_or_default();
                    crate::say!("logged in as {} ({})", n, uuid_text(&u));
                    stage(3);
                    c.send(login::S_ACKNOWLEDGED, &Out::new())?;
                    // ClientHandshakePacketListenerImpl: the brand goes first in the configuration.
                    crate::serverui::send_brand(&mut c)?;
                    break;
                }
                other => return Err(format!("unexpected login packet {}", other)),
            }
        }
        Ok(Login { conn: c, host: String::from(host), port, name, uuid: final_uuid, common, account: account.cloned() })
    }

    /// Joins with the login made by `login`: the configuration (resource packs, registries)
    /// and the start of play. `ui` is the window: the packs' prompt and reloads run on it.
    pub fn configure(login: Login, mut blocks: Blocks, view: i8, local_packs: Vec<String>, ui: &mut dyn crate::packs::Host) -> Result<Game, Join> {
        let Login { conn: mut c, host, port, name, uuid: final_uuid, mut common, .. } = login;
        // ---- configuration
        let mut biomes: Vec<String> = Vec::new();
        let mut dims: Vec<(String, i32, i32)> = Vec::new();
        let mut creative = crate::creative::Sync::new();
        let mut variants = crate::mobs::Variants::default();
        let mut packs = crate::packs::Packs::new(&host, port, &name, &final_uuid, local_packs);
        let configured = configure(&mut c, view, &mut biomes, &mut dims, &mut creative.registries, &mut variants, &mut packs, ui, None, &mut blocks, &mut common);
        if let Err(e) = configured {
            // A transfer in the configuration: the packs go back to the player's own, and the
            // server to connect to comes back for the next join (serverui's join_with).
            if let Some((h, p)) = common.transfer.take() {
                packs.leave(ui, None, &mut blocks);
                return Err(Join::Transfer(h, p, common));
            }
            return Err(Join::Failed(e));
        }
        let mut su = crate::serverui::ServerUi::new(common);
        su.online = c.cipher.is_some();
        let block_bits = blocks.global_bits();
        let mut world = World::new(block_bits, crate::blocks::ceil_log2(biomes.len().max(1)));
        world.set_biomes(&biomes, &blocks);
        world.dim_types = dims;
        Ok(Game {
            conn: c,
            world,
            blocks,
            player: Player { x: 0.0, y: 0.0, z: 0.0, yaw: 0.0, pitch: 0.0, on_ground: false, vy: 0.0 },
            name,
            uuid: final_uuid,
            entity_id: 0,
            game_mode: 0,
            may_fly: false,
            flying: false,
            health: 20.0,
            food: 20,
            armor: 0.0,
            max_health: 20.0,
            air: 300,
            server_host: host.clone(),
            voice_in: Vec::new(),
            voice_wanted: false,
            day_time: 6000,
            chat: Vec::new(),
            positioned: false,
            loaded_sent: false,
            sequence: 1,
            dead: false,
            disconnected: None,
            chunks_received: 0,
            inventory: [None; 46],
            selected: 0,
            inventory_state: 0,
            carried: None,
            container: None,
            xp_progress: 0.0,
            xp_level: 0,
            clock_rate: 1.0,
            entities: alloc::collections::BTreeMap::new(),
            swung: false,
            sleeping: false,
            game_time: 0,
            sounds: Vec::new(),
            placing: None,
            move_speed: 0.1f32 as f64,
            jump_strength: 0.42f32 as f64,
            mining_attrs: [0.0, 1.0, 0.2],
            fly_speed: 0.05,
            rain_level: 0.0,
            thunder_level: 0.0,
            block_entities: crate::signs::BlockEntities::new(),
            effects: crate::effects::Effects::new(),
            particle_events: Vec::new(),
            creative,
            variants,
            player_names: alloc::collections::BTreeMap::new(),
            packs,
            reconfigure: false,
            talk: crate::talk::Talk::new(),
            ui: su,
            menus: crate::menus::State::new(),
            maps: crate::maps::Maps::new(),
            riding: crate::riding::Links::default(),
        })
    }

    /// Connects, logs in and configures (main.rs's headless join).
    pub fn join(host: &str, port: u16, name: &str, account: Option<&Account>, blocks: Blocks, view: i8, local_packs: Vec<String>, ui: &mut dyn crate::packs::Host) -> Result<Game, String> {
        let login = Game::login(host, port, name, account, crate::serverui::Common::default(), false)?;
        Game::configure(login, blocks, view, local_packs, ui).map_err(|e| match e {
            Join::Failed(e) => e,
            Join::Transfer(h, p, _) => alloc::format!("the server transferred to {}:{} (penryn join does not follow transfers)", h, p),
        })
    }

    /// The configuration phase the server started from play (ClientboundStartConfiguration).
    pub fn reconfigure_now(&mut self, ui: &mut dyn crate::packs::Host) -> Result<(), String> {
        self.reconfigure = false;
        // The old listener and level are gone (the new play phase starts empty).
        self.reset_session();
        let mut b: Vec<String> = Vec::new();
        let mut dims: Vec<(String, i32, i32)> = Vec::new();
        configure(&mut self.conn, 8, &mut b, &mut dims, &mut self.creative.registries, &mut self.variants, &mut self.packs, ui, Some(&mut self.world), &mut self.blocks, &mut self.ui.common)?;
        self.world.dim_types = dims;
        if !b.is_empty() {
            self.world.biome_bits = crate::blocks::ceil_log2(b.len());
            self.world.set_biomes(&b, &self.blocks);
        }
        Ok(())
    }

    /// The resource packs' work for this frame (packs.rs): statuses, downloads, reloads.
    pub fn pack_step(&mut self, ui: &mut dyn crate::packs::Host) -> Result<(), String> {
        self.packs.step(&mut self.conn, play::S_RESOURCE_PACK, ui, Some(&mut self.world), &mut self.blocks)
    }

    /// Handles every packet that has arrived, without waiting.
    pub fn pump(&mut self) -> Result<(), String> {
        let mut n = 0;
        let start = crate::sys::now_ms();
        while !self.reconfigure {
            let Some(p) = self.conn.recv(0)? else { break };
            self.handle(p)?;
            n += 1;
            // Let the frame go on (a burst of chunks takes a while); the rest next time.
            if n > 2000 || crate::sys::now_ms() - start >= 4 {
                break;
            }
        }
        Ok(())
    }

    /// Waits up to `ms` for packets, handling them.
    pub fn wait(&mut self, ms: i32) -> Result<(), String> {
        if self.reconfigure {
            return Ok(());
        }
        if let Some(p) = self.conn.recv(ms)? {
            self.handle(p)?;
        }
        self.pump()
    }

    fn handle(&mut self, p: Packet) -> Result<(), String> {
        // A new listener: the reset comes before serverui::handle reads the login packet's
        // hardcore and immediate_respawn into the fresh ServerUi.
        if p.id == play::C_LOGIN {
            self.reset_session();
        }
        if self.talk_packet(&p)? || crate::serverui::handle(self, &p)? {
            return Ok(());
        }
        let mut r = p.reader();
        match p.id {
            play::C_KEEP_ALIVE => {
                let id = r.i64().unwrap_or(0);
                let mut o = Out::new();
                o.i64(id);
                self.conn.send(play::S_KEEP_ALIVE, &o)?;
            }
            play::C_PING => {
                let id = r.i32().unwrap_or(0);
                let mut o = Out::new();
                o.i32(id);
                self.conn.send(play::S_PONG, &o)?;
            }
            play::C_LOGIN => {
                // After a reconfiguration the server waits for PlayerLoaded again.
                self.positioned = false;
                self.loaded_sent = false;
                self.entity_id = r.i32().unwrap_or(0);
                let _hardcore = r.bool();
                let n = r.varint().unwrap_or(0);
                for _ in 0..n {
                    r.string();
                }
                let _max = r.varint();
                let _radius = r.varint();
                let _sim = r.varint();
                let _reduced = r.bool();
                let _death_screen = r.bool();
                let _limited = r.bool();
                self.spawn_info(&mut r);
                self.riding.clear();
                crate::say!("joined: entity {}, game mode {}", self.entity_id, self.game_mode);
            }
            play::C_RESPAWN => {
                self.reset_level();
                self.riding.clear();
                self.spawn_info(&mut r);
                self.dead = false;
                self.positioned = false;
                // 26.3 ignores movement after a respawn until PlayerLoaded comes again.
                self.loaded_sent = false;
            }
            play::C_PLAYER_POSITION => {
                let tp = r.varint().unwrap_or(0);
                let (x, y, z) = (r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0));
                let _d = (r.f64(), r.f64(), r.f64());
                let (yaw, pitch) = (r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0));
                let rel = r.i32().unwrap_or(0);
                let pl = &mut self.player;
                pl.x = if rel & 1 != 0 { pl.x + x } else { x };
                pl.y = if rel & 2 != 0 { pl.y + y } else { y };
                pl.z = if rel & 4 != 0 { pl.z + z } else { z };
                pl.yaw = if rel & 8 != 0 { pl.yaw + yaw } else { yaw };
                pl.pitch = if rel & 16 != 0 { pl.pitch + pitch } else { pitch };
                pl.vy = 0.0;
                if crate::sys::env("PENRYN_DEBUG").is_some() {
                    crate::say!("teleport {} to {:.2} {:.2} {:.2} yaw {} pitch {} rel {} ({} bytes)", tp, pl.x, pl.y, pl.z, pl.yaw, pl.pitch, rel, p.body.len());
                }
                // 26.3: the confirmation repeats where the teleport put us.
                let mut o = Out::new();
                o.varint(tp).f64(pl.x).f64(pl.y).f64(pl.z).f32(pl.yaw).f32(pl.pitch);
                self.conn.send(play::S_ACCEPT_TELEPORTATION, &o)?;
                // The position itself goes out with the next tick: one per client tick.
                self.positioned = true;
            }
            play::C_LEVEL_CHUNK_WITH_LIGHT => {
                let x = r.i32().unwrap_or(0);
                let z = r.i32().unwrap_or(0);
                let nh = r.varint().unwrap_or(0);
                for _ in 0..nh {
                    r.varint();
                    let n = r.varint().unwrap_or(0).max(0) as usize;
                    r.take(n * 8);
                }
                let len = r.varint().unwrap_or(0).max(0) as usize;
                if let Some(buf) = r.take(len) {
                    let blocks = &self.blocks;
                    let is_air = |s: u16| blocks.kind(s) == crate::blocks::AIR;
                    if self.world.load_column(x, z, buf, &is_air).is_none() {
                        crate::warn!("chunk {} {}: could not read its sections", x, z);
                    }
                    self.chunks_received += 1;
                }
                // Then the chunk's block entities: signs' text.
                if self.block_entities.read_chunk(x, z, &mut r).is_none() {
                    crate::warn!("chunk {} {}: could not read its block entities", x, z);
                } else if self.world.read_light(x, z, &mut r).is_none() {
                    crate::warn!("chunk {} {}: could not read its light", x, z);
                } else {
                    self.debug_light(x, z);
                }
            }
            play::C_LIGHT_UPDATE => {
                let x = r.varint().unwrap_or(0);
                let z = r.varint().unwrap_or(0);
                if self.world.read_light(x, z, &mut r).is_none() {
                    crate::warn!("light update {} {}: could not read it", x, z);
                }
            }
            play::C_FORGET_LEVEL_CHUNK => {
                let v = r.i64().unwrap_or(0);
                self.world.forget(v as i32, (v >> 32) as i32);
                self.block_entities.forget(v as i32, (v >> 32) as i32);
            }
            play::C_CHUNK_BATCH_FINISHED => {
                let mut o = Out::new();
                o.f32(20.0);
                self.conn.send(play::S_CHUNK_BATCH_RECEIVED, &o)?;
            }
            play::C_BLOCK_UPDATE => {
                let v = r.i64().unwrap_or(0);
                let s = r.varint().unwrap_or(0) as u16;
                let (x, y, z) = ((v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32);
                let blocks = &self.blocks;
                let old = self.world.get(x, y, z);
                crate::moving::set_block(&mut self.world, blocks, x, y, z, s);
                self.block_entities.block_changed(blocks, old, x, y, z, s);
                // The block this client placed: its place sound (BlockItem.place plays it).
                if let Some((pp, at)) = self.placing {
                    if pp == [x, y, z] && crate::sys::now_ms() < at + 1500 && blocks.kind(s) != crate::blocks::AIR {
                        self.placing = None;
                        let blk = blocks.of_state(s);
                        if blk.sounds[2] != 0xFFFF {
                            self.sounds.push(Sound {
                                sound: SoundRef::Id(blk.sounds[2] as u32),
                                pos: Some([x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5]),
                                volume: (blk.sound_volume + 1.0) / 2.0,
                                pitch: blk.sound_pitch * 0.8,
                                source: 4,
                            });
                        }
                    }
                }
            }
            play::C_SECTION_BLOCKS_UPDATE => {
                let v = r.i64().unwrap_or(0);
                let (sx, sz, sy) = ((v >> 42) as i32, (v << 22 >> 42) as i32, (v << 44 >> 44) as i32);
                let n = r.varint().unwrap_or(0);
                for _ in 0..n {
                    let c = r.varlong().unwrap_or(0);
                    let state = (c >> 12) as u16;
                    let (x, z, y) = ((c >> 8 & 15) as i32, (c >> 4 & 15) as i32, (c & 15) as i32);
                    let blocks = &self.blocks;
                    let old = self.world.get(sx * 16 + x, sy * 16 + y, sz * 16 + z);
                    crate::moving::set_block(&mut self.world, blocks, sx * 16 + x, sy * 16 + y, sz * 16 + z, state);
                    self.block_entities.block_changed(blocks, old, sx * 16 + x, sy * 16 + y, sz * 16 + z, state);
                }
            }
            play::C_BLOCK_ENTITY_DATA | play::C_OPEN_SIGN_EDITOR => {
                let v = r.i64().unwrap_or(0);
                let (x, y, z) = ((v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32);
                if p.id == play::C_OPEN_SIGN_EDITOR {
                    // SignTextSlot: 0 back, 1 front.
                    self.block_entities.open_request = Some(([x, y, z], r.varint().unwrap_or(1) == 1));
                } else {
                    let _kind = r.varint();
                    let _ = self.block_entities.read_tag(x, y, z, &mut r);
                }
            }
            play::C_BLOCK_EVENT => crate::moving::on_block_event(&mut self.world, &self.blocks, &mut r),
            play::C_UPDATE_MOB_EFFECT => self.effects.on_update(&mut r, self.entity_id),
            play::C_REMOVE_MOB_EFFECT => self.effects.on_remove(&mut r, self.entity_id),
            play::C_PLAYER_ABILITIES => {
                let f = r.u8().unwrap_or(0);
                self.flying = f & 2 != 0;
                self.may_fly = f & 4 != 0;
                self.fly_speed = r.f32().unwrap_or(0.05);
                let _walk = r.f32();
            }
            play::C_UPDATE_ATTRIBUTES => {
                // The player's own movement_speed (26) and jump_strength (19), worked out as
                // AttributeInstance.calculateValue does; sprinting is added by the mover.
                let id = r.varint().unwrap_or(-1);
                let n = r.varint().unwrap_or(0);
                for _ in 0..n.clamp(0, 64) {
                    let (Some(attr), Some(base), Some(mods)) = (r.varint(), r.f64(), r.varint()) else { break };
                    let mut add = 0.0;
                    let mut mul_base = 0.0;
                    let mut mul_total = 1.0;
                    for _ in 0..mods.clamp(0, 64) {
                        let (Some(name), Some(amount), Some(op)) = (r.string(), r.f64(), r.varint()) else { break };
                        if name == "minecraft:sprinting" {
                            continue;
                        }
                        match op {
                            0 => add += amount,
                            1 => mul_base += amount,
                            _ => mul_total *= 1.0 + amount,
                        }
                    }
                    let b = base + add;
                    let v = (b + b * mul_base) * mul_total;
                    if id == self.entity_id {
                        match attr {
                            26 => self.move_speed = v.clamp(0.0, 1024.0),
                            19 => self.jump_strength = v.clamp(0.0, 32.0),
                            24 => self.mining_attrs[0] = v.clamp(0.0, 1024.0),
                            7 => self.mining_attrs[1] = v.clamp(0.0, 1024.0),
                            34 => self.mining_attrs[2] = v.clamp(0.0, 20.0),
                            1 => self.armor = v.clamp(0.0, 30.0),
                            23 => self.max_health = v.clamp(1.0, 1024.0),
                            _ => {}
                        }
                    } else {
                        self.riding.on_attribute(id, attr, v);
                        // generic.scale (Attributes.SCALE, clamped 0.0625..16 as its range).
                        if attr == 30 {
                            if let Some(e) = self.entities.get_mut(&id) {
                                e.scale = v.clamp(0.0625, 16.0) as f32;
                            }
                        }
                    }
                }
            }
            play::C_SET_HEALTH => {
                self.health = r.f32().unwrap_or(20.0);
                self.food = r.varint().unwrap_or(20);
                if self.health <= 0.0 && !self.dead {
                    self.dead = true;
                    self.add_chat(String::from("You died. Press R to respawn."));
                }
            }
            play::C_SET_TIME => {
                // 26.3: the game time, then the world clocks (the first is the dimension's day).
                self.game_time = r.i64().unwrap_or(0);
                if r.varint().unwrap_or(0) > 0 {
                    let _clock = r.varint();
                    self.day_time = r.varlong().unwrap_or(6000);
                    let _partial = r.f32();
                    self.clock_rate = r.f32().unwrap_or(1.0);
                }
            }
            play::C_GAME_EVENT => {
                let e = r.u8().unwrap_or(0);
                let v = r.f32().unwrap_or(0.0);
                match e {
                    3 => self.game_mode = v as i32,
                    // Start raining: from level 0 up; stop: from 1 down (the server sends the levels).
                    1 => self.rain_level = 0.0,
                    2 => self.rain_level = 1.0,
                    // A NaN level would stick in the rain fog's smoothing: taken as 0.
                    7 => self.rain_level = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) },
                    8 => self.thunder_level = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) },
                    _ => {}
                }
            }
            play::C_CONTAINER_SET_CONTENT => {
                let id = r.varint().unwrap_or(-1);
                let state = r.varint().unwrap_or(0);
                // Every slot takes at least a byte: a count past the packet's end is a bad one.
                let n = (r.varint().unwrap_or(0).max(0) as usize).min(r.left());
                let mut ok = true;
                let mut got: Vec<Option<(i32, i32)>> = Vec::with_capacity(n);
                let mut patches: Vec<Vec<u8>> = Vec::with_capacity(n);
                for _ in 0..n {
                    match crate::creative::read_stack(&mut r) {
                        Some((item, rest_ok, patch)) => {
                            got.push(item);
                            patches.push(patch);
                            if !rest_ok {
                                ok = false;
                                break; // a component patch we cannot skip: the rest stays as it was
                            }
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    if let Some((c, _)) = slot(&mut r) {
                        self.carried = c;
                    }
                }
                if id == 0 {
                    self.inventory_state = state;
                    for (i, (it, p)) in got.into_iter().zip(patches).enumerate().take(46) {
                        self.inventory[i] = it;
                        self.creative.patches[i] = p;
                    }
                } else if let Some(c) = self.container.as_mut().filter(|c| c.id == id) {
                    c.state = state;
                    if c.slots.len() < n {
                        c.slots.resize(n, None);
                    }
                    if c.patches.len() < n {
                        c.patches.resize(n, Vec::new());
                    }
                    let own = crate::menus::player_start(c.menu, n);
                    for (i, it) in got.into_iter().enumerate() {
                        c.slots[i] = it;
                        c.patches[i] = patches.get(i).cloned().unwrap_or_default();
                        // The player's own rows at the bottom of every container.
                        if i >= own && i < own + 36 {
                            let k = i - own;
                            let menu = if k < 27 { 9 + k } else { 36 + k - 27 };
                            if menu < 46 {
                                self.inventory[menu] = it;
                                self.creative.patches[menu] = patches.get(i).cloned().unwrap_or_default();
                            }
                        }
                    }
                }
            }
            play::C_CONTAINER_SET_SLOT => {
                let id = r.varint().unwrap_or(-2);
                let state = r.varint().unwrap_or(0);
                let i = r.u16().unwrap_or(0xFFFF) as i16 as i32;
                let Some((item, _, patch)) = crate::creative::read_stack(&mut r) else { return Ok(()) };
                if id == -1 {
                    self.carried = item;
                } else if id == 0 {
                    self.inventory_state = state;
                    if (0..46).contains(&i) {
                        self.inventory[i as usize] = item;
                        self.creative.patches[i as usize] = patch;
                    }
                } else if let Some(c) = self.container.as_mut().filter(|c| c.id == id) {
                    c.state = state;
                    if i >= 0 {
                        if c.slots.len() <= i as usize {
                            c.slots.resize(i as usize + 1, None);
                        }
                        if c.patches.len() <= i as usize {
                            c.patches.resize(i as usize + 1, Vec::new());
                        }
                        c.slots[i as usize] = item;
                        c.patches[i as usize] = patch;
                    }
                }
            }
            play::C_SET_CURSOR_ITEM => {
                if let Some((item, _)) = slot(&mut r) {
                    self.carried = item;
                }
            }
            play::C_OPEN_SCREEN => {
                let id = r.varint().unwrap_or(0);
                let menu = r.varint().unwrap_or(0);
                let title = nbt::text(&mut r);
                self.menus.opened();
                self.container = Some(Container { id, menu, title, slots: Vec::new(), patches: Vec::new(), state: 0 });
            }
            play::C_CONTAINER_CLOSE => {
                let id = r.varint().unwrap_or(0);
                if self.container.as_ref().map_or(false, |c| c.id == id) {
                    self.container = None;
                }
            }
            play::C_SET_EXPERIENCE => {
                self.xp_progress = r.f32().unwrap_or(0.0);
                self.xp_level = r.varint().unwrap_or(0);
            }
            play::C_SET_PLAYER_INVENTORY => {
                // Inventory index: 0-8 hotbar, 9-35 main, 36-39 armour, 40 offhand.
                let i = r.varint().unwrap_or(-1);
                let menu = match i {
                    0..=8 => i + 36,
                    9..=35 => i,
                    36..=39 => 8 - (i - 36),
                    40 => 45,
                    _ => -1,
                };
                if let (Some((item, _, patch)), true) = (crate::creative::read_stack(&mut r), menu >= 0) {
                    self.inventory[menu as usize] = item;
                    self.creative.patches[menu as usize] = patch;
                }
            }
            play::C_ENTITY_EVENT => {
                // One arm for every entity event: the player's permission level (24-28), the hurt
                // flash (2), the death (3, LivingEntity.handleEntityEvent) and the particle effects
                // (particles.rs: the breed hearts, the totem of undying, the poofs).
                let (id, ev) = (r.i32().unwrap_or(0), r.u8().unwrap_or(0));
                if id == self.entity_id && (24..=28).contains(&ev) {
                    self.creative.op_level = ev as i32 - 24;
                }
                if let Some(e) = self.entities.get_mut(&id) {
                    match ev {
                        2 => e.hurt = 10,
                        3 => {
                            if e.item.is_none() {
                                e.health = Some(0.0);
                            }
                        }
                        _ => {}
                    }
                    if self.particle_events.len() < 512 {
                        self.particle_events.push(crate::particles::Effect::Entity(ev, e.pos, e.kind));
                    }
                }
            }
            play::C_SET_HELD_SLOT => {
                self.selected = r.varint().unwrap_or(0).clamp(0, 8);
            }
            play::C_ADD_ENTITY => {
                let id = r.varint().unwrap_or(0);
                let uuid = r.uuid().unwrap_or([0; 16]);
                let kind = r.varint().unwrap_or(-1);
                let pos = [r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0)];
                lp_vec3(&mut r);
                let pitch = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                let yaw = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                let head = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                let mut e = crate::entities::Entity::new(kind, uuid, pos, yaw, pitch, head);
                if crate::maps::is_frame(kind) {
                    // An item frame's facing (Direction.get3DDataValue).
                    e.data.insert(crate::maps::SPAWN_DATA, r.varint().unwrap_or(3) as i64);
                }
                if crate::sys::env("PENRYN_DEBUG").is_some() {
                    crate::say!("entity {} added: type {}, at {:.2} {:.2} {:.2}", id, kind, pos[0], pos[1], pos[2]);
                }
                self.entities.insert(id, e);
            }
            play::C_REMOVE_ENTITIES => {
                let n = r.varint().unwrap_or(0);
                for _ in 0..n {
                    if let Some(id) = r.varint() {
                        if crate::sys::env("PENRYN_DEBUG").is_some() && self.entities.contains_key(&id) {
                            crate::say!("entity {} removed", id);
                        }
                        self.entities.remove(&id);
                        self.riding.forget(id);
                    }
                }
            }
            play::C_MOVE_ENTITY_POS | play::C_MOVE_ENTITY_POS_ROT | play::C_MOVE_ENTITY_ROT => {
                let id = r.varint().unwrap_or(0);
                // The vehicle this client moves keeps its own place (isLocalInstanceAuthoritative).
                if self.riding.driven.map(|d| d.0) == Some(id) {
                    return Ok(());
                }
                let Some(e) = self.entities.get_mut(&id) else { return Ok(()) };
                if p.id == play::C_MOVE_ENTITY_ROT {
                    e.on_ground = r.bool().unwrap_or(false);
                    let (yaw, pitch) = (r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0, r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0);
                    // moveOrInterpolateTo(yaw, pitch): the glide restarts from where the entity is
                    // now, towards the same target position, over 3 steps (not cancelled).
                    let pos = e.target;
                    e.set_target(pos, yaw, pitch, 3);
                } else {
                    // 26.3: on-ground in bit 0, the number of timed steps above it. Each delta is
                    // relative to the server's position (the last target), and each one counts
                    // as a step of its own, so the entity glides to it instead of jumping.
                    let props = r.varint().unwrap_or(0) as u32;
                    e.on_ground = props & 1 != 0;
                    let steps = props >> 1;
                    let mut deltas = Vec::new();
                    if steps == 0 {
                        deltas.push([r.u16().unwrap_or(0) as i16, r.u16().unwrap_or(0) as i16, r.u16().unwrap_or(0) as i16]);
                    } else {
                        for _ in 0..steps.min(64) {
                            let _ticks = r.varint();
                            deltas.push([r.u16().unwrap_or(0) as i16, r.u16().unwrap_or(0) as i16, r.u16().unwrap_or(0) as i16]);
                        }
                    }
                    let (mut pos, mut yaw, mut pitch) = (e.target, e.target_yaw, e.target_pitch);
                    for d in deltas.iter() {
                        for a in 0..3 {
                            if d[a] != 0 {
                                // VecDeltaCodec: quantize the base to 1/4096, then add.
                                let q = crate::math::floor(pos[a] * 4096.0 + 0.5) as i64 + d[a] as i64;
                                pos[a] = q as f64 / 4096.0;
                            }
                        }
                    }
                    if p.id == play::C_MOVE_ENTITY_POS_ROT {
                        yaw = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                        pitch = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                    }
                    let n = (deltas.len() as u32).clamp(3, 8);
                    e.set_target(pos, yaw, pitch, n);
                }
            }
            play::C_TELEPORT_ENTITY => {
                let id = r.varint().unwrap_or(0);
                let pos = [r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0)];
                let _d = (r.f64(), r.f64(), r.f64());
                let (yaw, pitch) = (r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0));
                let rel = r.i32().unwrap_or(0);
                if self.riding.driven.map(|d| d.0) == Some(id) {
                    return Ok(());
                }
                if let Some(e) = self.entities.get_mut(&id) {
                    let mut to = e.target;
                    for a in 0..3 {
                        to[a] = if rel & (1 << a) != 0 { e.target[a] + pos[a] } else { pos[a] };
                    }
                    let (yw, pt) = (if rel & 8 != 0 { e.target_yaw + yaw } else { yaw }, if rel & 16 != 0 { e.target_pitch + pitch } else { pitch });
                    e.set_target(to, yw, pt, 3);
                }
            }
            play::C_ENTITY_POSITION_SYNC => {
                let id = r.varint().unwrap_or(0);
                // PositionPath: 0 linear (one position), 1 timed steps (the last one counts).
                let mut pos = None;
                match r.varint().unwrap_or(0) {
                    1 => {
                        let n = r.varint().unwrap_or(0);
                        for _ in 0..n.min(64) {
                            pos = Some([r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0)]);
                            let _t = r.varint();
                        }
                    }
                    _ => pos = Some([r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0)]),
                }
                let (yaw, pitch) = (r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0));
                if self.riding.driven.map(|d| d.0) == Some(id) {
                    return Ok(());
                }
                if let (Some(e), Some(p)) = (self.entities.get_mut(&id), pos) {
                    e.set_target(p, yaw, pitch, 3);
                }
            }
            crate::riding::C_SET_PASSENGERS => self.riding.on_passengers(&mut r),
            crate::riding::C_MOVE_VEHICLE => self.riding.on_move_vehicle(&mut r),
            crate::riding::C_SET_ENTITY_MOTION => self.riding.on_motion(&mut r, self.entity_id),
            play::C_ROTATE_HEAD => {
                let id = r.varint().unwrap_or(0);
                let h = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                if let Some(e) = self.entities.get_mut(&id) {
                    e.lerp_head_to(h);
                }
            }
            // ClientboundAnimatePacket (26.3): 0 WAKE_UP, 1 CRITICAL_HIT, 2 MAGIC_CRITICAL_HIT
            // (the swings are C_SWING_ANIMATION now).
            play::C_ANIMATE => {
                let id = r.varint().unwrap_or(0);
                let action = r.u8().unwrap_or(255);
                if let Some(e) = self.entities.get_mut(&id) {
                    match action {
                        // WAKE_UP: the pose comes with the entity data (nothing to animate).
                        0 => {}
                        1 | 2 if self.particle_events.len() < 512 => {
                            // ClientPacketListener.handleAnimate: 5 crit / magic crit particles
                            // over the target (Entity.crit, particles.rs).
                            self.particle_events.push(crate::particles::Effect::Entity(if action == 1 { CRIT_EVENT } else { MAGIC_CRIT_EVENT }, e.pos, e.kind));
                        }
                        _ => {}
                    }
                }
            }
            // A plugin message: the voice chat mod's channels (voicechat:*) go to voicechat/.
            play::C_CUSTOM_PAYLOAD => {
                let channel = r.string().unwrap_or_default();
                if self.voice_wanted && channel.starts_with("voicechat:") && self.voice_in.len() < 256 {
                    self.voice_in.push((channel, r.rest().to_vec()));
                }
            }
            // 26.3: the arm swing has a packet of its own (the entity, the hand: 0 main, 1 off).
            play::C_SWING_ANIMATION => {
                let id = r.varint().unwrap_or(0);
                let hand = r.varint().unwrap_or(0);
                if let Some(e) = self.entities.get_mut(&id) {
                    e.swing_arm(hand != 0);
                }
            }
            play::C_DAMAGE_EVENT | play::C_HURT_ANIMATION => {
                let id = r.varint().unwrap_or(0);
                if crate::sys::env("PENRYN_DEBUG").is_some() {
                    crate::say!("entity {} hurt", id);
                }
                if let Some(e) = self.entities.get_mut(&id) {
                    e.hurt = 10;
                }
            }
            play::C_SET_ENTITY_DATA => {
                let id = r.varint().unwrap_or(0);
                if id == self.entity_id {
                    // Our own flags: the server's word on gliding (special.rs).
                    let mut me = crate::entities::Entity::new(-1, [0; 16], [0.0; 3], 0.0, 0.0, 0.0);
                    entity_data(&mut r, &mut me);
                    if let Some(&f) = me.data.get(&0) {
                        self.riding.own_flags = Some(f as u8);
                    }
                    if let Some(&a) = me.data.get(&1) {
                        self.air = a as i32;
                    }
                    // Pose (data 6): 2 is sleeping.
                    if let Some(&pose) = me.data.get(&6) {
                        self.sleeping = pose as i64 == crate::poses::SLEEPING;
                    }
                }
                if let Some(e) = self.entities.get_mut(&id) {
                    let before = crate::display::snapshot(e);
                    let ok = entity_data(&mut r, e);
                    crate::display::data_changed(e, before);
                    e.version = e.version.wrapping_add(1);
                    if crate::sys::env("PENRYN_DEBUG").as_deref() == Some("2") {
                        crate::say!(
                            "entity {} data: {} (item {:?} {}, patch {} bytes, values {:?})",
                            id,
                            if ok.is_some() { "read" } else { "NOT READ TO THE END" },
                            e.item,
                            e.item.and_then(|(i, _)| self.blocks.items.get(i as usize)).map(|s| s.as_str()).unwrap_or(""),
                            e.item_patch.len(),
                            e.data
                        );
                    }
                }
            }
            crate::mobs::C_SET_EQUIPMENT => {
                let id = r.varint().unwrap_or(0);
                if let Some(e) = self.entities.get_mut(&id) {
                    crate::mobs::equipment(&mut r, e, crate::creative::read_stack);
                }
            }
            play::C_SOUND | play::C_SOUND_ENTITY => {
                // SoundEvent holder: registry id + 1, or 0 and an inline name (and range).
                let holder = r.varint().unwrap_or(0);
                let sound = if holder == 0 {
                    let name = r.string().unwrap_or_default();
                    if r.bool() == Some(true) {
                        let _range = r.f32();
                    }
                    SoundRef::Name(String::from(name.trim_start_matches("minecraft:")))
                } else {
                    SoundRef::Id(holder as u32 - 1)
                };
                let source = r.varint().unwrap_or(0).clamp(0, 10) as usize;
                let pos = if p.id == play::C_SOUND {
                    let (x, y, z) = (r.i32().unwrap_or(0), r.i32().unwrap_or(0), r.i32().unwrap_or(0));
                    Some([x as f64 / 8.0, y as f64 / 8.0, z as f64 / 8.0])
                } else {
                    let id = r.varint().unwrap_or(0);
                    self.entities.get(&id).map(|e| e.pos)
                };
                let (volume, pitch) = (r.f32().unwrap_or(1.0), r.f32().unwrap_or(1.0));
                if pos.is_some() || p.id == play::C_SOUND {
                    self.sounds.push(Sound { sound, pos, volume, pitch, source });
                }
            }
            play::C_LEVEL_EVENT => {
                let kind = r.i32().unwrap_or(0);
                let v = r.i64().unwrap_or(0);
                let data = r.i32().unwrap_or(0);
                let (x, y, z) = ((v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32);
                if self.particle_events.len() < 512 {
                    self.particle_events.push(if kind == 2001 {
                        crate::particles::Effect::Destroy([x, y, z], data as u16)
                    } else {
                        crate::particles::Effect::Level(kind, [x, y, z], data)
                    });
                }
                // 2001: a block destroyed (by someone else): its break sound.
                if kind == 2001 {
                    let blk = self.blocks.of_state(data as u16);
                    if blk.sounds[0] != 0xFFFF {
                        self.sounds.push(Sound {
                            sound: SoundRef::Id(blk.sounds[0] as u32),
                            pos: Some([x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5]),
                            volume: (blk.sound_volume + 1.0) / 2.0,
                            pitch: blk.sound_pitch * 0.8,
                            source: 4,
                        });
                    }
                }
            }
            play::C_LEVEL_PARTICLES | play::C_EXPLODE => {
                if self.particle_events.len() < 512 {
                    self.particle_events.push(crate::particles::Effect::Packet(p.id, p.body.clone()));
                }
            }
            play::C_TAKE_ITEM_ENTITY => {
                let item = r.varint().unwrap_or(0);
                if let Some(e) = self.entities.get(&item) {
                    let kind = if e.item.is_some() { "entity.item.pickup" } else { "entity.experience_orb.pickup" };
                    let seed = (crate::sys::now_ms() % 1000) as f32 / 1000.0;
                    self.sounds.push(Sound {
                        sound: SoundRef::Name(String::from(kind)),
                        pos: Some(e.pos),
                        volume: 0.2,
                        pitch: ((seed - 0.5) * 0.7 + 1.0) * 2.0,
                        source: 7,
                    });
                }
            }
            play::C_DISCONNECT => {
                let t = nbt::text(&mut r);
                self.disconnected = Some(t.clone());
                return Err(format!("disconnected: {}", t));
            }
            play::C_START_CONFIGURATION => {
                self.conn.send(play::S_CONFIGURATION_ACKNOWLEDGED, &Out::new())?;
                // The rest is configuration: run by reconfigure_now, with the window.
                self.reconfigure = true;
            }
            play::C_RESOURCE_PACK_PUSH => {
                self.packs.on_push(&mut r);
                self.packs.flush(&mut self.conn, play::S_RESOURCE_PACK)?;
            }
            play::C_RESOURCE_PACK_POP => self.packs.on_pop(&mut r),
            crate::maps::C_MAP_ITEM_DATA => {
                let _ = self.maps.packet(&mut r);
            }
            crate::books::C_OPEN_BOOK => self.menus.open_book = Some(r.varint().unwrap_or(0)),
            id => {
                crate::menus::packet(self, id, &mut r);
            }
        }
        Ok(())
    }

    /// What a new ClientLevel drops (ClientboundRespawn, and everything below): the entities and
    /// their links, block entities, map data, the open menu and the world border. The tab list,
    /// scoreboard, boss bars and inventory belong to the listener and stay.
    pub fn reset_level(&mut self) {
        if crate::sys::env("PENRYN_DEBUG").is_some() {
            crate::say!("new level: {} entities dropped", self.entities.len());
        }
        clear_entities(&mut self.entities, &mut self.riding);
        crate::skins::forget_entities();
        self.block_entities.clear();
        self.maps = crate::maps::Maps::new();
        self.container = None;
        self.carried = None;
        // The recipes belong to the listener (UpdateRecipes comes at login and /reload only).
        let stonecutter = core::mem::take(&mut self.menus.stonecutter);
        self.menus = crate::menus::State::new();
        self.menus.stonecutter = stonecutter;
        self.particle_events.clear();
        self.particle_events.push(crate::particles::Effect::Clear);
        self.placing = None;
        self.swung = false;
        self.ui.border = crate::serverui::Border::default();
        self.ui.camera = None;
    }

    /// A new ClientPacketListener (a reconfiguration, or a login): the level's state and every
    /// per-session thing of the listener starts empty. Kept: the connection, the world's
    /// dimension types and biomes and the registries (configuration data), the options, chat
    /// lines and history, cookies, links and block tags (ServerUi::common), the resource packs.
    pub fn reset_session(&mut self) {
        self.reset_level();
        self.world.columns.clear();
        self.world.dirty.clear();
        self.world.epoch += 1;
        self.ui.reset_session();
        self.menus.stonecutter.clear();
        self.player_names.clear();
        crate::skins::forget_profiles();
        let t = &mut self.talk;
        t.tree = None;
        t.suggestions = None;
        t.names.clear();
        t.title = None;
        t.subtitle = None;
        t.title_ticks = 0;
        t.overlay = None;
        self.inventory = [None; 46];
        self.inventory_state = 0;
        self.selected = 0;
        self.creative.patches = alloc::vec![Vec::new(); 46];
        self.creative.op_level = 0;
        self.effects.clear();
        self.sounds.clear();
        self.voice_in.clear();
        self.health = 20.0;
        self.food = 20;
        self.xp_progress = 0.0;
        self.xp_level = 0;
        self.armor = 0.0;
        self.max_health = 20.0;
        self.air = 300;
        self.move_speed = 0.1f32 as f64;
        self.mining_attrs = [0.0, 1.0, 0.2];
        self.jump_strength = 0.42f32 as f64;
        self.fly_speed = 0.05;
        self.may_fly = false;
        self.flying = false;
        self.dead = false;
        self.entity_id = 0;
        self.rain_level = 0.0;
        self.thunder_level = 0.0;
    }

    fn spawn_info(&mut self, r: &mut In) {
        self.effects.clear();
        let dim_type = r.varint().unwrap_or(0);
        let dim = r.string().unwrap_or_default();
        let _seed = r.i64();
        self.game_mode = r.varint().unwrap_or(0);
        self.world.set_dimension(&dim, dim_type);
        // A new level starts dry; the server sends its weather again.
        self.rain_level = 0.0;
        self.thunder_level = 0.0;
        self.block_entities.clear();
    }

    pub fn add_chat(&mut self, line: String) {
        crate::say!("[chat] {}", line);
        self.talk.push(crate::text::Rich::from_legacy(&line), crate::talk::Tag::SystemSinglePlayer, None);
        self.chat.push((line, crate::sys::now_ms()));
        if self.chat.len() > 100 {
            self.chat.remove(0);
        }
    }

    pub fn send_position(&mut self) -> Result<(), String> {
        let p = &self.player;
        let mut o = Out::new();
        o.f64(p.x).f64(p.y).f64(p.z).f32(p.yaw).f32(p.pitch).u8(p.on_ground as u8);
        self.conn.send(play::S_MOVE_PLAYER_POS_ROT, &o)
    }

    /// The 20 Hz bookkeeping: position, tick end, and "loaded" once the world is around us.
    pub fn tick(&mut self) -> Result<(), String> {
        self.talk_tick()?;
        self.day_time += self.clock_rate as i64;
        self.game_time += 1;
        crate::skins::tick(&self.entities);
        for e in self.entities.values_mut() {
            e.tick();
        }
        if !self.positioned {
            return Ok(());
        }
        if !self.loaded_sent && self.world.loaded(crate::math::ifloor(self.player.x), crate::math::ifloor(self.player.z)) {
            self.conn.send(play::S_PLAYER_LOADED, &Out::new())?;
            self.loaded_sent = true;
        }
        if !crate::riding::send(self)? {
            self.send_position()?;
        }
        self.conn.send(play::S_CLIENT_TICK_END, &Out::new())
    }

    pub fn send_input(&mut self, flags: u8) -> Result<(), String> {
        let mut o = Out::new();
        o.u8(flags);
        self.conn.send(play::S_PLAYER_INPUT, &o)
    }

    pub fn respawn(&mut self) -> Result<(), String> {
        let mut o = Out::new();
        o.varint(0);
        self.conn.send(play::S_CLIENT_COMMAND, &o)
    }

    /// A plugin message to the server (ServerboundCustomPayloadPacket: the channel, then the bytes).
    pub fn send_payload(&mut self, channel: &str, data: &[u8]) -> Result<(), String> {
        let mut o = Out::new();
        o.string(channel).bytes(data);
        self.conn.send(play::S_CUSTOM_PAYLOAD, &o)
    }

    /// A line from the chat box: a command (signed if it has message arguments) or chat.
    pub fn say(&mut self, text: &str) -> Result<(), String> {
        self.send_line(text)
    }

    fn pos_long(x: i32, y: i32, z: i32) -> i64 {
        ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
    }

    /// action 0 start digging, 2 abort, 3 finish (26.3 numbering).
    pub fn dig(&mut self, action: i32, x: i32, y: i32, z: i32, face: u8) -> Result<(), String> {
        self.sequence += 1;
        let mut o = Out::new();
        o.varint(action).i64(Self::pos_long(x, y, z)).u8(face).varint(self.sequence);
        self.conn.send(play::S_PLAYER_ACTION, &o)
    }

    pub fn place(&mut self, x: i32, y: i32, z: i32, face: i32, cursor: [f32; 3], inside: bool) -> Result<(), String> {
        self.place_with(0, x, y, z, face, cursor, inside)
    }

    /// ServerboundUseItemOn for `hand` (0 main, 1 off).
    #[allow(clippy::too_many_arguments)]
    pub fn place_with(&mut self, hand: i32, x: i32, y: i32, z: i32, face: i32, cursor: [f32; 3], inside: bool) -> Result<(), String> {
        self.sequence += 1;
        const OFF: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
        let o = OFF[face.clamp(0, 5) as usize];
        self.placing = Some(([x + o[0], y + o[1], z + o[2]], crate::sys::now_ms()));
        let mut o = Out::new();
        o.varint(hand).i64(Self::pos_long(x, y, z)).varint(face).f32(cursor[0]).f32(cursor[1]).f32(cursor[2]).bool(inside).bool(false).varint(self.sequence);
        self.conn.send(play::S_USE_ITEM_ON, &o)
    }

    /// A click in a container window (`ClickType`: 0 pickup, 1 quick move, 2 swap, 4 throw,
    /// 6 pickup all). No prediction is sent: the server answers with every slot that changed.
    pub fn click(&mut self, container: i32, slot: i16, button: i8, kind: i32) -> Result<(), String> {
        let state = if container == 0 {
            self.inventory_state
        } else {
            self.container.as_ref().map_or(0, |c| c.state)
        };
        let mut o = Out::new();
        o.varint(container).varint(state);
        o.b.extend_from_slice(&slot.to_be_bytes());
        o.u8(button as u8).varint(kind).varint(0).bool(false);
        self.conn.send(play::S_CONTAINER_CLICK, &o)
    }

    pub fn close_container(&mut self, id: i32) -> Result<(), String> {
        let mut o = Out::new();
        o.varint(id);
        if self.container.as_ref().map_or(false, |c| c.id == id) {
            self.container = None;
        }
        self.conn.send(play::S_CONTAINER_CLOSE, &o)
    }

    /// PlayerCommand: 1 start sprinting, 2 stop sprinting.
    pub fn command(&mut self, action: i32) -> Result<(), String> {
        let mut o = Out::new();
        o.varint(self.entity_id).varint(action).varint(0);
        self.conn.send(play::S_PLAYER_COMMAND, &o)
    }

    pub fn set_flying(&mut self, on: bool) -> Result<(), String> {
        self.flying = on;
        let mut o = Out::new();
        o.u8(if on { 2 } else { 0 });
        self.conn.send(play::S_PLAYER_ABILITIES, &o)
    }

    /// Attack an entity (26.3's own packet), with the swing.
    pub fn attack(&mut self, id: i32) -> Result<(), String> {
        let mut o = Out::new();
        o.varint(id);
        self.conn.send(play::S_ATTACK, &o)?;
        self.punch()
    }

    /// Use an entity (right click): where on it, as Vec3.LP_STREAM_CODEC, and whether sneaking.
    pub fn interact(&mut self, id: i32, at: [f64; 3], secondary: bool) -> Result<(), String> {
        let mut o = Out::new();
        o.varint(id).varint(0);
        lp_vec3_write(&mut o, at);
        o.bool(secondary);
        self.conn.send(play::S_INTERACT, &o)
    }

    pub fn punch(&mut self) -> Result<(), String> {
        self.swung = true;
        self.conn.send(play::S_PUNCH, &Out::new())
    }

    pub fn use_item(&mut self) -> Result<(), String> {
        self.use_item_with(0)
    }

    /// ServerboundUseItem for `hand` (0 main, 1 off).
    pub fn use_item_with(&mut self, hand: i32) -> Result<(), String> {
        self.sequence += 1;
        let mut o = Out::new();
        o.varint(hand).varint(self.sequence).f32(self.player.yaw).f32(self.player.pitch);
        self.conn.send(play::S_USE_ITEM, &o)
    }

    pub fn held(&self) -> Option<(i32, i32)> {
        self.inventory[36 + self.selected as usize]
    }

    /// The item in a hand (0 main, 1 off: inventory slot 45).
    pub fn hand_item(&self, hand: i32) -> Option<(i32, i32)> {
        if hand == 1 {
            self.inventory[45]
        } else {
            self.held()
        }
    }

    pub fn select_slot(&mut self, slot: i16) -> Result<(), String> {
        self.selected = slot as i32;
        let mut o = Out::new();
        o.b.extend_from_slice(&slot.to_be_bytes());
        self.conn.send(play::S_SET_CARRIED_ITEM, &o)
    }
}

/// The configuration phase, until the server finishes it. Counts the biomes it sends.
#[allow(clippy::too_many_arguments)]
fn configure(
    c: &mut Conn,
    view: i8,
    biomes: &mut Vec<String>,
    dims: &mut Vec<(String, i32, i32)>,
    regs: &mut Vec<(String, Vec<String>)>,
    variants: &mut crate::mobs::Variants,
    packs: &mut crate::packs::Packs,
    ui: &mut dyn crate::packs::Host,
    mut world: Option<&mut World>,
    blocks: &mut Blocks,
    common: &mut crate::serverui::Common,
) -> Result<(), String> {
    c.send(config::S_CLIENT_INFORMATION, &crate::options::client_information(view))?;
    let mut heard = crate::sys::now_ms();
    loop {
        // The server waits on the resource packs (ServerResourcePackConfigurationTask): their
        // prompt, download and reload run between packets.
        packs.step(c, config::S_RESOURCE_PACK, ui, world.as_deref_mut(), blocks)?;
        let Some(p) = c.recv(if packs.busy() { 15 } else { 200 })? else {
            if crate::sys::now_ms() > heard + 30000 && !packs.busy() {
                return Err(String::from("the server stopped answering"));
            }
            continue;
        };
        heard = crate::sys::now_ms();
        let mut r = p.reader();
        match p.id {
            config::C_DISCONNECT => {
                return Err(format!("disconnected while joining: {}", nbt::text(&mut r)));
            }
            config::C_KEEP_ALIVE => {
                let mut o = Out::new();
                o.i64(r.i64().unwrap_or(0));
                c.send(config::S_KEEP_ALIVE, &o)?;
            }
            config::C_PING => {
                let mut o = Out::new();
                o.i32(r.i32().unwrap_or(0));
                c.send(config::S_PONG, &o)?;
            }
            config::C_SELECT_KNOWN_PACKS => {
                // We know no packs: the server sends every registry entry in full.
                let mut o = Out::new();
                o.varint(0);
                c.send(config::S_SELECT_KNOWN_PACKS, &o)?;
            }
            config::C_REGISTRY_DATA => {
                crate::talk::registry(&p);
                let id = r.string().unwrap_or_default();
                let n = r.varint().unwrap_or(0).max(0) as usize;
                if crate::creative::REGISTRIES.contains(&id.as_str()) {
                    // Entry names in the server's order: creative stacks' components name them.
                    let mut names = Vec::new();
                    for _ in 0..n {
                        let Some(name) = r.string() else { break };
                        names.push(name);
                        if r.bool() == Some(true) && nbt::skip_root(&mut r).is_none() {
                            break;
                        }
                    }
                    regs.retain(|(k, _)| *k != id);
                    regs.push((id.clone(), names));
                    let mut r2 = p.reader();
                    let _ = r2.string();
                    crate::mobs::registry_data(&id, &mut r2, variants);
                } else if id == "minecraft:dimension_type" {
                    // Each dimension type's min_y and height, in the server's order (a type's id).
                    dims.clear();
                    for _ in 0..n {
                        let Some(name) = r.string() else { break };
                        let (mut min_y, mut height) = (-64, 384);
                        if r.bool() == Some(true) {
                            match nbt::dimension_height(&mut r) {
                                Some((a, b)) => (min_y, height) = (a, b),
                                None => {}
                            }
                        }
                        dims.push((name, min_y, height));
                    }
                } else if id == "minecraft:worldgen/biome" {
                    // The biome ids in the order the server numbers them.
                    biomes.clear();
                    for _ in 0..n {
                        let Some(name) = r.string() else { break };
                        biomes.push(name);
                        if r.bool() == Some(true) && nbt::skip_root(&mut r).is_none() {
                            break;
                        }
                    }
                } else {
                    let mut r2 = p.reader();
                    let _ = r2.string();
                    crate::mobs::registry_data(&id, &mut r2, variants);
                }
            }
            config::C_RESOURCE_PACK_PUSH => packs.on_push(&mut r),
            config::C_RESOURCE_PACK_POP => packs.on_pop(&mut r),
            config::C_CODE_OF_CONDUCT => {
                let text = r.string().unwrap_or_default();
                crate::say!("server code of conduct:\n{}", text);
                c.send(config::S_ACCEPT_CODE_OF_CONDUCT, &Out::new())?;
            }
            config::C_FINISH => {
                c.send(config::S_FINISH, &Out::new())?;
                return Ok(());
            }
            _ => {
                crate::serverui::config_packet(c, &p, common)?;
            }
        }
    }
}

/// Vec3.LP_STREAM_CODEC: three 15-bit fractions of a shared scale in 6 bytes (more for a
/// scale above 3).
fn lp_vec3_write(o: &mut Out, v: [f64; 3]) {
    let length = v[0].abs().max(v[1].abs()).max(v[2].abs());
    if length < 3.051_944_088_384_301e-5 {
        o.u8(0);
        return;
    }
    let scale = {
        let f = crate::math::floor(length);
        (if f < length { f + 1.0 } else { f }) as u64
    };
    let partial = scale & 3 != scale;
    let markers = if partial { scale & 3 | 4 } else { scale };
    let s = scale as f64;
    let pack = |x: f64| crate::math::floor((x / s * 0.5 + 0.5) * 32766.0 + 0.5) as u64;
    let buffer = markers | pack(v[0]) << 3 | pack(v[1]) << 18 | pack(v[2]) << 33;
    o.u8(buffer as u8).u8((buffer >> 8) as u8).i32((buffer >> 16) as i32);
    if partial {
        o.varint((scale >> 2) as i32);
    }
}

pub(crate) fn lp_vec3_out(o: &mut Out, v: [f64; 3]) {
    lp_vec3_write(o, v)
}

/// Skips a Vec3.LP_STREAM_CODEC velocity.
fn lp_vec3(r: &mut In) {
    let lowest = r.u8().unwrap_or(0);
    if lowest == 0 {
        return;
    }
    let _middle = r.u8();
    let _high = r.i32();
    if lowest & 4 != 0 {
        let _scale = r.varint();
    }
}

/// Entity data values: bytes and VarInts kept by index, an item stack taken (a dropped
/// item's DATA_ITEM). Stops at a value it cannot read past.
pub(crate) fn entity_data(r: &mut In, e: &mut crate::entities::Entity) -> Option<()> {
    loop {
        let index = r.u8()?;
        if index == 0xFF {
            return Some(());
        }
        let kind = r.varint()?;
        match kind {
            0 | 8 => {
                let b = r.u8()?;
                e.data.insert(index, b as i64);
            }
            1 | 12 | 14 | 15 | 19 | 20..=32 | 34..=38 | 42 | 43 => {
                let v = r.varint()?;
                e.data.insert(index, v as i64);
                // The synced variant registries' holders (cat, cow, wolf, frog, pig, chicken,
                // zombie nautilus): not their sound variants.
                if matches!(kind, 21 | 23 | 25 | 27 | 28 | 30 | 32) {
                    e.variant = v;
                }
            }
            2 => {
                let v = r.varlong()?;
                e.data.insert(index, v);
            }
            3 => {
                let v = f32::from_bits(r.i32()? as u32);
                // Display.DATA_VIEW_RANGE_ID (display.rs reads it back as bits).
                if index == 17 {
                    e.data.insert(index, v.to_bits() as i64);
                }
                // LivingEntity.DATA_HEALTH_ID.
                if index == 9 && e.item.is_none() && !crate::entities::nonliving_kind(e.kind) {
                    e.health = Some(v);
                }
                e.floats.insert(index, v);
            }
            4 => {
                r.string()?;
            }
            5 => {
                // Display.TextDisplay.DATA_TEXT_ID: kept, styled (display.rs).
                let start = r.pos;
                nbt::skip_root(r)?;
                if index == 23 {
                    e.disp_text = Some(crate::text::read(&mut crate::wire::In::new(&r.b[start..r.pos])));
                }
            }
            6 => {
                // Entity.DATA_CUSTOM_NAME.
                let name = if r.bool()? { Some(nbt::text(r)) } else { None };
                if index == 2 {
                    e.custom_name = name;
                }
            }
            7 => {
                let start = r.pos;
                let (item, ok, patch) = crate::creative::read_stack(r)?;
                e.item = item;
                e.item_patch = patch;
                // A framed map: its id, for the frame to draw it.
                match crate::maps::map_id_in_stack(&r.b[start..r.pos]) {
                    Some(m) if crate::maps::is_frame(e.kind) => {
                        e.data.insert(crate::maps::FRAMED_MAP, m as i64);
                    }
                    _ => {
                        e.data.remove(&crate::maps::FRAMED_MAP);
                    }
                }
                if !ok {
                    return None;
                }
            }
            9 => {
                r.take(12)?;
            }
            10 => {
                r.take(8)?;
            }
            11 => {
                if r.bool()? {
                    r.take(8)?;
                }
            }
            13 => {
                if r.bool()? {
                    r.take(16)?;
                }
            }
            18 => {
                // VillagerData: type, profession, level.
                e.villager = [r.varint()?, r.varint()?, r.varint()?];
            }
            // A particle, a list of them (LivingEntity.DATA_EFFECT_PARTICLES).
            16 => crate::mobs::skip_particle(r)?,
            17 => {
                for _ in 0..r.varint()?.max(0) {
                    crate::mobs::skip_particle(r)?;
                }
            }
            33 => {
                if r.bool()? {
                    r.string()?;
                    r.take(8)?;
                }
            }
            // Vector3f and Quaternionf (a display entity's translation, scale and rotations).
            39 => {
                let v = [f32::from_bits(r.i32()? as u32), f32::from_bits(r.i32()? as u32), f32::from_bits(r.i32()? as u32), 0.0];
                e.fvals.insert(index, v);
            }
            40 => {
                let v = [f32::from_bits(r.i32()? as u32), f32::from_bits(r.i32()? as u32), f32::from_bits(r.i32()? as u32), f32::from_bits(r.i32()? as u32)];
                e.fvals.insert(index, v);
            }
            _ => {
                // Once per kind, in the log: an entity data type this client does not know stops that packet's values.
                static SEEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                let bit = 1u64 << (kind as u32 & 63);
                if SEEN.fetch_or(bit, core::sync::atomic::Ordering::Relaxed) & bit == 0 {
                    crate::warn!("entity data: unknown value type {} at index {} of an entity of type {}", kind, index, e.kind);
                }
                return None;
            }
        }
    }
}

fn opt(r: &mut In, f: fn(&mut In) -> Option<()>) -> Option<()> {
    if r.bool()? {
        f(r)?;
    }
    Some(())
}

fn list(r: &mut In, f: fn(&mut In) -> Option<()>) -> Option<()> {
    let n = r.varint()?;
    if n < 0 || n as usize > r.left() {
        return None;
    }
    for _ in 0..n {
        f(r)?;
    }
    Some(())
}

/// MobEffectInstance.Details: amplifier, duration, ambient, particles, icon, then the hidden
/// effect's details (nested at most 16 deep here).
fn effect_details(r: &mut In, depth: u32) -> Option<()> {
    if depth > 16 {
        return None;
    }
    r.varint()?;
    r.varint()?;
    r.take(3)?;
    if r.bool()? {
        effect_details(r, depth + 1)?;
    }
    Some(())
}

fn ident(r: &mut In) -> Option<()> {
    r.string().map(drop)
}

/// ByteBufCodecs.holderSet: 0 and a tag, or the size plus one and the ids.
fn holder_set(r: &mut In) -> Option<()> {
    match r.varint()? {
        0 => ident(r),
        n if n > 0 => {
            for _ in 0..n - 1 {
                r.varint()?;
            }
            Some(())
        }
        _ => None,
    }
}

/// SoundEvent: a registry id plus one, or 0 and an inline location with an optional range.
fn sound(r: &mut In) -> Option<()> {
    if r.varint()? == 0 {
        ident(r)?;
        opt(r, |r| r.take(4).map(drop))?;
    }
    Some(())
}

/// Reads past one data component of `kind` exactly as its 26.3 stream codec writes it
/// (DataComponents' registry order; horizon-protocol's item/skip.rs). `None` for the ones
/// not handled here.
pub(crate) fn skip_component(kind: i32, r: &mut In) -> Option<()> {
    match kind {
        // custom_data, custom_name, item_name, bucket_entity_data, map_decorations: NBT
        0 | 6 | 9 | 61 | 49 => nbt::skip_root(r),
        // writable_book_content: Filterable pages; written_book_content: title, author,
        // generation, Filterable component pages, resolved
        56 => list(r, |r| {
            r.string()?;
            opt(r, |r| r.string().map(drop))
        }),
        57 => {
            r.string()?;
            opt(r, |r| r.string().map(drop))?;
            r.string()?;
            r.varint()?;
            list(r, |r| {
                nbt::skip_root(r)?;
                opt(r, nbt::skip_root)
            })?;
            r.bool().map(drop)
        }
        // max_stack_size, max_damage, damage, rarity, repair_cost, map ids, and the VarInt ones
        1 | 2 | 3 | 12 | 19 | 48 | 50 | 65 | 8 | 31 | 42 | 43 | 44 | 46 | 75 | 88..=108 | 110..=117 | 121 => r.varint().map(drop),
        // unbreakable, creative_slot_lock, glider, waxed: nothing
        4 | 20 | 34 | 120 => Some(()),
        5 => {
            r.bool()?;
            r.bool()?;
            r.take(4).map(drop)
        }
        7 | 54 => r.take(4).map(drop),
        10 | 35 | 73 => ident(r),
        11 => list(r, nbt::skip_root),
        13 | 45 => list(r, |r| {
            r.varint()?;
            r.varint().map(drop)
        }),
        // custom_model_data: floats, flags, strings, colours
        17 => {
            list(r, |r| r.take(4).map(drop))?;
            list(r, |r| r.u8().map(drop))?;
            list(r, ident)?;
            list(r, |r| r.take(4).map(drop))
        }
        // tooltip_display: hide everything, the hidden component types
        18 => {
            r.bool()?;
            list(r, |r| r.varint().map(drop))
        }
        21 => r.u8().map(drop),
        23 => {
            r.varint()?;
            r.take(4)?;
            r.u8().map(drop)
        }
        26 => {
            r.take(4)?;
            opt(r, ident)
        }
        27 | 33 | 67 => holder_set(r),
        28 => {
            list(r, |r| {
                holder_set(r)?;
                opt(r, |r| r.take(4).map(drop))?;
                opt(r, |r| r.u8().map(drop))
            })?;
            r.take(4)?;
            r.varint()?;
            r.u8().map(drop)
        }
        29 => {
            r.varint()?;
            r.take(4).map(drop)
        }
        32 => {
            r.varint()?;
            sound(r)?;
            opt(r, ident)?;
            opt(r, ident)?;
            opt(r, holder_set)?;
            r.take(5)?;
            sound(r)
        }
        40 | 41 => {
            r.varint()?;
            r.varint().map(drop)
        }
        47 => r.take(4).map(drop),
        // potion_contents (brewing stands, potions in chests): the potion, a custom colour,
        // custom effects, a custom name
        53 => {
            opt(r, |r| r.varint().map(drop))?;
            opt(r, |r| r.take(4).map(drop))?;
            list(r, |r| {
                r.varint()?;
                effect_details(r, 0)
            })?;
            opt(r, ident)
        }
        // banner_patterns: layers of a pattern holder (an id plus one, or 0 and its asset id
        // and translation key) and a DyeColor
        74 => list(r, |r| {
            if r.varint()? == 0 {
                r.string()?;
                r.string()?;
            }
            r.varint().map(drop)
        }),
        // block_entity_data, entity_data: the type, then a compound
        60 | 62 => {
            r.varint()?;
            nbt::skip_root(r)
        }
        // Every other component's codec (tooltip.rs).
        _ => crate::tooltip::skip(kind, r),
    }
}

/// An item stack: (item, count) or empty, and whether what follows can still be read
/// (false when the stack carries a component this client cannot read past).
fn slot(r: &mut In) -> Option<(Option<(i32, i32)>, bool)> {
    let count = r.varint()?;
    if count <= 0 {
        return Some((None, true));
    }
    let item = r.varint()?;
    let add = r.varint()?;
    let remove = r.varint()?;
    for _ in 0..add {
        let kind = r.varint()?;
        if skip_component(kind, r).is_none() {
            return Some((Some((item, count)), false));
        }
    }
    for _ in 0..remove {
        r.varint()?;
    }
    Some((Some((item, count)), true))
}

/// The text of a JSON chat component (login disconnects are JSON), roughly.
pub fn json_text(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\"text\":\"").or_else(|| rest.find("\"translate\":\"")) {
        let start = rest[i..].find(":\"").unwrap() + i + 2;
        let end = rest[start..].find('"').map(|e| e + start).unwrap_or(rest.len());
        let piece = &rest[start..end];
        out.push_str(&if rest[i..].starts_with("\"translate") { crate::lang::translate(piece) } else { String::from(piece) });
        rest = &rest[end..];
    }
    if out.is_empty() {
        String::from(s.trim_matches('"'))
    } else {
        out
    }
}
