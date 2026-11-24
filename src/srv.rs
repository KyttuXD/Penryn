//! The SRV record of a server address (vanilla ServerNameResolver and SrvRecordResolver):
//! `_minecraft._tcp.<host>` for a hostname typed without a port. The DNS message is built and
//! read here by hand, and sent over UDP to the first IPv4 nameserver of /etc/resolv.conf (or
//! PENRYN_DNS=ip:port, for tests). Every failure falls back to the typed host and port.

use crate::sys;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};

const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
const AF_INET: c_int = 2;
const SOCK_DGRAM: c_int = 2;
/// How long the nameserver gets to answer.
const TIMEOUT_MS: i32 = 3000;

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// The host and port to connect to for `address` (typed as in the server list): the SRV record
/// when the port is 25565 and the host is a name, else the address itself (25565 when none is
/// typed). A failed lookup gives the typed host and port.
pub fn target(address: &str) -> Option<(String, u16)> {
    let (host, port) = crate::servers::address(address)?;
    if !wants_srv(&host, port) {
        return Some((host, port));
    }
    match lookup(&format!("_minecraft._tcp.{}", host)) {
        Some(r) => Some((r.target, r.port)),
        None => Some((host, port)),
    }
}

/// What a join or ping needs for `address`: the typed host and port (the handshake's, and the
/// key of the server's pack setting) and the host and port to connect to (srv.rs `target`).
pub fn dial(address: &str) -> Option<((String, u16), (String, u16))> {
    let typed = crate::servers::address(address)?;
    let target = target(address)?;
    Some((typed, target))
}

/// Whether an address gets an SRV lookup (ServerNameResolver, as Horizon's resolver.rs has it):
/// port 25565, and a host that is a name rather than an IP literal or localhost.
fn wants_srv(host: &str, port: u16) -> bool {
    port == 25565 && !is_ip(host) && !host.eq_ignore_ascii_case("localhost")
}

/// A literal IPv4 or IPv6 address (no lookup for those).
fn is_ip(h: &str) -> bool {
    h.contains(':') || (!h.is_empty() && h.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// The SRV record for `name` picked the way vanilla does: the lowest priority, then a random
/// choice weighted by the records' weights. `None` when there is no record or no answer.
pub fn lookup(name: &str) -> Option<Record> {
    let ns = nameserver()?;
    let id = rand16();
    let q = query(id, name)?;
    let answers = exchange(ns, &q, id)?;
    pick(answers, rand16())
}

fn pick(mut recs: Vec<Record>, rnd: u16) -> Option<Record> {
    recs.retain(|r| !r.target.is_empty() && r.target != ".");
    let low = recs.iter().map(|r| r.priority).min()?;
    recs.retain(|r| r.priority == low);
    let total: u32 = recs.iter().map(|r| r.weight as u32).sum();
    if total == 0 {
        return recs.into_iter().next();
    }
    let mut at = (rnd as u32 * total) >> 16;
    for r in recs.iter() {
        if at < r.weight as u32 {
            return Some(r.clone());
        }
        at -= r.weight as u32;
    }
    None
}

/// The DNS query: the header (recursion desired, one question) and the name as SRV, class IN.
pub fn query(id: u16, name: &str) -> Option<Vec<u8>> {
    let mut m = Vec::new();
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    let name = name.strip_suffix('.').unwrap_or(name);
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    if m.len() > 255 {
        return None;
    }
    m.extend_from_slice(&TYPE_SRV.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    Some(m)
}

fn be16(m: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(i)?, *m.get(i + 1)?]))
}

/// A domain name at `pos`, with compression pointers; also the position after it where it sits.
fn read_name(msg: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut after = None;
    let mut hops = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            if after.is_none() {
                after = Some(pos + 1);
            }
            break;
        }
        if len & 0xC0 == 0xC0 {
            let low = *msg.get(pos + 1)? as usize;
            if after.is_none() {
                after = Some(pos + 2);
            }
            hops += 1;
            if hops > 32 {
                return None;
            }
            pos = ((len & 0x3F) << 8) | low;
            continue;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(core::str::from_utf8(label).ok()?);
        pos += 1 + len;
    }
    Some((out, after?))
}

/// The SRV answers in a response to query `id` (an error response or a bad message: None).
pub fn parse(msg: &[u8], id: u16) -> Option<Vec<Record>> {
    if be16(msg, 0)? != id {
        return None;
    }
    let flags = be16(msg, 2)?;
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return None;
    }
    let (qd, an) = (be16(msg, 4)?, be16(msg, 6)?);
    let mut pos = 12;
    for _ in 0..qd {
        pos = read_name(msg, pos)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        let (_, p) = read_name(msg, pos)?;
        let (typ, class, rdlen) = (be16(msg, p)?, be16(msg, p + 2)?, be16(msg, p + 8)? as usize);
        let rd = p + 10;
        msg.get(rd..rd + rdlen)?;
        if typ == TYPE_SRV && class == CLASS_IN {
            let (target, _) = read_name(msg, rd + 6)?;
            out.push(Record { priority: be16(msg, rd)?, weight: be16(msg, rd + 2)?, port: be16(msg, rd + 4)?, target });
        }
        pos = rd + rdlen;
    }
    Some(out)
}

/// The first IPv4 nameserver: PENRYN_DNS (ip:port) or /etc/resolv.conf (port 53).
fn nameserver() -> Option<([u8; 4], u16)> {
    if let Some(s) = sys::env("PENRYN_DNS") {
        return parse_ip(&s);
    }
    // Windows has no resolv.conf: no SRV lookup there (the typed host and port are used; PENRYN_DNS still works).
    if cfg!(windows) {
        return None;
    }
    let text = sys::read_file("/etc/resolv.conf")?;
    for line in String::from_utf8_lossy(&text).lines() {
        if let Some(ip) = line.trim().strip_prefix("nameserver") {
            if let Some(a) = parse_ip(ip.trim()) {
                return Some(a);
            }
        }
    }
    None
}

/// "a.b.c.d" (port 53) or "a.b.c.d:port".
fn parse_ip(s: &str) -> Option<([u8; 4], u16)> {
    let (ip, port) = match s.split_once(':') {
        Some((i, p)) => (i, p.parse().ok()?),
        None => (s, 53),
    };
    let mut a = [0u8; 4];
    let mut parts = ip.split('.');
    for x in a.iter_mut() {
        *x = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((a, port))
}

/// One UDP round trip: the query to the nameserver, the answers to it (3 s at most).
fn exchange(ns: ([u8; 4], u16), q: &[u8], id: u16) -> Option<Vec<Record>> {
    let fd = unsafe { sys::socket(AF_INET, SOCK_DGRAM, 0) };
    if fd < 0 {
        return None;
    }
    // struct sockaddr_in: macOS has the length byte first, Linux the 16-bit family.
    let mut sa = [0u8; 16];
    if cfg!(target_os = "macos") {
        sa[0] = 16;
        sa[1] = AF_INET as u8;
    } else {
        sa[0] = AF_INET as u8;
    }
    sa[2..4].copy_from_slice(&ns.1.to_be_bytes());
    sa[4..8].copy_from_slice(&ns.0);
    let mut answer = None;
    unsafe {
        if sys::connect(fd, sa.as_ptr() as *const c_void, 16) == 0
            && sys::send(fd, q.as_ptr() as *const c_void, q.len(), 0) == q.len() as isize
        {
            let mut p = sys::PollFd { fd, events: sys::POLLIN, revents: 0 };
            if sys::poll(&mut p, 1, TIMEOUT_MS) > 0 {
                let mut buf = [0u8; 1500];
                let n = sys::recv(fd, buf.as_mut_ptr() as *mut c_void, buf.len(), 0);
                if n > 0 {
                    answer = parse(&buf[..n as usize], id);
                }
            }
        }
        sys::sock_close(fd);
    }
    answer
}

fn rand16() -> u16 {
    match sys::read_urandom(2) {
        Some(b) if b.len() == 2 => u16::from_le_bytes([b[0], b[1]]),
        _ => 0x5a5a,
    }
}

/// Checks for the selftest: the query bytes, a parsed answer with a compressed target, the pick.
pub fn selftest(check: &mut dyn FnMut(&str, String, &str)) {
    let q = query(0x1234, "_minecraft._tcp.kyttu.example").unwrap_or_default();
    check("srv query bytes", format!("{:02x?}", &q[..12]), "[12, 34, 01, 00, 00, 01, 00, 00, 00, 00, 00, 00]");
    check("srv query name", format!("{}", q.len()), "47");
    // A response: the question at 12, one SRV answer whose name is a pointer to it (0xC00C).
    let mut m = q[..12].to_vec();
    m[2] = 0x81;
    m[3] = 0x80;
    m[7] = 1;
    m.extend_from_slice(&q[12..]);
    m.extend_from_slice(&[0xC0, 0x0C, 0, 33, 0, 1, 0, 0, 0, 60, 0, 13]);
    m.extend_from_slice(&[0, 5, 0, 10, 0x63, 0xDD]); // priority 5, weight 10, port 25565
    m.extend_from_slice(&[4, b'h', b'o', b's', b't', 0xC0, 0x0C]); // "host" then the pointer
    let recs = parse(&m, 0x1234).unwrap_or_default();
    check("srv parse", format!("{:?}", recs), "[Record { priority: 5, weight: 10, port: 25565, target: \"host._minecraft._tcp.kyttu.example\" }]");
    let mut bad = m.clone();
    bad[3] = 0x83; // SERVFAIL
    check("srv error response ignored", format!("{}", parse(&bad, 0x1234).is_none()), "true");
    let r = |p: u16, w: u16| Record { priority: p, weight: w, port: 1, target: String::from("t") };
    check("srv pick lowest priority", format!("{:?}", pick(alloc::vec![r(2, 9), r(1, 0), r(1, 4)], 0).map(|x| x.weight)), "Some(4)");
    check("srv port rule", format!("{} {} {}", wants_srv("mc.example.org", 25565), wants_srv("mc.example.org", 25566), wants_srv("localhost", 25565)), "true false false");
    check("srv ip literal", format!("{} {} {}", is_ip("1.2.3.4"), is_ip("mc.example.org"), wants_srv("127.0.0.1", 25565)), "true false false");
}
