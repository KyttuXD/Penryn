//! The Simple Voice Chat client state machine. No sockets, no threads, no clock, no audio
//! device: events go in, `Action`s come out, and the caller does the IO.
//!
//! Original code written from a protocol description (`voicechat-protocol.md`); section numbers
//! in comments refer to it.
//!
//! Time: the session only knows the time the caller gives it. `Event::Tick(now_ms)` should be
//! delivered regularly (every game tick is plenty); every other event is processed at the time
//! of the latest tick, or use `handle_at` to pass a time with any event.
//!
//! Order of actions: the returned `Vec` must be executed in order. In particular a
//! `Connect` is always emitted before the first `SendUdp` of that connection and a `Close`
//! before the `Connect` of its replacement.
//!
//! What the session does NOT do: Opus decoding/encoding, the jitter buffer, positioning of
//! `Play` packets (it needs entity positions), the microphone activation logic (PTT/VOICE).

use super::proto::{self, ClientGroup, Codec, GroupType, JoinedGroup, PlayerState, PluginPacket, Secret, UdpMessage, Uuid};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// Authenticate / ConnectionCheck are re-sent every second until acknowledged (6.4).
pub const AUTH_RETRY_MS: u64 = 1000;
/// A UI-only timeout for "no `secret` after `request_secret`" (6.3 recommends ~10 s).
pub const SECRET_WAIT_MS: u64 = 10_000;
/// Same period after which a still-unacknowledged handshake is reported to the UI (6.8: the Java
/// client's RECONNECT_DELAY). Retries continue regardless: there is no retry cap (6.4).
pub const HANDSHAKE_WARN_MS: u64 = 10_000;
/// Connection timeout = keep-alive period * 10 (3.2, 6.7).
pub const TIMEOUT_FACTOR: u64 = 10;
/// A channel without packets for this long is forgotten (7.1).
pub const CHANNEL_IDLE_MS: u64 = 30_000;
/// More lost packets than this (`output_buffer_size`, default 5) = a new utterance, no
/// compensation, decoder reset (7.1).
pub const MAX_COMPENSATE: i128 = 5;
/// Not in the Java client (trap 6 suggests it): a packet whose sequence number is this far
/// BELOW the last one is taken as a restarted sender (a reconnect without a stop packet)
/// instead of a stale one. 250 packets = 5 s of speech.
pub const RESTART_JUMP: i64 = 250;

// Outgoing rate: the v21 server kicks above 100 packets/s average (leaky bucket of 500 over 5 s,
// section 9). We keep to 90/s with a burst of 400 in every compat mode, counted in
// millipackets so the refill is exact in milliseconds.
const BUCKET_CAP: i64 = 400_000;
const BUCKET_REFILL_PER_MS: i64 = 90;
const PACKET_COST: i64 = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Voice is off (default) or the world was left.
    Disabled,
    /// `request_secret` sent, waiting for the server.
    Requesting,
    /// UDP handshake running.
    Connecting,
    Connected,
    Failed(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Status::Disabled => String::from("Voice chat: off"),
            Status::Requesting => String::from("Voice chat: contacting server..."),
            Status::Connecting => String::from("Voice chat: connecting..."),
            Status::Connected => String::from("Voice chat: connected"),
            Status::Failed(r) => alloc::format!("Voice chat: failed ({})", r),
        }
    }
}

/// What the host needs to open the socket and set up the encoder.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectInfo {
    /// `None` = the address of the Minecraft TCP connection.
    pub host: Option<String>,
    pub port: u16,
    /// Opus application for the encoder.
    pub codec: Codec,
    /// Largest Opus packet to produce: min(mtu, 1275).
    pub max_payload: usize,
    pub keep_alive_ms: u32,
    pub groups_enabled: bool,
    pub voice_distance: f64,
    pub player: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayKind {
    /// 0x2: positional at the entity `sender`; hearing distance in blocks.
    Player { distance: f32, whispering: bool },
    /// 0x3: static, not positional.
    Group,
    /// 0x4: positional at fixed coordinates.
    Location { x: f64, y: f64, z: f64, distance: f32 },
}

/// One received sound packet, already filtered for staleness (7.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    /// Key of the audio channel / decoder.
    pub channel_id: Uuid,
    /// The entity whose position the sound comes from (not always == channel_id, trap 19).
    pub sender: Uuid,
    pub kind: PlayKind,
    pub sequence: i64,
    /// Opus packet. EMPTY = end of speech: flush and reset that channel, play nothing.
    pub opus: Vec<u8>,
    pub category: Option<String>,
    /// Packets lost since the previous one of this channel (to be concealed via PLC/FEC, 7.1).
    /// Already 0 when `reset` is set.
    pub lost: u32,
    /// Start of an utterance (first packet, after an end-of-speech, after a long gap or a sender
    /// restart): reset the decoder before decoding.
    pub reset: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GroupEvent {
    Added(ClientGroup),
    Removed(Uuid),
    Joined(JoinedGroup),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Send a custom payload (play state) on this channel id.
    SendPlugin { channel: String, payload: Vec<u8> },
    /// Open the UDP socket (one socket for the whole session, trap 15).
    Connect(ConnectInfo),
    /// Close the UDP socket.
    Close,
    /// Send this datagram from that socket to the server.
    SendUdp(Vec<u8>),
    Play(Play),
    /// The decoder/playback of this channel can be dropped.
    CloseChannel(Uuid),
    Status(Status),
    /// A player state changed (or arrived).
    PlayerState(PlayerState),
    Group(GroupEvent),
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The user switches voice on (world loaded). First time: `minecraft:register` +
    /// `request_secret` (6.2); with a live session only `client_state(disabled=false)` (trap 13).
    Enable,
    /// The user switches voice off. The UDP session is kept and keep-alives are still answered
    /// (6.9); `client_state(disabled=true)`, a stop packet if speaking, sound is dropped.
    Disable,
    /// No working speaker/microphone: reported as `disabled=true` (3.3). Default true.
    AudioAvailable(bool),
    /// Microphone muted by the user: frames are dropped, a stop packet is sent (8.4).
    Muted(bool),
    /// Raw payload of `voicechat:secret`.
    Secret(Vec<u8>),
    /// Any other clientbound `voicechat:*` payload (states, groups, categories, secret).
    Plugin { channel: String, payload: Vec<u8> },
    /// Already decoded player states (e.g. from elsewhere).
    PlayerStates(Vec<PlayerState>),
    /// The play connection ended (left the world, kicked): everything is dropped.
    Disconnected,
    /// Time passes.
    Tick(u64),
    /// A datagram arrived on the UDP socket.
    Udp(Vec<u8>),
    /// One 20 ms Opus frame from the encoder.
    Mic { opus: Vec<u8>, whispering: bool },
    /// The speaker stopped (PTT released etc.): sends the stop packet (8.3).
    EndOfSpeech,
    CreateGroup { name: String, password: Option<String>, kind: GroupType },
    JoinGroup { group: Uuid, password: Option<String> },
    LeaveGroup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing requested (or world left).
    Idle,
    /// `request_secret` sent, no secret yet (or the session timed out and the server has not sent a new secret yet).
    Requested,
    /// Secret known; sending Authenticate until AuthenticateAck (6.4/6.5).
    Authenticating,
    /// Sending ConnectionCheck until ConnectionCheckAck (6.4/6.6).
    Checking,
    Connected,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub udp_bad: u32,
    pub plugin_bad: u32,
    pub mic_dropped: u32,
    pub mic_rate_dropped: u32,
    pub sound_stale: u32,
    pub udp_sent: u32,
}

struct Chan {
    last_seq: Option<i64>,
    last_ms: u64,
}

struct Conn {
    secret: Secret,
    key: [u8; 16],
    id: Uuid,
    phase: Phase,
    nonce: [u8; 16],
    next_auth: u64,
    started: u64,
    warned: bool,
    last_keepalive: u64,
    mic_seq: i64,
    speaking: bool,
    bucket: i64,
    bucket_at: u64,
    channels: BTreeMap<Uuid, Chan>,
}

impl Conn {
    fn refill(&mut self, now: u64) {
        let dt = now.saturating_sub(self.bucket_at).min(1_000_000) as i64;
        self.bucket = (self.bucket + dt * BUCKET_REFILL_PER_MS).min(BUCKET_CAP);
        self.bucket_at = now;
    }
}

/// Everything needed to turn a message into a datagram.
struct Tx {
    compat: u32,
    iv: Box<dyn FnMut() -> [u8; 12]>,
    stats: Stats,
}

impl Tx {
    /// Seals `msg` and queues the datagram. `limited` messages (mic) are refused when the send
    /// budget is empty; others are always sent but still use budget.
    fn udp(&mut self, c: &mut Conn, msg: &UdpMessage, limited: bool, out: &mut Vec<Action>) -> bool {
        if limited && c.bucket < PACKET_COST {
            self.stats.mic_rate_dropped += 1;
            return false;
        }
        let iv = (self.iv)();
        match proto::seal_to_server(self.compat, &c.key, &c.id, &iv, msg) {
            Some(d) => {
                c.bucket = (c.bucket - PACKET_COST).max(0);
                self.stats.udp_sent += 1;
                out.push(Action::SendUdp(d));
                true
            }
            None => false,
        }
    }
    /// Stop packet: empty Opus data, `seq` = next, whispering false, only if the previous packet
    /// was real audio (8.3).
    fn stop(&mut self, c: &mut Conn, out: &mut Vec<Action>) {
        if c.speaking {
            let m = UdpMessage::Mic { opus: Vec::new(), sequence: c.mic_seq, whispering: false };
            self.udp(c, &m, false, out);
            c.mic_seq = c.mic_seq.wrapping_add(1);
            c.speaking = false;
        }
    }
}

pub struct VoiceSession {
    tx: Tx,
    now: u64,
    enabled: bool,
    muted: bool,
    audio_ok: bool,
    requested: bool,
    requested_at: u64,
    fail: Option<String>,
    status: Status,
    conn: Option<Conn>,
    sent_state: Option<(bool, bool)>,
    players: BTreeMap<Uuid, PlayerState>,
}

impl VoiceSession {
    /// `compat` is `proto::COMPAT_V20` (default, voicechat 2.6.x) or `COMPAT_V21`. `iv_source`
    /// returns 12 fresh random bytes per call (4.1).
    pub fn new(compat: u32, iv_source: Box<dyn FnMut() -> [u8; 12]>) -> VoiceSession {
        VoiceSession {
            tx: Tx { compat, iv: iv_source, stats: Stats::default() },
            now: 0,
            enabled: false,
            muted: false,
            audio_ok: true,
            requested: false,
            requested_at: 0,
            fail: None,
            status: Status::Disabled,
            conn: None,
            sent_state: None,
            players: BTreeMap::new(),
        }
    }

    pub fn compat(&self) -> u32 {
        self.tx.compat
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
    pub fn phase(&self) -> Phase {
        match &self.conn {
            Some(c) => c.phase,
            None if self.requested => Phase::Requested,
            None => Phase::Idle,
        }
    }
    pub fn is_connected(&self) -> bool {
        self.phase() == Phase::Connected
    }
    pub fn stats(&self) -> &Stats {
        &self.tx.stats
    }
    pub fn player(&self, uuid: &Uuid) -> Option<&PlayerState> {
        self.players.get(uuid)
    }
    pub fn players(&self) -> impl Iterator<Item = &PlayerState> {
        self.players.values()
    }
    /// The next sequence number a mic packet will carry (starts at 0 per connection, 8.3).
    pub fn next_mic_sequence(&self) -> i64 {
        self.conn.as_ref().map_or(0, |c| c.mic_seq)
    }
    /// True while the user wants voice and audio is available.
    pub fn is_active(&self) -> bool {
        !self.disabled()
    }

    fn disabled(&self) -> bool {
        !self.enabled || !self.audio_ok
    }

    pub fn handle(&mut self, ev: Event) -> Vec<Action> {
        let now = self.now;
        self.handle_at(now, ev)
    }

    pub fn handle_at(&mut self, now: u64, ev: Event) -> Vec<Action> {
        if now > self.now {
            self.now = now;
        }
        let mut out = Vec::new();
        match ev {
            Event::Enable => self.on_enable(&mut out),
            Event::Disable => self.on_disable(&mut out),
            Event::AudioAvailable(v) => {
                if self.audio_ok != v {
                    self.audio_ok = v;
                    self.after_flags_changed(&mut out);
                }
            }
            Event::Muted(v) => {
                if self.muted != v {
                    self.muted = v;
                    if v {
                        self.stop_speaking(&mut out);
                    }
                    self.sync_state(false, &mut out);
                }
            }
            Event::Secret(d) => self.on_secret(&d, &mut out),
            Event::Plugin { channel, payload } => self.on_plugin(&channel, &payload, &mut out),
            Event::PlayerStates(v) => {
                for p in v {
                    self.upsert_player(p, &mut out);
                }
            }
            Event::Disconnected => self.on_disconnected(&mut out),
            Event::Tick(t) => {
                if t > self.now {
                    self.now = t;
                }
                self.on_tick(&mut out)
            }
            Event::Udp(d) => self.on_udp(&d, &mut out),
            Event::Mic { opus, whispering } => self.on_mic(opus, whispering, &mut out),
            Event::EndOfSpeech => self.stop_speaking(&mut out),
            Event::CreateGroup { name, password, kind } => {
                if let Some(p) = proto::encode_create_group(self.tx.compat, &name, password.as_deref(), kind) {
                    self.send_group_packet(proto::CH_CREATE_GROUP, p, &mut out);
                }
            }
            Event::JoinGroup { group, password } => {
                if let Some(p) = proto::encode_join_group(self.tx.compat, &group, password.as_deref()) {
                    self.send_group_packet(proto::CH_SET_GROUP, p, &mut out);
                }
            }
            Event::LeaveGroup => self.send_group_packet(proto::CH_LEAVE_GROUP, proto::encode_leave_group(), &mut out),
        }
        self.update_status(&mut out);
        out
    }

    // ---------------------------------------------------------------- user level

    fn on_enable(&mut self, out: &mut Vec<Action>) {
        if self.enabled {
            return;
        }
        self.enabled = true;
        self.fail = None;
        if self.conn.is_some() {
            // The server still holds our secret: a second request_secret would be ignored
            // (3.1, trap 13), so only flip the state flag.
            self.after_flags_changed(out);
        } else if !self.requested {
            // 6.2: the world is loaded, ask for a secret. `minecraft:register` first (section 2,
            // "cheap insurance"), then request_secret; nothing else is sent before the secret
            // arrived (trap 14).
            self.requested = true;
            self.requested_at = self.now;
            out.push(Action::SendPlugin { channel: String::from(proto::CH_REGISTER), payload: proto::encode_register(self.tx.compat) });
            out.push(Action::SendPlugin {
                channel: String::from(proto::CH_REQUEST_SECRET),
                payload: proto::encode_request_secret(self.tx.compat),
            });
        }
    }

    fn on_disable(&mut self, out: &mut Vec<Action>) {
        if !self.enabled {
            return;
        }
        self.enabled = false;
        self.after_flags_changed(out);
    }

    /// Enabled/audio flag changed: stop the mic if needed, report the state.
    fn after_flags_changed(&mut self, out: &mut Vec<Action>) {
        if self.disabled() {
            self.stop_speaking(out);
        }
        self.sync_state(false, out);
    }

    fn stop_speaking(&mut self, out: &mut Vec<Action>) {
        if let Some(c) = self.conn.as_mut() {
            self.tx.stop(c, out);
        }
    }

    /// `client_state` / `update_state` (3.3). Never before a secret arrived (trap 14). When
    /// `force` is false it is only sent if the values differ from the last ones sent, which also
    /// keeps us far below the server's plugin-channel rate limit (16/s, section 2).
    fn sync_state(&mut self, force: bool, out: &mut Vec<Action>) {
        if self.conn.is_none() {
            return;
        }
        let compat = self.tx.compat;
        // v20 has no muted flag: it must not make a difference there.
        let muted = compat >= proto::COMPAT_V21 && self.muted;
        let st = (muted, self.disabled());
        if !force && self.sent_state == Some(st) {
            return;
        }
        self.sent_state = Some(st);
        out.push(Action::SendPlugin {
            channel: String::from(proto::client_state_channel(compat)),
            payload: proto::encode_client_state(compat, st.0, st.1),
        });
    }

    fn send_group_packet(&mut self, channel: &str, payload: Vec<u8>, out: &mut Vec<Action>) {
        // 3.5: only meaningful once the server gave us a secret, and only when it has groups on.
        let ok = self.conn.as_ref().map_or(false, |c| c.secret.groups_enabled);
        if ok {
            out.push(Action::SendPlugin { channel: String::from(channel), payload });
        }
    }

    // ---------------------------------------------------------------- plugin channel

    fn on_plugin(&mut self, channel: &str, payload: &[u8], out: &mut Vec<Action>) {
        let compat = self.tx.compat;
        match proto::decode_plugin(channel, payload, compat) {
            None => {
                // unknown channels are not errors; known ones that fail to parse are counted
                if channel.starts_with("voicechat:") {
                    self.tx.stats.plugin_bad += 1;
                }
            }
            Some(PluginPacket::Secret(s)) => self.start_session(s, out),
            Some(PluginPacket::States(v)) => {
                for p in v {
                    self.upsert_player(p, out);
                }
            }
            Some(PluginPacket::State(p)) => self.upsert_player(p, out),
            Some(PluginPacket::RemoveState(u)) => {
                self.players.remove(&u);
                self.close_channel(&u, out);
            }
            Some(PluginPacket::AddGroup(g)) => out.push(Action::Group(GroupEvent::Added(g))),
            Some(PluginPacket::RemoveGroup(g)) => out.push(Action::Group(GroupEvent::Removed(g))),
            Some(PluginPacket::JoinedGroup(j)) => out.push(Action::Group(GroupEvent::Joined(j))),
            Some(PluginPacket::AddCategory(_)) | Some(PluginPacket::RemoveCategory(_)) => {} // ignored (3.6)
        }
    }

    fn upsert_player(&mut self, p: PlayerState, out: &mut Vec<Action>) {
        if p.disconnected {
            // Java: `closeAudioChannel(uuid)` when a state says disconnected (section 2.1).
            self.close_channel(&p.uuid, out);
        }
        self.players.insert(p.uuid, p.clone());
        out.push(Action::PlayerState(p));
    }

    fn close_channel(&mut self, u: &Uuid, out: &mut Vec<Action>) {
        if let Some(c) = self.conn.as_mut() {
            if c.channels.remove(u).is_some() {
                out.push(Action::CloseChannel(*u));
            }
        }
    }

    fn on_secret(&mut self, data: &[u8], out: &mut Vec<Action>) {
        match proto::decode_secret(data, self.tx.compat) {
            Some(s) => self.start_session(s, out),
            None => {
                self.tx.stats.plugin_bad += 1;
                self.fail = Some(String::from("malformed secret packet"));
            }
        }
    }

    /// 6.4: (re)start the UDP session from a secret. A secret may arrive at any time (server
    /// driven reconnect, 6.8, trap 12) and supersedes the previous one entirely.
    fn start_session(&mut self, s: Secret, out: &mut Vec<Action>) {
        if !self.requested {
            return; // we never asked: not ours
        }
        self.teardown(out);
        let compat = self.tx.compat;
        let target = match proto::parse_voice_target(&s.voice_host, s.server_port) {
            Some(t) => t,
            None => {
                self.fail = Some(String::from("invalid voice chat port"));
                return;
            }
        };
        if compat >= proto::COMPAT_V21 && s.connection_id.is_none() {
            self.fail = Some(String::from("secret without connection id"));
            return;
        }
        self.fail = None;
        let mut c = Conn {
            key: s.secret,
            id: s.udp_id(),
            phase: Phase::Authenticating,
            nonce: [0; 16],
            next_auth: self.now + AUTH_RETRY_MS,
            started: self.now,
            warned: false,
            last_keepalive: self.now,
            mic_seq: 0,
            speaking: false,
            bucket: BUCKET_CAP,
            bucket_at: self.now,
            channels: BTreeMap::new(),
            secret: s,
        };
        out.push(Action::Connect(ConnectInfo {
            host: target.host,
            port: target.port,
            codec: c.secret.codec,
            max_payload: c.secret.max_opus_payload(compat),
            keep_alive_ms: c.secret.keep_alive(),
            groups_enabled: c.secret.groups_enabled,
            voice_distance: c.secret.voice_chat_distance,
            player: c.secret.player,
        }));
        Self::send_auth(&mut self.tx, &mut c, out);
        self.conn = Some(c);
        // 3.3: report our own state right after the secret is processed.
        self.sync_state(true, out);
    }

    fn send_auth(tx: &mut Tx, c: &mut Conn, out: &mut Vec<Action>) {
        // v20: Authenticate carries player UUID + secret, ConnectionCheck is empty;
        // v21: Authenticate is empty, ConnectionCheck carries the nonce from the ack (5.5).
        let m = match c.phase {
            Phase::Authenticating => UdpMessage::Authenticate { player: c.secret.player, secret: c.secret.secret },
            _ => UdpMessage::ConnectionCheck { nonce: c.nonce },
        };
        tx.udp(c, &m, false, out);
    }

    /// Drops the current UDP session (if any).
    fn teardown(&mut self, out: &mut Vec<Action>) {
        if let Some(mut c) = self.conn.take() {
            self.tx.stop(&mut c, out);
            for k in c.channels.keys() {
                out.push(Action::CloseChannel(*k));
            }
            out.push(Action::Close);
        }
        self.sent_state = None;
    }

    fn on_disconnected(&mut self, out: &mut Vec<Action>) {
        self.teardown(out);
        self.requested = false;
        self.enabled = false;
        self.fail = None;
        self.players.clear();
    }

    // ---------------------------------------------------------------- time

    fn on_tick(&mut self, out: &mut Vec<Action>) {
        let now = self.now;
        if self.conn.is_none() {
            // 6.3: UI timeout while waiting for the secret
            if self.requested && self.fail.is_none() && now.saturating_sub(self.requested_at) >= SECRET_WAIT_MS {
                self.fail = Some(String::from("no reply: server has no voice chat or an incompatible version"));
            }
            return;
        }
        let Some(mut c) = self.conn.take() else { return };
        let mut dead = false;
        match c.phase {
            Phase::Authenticating | Phase::Checking => {
                if now >= c.next_auth {
                    Self::send_auth(&mut self.tx, &mut c, out);
                    c.next_auth = now + AUTH_RETRY_MS;
                }
                if !c.warned && now.saturating_sub(c.started) >= HANDSHAKE_WARN_MS {
                    c.warned = true;
                    self.fail = Some(String::from("voice server does not answer (UDP port blocked?)"));
                }
            }
            Phase::Connected => {
                // 6.7: timeout when no keep-alive arrived for keepAlive * 10 ms.
                let limit = c.secret.keep_alive() as u64 * TIMEOUT_FACTOR;
                if now.saturating_sub(c.last_keepalive) > limit {
                    dead = true;
                } else {
                    // 7.1: a channel is killed after 30 s without packets
                    let stale: Vec<Uuid> =
                        c.channels.iter().filter(|(_, ch)| now.saturating_sub(ch.last_ms) > CHANNEL_IDLE_MS).map(|(k, _)| *k).collect();
                    for k in stale {
                        c.channels.remove(&k);
                        out.push(Action::CloseChannel(k));
                    }
                }
            }
            Phase::Idle | Phase::Requested => {}
        }
        if dead {
            // The server drops us the same way and sends a NEW secret by itself (6.8); we must
            // not send request_secret again (it would be ignored while the server holds the old
            // secret, trap 13). Wait for that secret. `c` is dropped, nothing is sent on a dead link.
            for k in c.channels.keys() {
                out.push(Action::CloseChannel(*k));
            }
            out.push(Action::Close);
            self.sent_state = None;
            self.requested_at = now;
            self.fail = Some(String::from("connection timeout, waiting for the server to reconnect"));
        } else {
            self.conn = Some(c);
        }
    }

    // ---------------------------------------------------------------- UDP

    fn on_udp(&mut self, data: &[u8], out: &mut Vec<Action>) {
        let compat = self.tx.compat;
        let now = self.now;
        let Some(c) = self.conn.as_mut() else { return };
        // Frame, tag and body must all be valid; anything else is dropped silently (4.1, 6.4).
        let Some(msg) = proto::open_from_server(compat, &c.key, data) else {
            self.tx.stats.udp_bad += 1;
            return;
        };
        let mut sync = false;
        match msg {
            UdpMessage::AuthenticateAck { nonce } => {
                // 6.5: only the first one counts
                if c.phase == Phase::Authenticating {
                    c.nonce = nonce;
                    c.phase = Phase::Checking;
                    // the Java loop would send the check at its next 1 s round; sending it now
                    // is the same packet earlier
                    Self::send_auth(&mut self.tx, c, out);
                }
            }
            UdpMessage::ConnectionCheckAck => {
                // 6.6: only counted when authenticated and not yet connected
                if c.phase == Phase::Checking {
                    c.phase = Phase::Connected;
                    c.last_keepalive = now;
                    c.mic_seq = 0; // a new MicThread starts at 0 (8.3)
                    c.speaking = false;
                    c.bucket = BUCKET_CAP;
                    c.bucket_at = now;
                    self.fail = None;
                    sync = true; // "report client_state (disabled=false)", below
                }
            }
            UdpMessage::KeepAlive => {
                // 5.8 / 6.7: remember, echo (only this reply refreshes the server's timeout, trap 17)
                if c.phase == Phase::Connected {
                    c.last_keepalive = now;
                }
                if c.phase != Phase::Authenticating {
                    c.refill(now);
                    self.tx.udp(c, &UdpMessage::KeepAlive, false, out);
                }
            }
            UdpMessage::Ping { id, timestamp } => {
                // 5.7: echo unchanged
                if c.phase != Phase::Authenticating {
                    c.refill(now);
                    self.tx.udp(c, &UdpMessage::Ping { id, timestamp }, false, out);
                }
            }
            m @ (UdpMessage::PlayerSound { .. } | UdpMessage::GroupSound { .. } | UdpMessage::LocationSound { .. }) => {
                // 7.1: dropped when voice is disabled
                if c.phase == Phase::Connected && self.enabled && self.audio_ok {
                    Self::on_sound(c, m, now, &mut self.tx.stats, out);
                }
            }
            // client -> server types arriving from the server make no sense: ignore
            UdpMessage::Mic { .. } | UdpMessage::Authenticate { .. } | UdpMessage::ConnectionCheck { .. } => {}
        }
        if sync {
            self.sync_state(true, out);
        }
    }

    /// Per channel rules of 7.1: stale/duplicate drop, loss count, end of speech.
    fn on_sound(c: &mut Conn, m: UdpMessage, now: u64, stats: &mut Stats, out: &mut Vec<Action>) {
        let (cm, kind) = match m {
            UdpMessage::PlayerSound { c, distance, whispering } => (c, PlayKind::Player { distance, whispering }),
            UdpMessage::GroupSound { c } => (c, PlayKind::Group),
            UdpMessage::LocationSound { c, x, y, z, distance } => (c, PlayKind::Location { x, y, z, distance }),
            _ => return,
        };
        if c.channels.len() >= 256 && !c.channels.contains_key(&cm.channel_id) {
            return; // bounded memory against a hostile/buggy server
        }
        let ch = c.channels.entry(cm.channel_id).or_insert(Chan { last_seq: None, last_ms: now });
        ch.last_ms = now;
        let mut lost: u32 = 0;
        let mut reset = false;
        if cm.opus.is_empty() {
            // end of speech: reset sequence tracking, the listener flushes the decoder
            ch.last_seq = None;
            reset = true;
        } else {
            match ch.last_seq {
                None => reset = true,
                Some(last) => {
                    if cm.sequence <= last {
                        if last.saturating_sub(cm.sequence) >= RESTART_JUMP {
                            reset = true; // sender restarted without a stop packet (trap 6)
                        } else {
                            stats.sound_stale += 1; // stale or duplicate
                            return;
                        }
                    } else {
                        let gap = cm.sequence as i128 - last as i128 - 1;
                        if gap > MAX_COMPENSATE {
                            reset = true; // new utterance: no compensation
                        } else {
                            lost = gap as u32;
                        }
                    }
                }
            }
            ch.last_seq = Some(cm.sequence);
        }
        out.push(Action::Play(Play {
            channel_id: cm.channel_id,
            sender: cm.sender,
            kind,
            sequence: cm.sequence,
            opus: cm.opus,
            category: cm.category,
            lost,
            reset,
        }));
    }

    // ---------------------------------------------------------------- microphone

    fn on_mic(&mut self, opus: Vec<u8>, whispering: bool, out: &mut Vec<Action>) {
        let now = self.now;
        let disabled = self.disabled();
        let muted = self.muted;
        let Some(c) = self.conn.as_mut() else { return };
        // 8.5: only when authenticated AND connection-checked
        if c.phase != Phase::Connected {
            return;
        }
        if disabled || muted {
            // 8.4: nothing is transmitted; a running burst is ended properly
            self.tx.stop(c, out);
            return;
        }
        // An empty frame would be read as the stop marker: refuse it. Frames above the encoder
        // limit (min(mtu,1275)) cannot be parsed by receivers (trap 9).
        if opus.is_empty() || opus.len() > c.secret.max_opus_payload(self.tx.compat) {
            self.tx.stats.mic_dropped += 1;
            return;
        }
        c.refill(now);
        let m = UdpMessage::Mic { opus, sequence: c.mic_seq, whispering };
        if self.tx.udp(c, &m, true, out) {
            c.mic_seq = c.mic_seq.wrapping_add(1);
            c.speaking = true;
        }
    }

    // ---------------------------------------------------------------- status

    fn update_status(&mut self, out: &mut Vec<Action>) {
        let s = if !self.requested {
            Status::Disabled
        } else if let Some(f) = &self.fail {
            Status::Failed(f.clone())
        } else {
            match self.conn.as_ref().map(|c| c.phase) {
                None => Status::Requesting,
                Some(Phase::Connected) => {
                    if self.enabled {
                        Status::Connected
                    } else {
                        Status::Disabled
                    }
                }
                Some(_) => {
                    if self.enabled {
                        Status::Connecting
                    } else {
                        Status::Disabled
                    }
                }
            }
        };
        if s != self.status {
            self.status = s.clone();
            out.push(Action::Status(s));
        }
    }
}
