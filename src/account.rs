//! The signed-in Microsoft account, through penryn-relay (tools/relay.py), and the game files
//! the relay makes from the player's own copy of Minecraft.
//!
//! account.txt beside the game files (readable by its owner only): name, profile id, Minecraft
//! token, Microsoft refresh token, relay address, one per line.

use crate::client::Account;
use crate::http;
use crate::sys;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

fn path() -> String {
    format!("{}/account.txt", crate::data_dir())
}

fn parse(text: &str, relay: (String, u16)) -> Option<(Account, String)> {
    let l: Vec<&str> = text.lines().map(|s| s.trim()).collect();
    if l.len() < 4 || l[0].is_empty() || l[2].is_empty() {
        return None;
    }
    Some((Account { name: String::from(l[0]), uuid: String::from(l[1]), token: String::from(l[2]), relay }, String::from(l[3])))
}

fn save(a: &Account, refresh: &str) {
    // The sixth line: when the session was obtained (a Minecraft token lasts 24 hours).
    let text = format!("{}\n{}\n{}\n{}\n{}:{}\n{}\n", a.name, a.uuid, a.token, refresh, a.relay.0, a.relay.1, sys::now() as u64);
    if !sys::write_file_private(&path(), text.as_bytes()) {
        crate::warn!("could not save {}", path());
    }
}

/// The saved account, refreshed through its relay (a Minecraft token lasts a day).
pub fn load_refreshed() -> Result<Option<Account>, String> {
    let Some(d) = sys::read_file(&path()) else { return Ok(None) };
    let text = String::from_utf8_lossy(&d).into_owned();
    let relay = text.lines().nth(4).map(|r| http::host_port(r.trim(), 25580)).unwrap_or((String::new(), 25580));
    let Some((acc, refresh)) = parse(&text, relay.clone()) else { return Ok(None) };
    if relay.0.is_empty() || refresh.is_empty() {
        return Ok(Some(acc));
    }
    // Renewed only when it is getting old: Minecraft's sign-in refuses to be asked too often.
    let age = text.lines().nth(5).and_then(|t| t.trim().parse::<u64>().ok()).map(|t| (sys::now() as u64).saturating_sub(t));
    if age.map_or(false, |a| a < 20 * 3600) {
        return Ok(Some(acc));
    }
    let still_valid = age.map_or(false, |a| a < 24 * 3600);
    let r = match http::post(&relay.0, relay.1, "/login/refresh", refresh.as_bytes(), 60000) {
        Ok(r) => r,
        Err(e) if still_valid => {
            crate::warn!("could not renew the sign-in ({}); playing with the current one", e);
            return Ok(Some(acc));
        }
        Err(e) => return Err(format!("cannot reach the relay at {}:{} to renew the sign-in: {}", relay.0, relay.1, e)),
    };
    if r.status != 200 {
        if still_valid {
            crate::warn!("could not renew the sign-in ({}); playing with the current one", r.text().trim());
            return Ok(Some(acc));
        }
        return Err(format!("{} (sign in again: penryn login {}:{})", r.text().trim(), relay.0, relay.1));
    }
    let (fresh, refresh) = parse(&r.text(), relay).ok_or("bad answer from the relay")?;
    save(&fresh, &refresh);
    Ok(Some(fresh))
}

/// Starts a sign-in: (session, code to enter, where, poll seconds).
pub fn start(relay: &(String, u16)) -> Result<(String, String, String, u32), String> {
    let r = http::post(&relay.0, relay.1, "/login/start", b"", 30000)?;
    if r.status != 200 {
        return Err(String::from(r.text().trim()));
    }
    let t = r.text();
    let l: Vec<&str> = t.lines().collect();
    if l.len() < 4 {
        return Err(String::from("bad answer from the relay"));
    }
    Ok((String::from(l[0]), String::from(l[1]), String::from(l[2]), l[3].trim().parse().unwrap_or(5)))
}

/// Checks a sign-in once: `Ok(None)` while the player has not entered the code yet.
pub fn poll(relay: &(String, u16), session: &str) -> Result<Option<Account>, String> {
    let r = http::post(&relay.0, relay.1, "/login/poll", session.as_bytes(), 60000)?;
    match r.status {
        202 => Ok(None),
        200 => {
            let (acc, refresh) = parse(&r.text(), relay.clone()).ok_or("bad answer from the relay")?;
            save(&acc, &refresh);
            Ok(Some(acc))
        }
        _ => Err(String::from(r.text().trim())),
    }
}

/// `penryn login RELAY [ACCOUNT]`: the sign-in, through Horizon's launcher on the relay's
/// computer when it has an account signed in, else Microsoft's device code.
pub fn login_terminal(relay: (String, u16), who: Option<&str>) -> Result<(), String> {
    let list = http::post(&relay.0, relay.1, "/login/accounts", b"", 60000)?;
    let accounts: Vec<String> = list.text().lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    if list.status == 200 && !accounts.is_empty() {
        crate::say!("Accounts signed in to Horizon's launcher on the relay's computer (* plays):");
        for a in accounts.iter() {
            crate::say!("    {}", a.splitn(2, ' ').nth(1).unwrap_or(a));
        }
        let r = http::post(&relay.0, relay.1, "/login/launcher", who.unwrap_or("").as_bytes(), 120000)?;
        if r.status != 200 {
            return Err(String::from(r.text().trim()));
        }
        let (acc, refresh) = parse(&r.text(), relay.clone()).ok_or("bad answer from the relay")?;
        save(&acc, &refresh);
        crate::say!("Signed in as {}. The sign-in is saved in {}.", acc.name, path());
        return Ok(());
    }
    login_device_code(relay)
}

fn login_device_code(relay: (String, u16)) -> Result<(), String> {
    let (session, code, url, every) = start(&relay)?;
    crate::say!("To sign in, open {} on any device and enter the code:\n\n    {}\n", url, code);
    loop {
        sys::sleep_ms(every.max(1) * 1000);
        if let Some(a) = poll(&relay, &session)? {
            crate::say!("Signed in as {}. The sign-in is saved in {}.", a.name, path());
            return Ok(());
        }
    }
}

pub fn logout() {
    let _ = sys::write_file_private(&path(), b"");
    crate::say!("Signed out ({} emptied).", path());
}

/// The format each game file must have: a file from another version of Penryn is fetched again.
pub const BLOCKS_MAGIC: &[u8; 4] = b"PNB9";
pub const ATLAS_MAGIC: &[u8; 4] = b"PNA1";
pub const GUI_MAGIC: &[u8; 4] = b"PNG2";
pub const ENTITIES_MAGIC: &[u8; 4] = b"PNE2";
pub const SOUNDS_MAGIC: &[u8; 4] = b"PNS1";
pub const EFFECTS_MAGIC: &[u8; 4] = b"PNF1";

/// Fetches the game files from the relay when they are missing or from another version.
pub fn ensure_game_files(relay: Option<&(String, u16)>) -> Result<(), String> {
    let dir = crate::data_dir();
    const FILES: [(&str, &[u8; 4]); 4] =
        [("blocks.bin", BLOCKS_MAGIC), ("atlas.bin", ATLAS_MAGIC), ("gui.bin", GUI_MAGIC), ("entities.bin", ENTITIES_MAGIC)];
    let have = |n: &str, magic: &[u8; 4]| sys::read_head(&format!("{}/{}", dir, n), 4).map_or(false, |d| d == magic);
    if FILES.iter().all(|(n, m)| have(n, m)) && ((have("sounds.bin", SOUNDS_MAGIC) && have("effects.bin", EFFECTS_MAGIC)) || relay.is_none()) {
        return Ok(());
    }
    let Some(relay) = relay else {
        return Err(format!(
            "the game files (blocks.bin, atlas.bin, gui.bin, entities.bin) are not in {}: give a relay (--relay HOST) to fetch them",
            dir
        ));
    };
    for (n, magic) in FILES {
        if have(n, magic) {
            continue;
        }
        crate::say!("fetching {} from the relay ...", n);
        let r = http::get(&relay.0, relay.1, &format!("/assets/{}", n), 600000)?;
        if r.status != 200 {
            return Err(format!("the relay could not make the game files: {}", r.text().trim()));
        }
        if r.body.get(..4) != Some(&magic[..]) {
            return Err(format!("the relay sent a {} of another version: update the relay (tools/) too", n));
        }
        if !sys::write_file_private(&format!("{}/{}", dir, n), &r.body) {
            return Err(format!("cannot write {}/{}", dir, n));
        }
    }
    // The status effect icons are optional: without them effects show their backgrounds only.
    if !have("effects.bin", EFFECTS_MAGIC) {
        match http::get(&relay.0, relay.1, "/assets/effects.bin", 600000) {
            Ok(r) if r.status == 200 && r.body.get(..4) == Some(&EFFECTS_MAGIC[..]) => {
                if !sys::write_file_private(&format!("{}/effects.bin", dir), &r.body) {
                    crate::warn!("cannot write {}/effects.bin", dir);
                }
            }
            _ => crate::warn!("the relay has no effects.bin: status effects show without their icons"),
        }
    }
    // The sounds are optional: without them the game plays silent.
    if !have("sounds.bin", SOUNDS_MAGIC) {
        crate::say!("fetching sounds.bin from the relay (large, once) ...");
        let out = format!("{}/sounds.bin", dir);
        let mut shown = 0u64;
        let r = http::get_to_file(&relay.0, relay.1, "/assets/sounds.bin", &out, &mut |got, total| {
            if got >= shown + (25 << 20) || got == total {
                shown = got;
                crate::say!("  {} of {} MB", got >> 20, total >> 20);
            }
        });
        match r {
            Ok(200) if have("sounds.bin", SOUNDS_MAGIC) => {}
            Ok(200) => crate::warn!("the relay sent sounds of another version: no sound"),
            Ok(_) => crate::warn!("the relay has no sounds: the game plays silent"),
            Err(e) => crate::warn!("could not fetch the sounds ({}): the game plays silent", e),
        }
    }
    Ok(())
}
