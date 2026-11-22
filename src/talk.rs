//! Chat as vanilla 26.3's client keeps it, secure chat included, so Penryn can talk on servers
//! that enforce secure profiles (the vanilla default):
//!
//! - the account's profile key pair (fetched by the relay from the services' certificates
//!   endpoint, kept until it expires), the chat session made from it
//!   (ServerboundChatSessionUpdatePacket) and the chain of signatures over each message
//!   (SignedMessageChain.Encoder over SignedMessageLink and SignedMessageBody);
//! - the messages the client acknowledges as seen (LastSeenMessagesTracker, ChatAck) and the
//!   cache signatures are packed against (MessageSignatureCache);
//! - signed commands (ServerboundChatCommandSignedPacket) for `message` arguments, found
//!   through the server's command tree, and unsigned ones otherwise;
//! - incoming player chat checked as ClientPacketListener and ChatListener do (each sender's
//!   SignedMessageValidator, ChatTrustLevel: "Not Secure" and "Modified" tags), decorated by
//!   the registry's chat types, disguised and system chat, message deletion;
//! - titles, subtitles and the action bar (Gui.setTitle and friends).

use crate::client::Game;
use crate::crypto::{RsaKey, RsaPrivate};
use crate::text::{self, Rich, Style};
use crate::wire::{In, Out};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

// Packet ids of protocol 777 (Horizon's horizon-protocol ids.rs) that ids.rs lacks.
use crate::ids::play::{C_DISGUISED_CHAT, C_LOGIN, C_PLAYER_CHAT, C_PLAYER_INFO_REMOVE, C_PLAYER_INFO_UPDATE, C_SYSTEM_CHAT, S_CHAT, S_CHAT_COMMAND};
pub const C_CLEAR_TITLES: i32 = 14;
pub const C_COMMAND_SUGGESTIONS: i32 = 15;
pub const C_COMMANDS: i32 = 16;
pub const C_DELETE_CHAT: i32 = 31;
pub const C_SET_ACTION_BAR_TEXT: i32 = 89;
pub const C_SET_SUBTITLE_TEXT: i32 = 114;
pub const C_SET_TITLE_TEXT: i32 = 116;
pub const C_SET_TITLES_ANIMATION: i32 = 117;
pub const S_CHAT_ACK: i32 = 6;
pub const S_CHAT_COMMAND_SIGNED: i32 = 8;
pub const S_CHAT_SESSION_UPDATE: i32 = 10;
pub const S_COMMAND_SUGGESTION: i32 = 15;

pub type Sig = [u8; 256];

/// LastSeenMessages.LAST_SEEN_MESSAGES_MAX_LENGTH.
const LAST_SEEN: usize = 20;
/// ProfilePublicKey.EXPIRY_GRACE_PERIOD: 8 hours.
const GRACE_MS: i64 = 8 * 3600 * 1000;
/// PlayerChatMessage.MESSAGE_EXPIRES_AFTER_CLIENT: 7 days.
const EXPIRES_AFTER_CLIENT_MS: i64 = 7 * 24 * 3600 * 1000;
/// ChatComponent.TIME_BEFORE_MESSAGE_DELETION: 60 ticks.
const DELETE_AFTER_MS: u64 = 3000;

// ------------------------------------------------------------------ the profile key pair

/// The account's chat key pair (ProfileKeyPair): the private key that signs its chat, the
/// public key (X.509) with the services' signature over it, its expiry and when to refresh.
pub struct KeyPair {
    private: RsaPrivate,
    public_der: Vec<u8>,
    signature: Vec<u8>,
    pub expires_at: i64,
    refreshed_after: i64,
}

struct Request {
    relay: (String, u16),
    token: String,
}

/// The fetch thread's answer, waiting for the main thread.
static RESULT: AtomicPtr<Result<KeyPair, String>> = AtomicPtr::new(core::ptr::null_mut());
static FETCHING: AtomicBool = AtomicBool::new(false);

struct Keys {
    current: Option<KeyPair>,
    /// Bumped with every new key pair (setKeyPair makes a new session for one).
    generation: u32,
    account: Option<(String, u16, String, [u8; 16])>,
    next_try: u64,
}

struct Main<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Main<T> {}

static KEYS: Main<Keys> = Main(UnsafeCell::new(Keys { current: None, generation: 0, account: None, next_try: 0 }));

/// The main thread's key state (the game loop is single-threaded).
fn keys() -> &'static mut Keys {
    unsafe { &mut *KEYS.0.get() }
}

fn now_ms() -> i64 {
    (crate::sys::now() * 1000.0) as i64
}

/// The relay's /certificates answer: the private key (DER, base64), the public key (X.509
/// DER, base64), publicKeySignatureV2 (base64), expiresAt and refreshedAfter (epoch ms).
fn parse_certificates(body: &str) -> Result<KeyPair, String> {
    let l: Vec<&str> = body.lines().map(|l| l.trim()).collect();
    if l.len() < 5 {
        return Err(String::from("short answer"));
    }
    let b64 = |s: &str| crate::skins::base64(s).ok_or_else(|| String::from("bad base64"));
    let private = RsaPrivate::from_der(&b64(l[0])?).ok_or("unreadable private key")?;
    let public_der = b64(l[1])?;
    let public = RsaKey::from_spki(&public_der).ok_or("unreadable public key")?;
    if public.modulus() != private.public.modulus() {
        return Err(String::from("the key pair's halves do not match"));
    }
    Ok(KeyPair {
        private,
        public_der,
        signature: b64(l[2])?,
        expires_at: l[3].parse().map_err(|_| "bad expiry")?,
        refreshed_after: l[4].parse().map_err(|_| "bad refresh time")?,
    })
}

extern "C" fn fetch_thread(arg: *mut c_void) -> *mut c_void {
    let req = unsafe { Box::from_raw(arg as *mut Request) };
    let body = alloc::format!("{}\n", req.token);
    let res = match crate::http::post(&req.relay.0, req.relay.1, "/certificates", body.as_bytes(), 30000) {
        Ok(r) if r.status == 200 => parse_certificates(&r.text()),
        Ok(r) => Err(alloc::format!("the relay answered {}: {}", r.status, r.text().trim())),
        Err(e) => Err(e),
    };
    let old = RESULT.swap(Box::into_raw(Box::new(res)), Ordering::AcqRel);
    if !old.is_null() {
        drop(unsafe { Box::from_raw(old) });
    }
    FETCHING.store(false, Ordering::Release);
    core::ptr::null_mut()
}

/// Remembers the signed-in account for key fetches (Game::join).
pub fn set_account(a: Option<&crate::client::Account>) {
    let k = keys();
    k.account = a.map(|a| (a.relay.0.clone(), a.relay.1, a.token.clone(), crate::client::uuid_bytes(&a.uuid)));
}

/// prepareKeyPair: fetch one when there is none, or it is due a refresh (at most once an
/// hour after a failure, AccountProfileKeyPairManager's MINIMUM_PROFILE_KEY_REFRESH_INTERVAL).
fn prepare_keys(now: bool) {
    let k = keys();
    let Some((host, port, token, _)) = k.account.clone() else { return };
    let t = crate::sys::now_ms();
    if FETCHING.load(Ordering::Acquire) || (!now && t < k.next_try) {
        return;
    }
    if k.current.as_ref().map_or(false, |c| c.refreshed_after > now_ms()) {
        return;
    }
    k.next_try = t + 3600 * 1000;
    FETCHING.store(true, Ordering::Release);
    let req = Box::into_raw(Box::new(Request { relay: (host, port), token }));
    let mut th: usize = 0;
    if unsafe { crate::sys::pthread_create(&mut th, core::ptr::null(), fetch_thread, req as *mut c_void) } == 0 {
        unsafe { crate::sys::pthread_detach(th) };
    } else {
        drop(unsafe { Box::from_raw(req) });
        FETCHING.store(false, Ordering::Release);
    }
}

/// Takes the fetch thread's answer, if one came.
fn collect_keys() {
    let p = RESULT.swap(core::ptr::null_mut(), Ordering::AcqRel);
    if p.is_null() {
        return;
    }
    match *unsafe { Box::from_raw(p) } {
        Ok(kp) => {
            crate::say!("profile key pair: valid until {} (epoch ms)", kp.expires_at);
            let k = keys();
            k.current = Some(kp);
            k.generation += 1;
        }
        Err(e) => crate::warn!("Failed to retrieve profile key pair: {}", e),
    }
}

/// The key pair, unless it has expired.
fn key_pair() -> Option<&'static KeyPair> {
    keys().current.as_ref().filter(|k| k.expires_at > now_ms())
}

// ------------------------------------------------------------------ chat types

/// ChatTypeDecoration: its translation key, parameters (0 sender, 1 target, 2 content) and style.
#[derive(Clone, Debug)]
pub struct Decoration {
    pub key: String,
    pub params: Vec<u8>,
    pub style: Style,
    pub style_events: Vec<text::Events>,
}

static CHAT_TYPES: Main<Vec<Decoration>> = Main(UnsafeCell::new(Vec::new()));

fn chat_types() -> &'static mut Vec<Decoration> {
    unsafe { &mut *CHAT_TYPES.0.get() }
}

fn decoration_of(chat: &text::Tag) -> Decoration {
    let key = chat.get("translation_key").and_then(|k| k.str()).unwrap_or("chat.type.text");
    let params = chat
        .get("parameters")
        .map(|p| {
            p.list()
                .iter()
                .filter_map(|x| match x.str()? {
                    "sender" => Some(0),
                    "target" => Some(1),
                    "content" => Some(2),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut d = Decoration { key: String::from(key), params, style: Style::default(), style_events: Vec::new() };
    if let Some(st) = chat.get("style") {
        // The style as a component without contents gives its runs' style.
        let mut probe = st.clone();
        if let text::Tag::Compound(v) = &mut probe {
            v.push((String::from("text"), text::Tag::Str(String::from("x"))));
        }
        let r = text::component(&probe);
        if let Some((s, _)) = r.runs.first() {
            d.style = *s;
            d.style_events = r.events.clone();
        }
    }
    d
}

/// RegistryDataPacket for minecraft:chat_type (configuration): the decorations in the
/// server's order.
pub fn registry(p: &crate::conn::Packet) {
    let mut r = p.reader();
    if r.string().as_deref() != Some("minecraft:chat_type") {
        return;
    }
    let n = r.varint().unwrap_or(0).max(0) as usize;
    let list = chat_types();
    list.clear();
    for _ in 0..n {
        let Some(_name) = r.string() else { break };
        let has = r.bool() == Some(true);
        let d = if has {
            match text::read_tag(&mut r) {
                Some(t) => t.get("chat").map(decoration_of).unwrap_or_else(default_decoration),
                None => break,
            }
        } else {
            default_decoration()
        };
        list.push(d);
    }
}

fn default_decoration() -> Decoration {
    Decoration { key: String::from("chat.type.text"), params: alloc::vec![0, 2], style: Style::default(), style_events: Vec::new() }
}

/// ChatType.Bound: the type (registry id + 1, or 0 and the type inline), the sender's name
/// and the target's.
struct Bound {
    deco: Decoration,
    name: Rich,
    target: Option<Rich>,
}

fn read_bound(r: &mut In) -> Option<Bound> {
    let id = r.varint()?;
    let deco = if id == 0 {
        // ChatType.DIRECT_STREAM_CODEC: chat then narration decorations.
        let key = r.string()?;
        let n = r.varint()?.clamp(0, 8);
        let mut params = Vec::new();
        for _ in 0..n {
            params.push(r.varint()? as u8);
        }
        let st = text::read_tag(r)?;
        let mut d = decoration_of(&text::Tag::Compound(alloc::vec![(String::from("style"), st)]));
        d.key = key;
        d.params = params;
        r.string()?;
        let n = r.varint()?.clamp(0, 8);
        for _ in 0..n {
            r.varint()?;
        }
        text::read_tag(r)?;
        d
    } else {
        chat_types().get(id as usize - 1).cloned().unwrap_or_else(default_decoration)
    };
    let name = text::read(r);
    let target = if r.bool()? { Some(text::read(r)) } else { None };
    Some(Bound { deco, name, target })
}

impl Bound {
    /// ChatTypeDecoration.decorate: the translation with the parameters' components.
    fn decorate(&self, content: &Rich) -> Rich {
        let empty = Rich::default();
        let args: Vec<&Rich> = self
            .deco
            .params
            .iter()
            .map(|p| match p {
                0 => &self.name,
                1 => self.target.as_ref().unwrap_or(&empty),
                _ => content,
            })
            .collect();
        let mut st = self.deco.style;
        let mut out = Rich::default();
        if st.events != 0 {
            out.events = self.deco.style_events.clone();
        }
        let r = text::translate_rich(&self.deco.key, &args, Style { events: 0, ..st });
        st.events = 0;
        for (rs, t) in &r.runs {
            let mut rs = *rs;
            if rs.events == 0 && self.deco.style.events != 0 {
                rs.events = self.deco.style.events;
            } else if rs.events != 0 {
                // translate_rich numbered the arguments' events after none of ours.
                rs.events += out.events.len() as u16;
            }
            out.runs.push((rs, t.clone()));
        }
        out.events.extend(r.events.iter().cloned());
        let _ = st;
        out
    }
}

// ------------------------------------------------------------------ chat log

/// GuiMessageTag: the indicator beside a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    None,
    /// Disguised chat and the deleted marker (chat.tag.system), 0xD0D0D0.
    System,
    /// System chat from the server or the client (addServerSystemMessage and
    /// addClientSystemMessage: chat.tag.system_single_player), 0xD0D0D0.
    SystemSinglePlayer,
    /// chat.tag.not_secure, 0xD0D0D0.
    NotSecure,
    /// chat.tag.modified with the original text, 0x606060 and the chat_modified icon.
    Modified(String),
    /// chat.tag.error, 0xFF5555.
    Error,
}

impl Tag {
    pub fn color(&self) -> Option<u32> {
        match self {
            Tag::None => None,
            Tag::System | Tag::SystemSinglePlayer | Tag::NotSecure => Some(0xD0D0D0),
            Tag::Modified(_) => Some(0x606060),
            Tag::Error => Some(0xFF5555),
        }
    }
    /// The tooltip over the indicator.
    pub fn text(&self) -> Option<Rich> {
        Some(match self {
            Tag::None => return None,
            Tag::System => Rich::plain(&crate::lang::get("chat.tag.system")),
            Tag::SystemSinglePlayer => Rich::plain(&crate::lang::get("chat.tag.system_single_player")),
            Tag::NotSecure => Rich::plain(&crate::lang::get("chat.tag.not_secure")),
            Tag::Error => Rich::plain(&crate::lang::get("chat.tag.error")),
            Tag::Modified(orig) => {
                let mut r = Rich::plain(&crate::lang::get("chat.tag.modified"));
                r.push(Style::default(), "\n");
                r.append(&Rich::styled(orig, 0xAAAAAA, 0), &Style::default());
                r
            }
        })
    }
}

/// A GuiMessage: its text, when it came (ms), its tag and signature, and its wrapped lines
/// for the last chat width (made again only when the width changes).
pub struct Msg {
    pub rich: Rich,
    pub at: u64,
    pub tag: Tag,
    pub sig: Option<Box<Sig>>,
    pub wrapped: Option<(f32, Vec<text::Line>)>,
}

// ------------------------------------------------------------------ tracking

/// LastSeenMessagesTracker: the last 20 signed messages, whether each is still to be
/// acknowledged, and how many came since the last acknowledgement.
struct Tracker {
    tracked: [Option<(Box<Sig>, bool)>; LAST_SEEN],
    tail: usize,
    offset: i32,
    last: Option<Box<Sig>>,
}

impl Tracker {
    fn new() -> Tracker {
        Tracker { tracked: Default::default(), tail: 0, offset: 0, last: None }
    }
    /// addPending: false for the same signature twice in a row.
    fn add_pending(&mut self, s: &Sig, shown: bool) -> bool {
        if self.last.as_deref() == Some(s) {
            return false;
        }
        self.last = Some(Box::new(*s));
        self.tracked[self.tail] = if shown { Some((Box::new(*s), true)) } else { None };
        self.tail = (self.tail + 1) % LAST_SEEN;
        self.offset += 1;
        true
    }
    fn ignore_pending(&mut self, s: &Sig) {
        for e in self.tracked.iter_mut() {
            if matches!(e, Some((x, true)) if **x == *s) {
                *e = None;
            }
        }
    }
    /// generateAndApplyUpdate: the seen signatures oldest first, the offset, the 20-bit set
    /// and LastSeenMessages.computeChecksum.
    fn update(&mut self) -> (Vec<Sig>, i32, [u8; 3], i8) {
        let offset = core::mem::take(&mut self.offset);
        let mut bits = [0u8; 3];
        let mut seen = Vec::new();
        for i in 0..LAST_SEEN {
            let k = (self.tail + i) % LAST_SEEN;
            if let Some((s, pending)) = &mut self.tracked[k] {
                bits[i / 8] |= 1 << (i % 8);
                seen.push(**s);
                *pending = false;
            }
        }
        let c = checksum(&seen);
        (seen, offset, bits, c)
    }
}

/// Arrays.hashCode(byte[]).
fn java_hash(b: &[u8]) -> i32 {
    b.iter().fold(1i32, |h, &x| h.wrapping_mul(31).wrapping_add(x as i8 as i32))
}

/// LastSeenMessages.computeChecksum (0 is sent as 1).
pub fn checksum(entries: &[Sig]) -> i8 {
    let c = entries.iter().fold(1i32, |c, e| c.wrapping_mul(31).wrapping_add(java_hash(e)));
    match c as i8 {
        0 => 1,
        b => b,
    }
}

/// MessageSignatureCache (128 entries): what packed signatures refer to.
struct Cache(Vec<Option<Box<Sig>>>);

impl Cache {
    fn push(&mut self, last_seen: &[Sig], sig: Option<&Sig>) {
        let mut queue: alloc::collections::VecDeque<Sig> = last_seen.iter().copied().collect();
        if let Some(s) = sig {
            queue.push_back(*s);
        }
        let new: Vec<Sig> = queue.iter().copied().collect();
        for slot in self.0.iter_mut() {
            let Some(next) = queue.pop_back() else { break };
            if let Some(old) = slot.replace(Box::new(next)) {
                if !new.contains(&old) {
                    queue.push_front(*old);
                }
            }
        }
    }
    /// MessageSignature.Packed: an id + 1 into the cache, or 0 and the signature itself.
    fn unpack(&self, r: &mut In) -> Option<Option<Sig>> {
        let id = r.varint()?;
        if id == 0 {
            let mut s = [0u8; 256];
            s.copy_from_slice(r.take(256)?);
            return Some(Some(s));
        }
        Some(self.0.get(id as usize - 1).and_then(|x| x.as_deref().copied()))
    }
}

/// A player's chat session (RemoteChatSession) and SignedMessageValidator state.
struct Validator {
    session: Option<([u8; 16], RsaKey, i64)>,
    chain_valid: bool,
    last: Option<(i32, Sig)>,
}

/// SignedMessageLink + SignedMessageBody as PlayerChatMessage.updateSignature feeds them.
fn signed_payload(sender: &[u8; 16], session: &[u8; 16], index: i32, salt: i64, ts: i64, content: &str, last_seen: &[Sig]) -> Vec<u8> {
    let c = content.as_bytes();
    let mut o = Vec::with_capacity(64 + c.len() + last_seen.len() * 256);
    o.extend_from_slice(&1i32.to_be_bytes());
    o.extend_from_slice(sender);
    o.extend_from_slice(session);
    o.extend_from_slice(&index.to_be_bytes());
    o.extend_from_slice(&salt.to_be_bytes());
    o.extend_from_slice(&ts.div_euclid(1000).to_be_bytes());
    o.extend_from_slice(&(c.len() as i32).to_be_bytes());
    o.extend_from_slice(c);
    o.extend_from_slice(&(last_seen.len() as i32).to_be_bytes());
    for s in last_seen {
        o.extend_from_slice(s);
    }
    o
}

/// The local chat session (LocalChatSession): its id, the next link of its chain, and the
/// key pair generation it was made from.
struct Session {
    id: [u8; 16],
    next: Option<i32>,
    generation: u32,
}

/// One server suggestion (ClientboundCommandSuggestionsPacket.Entry).
pub struct Suggestion {
    pub text: String,
    pub tooltip: Option<Rich>,
}

pub struct Talk {
    pub msgs: Vec<Msg>,
    deletions: Vec<(Box<Sig>, u64)>,
    pub online_mode: bool,
    pub enforces_secure_chat: bool,
    session: Option<Session>,
    tracker: Tracker,
    cache: Cache,
    players: BTreeMap<[u8; 16], Validator>,
    /// The players' names (the tab list's profiles), for suggestions.
    pub names: BTreeMap<[u8; 16], String>,
    next_index: i32,
    /// The server's command tree.
    pub tree: Option<crate::commands::Tree>,
    /// The last suggestion request's id, and the answer: (id, start, length, entries).
    pub suggest_id: i32,
    pub suggestions: Option<(i32, usize, usize, Vec<Suggestion>)>,
    /// Gui's title, subtitle, their times (fade in, stay, fade out) and ticks left.
    pub title: Option<Rich>,
    pub subtitle: Option<Rich>,
    pub title_times: (i32, i32, i32),
    pub title_ticks: i32,
    /// The action bar (overlay message) and its ticks left.
    pub overlay: Option<(Rich, i32)>,
    /// ChatComponent.recentChat (sent lines, for the arrow keys), at most 100.
    pub history: Vec<String>,
}

impl Talk {
    pub fn new() -> Talk {
        Talk {
            msgs: Vec::new(),
            deletions: Vec::new(),
            online_mode: false,
            enforces_secure_chat: false,
            session: None,
            tracker: Tracker::new(),
            cache: Cache((0..128).map(|_| None).collect()),
            players: BTreeMap::new(),
            names: BTreeMap::new(),
            next_index: 0,
            tree: None,
            suggest_id: 0,
            suggestions: None,
            title: None,
            subtitle: None,
            title_times: (10, 70, 20),
            title_ticks: 0,
            overlay: None,
            history: Vec::new(),
        }
    }

    pub fn push(&mut self, rich: Rich, tag: Tag, sig: Option<&Sig>) {
        self.msgs.push(Msg { rich, at: crate::sys::now_ms(), tag, sig: sig.map(|s| Box::new(*s)), wrapped: None });
        if self.msgs.len() > 100 {
            self.msgs.remove(0);
        }
    }
}

fn random_i64() -> i64 {
    let b = crate::sys::read_urandom(8).unwrap_or_else(|| ((crate::sys::now() * 1e6) as u64).to_be_bytes().to_vec());
    i64::from_be_bytes(b[..8].try_into().unwrap())
}

/// UUID.randomUUID.
fn random_uuid() -> [u8; 16] {
    let mut u = [0u8; 16];
    let b = crate::sys::read_urandom(16).unwrap_or_else(|| (0..16).map(|i| ((crate::sys::now() * 1e6) as u64 >> (i % 8 * 8)) as u8 ^ i as u8).collect());
    u.copy_from_slice(&b[..16]);
    u[6] = u[6] & 0x0f | 0x40;
    u[8] = u[8] & 0x3f | 0x80;
    u
}

/// FriendlyByteBuf.readUtf(256) counts UTF-16 units: a message cut to fit.
pub fn truncate_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

fn write_update(o: &mut Out, offset: i32, bits: [u8; 3], c: i8) {
    o.varint(offset).bytes(&bits).u8(c as u8);
}

impl Game {
    /// A message for the chat (ChatComponent.addMessage): the log, the text lines (g.chat)
    /// and the standard output.
    pub fn add_rich(&mut self, rich: Rich, tag: Tag, sig: Option<&Sig>) {
        let line = rich.legacy();
        crate::say!("[chat] {}", rich.string());
        self.chat.push((line, crate::sys::now_ms()));
        if self.chat.len() > 100 {
            self.chat.remove(0);
        }
        self.talk.push(rich, tag, sig);
    }

    /// The 20 Hz part: key pairs arriving or due, the chat session (setKeyPair), delayed
    /// deletions and the title and action bar timers.
    pub fn talk_tick(&mut self) -> Result<(), String> {
        collect_keys();
        if self.talk.online_mode {
            if self.talk.session.is_some() {
                prepare_keys(false);
            }
            let acct = keys().account.as_ref().map(|a| a.3);
            if let (Some(k), Some(uuid)) = (key_pair(), acct) {
                let gen = keys().generation;
                if uuid == self.uuid && self.talk.session.as_ref().map_or(true, |s| s.generation != gen) {
                    let id = random_uuid();
                    let mut o = Out::new();
                    o.bytes(&id).i64(k.expires_at).varint(k.public_der.len() as i32).bytes(&k.public_der);
                    o.varint(k.signature.len() as i32).bytes(&k.signature);
                    self.conn.send(S_CHAT_SESSION_UPDATE, &o)?;
                    crate::say!("chat session {} sent: chat is signed", crate::wire::uuid_text(&id));
                    self.talk.session = Some(Session { id, next: Some(0), generation: gen });
                }
            } else if self.talk.session.is_some() && key_pair().is_none() {
                // The key expired: what is sent goes unsigned until a new one comes.
                self.talk.session = None;
            }
        }
        // ChatComponent.processMessageDeletionQueue.
        let now = crate::sys::now_ms();
        let due: Vec<Box<Sig>> = {
            let mut d = Vec::new();
            self.talk.deletions.retain(|(s, t)| {
                if *t <= now {
                    d.push(s.clone());
                    false
                } else {
                    true
                }
            });
            d
        };
        for s in due {
            self.delete_message(&s);
        }
        if self.talk.title_ticks > 0 {
            self.talk.title_ticks -= 1;
            if self.talk.title_ticks == 0 {
                self.talk.title = None;
                self.talk.subtitle = None;
            }
        }
        if let Some((_, t)) = self.talk.overlay.as_mut() {
            *t -= 1;
            if *t <= 0 {
                self.talk.overlay = None;
            }
        }
        Ok(())
    }

    /// ChatComponent.deleteMessageOrDelay: a message 3 s old or more becomes the deleted
    /// marker; a newer one is deleted when it is.
    fn delete_message(&mut self, s: &Sig) {
        let now = crate::sys::now_ms();
        let Some(i) = self.talk.msgs.iter().position(|m| m.sig.as_deref() == Some(s)) else { return };
        let at = self.talk.msgs[i].at;
        if now < at + DELETE_AFTER_MS {
            self.talk.deletions.push((Box::new(*s), at + DELETE_AFTER_MS));
            return;
        }
        let marker = Rich::styled(&crate::lang::get("chat.deleted_marker"), 0xAAAAAA, text::ITALIC);
        let off = self.chat.len().saturating_sub(self.talk.msgs.len());
        if let Some(c) = self.chat.get_mut(i + off) {
            c.0 = marker.legacy();
        }
        let m = &mut self.talk.msgs[i];
        m.rich = marker;
        m.tag = Tag::System;
        m.sig = None;
        m.wrapped = None;
    }

    /// markMessageAsProcessed: tracked as seen (when shown), acknowledged after 64.
    fn processed(&mut self, s: &Sig, shown: bool) -> Result<(), String> {
        if self.talk.tracker.add_pending(s, shown) && self.talk.tracker.offset > 64 {
            let n = core::mem::take(&mut self.talk.tracker.offset);
            if n > 0 {
                let mut o = Out::new();
                o.varint(n);
                self.conn.send(S_CHAT_ACK, &o)?;
            }
        }
        Ok(())
    }

    /// ClientPacketListener.sendChat: the last-seen update with every message, the signature
    /// only with a chat session.
    pub fn send_chat(&mut self, text: &str) -> Result<(), String> {
        let content = truncate_utf16(text, 256);
        let ts = now_ms();
        let salt = random_i64();
        let (seen, offset, bits, c) = self.talk.tracker.update();
        let sig = self.sign(&content, ts, salt, &seen);
        let mut o = Out::new();
        o.string(&content).i64(ts).i64(salt);
        match &sig {
            Some(s) => {
                o.bool(true).bytes(s);
            }
            None => {
                o.bool(false);
            }
        }
        write_update(&mut o, offset, bits, c);
        self.conn.send(S_CHAT, &o)
    }

    /// SignedMessageChain.Encoder.pack: the next link's signature, if there is a session.
    fn sign(&mut self, content: &str, ts: i64, salt: i64, seen: &[Sig]) -> Option<Vec<u8>> {
        let kp = key_pair()?;
        let s = self.talk.session.as_mut()?;
        let index = s.next?;
        let payload = signed_payload(&self.uuid, &s.id, index, salt, ts, content, seen);
        let sig = kp.private.sign_sha256(&payload);
        if sig.is_none() {
            crate::warn!("Failed to sign a chat message");
            return None;
        }
        s.next = index.checked_add(1);
        sig
    }

    /// ClientPacketListener.sendCommand: signed when the command has message arguments
    /// (SignableCommand.of over the server's tree), each signed with the same time, salt and
    /// last seen; the unsigned packet otherwise.
    pub fn send_command(&mut self, command: &str) -> Result<(), String> {
        let args = match &self.talk.tree {
            Some(t) => {
                let p = t.parse(command, 0);
                t.signable(command, &p)
            }
            None => Vec::new(),
        };
        if args.is_empty() {
            return self.send_command_unsigned(command);
        }
        let ts = now_ms();
        let salt = random_i64();
        let (seen, offset, bits, c) = self.talk.tracker.update();
        let mut sigs: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, value) in args.iter().take(8) {
            if let Some(s) = self.sign(value, ts, salt, &seen) {
                sigs.push((name.clone(), s));
            }
        }
        let mut o = Out::new();
        o.string(command).i64(ts).i64(salt).varint(sigs.len() as i32);
        for (name, s) in &sigs {
            o.string(name).bytes(s);
        }
        write_update(&mut o, offset, bits, c);
        self.conn.send(S_CHAT_COMMAND_SIGNED, &o)
    }

    /// ServerboundChatCommandPacket: a command sent unsigned whatever it holds (a clicked
    /// command the player confirmed).
    pub fn send_command_unsigned(&mut self, command: &str) -> Result<(), String> {
        let mut o = Out::new();
        o.string(command);
        self.conn.send(S_CHAT_COMMAND, &o)
    }

    /// ChatScreen.handleChatInput: a command without its slash, or chat; kept in the history.
    pub fn send_line(&mut self, line: &str) -> Result<(), String> {
        // ChatScreen.normalizeChatMessage: trimmed, runs of spaces made one, at most 256;
        // characters the server refuses (StringUtil.isAllowedChatCharacter) left out.
        let mut norm = String::new();
        for c in line.trim().chars() {
            if c == '\u{a7}' || c < ' ' || c == '\u{7f}' {
                continue;
            }
            if c.is_whitespace() && norm.ends_with(' ') {
                continue;
            }
            norm.push(if c.is_whitespace() { ' ' } else { c });
        }
        let line = truncate_utf16(&norm, 256);
        if line.is_empty() {
            return Ok(());
        }
        if self.talk.history.last().map_or(true, |l| *l != line) {
            self.talk.history.push(line.clone());
            if self.talk.history.len() > 100 {
                self.talk.history.remove(0);
            }
        }
        match line.strip_prefix('/') {
            Some(cmd) => self.send_command(cmd),
            None => self.send_chat(&line),
        }
    }

    /// ServerboundCommandSuggestionPacket: the server's suggestions for `text` (with its
    /// slash) up to the cursor. Returns the request id.
    pub fn ask_suggestions(&mut self, text: &str) -> Result<i32, String> {
        self.talk.suggest_id += 1;
        let id = self.talk.suggest_id;
        let mut o = Out::new();
        o.varint(id).string(&truncate_utf16(text, 32500));
        self.conn.send(S_COMMAND_SUGGESTION, &o)?;
        Ok(id)
    }

    /// The packets this module reads. Returns true when it handled `p` completely.
    pub fn talk_packet(&mut self, p: &crate::conn::Packet) -> Result<bool, String> {
        let mut r = p.reader();
        match p.id {
            C_LOGIN => {
                self.login_info(&mut r);
                Ok(false)
            }
            C_SYSTEM_CHAT => {
                let t = text::read(&mut r);
                let overlay = r.bool().unwrap_or(false);
                if overlay {
                    // Gui.setOverlayMessage: 3 s.
                    self.talk.overlay = Some((t, 60));
                } else {
                    self.add_rich(t, Tag::SystemSinglePlayer, None);
                }
                Ok(true)
            }
            C_DISGUISED_CHAT => {
                let msg = text::read(&mut r);
                if let Some(b) = read_bound(&mut r) {
                    let d = b.decorate(&msg);
                    self.add_rich(d, Tag::System, None);
                }
                Ok(true)
            }
            C_PLAYER_CHAT => {
                self.player_chat(&mut r)?;
                Ok(true)
            }
            C_DELETE_CHAT => {
                if let Some(Some(s)) = self.talk.cache.unpack(&mut r) {
                    self.talk.tracker.ignore_pending(&s);
                    self.delete_message(&s);
                }
                Ok(true)
            }
            C_PLAYER_INFO_UPDATE => {
                self.chat_sessions(&mut r);
                Ok(false)
            }
            C_PLAYER_INFO_REMOVE => {
                let n = r.varint().unwrap_or(0);
                for _ in 0..n {
                    let Some(u) = r.uuid() else { break };
                    self.talk.players.remove(&u);
                    self.talk.names.remove(&u);
                }
                Ok(false)
            }
            C_COMMANDS => {
                self.talk.tree = crate::commands::Tree::read(&mut r);
                if self.talk.tree.is_none() {
                    crate::warn!("the server's command tree is unreadable");
                }
                Ok(true)
            }
            C_COMMAND_SUGGESTIONS => {
                let id = r.varint().unwrap_or(-1);
                let start = r.varint().unwrap_or(0).max(0) as usize;
                let len = r.varint().unwrap_or(0).max(0) as usize;
                let n = r.varint().unwrap_or(0).max(0) as usize;
                let mut v = Vec::new();
                for _ in 0..n.min(1000) {
                    let Some(t) = r.string() else { break };
                    let tooltip = if r.bool() == Some(true) { Some(text::read(&mut r)) } else { None };
                    v.push(Suggestion { text: t, tooltip });
                }
                self.talk.suggestions = Some((id, start, len, v));
                Ok(true)
            }
            C_SET_TITLE_TEXT => {
                self.talk.title = Some(text::read(&mut r));
                let (a, b, c) = self.talk.title_times;
                self.talk.title_ticks = a + b + c;
                Ok(true)
            }
            C_SET_SUBTITLE_TEXT => {
                self.talk.subtitle = Some(text::read(&mut r));
                Ok(true)
            }
            C_SET_ACTION_BAR_TEXT => {
                self.talk.overlay = Some((text::read(&mut r), 60));
                Ok(true)
            }
            C_SET_TITLES_ANIMATION => {
                let (i, s, o) = (r.i32().unwrap_or(10), r.i32().unwrap_or(70), r.i32().unwrap_or(20));
                let t = &mut self.talk;
                if i >= 0 {
                    t.title_times.0 = i;
                }
                if s >= 0 {
                    t.title_times.1 = s;
                }
                if o >= 0 {
                    t.title_times.2 = o;
                }
                if t.title_ticks > 0 {
                    t.title_ticks = t.title_times.0 + t.title_times.1 + t.title_times.2;
                }
                Ok(true)
            }
            C_CLEAR_TITLES => {
                let reset = r.bool().unwrap_or(false);
                let t = &mut self.talk;
                t.title = None;
                t.subtitle = None;
                t.title_ticks = 0;
                if reset {
                    t.title_times = (10, 70, 20);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// ClientboundLoginPacket's end: online mode and enforcesSecureChat; a new connection's
    /// secure chat state (and prepareKeyPair).
    fn login_info(&mut self, r: &mut In) {
        let mut go = || -> Option<(bool, bool)> {
            r.i32()?;
            r.bool()?;
            let n = r.varint()?;
            for _ in 0..n {
                r.string()?;
            }
            r.varint()?;
            r.varint()?;
            r.varint()?;
            r.bool()?;
            r.bool()?;
            r.bool()?;
            r.varint()?;
            r.string()?;
            r.i64()?;
            r.varint()?;
            r.varint()?;
            r.bool()?;
            r.bool()?;
            if r.bool()? {
                r.string()?;
                r.i64()?;
            }
            r.varint()?;
            r.varint()?;
            Some((r.bool()?, r.bool()?))
        };
        let (online, enforce) = go().unwrap_or((false, false));
        let t = &mut self.talk;
        t.online_mode = online;
        t.enforces_secure_chat = enforce;
        t.session = None;
        t.tracker = Tracker::new();
        t.cache = Cache((0..128).map(|_| None).collect());
        t.next_index = 0;
        if online {
            prepare_keys(true);
        }
    }

    /// ClientboundPlayerInfoUpdatePacket's INITIALIZE_CHAT: each player's chat session.
    fn chat_sessions(&mut self, r: &mut In) {
        let mut go = || -> Option<()> {
            let actions = r.u8()?;
            let n = r.varint()?;
            for _ in 0..n {
                let uuid = r.uuid()?;
                if actions & 1 != 0 {
                    let name = r.string()?;
                    self.talk.names.insert(uuid, name);
                    let props = r.varint()?;
                    for _ in 0..props {
                        r.string()?;
                        r.string()?;
                        if r.bool()? {
                            r.string()?;
                        }
                    }
                    self.talk.players.entry(uuid).or_insert(Validator { session: None, chain_valid: true, last: None });
                }
                if actions & 2 != 0 {
                    let session = if r.bool()? {
                        let mut id = [0u8; 16];
                        id.copy_from_slice(r.take(16)?);
                        let expires = r.i64()?;
                        let n = r.varint()?.max(0) as usize;
                        let key = r.take(n)?.to_vec();
                        let n = r.varint()?.max(0) as usize;
                        r.take(n)?;
                        RsaKey::from_spki(&key).map(|k| (id, k, expires))
                    } else {
                        None
                    };
                    let v = self.talk.players.entry(uuid).or_insert(Validator { session: None, chain_valid: true, last: None });
                    // PlayerInfo.setChatSession / clearChatSession (fallback validator).
                    *v = Validator { session, chain_valid: true, last: None };
                }
                if actions & 4 != 0 {
                    r.varint()?;
                }
                if actions & 8 != 0 {
                    r.bool()?;
                }
                if actions & 16 != 0 {
                    r.varint()?;
                }
                if actions & 32 != 0 && r.bool()? {
                    text::read_tag(r)?;
                }
                if actions & 64 != 0 {
                    r.varint()?;
                }
                if actions & 128 != 0 {
                    r.bool()?;
                }
            }
            Some(())
        };
        go();
    }

    /// ClientPacketListener.handlePlayerChat and ChatListener.handlePlayerChatMessage.
    fn player_chat(&mut self, r: &mut In) -> Result<(), String> {
        let Some(m) = read_player_chat(r, &self.talk.cache) else {
            crate::warn!("unreadable player chat packet");
            return Ok(());
        };
        if m.global != self.talk.next_index {
            crate::warn!("Missing or out-of-order chat message from server, expected index {} but got {}", self.talk.next_index, m.global);
        }
        self.talk.next_index = m.global + 1;
        if m.unknown_sig {
            crate::warn!("Message from player referenced unrecognized signature id");
        }
        self.talk.cache.push(&m.last_seen, m.sig.as_ref());
        let enforce = self.talk.enforces_secure_chat;
        let local = m.sender == self.uuid;
        // SignedMessageValidator.updateAndValidate: None is a validation error.
        let valid: Option<Option<Sig>> = match self.talk.players.get_mut(&m.sender) {
            None => None,
            Some(v) => match &v.session {
                None => {
                    if enforce {
                        None
                    } else {
                        Some(None) // ACCEPT_UNSIGNED: the signature is dropped
                    }
                }
                Some((sid, key, expires)) => {
                    let ok = v.chain_valid
                        && *expires + GRACE_MS > now_ms()
                        && m.sig.as_ref().map_or(false, |s| {
                            let payload = signed_payload(&m.sender, sid, m.index, m.salt, m.ts, &m.content, &m.last_seen);
                            key.verify_sha256(&payload, s)
                        })
                        && match (&v.last, &m.sig) {
                            (None, _) => true,
                            (Some((li, ls)), Some(s)) => (*li == m.index && ls == s) || m.index > *li,
                            _ => false,
                        };
                    v.chain_valid = ok;
                    if ok {
                        v.last = m.sig.map(|s| (m.index, s));
                        Some(m.sig)
                    } else {
                        None
                    }
                }
            },
        };
        let Some(sig) = valid else {
            // ChatListener.handleChatMessageError.
            if let Some(s) = m.sig {
                self.processed(&s, false)?;
            }
            let err = Rich::styled(&crate::lang::get("chat.validation_error"), 0xFF5555, text::ITALIC);
            let d = m.bound.decorate(&err);
            self.add_rich(d, Tag::Error, m.sig.as_ref());
            return Ok(());
        };
        // FilterMask: 1 fully filtered (not shown), 2 some characters hidden.
        let shown_content = match m.filter {
            1 => None,
            2 => Some(mask(&m.content, &m.mask)),
            _ => Some(m.content.clone()),
        };
        let mut shown = false;
        if let Some(content) = shown_content {
            let decorated = if m.filter == 2 {
                Rich::plain(&content)
            } else {
                m.unsigned.clone().unwrap_or_else(|| Rich::plain(&m.content))
            };
            let line = m.bound.decorate(&decorated);
            // ChatTrustLevel.evaluate (the local player's own messages are secure).
            let tag = if local {
                Tag::None
            } else if sig.is_none() || m.ts + EXPIRES_AFTER_CLIENT_MS < now_ms() {
                Tag::NotSecure
            } else if !line.string().contains(&m.content) || m.unsigned.as_ref().map_or(false, |u| u.runs.iter().any(|r| r.0.font != 0)) {
                Tag::Modified(m.content.clone())
            } else {
                Tag::None
            };
            self.add_rich(line, tag, sig.as_ref());
            shown = true;
        }
        if let Some(s) = sig {
            self.processed(&s, shown)?;
        }
        Ok(())
    }
}

/// FilterMask.apply: the hidden characters as '#'.
fn mask(s: &str, bits: &[i64]) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| if bits.get(i / 64).map_or(false, |w| (*w >> (i % 64)) & 1 != 0) { '#' } else { c })
        .collect()
}

struct PlayerChat {
    global: i32,
    sender: [u8; 16],
    index: i32,
    sig: Option<Sig>,
    content: String,
    ts: i64,
    salt: i64,
    last_seen: Vec<Sig>,
    unknown_sig: bool,
    unsigned: Option<Rich>,
    filter: i32,
    mask: Vec<i64>,
    bound: Bound,
}

fn read_player_chat(r: &mut In, cache: &Cache) -> Option<PlayerChat> {
    let global = r.varint()?;
    let sender = r.uuid()?;
    let index = r.varint()?;
    let sig = if r.bool()? {
        let mut s = [0u8; 256];
        s.copy_from_slice(r.take(256)?);
        Some(s)
    } else {
        None
    };
    let content = r.string()?;
    let ts = r.i64()?;
    let salt = r.i64()?;
    let n = r.varint()?.clamp(0, 20);
    let mut last_seen = Vec::new();
    let mut unknown_sig = false;
    for _ in 0..n {
        match cache.unpack(r)? {
            Some(s) => last_seen.push(s),
            None => unknown_sig = true,
        }
    }
    let unsigned = if r.bool()? { Some(text::read(r)) } else { None };
    let filter = r.varint()?;
    let mut mask = Vec::new();
    if filter == 2 {
        let n = r.varint()?.clamp(0, 1024);
        for _ in 0..n {
            mask.push(r.i64()?);
        }
    }
    let bound = read_bound(r)?;
    Some(PlayerChat { global, sender, index, sig, content, ts, salt, last_seen, unknown_sig, unsigned, filter, mask, bound })
}

// ------------------------------------------------------------------ checks

const PKCS8: &str = "30820275020100300d06092a864886f70d01010105000482025f3082025b02010002818100a483a9a2ce1762edb78b7359b96415e35885db2cebb0615d36ce314623a5c3ddb5fe1dccdb6066c4dfe16125baf73ca48b4658285da6adf1ccf0bd18be0f67e3a1d68e9547fc129b6d3fe74413ed5f987123f556b54a8c3a434a0fd4a02d4599d73fbdbf403bc229ed3c6e45877fbb2a20fd3850f1a19c5adbefb703eac785eb02030100010281803e646cb68100df9516523dea1e6475e4a61305ffb0575165bfbe4d73b35b4fc04b2057fef48848118c72f69c0f9f6a9160fcc9bc6d6024821ade8668d6d412eae70f2c19d1890877c4fe46aebbbf2324ad3f304dfc7bd6ec484befe4730cc72c749a557c0d0bb6a73093ef571e4fc543ff5a5ec146e9799ca98f9a71b6c08831024100d5e071afe69c04174eea3023c16a3226a577a3ae01c0166ce68dac20791c75aec47ee115ab52a86ae0a7860ba87bfb6f37a8754d72feec7073eff4d6485dd2b3024100c4ea63df21d6711e15a431400a84c127acb6a77c22f475fc807be941a6a80fc0edd4bab09f09a157e517580777c2caf832e5fa07375fc8f3f1286e9f9e9ebbe90240609199e37975a2673fcb7ff6f502d5281bf83c207a4bd8144db95429c9656f417d1968da16e4188541708296caf9481afc7f848f76c7e1cc189d14e897fe3cff024043ba4e817774f7511ea66b9e184012e35f7f2f59fdf7d100f8fc257d4421f7d126bc0d3e24653e8e29a87ec98ac27fe72032836da9f912f7fc5128c353de4ac902401a16b013e8717963d6f938b0126db15eecc678ccd0b3b8497aa6528d522f89087112694fd5082b9838ba162de4831589195284a6aa24711fc7d54ec073dfddf1";
const PKCS8_SPKI: &str = "30819f300d06092a864886f70d010101050003818d0030818902818100a483a9a2ce1762edb78b7359b96415e35885db2cebb0615d36ce314623a5c3ddb5fe1dccdb6066c4dfe16125baf73ca48b4658285da6adf1ccf0bd18be0f67e3a1d68e9547fc129b6d3fe74413ed5f987123f556b54a8c3a434a0fd4a02d4599d73fbdbf403bc229ed3c6e45877fbb2a20fd3850f1a19c5adbefb703eac785eb0203010001";
const PKCS8_SIG_CHAT: &str = "3a6ed84b109a293d92a53b2120925f3fa33d3b81b686e9a9f0f1f935bd80a263b0912cf0bcd5ac5ca14ffcd3301a23cb2c3d828a092bb49cc8831a3c1f9be674a44a7c906dd8d48e00edcf7c899a75b19e2eea6d94f868507005448692f8c500192268a286988a025c09ad8fa99ffb50de52c2e892310111c22cdb8b3ac4eb8a";
/// A 2048-bit chat key as the services hand them out (PKCS#8 with CRT values).
const CHAT_KEY_PKCS8: &str = "308204bc020100300d06092a864886f70d0101010500048204a6308204a20201000282010100bebf25d067e367ba25767067c0464701a109a37699f90a49e413cfba58694c4e51c317e43ad90b171f08dae88aabe0f51958871becf2e62e65282fbb256b70247aa0a7f186b044db793c379d1a332ccc5b50832b8564cf31282d3229abc982439fa3d8085824a8705b1d0d2a4323e300f1ce266f82f09282bbb77ebcd02da8d11c5f118141671497c3e08201e26764f681f50c84ab1475d57bfdc3f648431dcf9c2fa9e215002093ca4c33fc6986bc7e5e430ee4fb269e6b4d04bfd6bbd9d50c9ded5d04325d298815c22334a31e156eeb18aecffa2f64899c6d344723f93691531d816b440fd12a6ede94cdcfa2b86dd32077ae1be0e39c456b5d2d06aee9350203010001028201001bd2874a3b010b3df65d6c7f61a70e8fe104a62474488454eb50ed758d944c56472c49059804d50e8a0f11592672f424e40dcae54a9e526f557f7358dd982a7b13acefda35ae6bd8c47a0155a7beebaba22d1ff8dee3e9a9cea5ab01b38638bb32e3a3ecf1a8652e40480045d815b9b48750adc009ae1bfcb8334767f488986d2978982f12a06f9763bf9eebfdeb60f617521b0bce0756ec379cab2a22d00176b058fabca90175860f66a0634a0dd4219eba51c8c43ab3abbd2d2de4466bef6a815d84585f3cd76420bceaacd15be37fd9e957869ca25d0644c50e5ad399c05359b455b885ed135d3a63b1f80705def62a29cab8904f5ae5fd0f8cfcc9f2048902818100e0611a0d0009e190af7a99e9070ddcf127b41b578515888e18c555488567c2b3ebf970695e44fc7755580245b191fda81a8f670475a4048304652fb7ea9003b2b9985fafbac9f59695132bc6d6e35a3503f407ac2e6efa8dd736d617fad4946c80aabdcf06ab67da7a5cc4a0d69d9320964dc618feb2fc2cfb39c2e11bd1295902818100d9a0afc08108ba70c0a46bb82f1d8beb12cfd50a106d5483fbdf17a7919e0e1373a4c05c4287256faf7be8c42f1788ec3dd33abead70c4a67ec30ef930800f1bb34f5a762946e6fa3c7e296356d1a1059a7068d2bdac29ef4bd5cd25203ffc1a5d10a9bb1d15f6c72e5c830ff9fe5d2f445ce3da61693e6353a8d8fca7f7a73d02818040ef18f0e62ff5dd3f4b45bb50554cb9657a13e79216173cb8227379d7519de11fcb4222dc2b2ebd022fc4089348e295818a3a8fe9d39b92a6f4f9884a137f5e7ba3831d81faec5db5aaa4a29891970b7ca7648909af0847801ed87a7c168515996ed01b579cc7234d0207cd5c0ea2036e3bf7b6480f58375444f506dc5bfd91028180276aa12a7eb47400440403b6a261f99ba4169c816c2441307e8813a2b80f2681836a8da4a8300e29c260f0f1c78dbdb0fc29abf42939c984566541eb044bcd70b8f36071125cd32cf393ee2f1de46e03b3cb345127256d9af7e34b7ab3f2a2d8ab8e7f0f72df2d8939ee6b969332ecf1c8b2025ae555c884f8fc1600fcf330090281802985deefe6ccc4ceb42bbb1ce9a2ecacea4974f5ba8090ba33049e36d0c43a7c9be2da95fc0bf2c7fb0ec2ee73ee628bfda786afa68c75fa08d996b6c1674a759f7f1ec67d95206d69c4b28234b55933748b7930feb9818fa6944bdbf0258894164706d498d2cf29e241fe1b99ec94021cd29ed8a528ee5cf4fe6e6ddcbd1d97";
/// A PKCS#1 key without CRT values (n, e, d) and OpenSSL's SHA256withRSA of "hello".
const TEST_N: &str = "ab026074f92450a8f726f3895bb11176ca8f065f3100cd4143bac4f7d3674edcff95b41a0525d2365a549c106ec6685881b392177420fd31ff5c144ff11ba24311e751cd2c6b5d1817b6af6686a43c95d61243613005e6945b6093cd95223cec5e8856826da9d2ee1b323362ac1cebeab3659cbc45319a9b540dda816a4798ff";
const TEST_D: &str = "172bcb460e3c0d71d78b3f946d082105a78c99a06c535f7c6860f62f5e45b139e0a7b82f8e630188bbd49b10def1a351dac12a1a6371d4dce16ce893ac67c2247b9ee13e3a840abaf9ea685304558f1a93c4f354193f5713c3e53bd10dd275300db36e764fc081549afbd30cd00c31e6eb726720337a963913d96b50e4d8fd1";
const TEST_SIG_HELLO: &str = "0bc8716622e44f9bf72432045e91bf1b40a96aae3c402fa86262b83758db1c90d2ed16b9fa86f29af175f2e6eedb864bf8da2667a7ada2bf5433fba36c1951af0c356e45bd5f90fb905b9a08f33a0666c4968ccffbf32fda643d736deec66f82c75fe4a8075c7f2b8a6c0ca630e0ffe4805a1f1d672506b82fc188510fb4ef80";

fn unhex(s: &str) -> Vec<u8> {
    // An odd number of digits has its leading zero left out.
    let padded;
    let s = if s.len() % 2 == 1 {
        padded = alloc::format!("0{}", s);
        padded.as_str()
    } else {
        s
    };
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0)).collect()
}

fn hex(b: &[u8]) -> String {
    let mut s = String::new();
    for x in b {
        s.push_str(&alloc::format!("{:02x}", x));
    }
    s
}

fn der_int(v: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    if v.first().map_or(true, |b| *b & 0x80 != 0) {
        body.push(0);
    }
    body.extend_from_slice(v);
    der_el(0x02, &body)
}

fn der_el(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut o = alloc::vec![tag];
    if body.len() < 0x80 {
        o.push(body.len() as u8);
    } else if body.len() < 0x100 {
        o.extend_from_slice(&[0x81, body.len() as u8]);
    } else {
        o.extend_from_slice(&[0x82, (body.len() >> 8) as u8, body.len() as u8]);
    }
    o.extend_from_slice(body);
    o
}

fn encode64(d: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    out.push(("SHA-256 abc", hex(&crate::crypto::sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    let long: Vec<u8> = (0..1000u32).map(|i| (i * 31 % 256) as u8).collect();
    let mut h = crate::crypto::Sha256::new();
    for c in long.chunks(37) {
        h.update(c);
    }
    out.push(("SHA-256 in pieces", alloc::format!("{}", h.finish() == crate::crypto::sha256(&long)), "true"));
    out.push(("SHA-256 empty", hex(&crate::crypto::sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    let key = RsaPrivate::from_der(&unhex(PKCS8));
    let sig = key.as_ref().and_then(|k| k.sign_sha256(b"chat")).map(|s| hex(&s)).unwrap_or_default();
    out.push(("RSA sign (PKCS#8, CRT)", alloc::format!("{}", sig == PKCS8_SIG_CHAT), "true"));
    let public = RsaKey::from_spki(&unhex(PKCS8_SPKI));
    let ok = public.as_ref().map_or(false, |p| p.verify_sha256(b"chat", &unhex(PKCS8_SIG_CHAT)) && !p.verify_sha256(b"chaT", &unhex(PKCS8_SIG_CHAT)));
    out.push(("RSA verify", alloc::format!("{}", ok), "true"));
    let body = [der_int(&[0]), der_int(&unhex(TEST_N)), der_int(&[1, 0, 1]), der_int(&unhex(TEST_D))].concat();
    let k1 = RsaPrivate::from_der(&der_el(0x30, &body));
    let sig = k1.and_then(|k| k.sign_sha256(b"hello")).map(|s| hex(&s)).unwrap_or_default();
    let k2 = RsaPrivate::from_der(&unhex(CHAT_KEY_PKCS8));
    let ok = k2.as_ref().and_then(|k| k.sign_sha256(b"secure chat").map(|s| k.public.verify_sha256(b"secure chat", &s) && s.len() == 256)).unwrap_or(false);
    out.push(("RSA-2048 sign and verify", alloc::format!("{}", ok), "true"));
    out.push(("RSA sign (PKCS#1, no CRT)", alloc::format!("{}", sig == TEST_SIG_HELLO), "true"));
    // A signed message's bytes (Horizon's payload_layout test).
    let p = signed_payload(&[0; 16], &[1; 16], 3, -2, 1_700_000_123_456, "h\u{e9}", &[[7u8; 256]]);
    out.push(("chat signature payload", alloc::format!("{} {}", p.len(), hex(&p[36..63])), "323 00000003fffffffffffffffe000000006553f17b0000000368c3a9"));
    out.push(("last seen checksum", alloc::format!("{} {}", checksum(&[]), java_hash(&[0xff])), "1 30"));
    let mut t = Tracker::new();
    t.add_pending(&[1; 256], true);
    t.add_pending(&[1; 256], true);
    t.add_pending(&[2; 256], false);
    t.add_pending(&[3; 256], true);
    let (seen, offset, bits, _) = t.update();
    out.push(("last seen update", alloc::format!("{} {} {:02x}{:02x}{:02x}", seen.len(), offset, bits[0], bits[1], bits[2]), "2 3 00000a"));
    let mut c = Cache((0..128).map(|_| None).collect());
    c.push(&[[1; 256]], Some(&[2; 256]));
    c.push(&[], Some(&[3; 256]));
    let pos = |s: u8| c.0.iter().position(|e| e.as_deref() == Some(&[s; 256]));
    out.push(("signature cache order", alloc::format!("{:?} {:?} {:?}", pos(3), pos(2), pos(1)), "Some(0) Some(1) Some(2)"));
    let certs = alloc::format!(
        "{}\n{}\nc2ln\n1700000000000\n1690000000000\n",
        encode64(&unhex(PKCS8)),
        encode64(&unhex(PKCS8_SPKI))
    );
    let kp = parse_certificates(&certs);
    out.push(("relay certificates answer", alloc::format!("{:?}", kp.as_ref().map(|k| (k.expires_at, k.signature.len())).map_err(|e| e.clone())), "Ok((1700000000000, 3))"));
    out
}
