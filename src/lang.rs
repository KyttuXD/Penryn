//! The game's en_us translations (from the player's own game files, via gui.bin), and
//! vanilla's substitution of arguments into them (%s, %1$s, %%).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use core::sync::atomic::{AtomicPtr, Ordering};

static TABLE: AtomicPtr<BTreeMap<String, String>> = AtomicPtr::new(core::ptr::null_mut());

pub fn set(t: BTreeMap<String, String>) {
    TABLE.store(Box::into_raw(Box::new(t)), Ordering::Release);
}

/// The translation of `key`, or the key itself when there is none.
pub fn get(key: &str) -> String {
    let p = TABLE.load(Ordering::Acquire);
    if !p.is_null() {
        if let Some(v) = unsafe { &*p }.get(key) {
            return v.clone();
        }
    }
    String::from(key)
}

pub fn has(key: &str) -> bool {
    let p = TABLE.load(Ordering::Acquire);
    !p.is_null() && unsafe { &*p }.contains_key(key)
}

/// `TranslatableContents`: the pattern with its arguments in place.
pub fn format(key: &str, args: &[String]) -> String {
    let pat = get(key);
    let mut out = String::new();
    let mut next = 0;
    let b: alloc::vec::Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '%' && i + 1 < b.len() {
            if b[i + 1] == '%' {
                out.push('%');
                i += 2;
                continue;
            }
            if b[i + 1] == 's' {
                out.push_str(args.get(next).map(|s| s.as_str()).unwrap_or(""));
                next += 1;
                i += 2;
                continue;
            }
            // %1$s
            let mut j = i + 1;
            let mut n = 0usize;
            while j < b.len() && b[j].is_ascii_digit() {
                n = n * 10 + b[j] as usize - '0' as usize;
                j += 1;
            }
            if j + 1 < b.len() && j > i + 1 && b[j] == '$' && b[j + 1] == 's' {
                out.push_str(args.get(n.saturating_sub(1)).map(|s| s.as_str()).unwrap_or(""));
                i = j + 2;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// An item's name as vanilla shows it ("item.minecraft.x", else "block.minecraft.x").
pub fn item_name(id: &str) -> String {
    let k = alloc::format!("item.minecraft.{}", id);
    if has(&k) {
        return get(&k);
    }
    let k = alloc::format!("block.minecraft.{}", id);
    if has(&k) {
        return get(&k);
    }
    String::from(id)
}

/// Kept for callers that only have a key.
pub fn translate(key: &str) -> String {
    get(key)
}
