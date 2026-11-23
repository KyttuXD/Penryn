//! Update on start from the GitHub "nightly" release (CI replaces it on every push to main).
//!
//! Penryn cannot speak HTTPS, so the relay (tools/relay.py) reads the release: GET /update/check
//! answers "SHA-256 size" of this platform's release file, GET /update/download its files as one
//! "PNU1" archive (the relay has checked the SHA-256 against what GitHub reports for the file).
//! The release file's SHA-256 of the last install is kept in the data folder
//! (installed-release.txt): when the release's differs, the files are put in place and the game is
//! started again.
//!
//! Only builds made by CI check (the build has PENRYN_BUILD, the CI run number); a build made by hand
//! never replaces itself. `--no-update`, PENRYN_NO_UPDATE=1 or UPDATE=no in the Mac app's
//! settings.txt turn it off. The Mac app is updated in place (PENRYN_APP names it: the launcher sets
//! it); the Linux program replaces itself and the files beside it.

use crate::{http, sys};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The CI run number this build was made in (None for a build made by hand).
pub const BUILD: Option<&str> = option_env!("PENRYN_BUILD");

const PLATFORM: &str = if cfg!(target_os = "macos") { "macos" } else { "linux" };

/// One thing in the update archive.
#[derive(Debug, PartialEq)]
pub struct Entry {
    /// 0 file, 1 folder, 2 symbolic link (data: the target).
    pub kind: u8,
    pub mode: u32,
    pub path: String,
    pub data: Vec<u8>,
}

/// A path that stays inside the folder it is made in: relative, no "..", no NUL or backslash.
pub fn path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() < 512
        && !p.starts_with('/')
        && !p.contains('\0')
        && !p.contains('\\')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The files of a "PNU1" archive, every path checked. Err: it is damaged or reaches outside.
pub fn parse_archive(b: &[u8]) -> Result<Vec<Entry>, String> {
    let bad = || String::from("the update archive is damaged");
    if b.get(..4) != Some(b"PNU1") {
        return Err(bad());
    }
    let n = u32::from_le_bytes(b.get(4..8).ok_or_else(bad)?.try_into().map_err(|_| bad())?) as usize;
    if n > 100_000 {
        return Err(bad());
    }
    let mut o = 8usize;
    let mut out = Vec::new();
    for _ in 0..n {
        let head = b.get(o..o + 5).ok_or_else(bad)?;
        let (kind, mode, plen) = (head[0], u16::from_le_bytes([head[1], head[2]]) as u32, u16::from_le_bytes([head[3], head[4]]) as usize);
        o += 5;
        let path = core::str::from_utf8(b.get(o..o + plen).ok_or_else(bad)?).map_err(|_| bad())?;
        o += plen;
        let len = u32::from_le_bytes(b.get(o..o + 4).ok_or_else(bad)?.try_into().map_err(|_| bad())?) as usize;
        o += 4;
        let data = b.get(o..o.checked_add(len).ok_or_else(bad)?).ok_or_else(bad)?;
        o += len;
        if kind > 2 || !path_ok(path) {
            return Err(format!("the update archive has an unsafe entry ({})", path));
        }
        if kind == 2 {
            let t = core::str::from_utf8(data).map_err(|_| bad())?;
            // A link stays inside the folder: relative, and not up and out.
            if t.is_empty() || t.starts_with('/') || t.contains('\0') || t.split('/').any(|c| c == "..") {
                return Err(format!("the update archive has an unsafe link ({})", path));
            }
        }
        // Nothing is made below a link (a file written "through" it would land wherever it points).
        if out.iter().any(|e: &Entry| e.kind == 2 && path.len() > e.path.len() && path.starts_with(e.path.as_str()) && path.as_bytes()[e.path.len()] == b'/') {
            return Err(format!("the update archive writes below a link ({})", path));
        }
        out.push(Entry { kind, mode, path: String::from(path), data: data.to_vec() });
    }
    if o != b.len() {
        return Err(bad());
    }
    Ok(out)
}

/// `s` in single quotes for a shell.
pub fn shell_quote(s: &str) -> String {
    let mut o = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            o.push_str("'\\''");
        } else {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

/// "SHA-256 size" from /update/check.
fn parse_check(t: &str) -> Option<String> {
    let sha = t.split_whitespace().next()?;
    (sha.len() == 64 && sha.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))).then(|| String::from(sha))
}

/// Where the update is put: the Mac app (PENRYN_APP), or the folder of the Linux program.
fn root() -> Option<String> {
    if cfg!(target_os = "macos") {
        sys::env("PENRYN_APP").filter(|p| p.ends_with(".app") || p.ends_with(".app/"))
    } else {
        let exe = sys::exe_path()?;
        exe.rsplit_once('/').map(|(d, _)| String::from(d))
    }
}

fn make_dirs(root: &str, rel: &str) {
    let mut cur = String::from(root);
    for part in rel.split('/') {
        cur.push('/');
        cur.push_str(part);
        sys::make_dir(&cur);
    }
}

/// Puts the archive's files under `root` (staged beside their place first, then renamed into it, so a
/// failure leaves the old files). Files that already are the same are left alone. The program itself
/// is renamed last. Returns how many files changed.
fn install(root: &str, entries: &[Entry]) -> Result<usize, String> {
    let mut staged: Vec<(String, String)> = Vec::new();
    let cleanup = |staged: &[(String, String)]| {
        for (tmp, _) in staged {
            sys::remove_file(tmp);
        }
    };
    for e in entries {
        let dest = format!("{}/{}", root, e.path);
        match e.kind {
            1 => make_dirs(root, &e.path),
            0 => {
                if sys::read_file(&dest).as_deref() == Some(&e.data[..]) {
                    continue;
                }
                if let Some((dir, _)) = e.path.rsplit_once('/') {
                    make_dirs(root, dir);
                }
                let tmp = format!("{}.penryn-new", dest);
                if !sys::write_file_mode(&tmp, &e.data, (e.mode | 0o600) as i32 & 0o777) {
                    cleanup(&staged);
                    return Err(format!("cannot write {}", tmp));
                }
                staged.push((tmp, dest));
            }
            _ => {
                if let Some((dir, _)) = e.path.rsplit_once('/') {
                    make_dirs(root, dir);
                }
                // A link is cheap and replaced at once (the old link, if any, goes).
                let target = String::from_utf8_lossy(&e.data).into_owned();
                // Not fatal: where a real folder already is (a copy that followed the links), the files are there.
                if !sys::make_symlink(&target, &dest) {
                    crate::warn!("update: cannot make the link {}", dest);
                }
            }
        }
    }
    // The program last: if the process dies in between, the old program still finds its files.
    let exe_name = if cfg!(target_os = "macos") { "Contents/MacOS/penryn-bin" } else { "penryn" };
    staged.sort_by_key(|(_, dest)| dest.ends_with(exe_name));
    let n = staged.len();
    for (i, (tmp, dest)) in staged.iter().enumerate() {
        if !sys::rename_file(tmp, dest) {
            cleanup(&staged[i..]);
            return Err(format!("cannot replace {}", dest));
        }
    }
    Ok(n)
}

/// Starts the program again (detached) and ends this one.
fn restart(root: &str, args: &[String]) -> ! {
    let cmd = if cfg!(target_os = "macos") {
        format!("open -n {} >/dev/null 2>&1 &", shell_quote(root))
    } else {
        let mut c = shell_quote(&format!("{}/penryn", root));
        for a in args.iter().skip(1).filter(|a| a.as_str() != "--no-update") {
            c.push(' ');
            c.push_str(&shell_quote(a));
        }
        format!("{} >/dev/null 2>&1 &", c)
    };
    sys::shell(&cmd);
    unsafe { sys::exit(0) }
}

/// Looks for a newer build and installs it (and starts it: this does not return then). Every
/// failure is only logged: the game starts with what it has.
pub fn run(relay: Option<&(String, u16)>, args: &[String]) {
    let Some(build) = BUILD.filter(|b| !b.is_empty()) else { return };
    if args.iter().any(|a| a == "--no-update") || sys::env("PENRYN_NO_UPDATE").is_some() {
        return;
    }
    let (Some(relay), Some(root)) = (relay, root()) else { return };
    if let Err(e) = try_update(relay, &root, args, build) {
        crate::warn!("update: {}", e);
    }
}

fn try_update(relay: &(String, u16), root: &str, args: &[String], build: &str) -> Result<(), String> {
    let dir = crate::data_dir();
    // The question runs on a thread: connecting to a relay that is off or asleep has no time limit of its own
    // (the system's, a minute or two), and the game must not wait for that.
    let (host, port) = (relay.0.clone(), relay.1);
    let mut asking = crate::task::spawn(move || http::get(&host, port, &format!("/update/check?platform={}", PLATFORM), 15000));
    let deadline = sys::now_ms() + 10_000;
    let r = loop {
        if let Some(r) = asking.take() {
            break r?;
        }
        if sys::now_ms() > deadline {
            return Err(String::from("the relay did not answer in time"));
        }
        sys::sleep_ms(20);
    };
    if r.status != 200 {
        return Err(format!("the relay could not read the release ({} {})", r.status, r.text().trim()));
    }
    let want = parse_check(&r.text()).ok_or("the relay's answer is not a checksum")?;
    let record = format!("{}/installed-release.txt", dir);
    let have = sys::read_file(&record).map(|b| String::from(String::from_utf8_lossy(&b).trim())).unwrap_or_default();
    let attempts = format!("{}/update-attempts.txt", dir);
    if have == want {
        sys::remove_file(&attempts);
        crate::say!("update: this is the nightly build (build {})", build);
        return Ok(());
    }
    // Loop guard: the same release twice and the program is still not it, so stop trying.
    let tried = sys::read_file(&attempts).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let (tried_sha, tried_n) = match tried.trim().split_once(' ') {
        Some((s, n)) => (String::from(s), n.parse::<u32>().unwrap_or(0)),
        None => (String::new(), 0),
    };
    if tried_sha == want && tried_n >= 2 {
        return Err(String::from("this release was installed twice and the program is still not it: not installing it again"));
    }
    crate::say!("update: downloading the nightly build ({}) ...", &want[..12]);
    let r = http::get(&relay.0, relay.1, &format!("/update/download?platform={}&sha={}", PLATFORM, want), 600_000)?;
    if r.status != 200 {
        return Err(format!("the relay could not give the release ({} {})", r.status, r.text().trim()));
    }
    let entries = parse_archive(&r.body)?;
    if !entries.iter().any(|e| e.kind == 0 && e.path.ends_with(if cfg!(target_os = "macos") { "MacOS/penryn-bin" } else { "penryn" })) {
        return Err(String::from("the update has no program"));
    }
    let n = install(root, &entries)?;
    if !sys::write_file_private(&record, want.as_bytes()) {
        return Err(String::from("cannot write installed-release.txt: not starting the new build (it would update again)"));
    }
    // Counted when it was installed (a failed download is no attempt): the guard stops a release that is
    // installed twice and still is not the running program.
    sys::write_file_private(&attempts, format!("{} {}", want, if tried_sha == want { tried_n + 1 } else { 1 }).as_bytes());
    crate::say!("update: {} files replaced, starting the new build", n);
    restart(root, args)
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    fn archive(entries: &[(u8, u16, &str, &[u8])]) -> Vec<u8> {
        let mut o = Vec::from(&b"PNU1"[..]);
        o.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (k, m, p, d) in entries {
            o.push(*k);
            o.extend_from_slice(&m.to_le_bytes());
            o.extend_from_slice(&(p.len() as u16).to_le_bytes());
            o.extend_from_slice(p.as_bytes());
            o.extend_from_slice(&(d.len() as u32).to_le_bytes());
            o.extend_from_slice(d);
        }
        o
    }
    let good = archive(&[(1, 0o755, "Contents", b""), (0, 0o755, "Contents/MacOS/penryn-bin", b"abc"), (2, 0o777, "Contents/Frameworks/x", b"Versions/A")]);
    let parsed = parse_archive(&good);
    let shape = match &parsed {
        Ok(v) => format!("{} entries, {} bytes, link to {}", v.len(), v[1].data.len(), String::from_utf8_lossy(&v[2].data)),
        Err(e) => e.clone(),
    };
    let dotdot = archive(&[(0, 0o644, "a/../../etc/x", b"x")]);
    let abs = archive(&[(0, 0o644, "/etc/x", b"x")]);
    let link_out = archive(&[(2, 0o777, "l", b"../../x")]);
    let mut cut = good.clone();
    cut.truncate(cut.len() - 3);
    let through = archive(&[(2, 0o777, "l", b"x"), (0, 0o644, "l/f", b"x")]);
    let mut extra = good.clone();
    extra.push(0);
    alloc::vec![
        ("update archive parses", shape, "3 entries, 3 bytes, link to Versions/A"),
        ("update rejects ..", format!("{}", parse_archive(&dotdot).is_err()), "true"),
        ("update rejects absolute paths", format!("{}", parse_archive(&abs).is_err()), "true"),
        ("update rejects links out of the folder", format!("{}", parse_archive(&link_out).is_err()), "true"),
        ("update rejects writing through a link", format!("{}", parse_archive(&through).is_err()), "true"),
        ("update rejects a cut archive", format!("{}", parse_archive(&cut).is_err()), "true"),
        ("update rejects trailing bytes", format!("{}", parse_archive(&extra).is_err()), "true"),
        ("update rejects another magic", format!("{}", parse_archive(b"PNU2\0\0\0\0").is_err()), "true"),
        ("update shell quoting", shell_quote("a'b c"), "'a'\\''b c'"),
        ("update check answer", format!("{:?}", parse_check(&format!("{} 12\n", "ab".repeat(32))).map(|s| s.len())), "Some(64)"),
        ("update check answer, junk", format!("{:?}", parse_check("nope 1")), "None"),
    ]
}
