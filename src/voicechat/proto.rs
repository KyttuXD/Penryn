//! Simple Voice Chat wire formats: the `voicechat:*` plugin-channel payloads and the encrypted
//! UDP datagrams. Pure encode/decode, no IO. Every decoder returns `None` on any malformed input
//! and never panics.
//!
//! Original code written from a protocol description (`voicechat-protocol.md`, itself read from
//! the mod's source); section numbers in comments refer to that document. Rules marked [v20] /
//! [v21] differ between compatibility version 20 (voicechat 2.6.x servers, the default) and 21
//! (2.7.0-pre); both are selected through the `compat` argument.

use super::gcm;
use alloc::string::String;
use alloc::vec::Vec;

/// Compatibility versions this module can speak (spec section 0).
pub const COMPAT_V20: u32 = 20;
pub const COMPAT_V21: u32 = 21;
/// The version a client uses unless configured otherwise.
pub const DEFAULT_COMPAT: u32 = COMPAT_V20;

pub type Uuid = [u8; 16];

/// First byte of every UDP datagram (4.2).
pub const MAGIC: u8 = 0xFF;
/// `readByteArray(2048)` on the datagram payload (4.2).
pub const MAX_PAYLOAD: usize = 2048;
/// Largest Opus packet either side parses (`readByteArray(1275)`, 5.1).
pub const MAX_OPUS: usize = 1275;
/// The "unauthenticated ping" UUID 58bc9ae9-c7a8-45e4-a11c-efbb67199425 (5.9).
pub const PING_UUID: Uuid = [
    0x58, 0xbc, 0x9a, 0xe9, 0xc7, 0xa8, 0x45, 0xe4, 0xa1, 0x1c, 0xef, 0xbb, 0x67, 0x19, 0x94, 0x25,
];

// Plugin channel ids (section 2.1).
pub const CH_REQUEST_SECRET: &str = "voicechat:request_secret";
pub const CH_SECRET: &str = "voicechat:secret";
pub const CH_UPDATE_STATE: &str = "voicechat:update_state"; // [v20]
pub const CH_CLIENT_STATE: &str = "voicechat:client_state"; // [v21]
pub const CH_STATES: &str = "voicechat:states";
pub const CH_STATE: &str = "voicechat:state";
pub const CH_REMOVE_STATE: &str = "voicechat:remove_state";
pub const CH_ADD_GROUP: &str = "voicechat:add_group";
pub const CH_REMOVE_GROUP: &str = "voicechat:remove_group";
pub const CH_JOINED_GROUP: &str = "voicechat:joined_group";
pub const CH_CREATE_GROUP: &str = "voicechat:create_group";
pub const CH_SET_GROUP: &str = "voicechat:set_group"; // = "join group"
pub const CH_LEAVE_GROUP: &str = "voicechat:leave_group";
pub const CH_ADD_CATEGORY: &str = "voicechat:add_category";
pub const CH_REMOVE_CATEGORY: &str = "voicechat:remove_category";
/// The vanilla channel a client uses to announce plugin channels (2, "cheap insurance").
pub const CH_REGISTER: &str = "minecraft:register";

pub fn client_state_channel(compat: u32) -> &'static str {
    if compat >= COMPAT_V21 {
        CH_CLIENT_STATE
    } else {
        CH_UPDATE_STATE
    }
}

// ------------------------------------------------------------------ byte helpers (private)

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Rd<'a> {
        Rd { b, p: 0 }
    }
    fn left(&self) -> usize {
        self.b.len() - self.p
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.left() {
            return None;
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Some(s)
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        let s = self.bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_be_bytes(self.arr()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.arr()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.arr()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_be_bytes(self.arr()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.arr()?))
    }
    /// A UUID is two big-endian longs, most significant first: 16 plain bytes.
    fn uuid(&mut self) -> Option<Uuid> {
        self.arr()
    }
    /// Minecraft VarInt (max 5 bytes). Returned as the u32 bit pattern.
    fn varint(&mut self) -> Option<u32> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    /// A length VarInt: negative (top bit set) values are errors.
    fn len(&mut self) -> Option<usize> {
        let v = self.varint()?;
        if v > i32::MAX as u32 {
            return None;
        }
        Some(v as usize)
    }
    /// `readByteArray(max)`.
    fn byte_array(&mut self, max: usize) -> Option<&'a [u8]> {
        let n = self.len()?;
        if n > max {
            return None;
        }
        self.bytes(n)
    }
    /// `readUtf(max_chars)`: VarInt byte length (at most 3 per char), UTF-8, at most `max_chars` chars.
    fn string(&mut self, max_chars: usize) -> Option<String> {
        let n = self.len()?;
        if n > max_chars.saturating_mul(3) {
            return None;
        }
        let s = core::str::from_utf8(self.bytes(n)?).ok()?;
        if s.chars().count() > max_chars {
            return None;
        }
        Some(String::from(s))
    }
    /// bool present, then the value.
    fn opt_string(&mut self, max_chars: usize) -> Option<Option<String>> {
        if self.bool()? {
            Some(Some(self.string(max_chars)?))
        } else {
            Some(None)
        }
    }
}

struct Wr(Vec<u8>);

impl Wr {
    fn new() -> Wr {
        Wr(Vec::new())
    }
    fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn i16(&mut self, v: i16) {
        self.raw(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.raw(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.raw(&v.to_be_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.raw(&v.to_be_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.raw(&v.to_be_bytes());
    }
    fn uuid(&mut self, v: &Uuid) {
        self.raw(v);
    }
    fn varint(&mut self, mut v: u32) {
        loop {
            if v < 0x80 {
                self.0.push(v as u8);
                return;
            }
            self.0.push((v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
    }
    fn byte_array(&mut self, b: &[u8]) {
        self.varint(b.len() as u32);
        self.raw(b);
    }
    fn string(&mut self, s: &str) {
        self.byte_array(s.as_bytes());
    }
}

// ------------------------------------------------------------------ plugin channel: types

/// `codec` byte of the secret packet = the Opus application of the encoder (3.2, 8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Voip,
    Audio,
    RestrictedLowDelay,
}

impl Codec {
    /// Out-of-range ordinals crash the Java client; here they fall back to VOIP (trap 8).
    pub fn from_byte(b: u8) -> Codec {
        match b {
            1 => Codec::Audio,
            2 => Codec::RestrictedLowDelay,
            _ => Codec::Voip,
        }
    }
}

/// `voicechat:secret` (3.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Secret {
    /// Raw AES-128 key.
    pub secret: [u8; 16],
    /// [v21] only: random id that replaces the player UUID in the UDP header.
    pub connection_id: Option<Uuid>,
    pub server_port: i32,
    pub player: Uuid,
    pub codec: Codec,
    pub mtu_size: i32,
    pub voice_chat_distance: f64,
    pub keep_alive_ms: i32,
    pub groups_enabled: bool,
    pub voice_host: String,
    pub allow_recording: bool,
}

impl Secret {
    /// The id placed after the magic byte of every client->server datagram (4.2, trap 1).
    pub fn udp_id(&self) -> Uuid {
        self.connection_id.unwrap_or(self.player)
    }
    /// Largest Opus packet the encoder may produce: `min(mtu, 1275)` (3.2, trap 9). [v21] clamps
    /// the mtu to 512..2048 first; [v20] does not, here it is merely kept above zero.
    pub fn max_opus_payload(&self, compat: u32) -> usize {
        let mtu = if compat >= COMPAT_V21 { self.mtu_size.clamp(512, 2048) } else { self.mtu_size.max(1) };
        (mtu as usize).min(MAX_OPUS)
    }
    /// Keep-alive period; the server's minimum is 1000 ms, so anything non-positive means 1000.
    pub fn keep_alive(&self) -> u32 {
        if self.keep_alive_ms <= 0 {
            1000
        } else {
            self.keep_alive_ms as u32
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupType {
    Normal = 0,
    Open = 1,
    Isolated = 2,
}

impl GroupType {
    /// Any other value is read as NORMAL (3.5).
    pub fn from_i16(v: i16) -> GroupType {
        match v {
            1 => GroupType::Open,
            2 => GroupType::Isolated,
            _ => GroupType::Normal,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClientGroup {
    pub id: Uuid,
    pub name: String,
    pub has_password: bool,
    pub persistent: bool,
    pub hidden: bool,
    pub kind: GroupType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinedGroup {
    pub group: Option<Uuid>,
    pub wrong_password: bool,
}

/// One entry of `states` / `state` (3.4).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub uuid: Uuid,
    pub name: String,
    /// [v21] only, false on v20.
    pub muted: bool,
    pub force_muted: bool,
    /// [v20]: already includes force-disabled.
    pub disabled: bool,
    pub force_disabled: bool,
    /// "has no voice chat connection".
    pub disconnected: bool,
    pub group: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

/// Every clientbound plugin packet the client understands.
#[derive(Clone, Debug, PartialEq)]
pub enum PluginPacket {
    Secret(Secret),
    States(Vec<PlayerState>),
    State(PlayerState),
    RemoveState(Uuid),
    AddGroup(ClientGroup),
    RemoveGroup(Uuid),
    JoinedGroup(JoinedGroup),
    /// Parsed only far enough to be skipped safely (3.6).
    AddCategory(Category),
    RemoveCategory(String),
}

// ------------------------------------------------------------------ plugin channel: encode

/// `voicechat:request_secret` (3.1): one i32.
pub fn encode_request_secret(compat: u32) -> Vec<u8> {
    let mut w = Wr::new();
    w.i32(compat as i32);
    w.0
}

/// Payload of `minecraft:register` listing the five serverbound channels, NUL separated (2).
pub fn encode_register(compat: u32) -> Vec<u8> {
    let list = [CH_REQUEST_SECRET, client_state_channel(compat), CH_CREATE_GROUP, CH_SET_GROUP, CH_LEAVE_GROUP];
    let mut out = Vec::new();
    for (i, c) in list.iter().enumerate() {
        if i > 0 {
            out.push(0);
        }
        out.extend_from_slice(c.as_bytes());
    }
    out
}

/// [v20] `update_state`: `bool disabled`. [v21] `client_state`: `bool muted, bool disabled` (3.3).
/// Channel: `client_state_channel(compat)`.
pub fn encode_client_state(compat: u32, muted: bool, disabled: bool) -> Vec<u8> {
    let mut w = Wr::new();
    if compat >= COMPAT_V21 {
        w.bool(muted);
    }
    w.bool(disabled);
    w.0
}

/// `create_group` (3.5): name (max 24 chars), optional password, i16 type. `None` when a limit
/// is exceeded (the server would kick a client whose string fails to parse).
pub fn encode_create_group(compat: u32, name: &str, password: Option<&str>, kind: GroupType) -> Option<Vec<u8>> {
    if name.chars().count() > 24 {
        return None;
    }
    let max_pw = if compat >= COMPAT_V21 { 256 } else { 24 };
    let mut w = Wr::new();
    w.string(name);
    match password {
        Some(p) => {
            if p.chars().count() > max_pw {
                return None;
            }
            w.bool(true);
            w.string(p);
        }
        None => w.bool(false),
    }
    w.i16(kind as i16);
    Some(w.0)
}

/// `set_group` = join (3.5): group UUID, optional password (max 256 [v21] / 512 [v20]).
pub fn encode_join_group(compat: u32, group: &Uuid, password: Option<&str>) -> Option<Vec<u8>> {
    let max_pw = if compat >= COMPAT_V21 { 256 } else { 512 };
    let mut w = Wr::new();
    w.uuid(group);
    match password {
        Some(p) => {
            if p.chars().count() > max_pw {
                return None;
            }
            w.bool(true);
            w.string(p);
        }
        None => w.bool(false),
    }
    Some(w.0)
}

/// `leave_group`: empty body.
pub fn encode_leave_group() -> Vec<u8> {
    Vec::new()
}

// Server-side encoders: needed by tests (a mock server) and harmless in the client.

pub fn encode_secret(compat: u32, s: &Secret) -> Vec<u8> {
    let mut w = Wr::new();
    w.raw(&s.secret);
    if compat >= COMPAT_V21 {
        w.uuid(&s.connection_id.unwrap_or([0; 16]));
    }
    w.i32(s.server_port);
    w.uuid(&s.player);
    w.u8(match s.codec {
        Codec::Voip => 0,
        Codec::Audio => 1,
        Codec::RestrictedLowDelay => 2,
    });
    w.i32(s.mtu_size);
    w.f64(s.voice_chat_distance);
    w.i32(s.keep_alive_ms);
    w.bool(s.groups_enabled);
    w.string(&s.voice_host);
    w.bool(s.allow_recording);
    w.0
}

pub fn encode_player_state(compat: u32, p: &PlayerState) -> Vec<u8> {
    let mut w = Wr::new();
    put_player_state(&mut w, compat, p);
    w.0
}

fn put_player_state(w: &mut Wr, compat: u32, p: &PlayerState) {
    if compat >= COMPAT_V21 {
        let flags = (p.muted as u8)
            | (p.force_muted as u8) << 1
            | (p.disabled as u8) << 2
            | (p.force_disabled as u8) << 3
            | (p.disconnected as u8) << 4
            | (p.group.is_some() as u8) << 5;
        w.u8(flags);
        w.uuid(&p.uuid);
        w.string(&p.name);
        if let Some(g) = &p.group {
            w.uuid(g);
        }
    } else {
        w.bool(p.disabled);
        w.bool(p.disconnected);
        w.uuid(&p.uuid);
        w.string(&p.name);
        w.bool(p.group.is_some());
        if let Some(g) = &p.group {
            w.uuid(g);
        }
    }
}

pub fn encode_states(compat: u32, list: &[PlayerState]) -> Vec<u8> {
    let mut w = Wr::new();
    w.i32(list.len() as i32);
    for p in list {
        put_player_state(&mut w, compat, p);
    }
    w.0
}

pub fn encode_add_group(g: &ClientGroup) -> Vec<u8> {
    let mut w = Wr::new();
    w.uuid(&g.id);
    w.string(&g.name);
    w.bool(g.has_password);
    w.bool(g.persistent);
    w.bool(g.hidden);
    w.i16(g.kind as i16);
    w.0
}

pub fn encode_joined_group(j: &JoinedGroup) -> Vec<u8> {
    let mut w = Wr::new();
    w.bool(j.group.is_some());
    if let Some(g) = &j.group {
        w.uuid(g);
    }
    w.bool(j.wrong_password);
    w.0
}

// ------------------------------------------------------------------ plugin channel: decode

/// `voicechat:secret` (3.2). Trailing bytes are tolerated like the Java reader does.
pub fn decode_secret(data: &[u8], compat: u32) -> Option<Secret> {
    let mut r = Rd::new(data);
    let secret = r.arr::<16>()?;
    let connection_id = if compat >= COMPAT_V21 { Some(r.uuid()?) } else { None };
    let server_port = r.i32()?;
    let player = r.uuid()?;
    let codec = Codec::from_byte(r.u8()?);
    let mtu_size = r.i32()?;
    let voice_chat_distance = r.f64()?;
    let keep_alive_ms = r.i32()?;
    let groups_enabled = r.bool()?;
    let voice_host = r.string(32767)?;
    let allow_recording = r.bool()?;
    Some(Secret {
        secret,
        connection_id,
        server_port,
        player,
        codec,
        mtu_size,
        voice_chat_distance,
        keep_alive_ms,
        groups_enabled,
        voice_host,
        allow_recording,
    })
}

fn read_player_state(r: &mut Rd, compat: u32) -> Option<PlayerState> {
    if compat >= COMPAT_V21 {
        let flags = r.u8()?;
        let uuid = r.uuid()?;
        let name = r.string(256)?;
        let group = if flags & 0x20 != 0 { Some(r.uuid()?) } else { None };
        Some(PlayerState {
            uuid,
            name,
            muted: flags & 1 != 0,
            force_muted: flags & 2 != 0,
            disabled: flags & 4 != 0,
            force_disabled: flags & 8 != 0,
            disconnected: flags & 16 != 0,
            group,
        })
    } else {
        let disabled = r.bool()?;
        let disconnected = r.bool()?;
        let uuid = r.uuid()?;
        let name = r.string(32767)?;
        let group = if r.bool()? { Some(r.uuid()?) } else { None };
        Some(PlayerState { uuid, name, muted: false, force_muted: false, disabled, force_disabled: false, disconnected, group })
    }
}

pub fn decode_player_state(data: &[u8], compat: u32) -> Option<PlayerState> {
    read_player_state(&mut Rd::new(data), compat)
}

/// `states`: i32 count then that many states. The count is never trusted for allocation.
pub fn decode_states(data: &[u8], compat: u32) -> Option<Vec<PlayerState>> {
    let mut r = Rd::new(data);
    let n = r.i32()?;
    if n < 0 {
        return None;
    }
    let mut v = Vec::new();
    for _ in 0..n {
        v.push(read_player_state(&mut r, compat)?);
    }
    Some(v)
}

pub fn decode_remove_state(data: &[u8]) -> Option<Uuid> {
    Rd::new(data).uuid()
}

/// `add_group`. The name limit is 24 chars [v21] / 512 [v20].
pub fn decode_add_group(data: &[u8], compat: u32) -> Option<ClientGroup> {
    let mut r = Rd::new(data);
    let id = r.uuid()?;
    let name = r.string(if compat >= COMPAT_V21 { 24 } else { 512 })?;
    let has_password = r.bool()?;
    let persistent = r.bool()?;
    let hidden = r.bool()?;
    let kind = GroupType::from_i16(r.i16()?);
    Some(ClientGroup { id, name, has_password, persistent, hidden, kind })
}

pub fn decode_remove_group(data: &[u8]) -> Option<Uuid> {
    Rd::new(data).uuid()
}

/// `joined_group`: `bool hasGroup, [UUID], bool wrongPassword` (trap 22).
pub fn decode_joined_group(data: &[u8]) -> Option<JoinedGroup> {
    let mut r = Rd::new(data);
    let group = if r.bool()? { Some(r.uuid()?) } else { None };
    let wrong_password = r.bool()?;
    Some(JoinedGroup { group, wrong_password })
}

/// `add_category` (3.6). Fully walked so that a caller knows the packet is well formed; only
/// the id and name are returned. An icon is 256 i32.
pub fn decode_add_category(data: &[u8], compat: u32) -> Option<Category> {
    let mut r = Rd::new(data);
    let id = r.string(16)?;
    let name = r.string(16)?;
    // 2.6.x reads these with readUtf(32767), master with 256.
    let limit = if compat >= COMPAT_V21 { 256 } else { 32767 };
    r.opt_string(limit)?; // nameKey
    r.opt_string(limit)?; // description
    r.opt_string(limit)?; // descriptionKey
    if r.bool()? {
        r.bytes(256 * 4)?;
    }
    if compat >= COMPAT_V21 {
        r.opt_string(16)?; // parent
    }
    Some(Category { id, name })
}

pub fn decode_remove_category(data: &[u8]) -> Option<String> {
    Rd::new(data).string(16)
}

/// Decodes a clientbound plugin payload by channel id. `None` for an unknown channel (the caller
/// drops it) or a malformed payload.
pub fn decode_plugin(channel: &str, data: &[u8], compat: u32) -> Option<PluginPacket> {
    Some(match channel {
        CH_SECRET => PluginPacket::Secret(decode_secret(data, compat)?),
        CH_STATES => PluginPacket::States(decode_states(data, compat)?),
        CH_STATE => PluginPacket::State(decode_player_state(data, compat)?),
        CH_REMOVE_STATE => PluginPacket::RemoveState(decode_remove_state(data)?),
        CH_ADD_GROUP => PluginPacket::AddGroup(decode_add_group(data, compat)?),
        CH_REMOVE_GROUP => PluginPacket::RemoveGroup(decode_remove_group(data)?),
        CH_JOINED_GROUP => PluginPacket::JoinedGroup(decode_joined_group(data)?),
        CH_ADD_CATEGORY => PluginPacket::AddCategory(decode_add_category(data, compat)?),
        CH_REMOVE_CATEGORY => PluginPacket::RemoveCategory(decode_remove_category(data)?),
        _ => return None,
    })
}

// ------------------------------------------------------------------ voice host / target (3.2)

/// Where to send UDP. `host == None` means "the address of the Minecraft TCP connection".
#[derive(Clone, Debug, PartialEq)]
pub struct UdpTarget {
    pub host: Option<String>,
    pub port: u16,
}

/// `InitializationData.parseAddress` (3.2, trap 18). `None` only when the base `server_port` is
/// unusable and the voice host does not supply a port.
pub fn parse_voice_target(voice_host: &str, server_port: i32) -> Option<UdpTarget> {
    let valid = |p: i64| if (1..=65535).contains(&p) { Some(p as u16) } else { None };
    let base = valid(server_port as i64);
    if voice_host.is_empty() {
        return Some(UdpTarget { host: None, port: base? });
    }
    if voice_host.bytes().all(|b| b.is_ascii_digit()) {
        // all digits: a port that replaces serverPort when valid, else warning and keep
        let p = voice_host.parse::<i64>().ok().and_then(valid).or(base)?;
        return Some(UdpTarget { host: None, port: p });
    }
    // "voicechat://" + voiceHost as a URI: [host][:port], IPv6 in brackets; a path is ignored.
    let s = voice_host.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or("");
    let (host, port): (&str, Option<&str>) = if let Some(rest) = s.strip_prefix('[') {
        match rest.find(']') {
            Some(i) => (&s[..i + 2], rest[i + 1..].strip_prefix(':')),
            None => (s, None),
        }
    } else if s.matches(':').count() == 1 {
        let i = s.find(':').unwrap_or(0);
        (&s[..i], Some(&s[i + 1..]))
    } else {
        (s, None) // no port, or a bare IPv6 literal
    };
    let port = port.and_then(|p| p.parse::<i64>().ok()).filter(|p| *p > 0).and_then(valid).or(base)?;
    let host = if host.is_empty() { None } else { Some(String::from(host)) };
    Some(UdpTarget { host, port })
}

// ------------------------------------------------------------------ UDP messages

pub const T_MIC: u8 = 0x1;
pub const T_PLAYER_SOUND: u8 = 0x2;
pub const T_GROUP_SOUND: u8 = 0x3;
pub const T_LOCATION_SOUND: u8 = 0x4;
pub const T_AUTHENTICATE: u8 = 0x5;
pub const T_AUTHENTICATE_ACK: u8 = 0x6;
pub const T_PING: u8 = 0x7;
pub const T_KEEP_ALIVE: u8 = 0x8;
pub const T_CONNECTION_CHECK: u8 = 0x9;
pub const T_CONNECTION_CHECK_ACK: u8 = 0xA;

/// Fields shared by the three sound packets. An empty `opus` is the end-of-speech marker.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundCommon {
    pub channel_id: Uuid,
    pub sender: Uuid,
    pub opus: Vec<u8>,
    pub sequence: i64,
    /// Volume category, max 16 chars.
    pub category: Option<String>,
}

/// The inner (plaintext) UDP message, before encryption (4.3, section 5).
#[derive(Clone, Debug, PartialEq)]
pub enum UdpMessage {
    /// 0x1, client -> server (5.1).
    Mic { opus: Vec<u8>, sequence: i64, whispering: bool },
    /// 0x2 (5.2).
    PlayerSound { c: SoundCommon, distance: f32, whispering: bool },
    /// 0x3 (5.3).
    GroupSound { c: SoundCommon },
    /// 0x4 (5.4).
    LocationSound { c: SoundCommon, x: f64, y: f64, z: f64, distance: f32 },
    /// 0x5 (5.5). [v20] body: player UUID + secret. [v21]: empty (fields are zero when decoded).
    Authenticate { player: Uuid, secret: [u8; 16] },
    /// 0x6 (5.6). [v21] body: 16 byte nonce. [v20]: empty (nonce zero when decoded).
    AuthenticateAck { nonce: [u8; 16] },
    /// 0x7 (5.7).
    Ping { id: Uuid, timestamp: i64 },
    /// 0x8, empty body (5.8).
    KeepAlive,
    /// 0x9 (5.5). [v21] body: the nonce echoed from the ack. [v20]: empty.
    ConnectionCheck { nonce: [u8; 16] },
    /// 0xA, empty body (5.6).
    ConnectionCheckAck,
}

fn category_flags(c: &SoundCommon, extra: u8) -> u8 {
    extra | if c.category.is_some() { 0x02 } else { 0 }
}

fn put_category(w: &mut Wr, c: &SoundCommon) {
    if let Some(cat) = &c.category {
        w.string(cat);
    }
}

/// Encodes the plaintext `type || body`. `None` when an Opus payload exceeds 1275 bytes or a
/// category exceeds 16 chars (the peer could not parse it).
pub fn encode_message(compat: u32, m: &UdpMessage) -> Option<Vec<u8>> {
    let mut w = Wr::new();
    let check = |c: &SoundCommon| -> bool {
        c.opus.len() <= MAX_OPUS && c.category.as_ref().map_or(true, |s| s.chars().count() <= 16)
    };
    match m {
        UdpMessage::Mic { opus, sequence, whispering } => {
            if opus.len() > MAX_OPUS {
                return None;
            }
            w.u8(T_MIC);
            w.byte_array(opus);
            w.i64(*sequence);
            w.bool(*whispering);
        }
        UdpMessage::PlayerSound { c, distance, whispering } => {
            if !check(c) {
                return None;
            }
            w.u8(T_PLAYER_SOUND);
            w.uuid(&c.channel_id);
            w.uuid(&c.sender);
            w.byte_array(&c.opus);
            w.i64(c.sequence);
            w.f32(*distance);
            w.u8(category_flags(c, *whispering as u8));
            put_category(&mut w, c);
        }
        UdpMessage::GroupSound { c } => {
            if !check(c) {
                return None;
            }
            w.u8(T_GROUP_SOUND);
            w.uuid(&c.channel_id);
            w.uuid(&c.sender);
            w.byte_array(&c.opus);
            w.i64(c.sequence);
            w.u8(category_flags(c, 0));
            put_category(&mut w, c);
        }
        UdpMessage::LocationSound { c, x, y, z, distance } => {
            if !check(c) {
                return None;
            }
            w.u8(T_LOCATION_SOUND);
            w.uuid(&c.channel_id);
            w.uuid(&c.sender);
            w.f64(*x);
            w.f64(*y);
            w.f64(*z);
            w.byte_array(&c.opus);
            w.i64(c.sequence);
            w.f32(*distance);
            w.u8(category_flags(c, 0));
            put_category(&mut w, c);
        }
        UdpMessage::Authenticate { player, secret } => {
            w.u8(T_AUTHENTICATE);
            if compat < COMPAT_V21 {
                w.uuid(player);
                w.raw(secret);
            }
        }
        UdpMessage::AuthenticateAck { nonce } => {
            w.u8(T_AUTHENTICATE_ACK);
            if compat >= COMPAT_V21 {
                w.raw(nonce);
            }
        }
        UdpMessage::Ping { id, timestamp } => {
            w.u8(T_PING);
            w.uuid(id);
            w.i64(*timestamp);
        }
        UdpMessage::KeepAlive => w.u8(T_KEEP_ALIVE),
        UdpMessage::ConnectionCheck { nonce } => {
            w.u8(T_CONNECTION_CHECK);
            if compat >= COMPAT_V21 {
                w.raw(nonce);
            }
        }
        UdpMessage::ConnectionCheckAck => w.u8(T_CONNECTION_CHECK_ACK),
    }
    Some(w.0)
}

fn read_sound_common(r: &mut Rd) -> Option<(Uuid, Uuid)> {
    Some((r.uuid()?, r.uuid()?))
}

fn read_category(r: &mut Rd, flags: u8) -> Option<Option<String>> {
    if flags & 0x02 != 0 {
        Some(Some(r.string(16)?))
    } else {
        Some(None)
    }
}

/// Decodes the plaintext `type || body`. Unknown type or malformed body gives `None`.
pub fn decode_message(compat: u32, plain: &[u8]) -> Option<UdpMessage> {
    let mut r = Rd::new(plain);
    let ty = r.u8()?;
    Some(match ty {
        T_MIC => {
            let opus = r.byte_array(MAX_OPUS)?.to_vec();
            let sequence = r.i64()?;
            let whispering = r.bool()?;
            UdpMessage::Mic { opus, sequence, whispering }
        }
        T_PLAYER_SOUND => {
            let (channel_id, sender) = read_sound_common(&mut r)?;
            let opus = r.byte_array(MAX_OPUS)?.to_vec();
            let sequence = r.i64()?;
            let distance = r.f32()?;
            let flags = r.u8()?; // bit0 whispering, bit1 hasCategory (trap 24)
            let category = read_category(&mut r, flags)?;
            UdpMessage::PlayerSound { c: SoundCommon { channel_id, sender, opus, sequence, category }, distance, whispering: flags & 1 != 0 }
        }
        T_GROUP_SOUND => {
            let (channel_id, sender) = read_sound_common(&mut r)?;
            let opus = r.byte_array(MAX_OPUS)?.to_vec();
            let sequence = r.i64()?;
            let flags = r.u8()?;
            let category = read_category(&mut r, flags)?;
            UdpMessage::GroupSound { c: SoundCommon { channel_id, sender, opus, sequence, category } }
        }
        T_LOCATION_SOUND => {
            let (channel_id, sender) = read_sound_common(&mut r)?;
            let (x, y, z) = (r.f64()?, r.f64()?, r.f64()?);
            let opus = r.byte_array(MAX_OPUS)?.to_vec();
            let sequence = r.i64()?;
            let distance = r.f32()?;
            let flags = r.u8()?;
            let category = read_category(&mut r, flags)?;
            UdpMessage::LocationSound { c: SoundCommon { channel_id, sender, opus, sequence, category }, x, y, z, distance }
        }
        T_AUTHENTICATE => {
            if compat >= COMPAT_V21 {
                UdpMessage::Authenticate { player: [0; 16], secret: [0; 16] }
            } else {
                let player = r.uuid()?;
                let secret = r.arr::<16>()?;
                UdpMessage::Authenticate { player, secret }
            }
        }
        T_AUTHENTICATE_ACK => {
            let nonce = if compat >= COMPAT_V21 { r.arr::<16>()? } else { [0; 16] };
            UdpMessage::AuthenticateAck { nonce }
        }
        T_PING => UdpMessage::Ping { id: r.uuid()?, timestamp: r.i64()? },
        T_KEEP_ALIVE => UdpMessage::KeepAlive,
        T_CONNECTION_CHECK => {
            let nonce = if compat >= COMPAT_V21 { r.arr::<16>()? } else { [0; 16] };
            UdpMessage::ConnectionCheck { nonce }
        }
        T_CONNECTION_CHECK_ACK => UdpMessage::ConnectionCheckAck,
        _ => return None,
    })
}

// ------------------------------------------------------------------ UDP framing and encryption

/// `IV(12) || ciphertext || tag(16)` (4.1). The IV must be fresh random bytes per packet.
pub fn encrypt_payload(key: &[u8; 16], iv: &[u8; 12], plain: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + plain.len() + gcm::TAG_LEN);
    out.extend_from_slice(iv);
    out.extend_from_slice(&gcm::seal(key, iv, plain));
    out
}

/// Inverse of `encrypt_payload`; `None` when the tag does not verify (such a datagram is dropped
/// silently by both sides).
pub fn decrypt_payload(key: &[u8; 16], payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 12 + gcm::TAG_LEN {
        return None;
    }
    let mut iv = [0u8; 12];
    iv.copy_from_slice(&payload[..12]);
    gcm::open(key, &iv, &payload[12..])
}

/// Client -> server frame: `0xFF | UUID | VarInt len | payload` (4.2). `id` is the connection id
/// [v21] or the player UUID [v20].
pub fn frame_to_server(id: &Uuid, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut w = Wr::new();
    w.u8(MAGIC);
    w.uuid(id);
    w.byte_array(payload);
    Some(w.0)
}

/// Server -> client frame: `0xFF | VarInt len | payload`, no UUID (4.2, trap 2).
pub fn frame_to_client(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut w = Wr::new();
    w.u8(MAGIC);
    w.byte_array(payload);
    Some(w.0)
}

/// Splits a client -> server datagram into (id, payload).
pub fn parse_to_server(data: &[u8]) -> Option<(Uuid, &[u8])> {
    let mut r = Rd::new(data);
    if r.u8()? != MAGIC {
        return None;
    }
    let id = r.uuid()?;
    let p = r.byte_array(MAX_PAYLOAD)?;
    Some((id, p))
}

/// Splits a server -> client datagram into its payload.
pub fn parse_to_client(data: &[u8]) -> Option<&[u8]> {
    let mut r = Rd::new(data);
    if r.u8()? != MAGIC {
        return None;
    }
    r.byte_array(MAX_PAYLOAD)
}

/// Complete client -> server datagram for `msg`: encode, encrypt with a fresh `iv`, frame.
pub fn seal_to_server(compat: u32, key: &[u8; 16], id: &Uuid, iv: &[u8; 12], msg: &UdpMessage) -> Option<Vec<u8>> {
    let plain = encode_message(compat, msg)?;
    frame_to_server(id, &encrypt_payload(key, iv, &plain))
}

/// Complete server -> client datagram (used by the mock server in tests).
pub fn seal_to_client(compat: u32, key: &[u8; 16], iv: &[u8; 12], msg: &UdpMessage) -> Option<Vec<u8>> {
    let plain = encode_message(compat, msg)?;
    frame_to_client(&encrypt_payload(key, iv, &plain))
}

/// Parses, authenticates and decodes a server -> client datagram.
pub fn open_from_server(compat: u32, key: &[u8; 16], data: &[u8]) -> Option<UdpMessage> {
    let plain = decrypt_payload(key, parse_to_client(data)?)?;
    decode_message(compat, &plain)
}

/// Server side of the above, for the mock server: returns (id from the header, message). The
/// caller looks the key up by id first, hence the two steps.
pub fn open_from_client(compat: u32, key: &[u8; 16], payload: &[u8]) -> Option<UdpMessage> {
    decode_message(compat, &decrypt_payload(key, payload)?)
}

/// The unauthenticated reachability probe (5.9): `0xFF | PING_UUID | VarInt 24 | UUID id | i64 ts`.
pub fn encode_unauth_ping(id: &Uuid, timestamp: i64) -> Vec<u8> {
    let mut body = Wr::new();
    body.uuid(id);
    body.i64(timestamp);
    // PING_UUID goes where the connection id normally is
    frame_to_server(&PING_UUID, &body.0).unwrap_or_default()
}

/// The reply to the probe is exactly 24 raw bytes, no magic, no framing, no encryption.
pub fn decode_unauth_ping_reply(data: &[u8]) -> Option<(Uuid, i64)> {
    if data.len() != 24 {
        return None;
    }
    let mut r = Rd::new(data);
    Some((r.uuid()?, r.i64()?))
}
