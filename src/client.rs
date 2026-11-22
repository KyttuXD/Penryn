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
    /// The container screens' own state (menus.rs) and the maps the server sent (maps.rs).
    pub menus: crate::menus::State,
    pub maps: crate::maps::Maps,
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

fn uuid_bytes(hex: &str) -> [u8; 16] {
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
pub fn status(host: &str, port: u16) -> Result<String, String> {
    let mut c = Conn::open(host, port)?;
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

impl Game {
    /// Connects and logs in, through configuration, to the start of play.
    pub fn join(host: &str, port: u16, name: &str, account: Option<&Account>, blocks: Blocks, view: i8) -> Result<Game, String> {
        let mut c = Conn::open(host, port)?;
        handshake(&mut c, host, port, 2)?;
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
                login::C_COOKIE_REQUEST => {
                    let key = r.string().unwrap_or_default();
                    let mut o = Out::new();
                    o.string(&key).bool(false);
                    c.send(login::S_COOKIE_RESPONSE, &o)?;
                }
                login::C_FINISHED => {
                    let u = r.uuid().unwrap_or([0; 16]);
                    final_uuid = u;
                    let n = r.string().unwrap_or_default();
                    crate::say!("logged in as {} ({})", n, uuid_text(&u));
                    c.send(login::S_ACKNOWLEDGED, &Out::new())?;
                    break;
                }
                other => return Err(format!("unexpected login packet {}", other)),
            }
        }
        // ---- configuration
        let mut biomes: Vec<String> = Vec::new();
        let mut creative = crate::creative::Sync::new();
        let mut variants = crate::mobs::Variants::default();
        configure(&mut c, view, &mut biomes, &mut creative.registries, &mut variants)?;
        let block_bits = blocks.global_bits();
        let mut world = World::new(block_bits, crate::blocks::ceil_log2(biomes.len().max(1)));
        world.set_biomes(&biomes, &blocks);
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
            game_time: 0,
            sounds: Vec::new(),
            placing: None,
            move_speed: 0.1f32 as f64,
            jump_strength: 0.42f32 as f64,
            fly_speed: 0.05,
            rain_level: 0.0,
            thunder_level: 0.0,
            block_entities: crate::signs::BlockEntities::new(),
            effects: crate::effects::Effects::new(),
            particle_events: Vec::new(),
            creative,
            variants,
            player_names: alloc::collections::BTreeMap::new(),
            menus: crate::menus::State::new(),
            maps: crate::maps::Maps::new(),
        })
    }

    /// Handles every packet that has arrived, without waiting.
    pub fn pump(&mut self) -> Result<(), String> {
        let mut n = 0;
        while let Some(p) = self.conn.recv(0)? {
            self.handle(p)?;
            n += 1;
            if n > 2000 {
                break; // let the frame go on; the rest next time
            }
        }
        Ok(())
    }

    /// Waits up to `ms` for packets, handling them.
    pub fn wait(&mut self, ms: i32) -> Result<(), String> {
        if let Some(p) = self.conn.recv(ms)? {
            self.handle(p)?;
        }
        self.pump()
    }

    fn handle(&mut self, p: Packet) -> Result<(), String> {
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
                crate::say!("joined: entity {}, game mode {}", self.entity_id, self.game_mode);
            }
            play::C_RESPAWN => {
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
                            _ => {}
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
            play::C_SYSTEM_CHAT => {
                let t = nbt::text(&mut r);
                let overlay = r.bool().unwrap_or(false);
                if !overlay && !t.is_empty() {
                    self.add_chat(t);
                }
            }
            play::C_DISGUISED_CHAT => {
                let msg = nbt::text(&mut r);
                let _ty = r.varint();
                let who = nbt::text(&mut r);
                self.add_chat(format!("<{}> {}", who, msg));
            }
            play::C_PLAYER_CHAT => {
                if let Some(line) = player_chat(&mut r) {
                    self.add_chat(line);
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
                // 24-28: the player's permission level (0-4).
                let (id, ev) = (r.i32().unwrap_or(0), r.u8().unwrap_or(0));
                if id == self.entity_id && (24..=28).contains(&ev) {
                    self.creative.op_level = ev as i32 - 24;
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
                    }
                }
            }
            play::C_MOVE_ENTITY_POS | play::C_MOVE_ENTITY_POS_ROT | play::C_MOVE_ENTITY_ROT => {
                let id = r.varint().unwrap_or(0);
                let Some(e) = self.entities.get_mut(&id) else { return Ok(()) };
                if p.id == play::C_MOVE_ENTITY_ROT {
                    e.on_ground = r.bool().unwrap_or(false);
                    e.yaw = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                    e.pitch = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                } else {
                    // 26.3: on-ground in bit 0, the number of timed steps above it.
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
                    for d in deltas {
                        for a in 0..3 {
                            if d[a] != 0 {
                                // VecDeltaCodec: quantize the base to 1/4096, then add.
                                let q = crate::math::floor(e.pos[a] * 4096.0 + 0.5) as i64 + d[a] as i64;
                                e.pos[a] = q as f64 / 4096.0;
                            }
                        }
                    }
                    if p.id == play::C_MOVE_ENTITY_POS_ROT {
                        e.yaw = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                        e.pitch = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                    }
                }
            }
            play::C_TELEPORT_ENTITY => {
                let id = r.varint().unwrap_or(0);
                let pos = [r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0), r.f64().unwrap_or(0.0)];
                let _d = (r.f64(), r.f64(), r.f64());
                let (yaw, pitch) = (r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0));
                let rel = r.i32().unwrap_or(0);
                if let Some(e) = self.entities.get_mut(&id) {
                    for a in 0..3 {
                        e.pos[a] = if rel & (1 << a) != 0 { e.pos[a] + pos[a] } else { pos[a] };
                    }
                    e.yaw = if rel & 8 != 0 { e.yaw + yaw } else { yaw };
                    e.pitch = if rel & 16 != 0 { e.pitch + pitch } else { pitch };
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
                if let (Some(e), Some(p)) = (self.entities.get_mut(&id), pos) {
                    e.pos = p;
                    e.yaw = yaw;
                    e.pitch = pitch;
                }
            }
            play::C_ROTATE_HEAD => {
                let id = r.varint().unwrap_or(0);
                let h = r.u8().unwrap_or(0) as i8 as f32 * 360.0 / 256.0;
                if let Some(e) = self.entities.get_mut(&id) {
                    e.head_yaw = h;
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
            play::C_PLAYER_INFO_UPDATE => crate::skins::player_info(&mut r, self.uuid),
            play::C_PLAYER_INFO_REMOVE => crate::skins::player_remove(&mut r),
            play::C_SET_ENTITY_DATA => {
                let id = r.varint().unwrap_or(0);
                if let Some(e) = self.entities.get_mut(&id) {
                    entity_data(&mut r, e);
                    e.version = e.version.wrapping_add(1);
                }
            }
            crate::mobs::C_SET_EQUIPMENT => {
                let id = r.varint().unwrap_or(0);
                if let Some(e) = self.entities.get_mut(&id) {
                    crate::mobs::equipment(&mut r, e, slot);
                }
            }
            crate::mobs::C_PLAYER_INFO_UPDATE => {
                crate::mobs::player_info(&mut r, &mut self.player_names);
            }
            crate::mobs::C_PLAYER_INFO_REMOVE => {
                crate::mobs::player_info_remove(&mut r, &mut self.player_names);
            }
            crate::mobs::C_ENTITY_EVENT => {
                // 3: LivingEntity.handleEntityEvent's death (health to 0, it falls over).
                let id = r.i32().unwrap_or(0);
                if r.u8() == Some(3) {
                    if let Some(e) = self.entities.get_mut(&id) {
                        if e.item.is_none() {
                            e.health = Some(0.0);
                        }
                    }
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
            play::C_ENTITY_EVENT => {
                let (id, ev) = (r.i32().unwrap_or(0), r.u8().unwrap_or(0));
                if let Some(e) = self.entities.get(&id) {
                    if self.particle_events.len() < 512 {
                        self.particle_events.push(crate::particles::Effect::Entity(ev, e.pos, e.kind));
                    }
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
                let mut b: Vec<String> = Vec::new();
                configure(&mut self.conn, 8, &mut b, &mut self.creative.registries, &mut self.variants)?;
                if !b.is_empty() {
                    self.world.biome_bits = crate::blocks::ceil_log2(b.len());
                    self.world.set_biomes(&b, &self.blocks);
                }
            }
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

    fn spawn_info(&mut self, r: &mut In) {
        self.effects.clear();
        let _dim_type = r.varint();
        let dim = r.string().unwrap_or_default();
        let _seed = r.i64();
        self.game_mode = r.varint().unwrap_or(0);
        self.world.set_dimension(&dim);
        // A new level starts dry; the server sends its weather again.
        self.rain_level = 0.0;
        self.thunder_level = 0.0;
        self.block_entities.clear();
    }

    pub fn add_chat(&mut self, line: String) {
        crate::say!("[chat] {}", line);
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
        self.send_position()?;
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

    pub fn say(&mut self, text: &str) -> Result<(), String> {
        let mut o = Out::new();
        if let Some(cmd) = text.strip_prefix('/') {
            o.string(cmd);
            return self.conn.send(play::S_CHAT_COMMAND, &o);
        }
        let ts = (crate::sys::now() * 1000.0) as i64;
        o.string(text).i64(ts).i64(ts ^ 0x5DEECE66D).bool(false).varint(0).bytes(&[0, 0, 0]).u8(0);
        self.conn.send(play::S_CHAT, &o)
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
        self.sequence += 1;
        const OFF: [[i32; 3]; 6] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
        let o = OFF[face.clamp(0, 5) as usize];
        self.placing = Some(([x + o[0], y + o[1], z + o[2]], crate::sys::now_ms()));
        let mut o = Out::new();
        o.varint(0).i64(Self::pos_long(x, y, z)).varint(face).f32(cursor[0]).f32(cursor[1]).f32(cursor[2]).bool(inside).bool(false).varint(self.sequence);
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
        self.sequence += 1;
        let mut o = Out::new();
        o.varint(0).varint(self.sequence).f32(self.player.yaw).f32(self.player.pitch);
        self.conn.send(play::S_USE_ITEM, &o)
    }

    pub fn held(&self) -> Option<(i32, i32)> {
        self.inventory[36 + self.selected as usize]
    }

    pub fn select_slot(&mut self, slot: i16) -> Result<(), String> {
        self.selected = slot as i32;
        let mut o = Out::new();
        o.b.extend_from_slice(&slot.to_be_bytes());
        self.conn.send(play::S_SET_CARRIED_ITEM, &o)
    }
}

/// The configuration phase, until the server finishes it. Counts the biomes it sends.
fn configure(
    c: &mut Conn,
    view: i8,
    biomes: &mut Vec<String>,
    regs: &mut Vec<(String, Vec<String>)>,
    variants: &mut crate::mobs::Variants,
) -> Result<(), String> {
    c.send(config::S_CLIENT_INFORMATION, &crate::options::client_information(view))?;
    loop {
        let p = next(c)?;
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
            config::C_COOKIE_REQUEST => {
                let mut o = Out::new();
                o.string(&r.string().unwrap_or_default()).bool(false);
                c.send(config::S_COOKIE_RESPONSE, &o)?;
            }
            config::C_SELECT_KNOWN_PACKS => {
                // We know no packs: the server sends every registry entry in full.
                let mut o = Out::new();
                o.varint(0);
                c.send(config::S_SELECT_KNOWN_PACKS, &o)?;
            }
            config::C_REGISTRY_DATA => {
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
            config::C_RESOURCE_PACK_PUSH => {
                // Accept (we do not download it): 3 accepted, then 0 loaded.
                let u = r.uuid().unwrap_or([0; 16]);
                for status in [3, 0] {
                    let mut o = Out::new();
                    o.bytes(&u).varint(status);
                    c.send(config::S_RESOURCE_PACK, &o)?;
                }
            }
            config::C_CODE_OF_CONDUCT => {
                let text = r.string().unwrap_or_default();
                crate::say!("server code of conduct:\n{}", text);
                c.send(config::S_ACCEPT_CODE_OF_CONDUCT, &Out::new())?;
            }
            config::C_FINISH => {
                c.send(config::S_FINISH, &Out::new())?;
                return Ok(());
            }
            _ => {}
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
fn entity_data(r: &mut In, e: &mut crate::entities::Entity) -> Option<()> {
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
                // LivingEntity.DATA_HEALTH_ID.
                if index == 9 && e.item.is_none() {
                    e.health = Some(v);
                }
            }
            4 => {
                r.string()?;
            }
            5 => nbt::skip_root(r)?,
            6 => {
                // Entity.DATA_CUSTOM_NAME.
                let name = if r.bool()? { Some(nbt::text(r)) } else { None };
                if index == 2 {
                    e.custom_name = name;
                }
            }
            7 => {
                let start = r.pos;
                let (item, ok) = slot(r)?;
                e.item = item;
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
            39 => {
                r.take(12)?;
            }
            40 => {
                r.take(16)?;
            }
            _ => return None,
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
        _ => None,
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

/// `ClientboundPlayerChatPacket`: "<sender> message".
fn player_chat(r: &mut In) -> Option<String> {
    let _global = r.varint()?;
    let _sender = r.uuid()?;
    let _index = r.varint()?;
    if r.bool()? {
        r.take(256)?;
    }
    let msg = r.string()?;
    let _ts = r.i64()?;
    let _salt = r.i64()?;
    let n = r.varint()?;
    for _ in 0..n {
        if r.varint()? == 0 {
            r.take(256)?;
        }
    }
    let unsigned = if r.bool()? { Some(nbt::text(r)) } else { None };
    if r.varint()? == 2 {
        let n = r.varint()?;
        r.take(8 * n.max(0) as usize)?;
    }
    let ty = r.varint()?;
    if ty == 0 {
        return Some(unsigned.unwrap_or(msg));
    }
    let who = nbt::text(r);
    Some(format!("<{}> {}", who, unsigned.unwrap_or(msg)))
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
