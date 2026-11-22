//! The multiplayer server list as vanilla keeps it: servers.dat in the data folder (ServerList,
//! ServerData: uncompressed NBT, `servers` a list of compounds with name, ip, icon,
//! acceptTextures, hidden), the server-list ping (ServerStatusPinger: handshake, status
//! request, the JSON answer, then the ping), and LAN worlds (LanServerDetection: UDP
//! multicast 224.0.2.60:4445).

use crate::conn::Conn;
use crate::json::{self, J};
use crate::sys;
use crate::wire::{In, Out};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

/// The resource pack setting (ServerData.ServerPackStatus): stored as acceptTextures, absent
/// for Prompt.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pack {
    Enabled,
    Disabled,
    Prompt,
}

impl Pack {
    pub fn key(self) -> &'static str {
        match self {
            Pack::Enabled => "manageServer.resourcePack.enabled",
            Pack::Disabled => "manageServer.resourcePack.disabled",
            Pack::Prompt => "manageServer.resourcePack.prompt",
        }
    }
    /// The next value of the cycle button (ServerPackStatus.values() order).
    pub fn next(self, back: bool) -> Pack {
        const ALL: [Pack; 3] = [Pack::Enabled, Pack::Disabled, Pack::Prompt];
        let i = ALL.iter().position(|p| *p == self).unwrap_or(2) as i32;
        ALL[(i + if back { 2 } else { 1 }).rem_euclid(3) as usize]
    }
}

/// The server being joined's resource pack setting, for the resource pack code to honour
/// (ClientPackSource / ServerPackManager: Enabled accepts, Disabled declines, Prompt asks).
static JOIN_PACK: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(2);

pub fn set_join_pack(p: Pack) {
    JOIN_PACK.store(p as u8, Ordering::Relaxed);
}

pub fn join_pack() -> Pack {
    match JOIN_PACK.load(Ordering::Relaxed) {
        0 => Pack::Enabled,
        1 => Pack::Disabled,
        _ => Pack::Prompt,
    }
}

#[derive(Clone)]
pub struct Server {
    pub name: String,
    pub ip: String,
    /// The favicon PNG the last ping gave (servers.dat keeps it, base64).
    pub icon: Option<Vec<u8>>,
    pub pack: Pack,
    pub code_of_conduct: i32,
}

impl Server {
    pub fn new(name: &str, ip: &str) -> Server {
        Server { name: String::from(name), ip: String::from(ip), icon: None, pack: Pack::Prompt, code_of_conduct: 0 }
    }
}

/// ServerList: the servers shown, and the hidden ones Direct Connection remembers (at most 16,
/// newest first).
#[derive(Default)]
pub struct List {
    pub servers: Vec<Server>,
    pub hidden: Vec<Server>,
}

pub fn path() -> String {
    format!("{}/servers.dat", crate::data_dir())
}

// ------------------------------------------------------------------ NBT (big-endian, named root)

enum Tag {
    Str(String),
    Num(i64),
    Bytes(Vec<u8>),
    List(Vec<Tag>),
    Comp(Vec<(String, Tag)>),
    Other,
}

fn read_tag(r: &mut In, t: u8, depth: u32) -> Option<Tag> {
    if depth > 32 {
        return None;
    }
    Some(match t {
        1 => Tag::Num(r.u8()? as i8 as i64),
        2 => Tag::Num(r.u16()? as i16 as i64),
        3 => Tag::Num(r.i32()? as i64),
        4 => Tag::Num(r.i64()?),
        5 => {
            r.take(4)?;
            Tag::Other
        }
        6 => {
            r.take(8)?;
            Tag::Other
        }
        7 => {
            let n = r.i32()?.max(0) as usize;
            Tag::Bytes(r.take(n)?.to_vec())
        }
        8 => {
            let n = r.u16()? as usize;
            Tag::Str(String::from_utf8_lossy(r.take(n)?).into_owned())
        }
        9 => {
            let et = r.u8()?;
            let n = r.i32()?.max(0);
            if et == 0 && n > 0 {
                return None;
            }
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(read_tag(r, et, depth + 1)?);
            }
            Tag::List(v)
        }
        10 => {
            let mut v = Vec::new();
            loop {
                let ct = r.u8()?;
                if ct == 0 {
                    break;
                }
                let n = r.u16()? as usize;
                let name = String::from_utf8_lossy(r.take(n)?).into_owned();
                v.push((name, read_tag(r, ct, depth + 1)?));
            }
            Tag::Comp(v)
        }
        11 => {
            let n = r.i32()?.max(0) as usize;
            r.take(n * 4)?;
            Tag::Other
        }
        12 => {
            let n = r.i32()?.max(0) as usize;
            r.take(n * 8)?;
            Tag::Other
        }
        _ => return None,
    })
}

fn field<'a>(c: &'a [(String, Tag)], k: &str) -> Option<&'a Tag> {
    c.iter().find(|(n, _)| n == k).map(|(_, t)| t)
}

fn put_name(o: &mut Out, t: u8, name: &str) {
    o.u8(t).u16(name.len() as u16).bytes(name.as_bytes());
}

fn put_str(o: &mut Out, name: &str, v: &str) {
    put_name(o, 8, name);
    let b = v.as_bytes();
    let n = b.len().min(65535);
    o.u16(n as u16).bytes(&b[..n]);
}

/// Base64 (RFC 4648, padded), as ExtraCodecs.BASE64_STRING writes it.
pub fn encode64(d: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((d.len() + 2) / 3 * 4);
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

impl List {
    /// ServerList.load: what servers.dat holds (nothing when it is missing or damaged).
    pub fn load() -> List {
        let mut l = List::default();
        let Some(d) = sys::read_file(&path()) else { return l };
        l.parse(&d);
        l
    }

    fn parse(&mut self, d: &[u8]) {
        let mut r = In::new(d);
        if r.u8() != Some(10) {
            return;
        }
        let Some(n) = r.u16() else { return };
        if r.take(n as usize).is_none() {
            return;
        }
        let Some(Tag::Comp(root)) = read_tag(&mut r, 10, 0) else { return };
        let Some(Tag::List(list)) = field(&root, "servers") else { return };
        for t in list {
            let Tag::Comp(c) = t else { continue };
            let s = |k| match field(c, k) {
                Some(Tag::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let num = |k| match field(c, k) {
                Some(Tag::Num(n)) => Some(*n),
                _ => None,
            };
            let mut sv = Server::new(&s("name"), &s("ip"));
            sv.icon = match field(c, "icon") {
                Some(Tag::Str(b)) => crate::skins::base64(b),
                Some(Tag::Bytes(b)) => Some(b.clone()),
                _ => None,
            };
            sv.pack = match num("acceptTextures") {
                Some(0) => Pack::Disabled,
                Some(_) => Pack::Enabled,
                None => Pack::Prompt,
            };
            sv.code_of_conduct = num("acceptedCodeOfConduct").unwrap_or(0) as i32;
            if num("hidden").unwrap_or(0) != 0 {
                self.hidden.push(sv);
            } else {
                self.servers.push(sv);
            }
        }
    }

    /// The file as ServerList.save writes it.
    pub fn bytes(&self) -> Vec<u8> {
        let mut o = Out::new();
        put_name(&mut o, 10, "");
        put_name(&mut o, 9, "servers");
        o.u8(10).i32((self.servers.len() + self.hidden.len()) as i32);
        for (sv, hidden) in self.servers.iter().map(|s| (s, false)).chain(self.hidden.iter().map(|s| (s, true))) {
            put_str(&mut o, "name", &sv.name);
            put_str(&mut o, "ip", &sv.ip);
            if let Some(icon) = &sv.icon {
                put_str(&mut o, "icon", &encode64(icon));
            }
            if sv.pack != Pack::Prompt {
                put_name(&mut o, 1, "acceptTextures");
                o.u8((sv.pack == Pack::Enabled) as u8);
            }
            if sv.code_of_conduct != 0 {
                put_name(&mut o, 3, "acceptedCodeOfConduct");
                o.i32(sv.code_of_conduct);
            }
            put_name(&mut o, 1, "hidden");
            o.u8(hidden as u8);
            o.u8(0);
        }
        o.u8(0);
        o.b
    }

    /// Writes servers.dat (through servers.dat_tmp, as vanilla keeps a whole file).
    pub fn save(&self) {
        let p = path();
        let tmp = format!("{}_tmp", p);
        if sys::write_file_private(&tmp, &self.bytes()) {
            sys::rename_file(&tmp, &p);
        } else {
            crate::warn!("cannot write {}", p);
        }
    }

    /// ServerList.add with hidden: the front of the remembered list, at most 16.
    pub fn add_hidden(&mut self, s: Server) {
        self.hidden.insert(0, s);
        self.hidden.truncate(16);
    }

    /// ServerList.unhide: a remembered Direct Connection address becomes a listed server.
    pub fn unhide(&mut self, ip: &str) -> Option<Server> {
        let i = self.hidden.iter().position(|s| s.ip == ip)?;
        let s = self.hidden.remove(i);
        self.servers.push(s.clone());
        Some(s)
    }

    pub fn find(&self, ip: &str) -> bool {
        self.servers.iter().chain(self.hidden.iter()).any(|s| s.ip == ip)
    }
}

// ------------------------------------------------------------------ the ping

/// What a server answered (ServerData after ServerStatusPinger.pingServer).
pub struct Status {
    pub motd: String,
    /// Players online and the most allowed (None: the server does not say).
    pub players: Option<(i32, i32)>,
    pub sample: Vec<String>,
    pub version: String,
    pub protocol: i32,
    pub ping: u64,
    /// The favicon PNG.
    pub icon: Option<Vec<u8>>,
}

pub enum Ping {
    Ok(Status),
    /// "Can't resolve hostname" / "Can't connect to server".
    CantResolve,
    CantConnect,
}

/// ServerAddress.parseString: host and port (25565 when none is given, [v6]:port).
pub fn address(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if tail.is_empty() => 25565,
            None => return None,
        };
        return Some((String::from(h), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => {
            if h.is_empty() {
                return None;
            }
            Some((String::from(h), p.parse().ok()?))
        }
        Some(_) => Some((String::from(s), 25565)), // a bare IPv6 address
        None => Some((String::from(s), 25565)),
    }
}

/// ServerAddress.isValidAddress.
pub fn valid(s: &str) -> bool {
    address(s).map_or(false, |(h, _)| !h.contains(' ') && !h.is_empty())
}

pub fn ping(ip: &str) -> Ping {
    let Some((host, port)) = address(ip) else { return Ping::CantResolve };
    let mut c = match Conn::open(&host, port) {
        Ok(c) => c,
        Err(e) if e.starts_with("cannot resolve") => return Ping::CantResolve,
        Err(_) => return Ping::CantConnect,
    };
    let mut o = Out::new();
    o.varint(crate::ids::PROTOCOL).string(&host).u16(port).varint(1);
    if c.send(0, &o).is_err() || c.send(0, &Out::new()).is_err() {
        return Ping::CantConnect;
    }
    let Ok(Some(p)) = c.recv(10000) else { return Ping::CantConnect };
    let Some(text) = p.reader().string() else { return Ping::CantConnect };
    let Some(j) = json::parse(&text) else { return Ping::CantConnect };
    let t0 = sys::now_ms();
    let mut o = Out::new();
    o.i64(t0 as i64);
    let pong = c.send(1, &o).is_ok() && matches!(c.recv(5000), Ok(Some(_)));
    let ping = if pong { sys::now_ms() - t0 } else { 0 };
    Ping::Ok(status_of(&j, ping))
}

pub fn status_of(j: &J, ping: u64) -> Status {
    let motd = j.get("description").map(json::text).unwrap_or_default();
    let version = j.get("version").and_then(|v| v.get("name")).and_then(|n| n.str()).map(String::from).unwrap_or_else(|| crate::lang::get("multiplayer.status.old"));
    let protocol = j.get("version").and_then(|v| v.get("protocol")).and_then(|n| n.num()).map_or(-1, |n| n as i32);
    let players = j.get("players").map(|p| {
        (p.get("online").and_then(|n| n.num()).unwrap_or(0.0) as i32, p.get("max").and_then(|n| n.num()).unwrap_or(0.0) as i32)
    });
    let sample = j
        .get("players")
        .and_then(|p| p.get("sample"))
        .map(|s| s.arr().iter().filter_map(|e| e.get("name").and_then(|n| n.str()).map(String::from)).collect())
        .unwrap_or_default();
    let icon = j.get("favicon").and_then(|f| f.str()).and_then(|f| f.strip_prefix("data:image/png;base64,")).and_then(|b| {
        let clean: String = b.chars().filter(|c| !c.is_whitespace()).collect();
        crate::skins::base64(&clean)
    });
    Status { motd, players, sample, version, protocol, ping, icon }
}

// ------------------------------------------------------------------ LAN worlds

struct Shared {
    found: Vec<(String, String, u64)>,
    started: bool,
}
struct Locked(AtomicBool, UnsafeCell<Shared>);
unsafe impl Sync for Locked {}
static LAN: Locked = Locked(AtomicBool::new(false), UnsafeCell::new(Shared { found: Vec::new(), started: false }));

fn lan<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    while LAN.0.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        sys::sleep_ms(1);
    }
    let r = f(unsafe { &mut *LAN.1.get() });
    LAN.0.store(false, Ordering::Release);
    r
}

#[cfg(target_os = "macos")]
const SOCKOPTS: (i32, i32, i32, i32) = (0xffff, 4 | 0x200, 0, 12); // SOL_SOCKET, SO_REUSEADDR|SO_REUSEPORT, IPPROTO_IP, IP_ADD_MEMBERSHIP
#[cfg(not(target_os = "macos"))]
const SOCKOPTS: (i32, i32, i32, i32) = (1, 2, 0, 35);

fn sockaddr(port: u16, addr: [u8; 4]) -> [u8; 16] {
    let mut a = [0u8; 16];
    if cfg!(target_os = "macos") {
        a[0] = 16;
        a[1] = 2;
    } else {
        a[0] = 2;
    }
    a[2..4].copy_from_slice(&port.to_be_bytes());
    a[4..8].copy_from_slice(&addr);
    a
}

/// LanServerPinger's message: "[MOTD]motd[/MOTD][AD]port[/AD]".
pub fn lan_message(msg: &str, from: [u8; 4]) -> Option<(String, String)> {
    let between = |a: &str, b: &str| -> Option<String> {
        let i = msg.find(a)? + a.len();
        let j = msg[i..].find(b)? + i;
        Some(String::from(&msg[i..j]))
    };
    let motd = between("[MOTD]", "[/MOTD]").unwrap_or_else(|| String::from("missing no"));
    let ad = between("[AD]", "[/AD]")?;
    let ip = format!("{}.{}.{}.{}", from[0], from[1], from[2], from[3]);
    let address = if ad.contains(':') { ad } else { format!("{}:{}", ip, ad) };
    Some((motd, address))
}

extern "C" fn listen(_: *mut c_void) -> *mut c_void {
    let (sol, reuse, ipproto, add) = SOCKOPTS;
    unsafe {
        let fd = sys::socket(2, 2, 0);
        if fd < 0 {
            return core::ptr::null_mut();
        }
        let one: i32 = 1;
        for bit in [reuse & 0xff, reuse & !0xff] {
            if bit != 0 {
                sys::setsockopt(fd, sol, bit, &one as *const i32 as *const c_void, 4);
            }
        }
        let a = sockaddr(4445, [0; 4]);
        if sys::bind(fd, a.as_ptr() as *const c_void, 16) != 0 {
            sys::close(fd);
            return core::ptr::null_mut();
        }
        let mreq: [u8; 8] = [224, 0, 2, 60, 0, 0, 0, 0];
        sys::setsockopt(fd, ipproto, add, mreq.as_ptr() as *const c_void, 8);
        let mut buf = [0u8; 1024];
        loop {
            let mut p = sys::PollFd { fd, events: sys::POLLIN, revents: 0 };
            if sys::poll(&mut p, 1, 1500) <= 0 {
                continue;
            }
            let mut from = [0u8; 16];
            let mut flen: u32 = 16;
            let n = sys::recvfrom(fd, buf.as_mut_ptr() as *mut c_void, buf.len(), 0, from.as_mut_ptr() as *mut c_void, &mut flen);
            if n <= 0 {
                continue;
            }
            let msg = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
            if let Some((motd, address)) = lan_message(&msg, [from[4], from[5], from[6], from[7]]) {
                let now = sys::now_ms();
                lan(|s| match s.found.iter_mut().find(|f| f.1 == address) {
                    Some(f) => {
                        f.0 = motd;
                        f.2 = now;
                    }
                    None => s.found.push((motd, address, now)),
                });
            }
        }
    }
}

/// LanServerDetector: starts listening (once) and gives the LAN worlds heard so far:
/// (motd, address).
pub fn lan_worlds() -> Vec<(String, String)> {
    let start = lan(|s| !core::mem::replace(&mut s.started, true));
    if start {
        let mut t: usize = 0;
        if unsafe { sys::pthread_create(&mut t, core::ptr::null(), listen, core::ptr::null_mut()) } == 0 {
            unsafe { sys::pthread_detach(t) };
        }
    }
    lan(|s| s.found.iter().map(|f| (f.0.clone(), f.1.clone())).collect())
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    let mut out = Vec::new();
    let mut l = List::default();
    let mut a = Server::new("Horizon", "127.0.0.1:25600");
    a.pack = Pack::Enabled;
    a.icon = Some(alloc::vec![0x89, b'P', b'N', b'G', 1, 2, 3]);
    l.servers.push(a);
    l.servers.push(Server::new("Minecraft Server", "example.org"));
    l.add_hidden(Server::new("Minecraft Server", "10.0.0.2"));
    let mut back = List::default();
    back.parse(&l.bytes());
    let got = format!(
        "{} {} {} {:?} {:?} {} {}",
        back.servers.len(),
        back.hidden.len(),
        back.servers[0].ip,
        back.servers[0].pack,
        back.servers[1].pack,
        back.servers[0].icon.as_ref().map_or(0, |i| i.len()),
        back.hidden[0].ip
    );
    out.push(("servers.dat round trip", got, "2 1 127.0.0.1:25600 Enabled Prompt 7 10.0.0.2"));
    let addr = |s: &str| address(s).map_or(String::from("none"), |(h, p)| format!("{}|{}", h, p));
    out.push((
        "server addresses",
        format!("{} {} {} {} {}", addr("mc.example.org"), addr("1.2.3.4:25600"), addr("[::1]:1234"), addr(""), valid("a b")),
        "mc.example.org|25565 1.2.3.4|25600 ::1|1234 none false",
    ));
    out.push((
        "LAN world message",
        format!("{:?}", lan_message("[MOTD]Steve - New World[/MOTD][AD]51234[/AD]", [192, 168, 1, 5])),
        "Some((\"Steve - New World\", \"192.168.1.5:51234\"))",
    ));
    out.push(("base64", encode64(b"Penryn!"), "UGVucnluIQ=="));
    out
}
