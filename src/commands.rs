//! The server's command tree (ClientboundCommandsPacket) as Brigadier holds it on the client:
//! parsing what the player types (CommandDispatcher.parse: literals first, every argument
//! node tried, the best parse kept, redirects followed), Brigadier's suggestion context and
//! smart usage, the 26.3 argument parsers' readers (close enough to tell where an argument
//! ends and whether it is well formed), and SignableCommand.of (the `message` arguments a
//! signed command signs).

use crate::wire::In;
use alloc::string::String;
use alloc::vec::Vec;

/// The `minecraft:command_argument_type` registry of 26.3 in id order
/// (ArgumentTypeInfos.bootstrap, as Horizon's protocol tables list it).
pub const ARGUMENT_TYPES: &[&str] = &[
    "brigadier:bool", "brigadier:float", "brigadier:double", "brigadier:integer", "brigadier:long", "brigadier:string",
    "minecraft:entity", "minecraft:game_profile", "minecraft:block_pos", "minecraft:column_pos", "minecraft:vec3",
    "minecraft:vec2", "minecraft:block_state", "minecraft:block_predicate", "minecraft:item_stack",
    "minecraft:item_predicate", "minecraft:team_color", "minecraft:hex_color", "minecraft:component", "minecraft:style",
    "minecraft:message", "minecraft:nbt_compound_tag", "minecraft:nbt_tag", "minecraft:nbt_path", "minecraft:objective",
    "minecraft:objective_criteria", "minecraft:operation", "minecraft:particle", "minecraft:angle", "minecraft:rotation",
    "minecraft:scoreboard_slot", "minecraft:score_holder", "minecraft:swizzle", "minecraft:team", "minecraft:item_slot",
    "minecraft:item_slots", "minecraft:resource_location", "minecraft:function", "minecraft:entity_anchor",
    "minecraft:int_range", "minecraft:float_range", "minecraft:dimension", "minecraft:gamemode", "minecraft:time",
    "minecraft:resource_or_tag", "minecraft:resource_or_tag_key", "minecraft:resource", "minecraft:resource_key",
    "minecraft:resource_selector", "minecraft:template_mirror", "minecraft:template_rotation", "minecraft:heightmap",
    "minecraft:loot_table", "minecraft:loot_predicate", "minecraft:loot_modifier", "minecraft:context_float_provider",
    "minecraft:context_int_provider", "minecraft:slot_source", "minecraft:dialog", "minecraft:feature",
    "minecraft:swing_animation", "minecraft:uuid",
];

const T_BOOL: u16 = 0;
const T_FLOAT: u16 = 1;
const T_DOUBLE: u16 = 2;
const T_INT: u16 = 3;
const T_LONG: u16 = 4;
const T_STRING: u16 = 5;
const T_ENTITY: u16 = 6;
const T_PROFILE: u16 = 7;
const T_BLOCK_POS: u16 = 8;
const T_COLUMN_POS: u16 = 9;
const T_VEC3: u16 = 10;
const T_VEC2: u16 = 11;
const T_BLOCK_STATE: u16 = 12;
const T_ITEM_STACK: u16 = 14;
const T_TEAM_COLOR: u16 = 16;
const T_MESSAGE: u16 = 20;
const T_ROTATION: u16 = 29;
const T_SCORE_HOLDER: u16 = 31;
const T_ANCHOR: u16 = 38;
const T_DIMENSION: u16 = 41;
const T_GAMEMODE: u16 = 42;
const T_TIME: u16 = 43;

#[derive(Clone, Debug)]
pub struct Node {
    /// 0 root, 1 literal, 2 argument.
    pub kind: u8,
    pub executable: bool,
    pub children: Vec<u32>,
    pub redirect: Option<u32>,
    pub name: String,
    /// The argument parser's registry id.
    pub ty: u16,
    /// Integer/float bounds, string kind, entity flags.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub flags: i32,
    /// A custom suggestion provider (minecraft:ask_server, available_sounds...).
    pub suggests: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: usize,
}

/// ArgumentTypeInfo.deserializeFromNetwork: the properties after a parser's id.
fn properties(r: &mut In, ty: u16, n: &mut Node) -> Option<()> {
    match ty {
        T_FLOAT | T_DOUBLE | T_INT | T_LONG => {
            let f = r.u8()?;
            let val = |r: &mut In| -> Option<f64> {
                Some(match ty {
                    T_FLOAT => r.f32()? as f64,
                    T_DOUBLE => r.f64()?,
                    T_INT => r.i32()? as f64,
                    _ => r.i64()? as f64,
                })
            };
            if f & 1 != 0 {
                n.min = Some(val(r)?);
            }
            if f & 2 != 0 {
                n.max = Some(val(r)?);
            }
        }
        T_STRING => n.flags = r.varint()?,
        T_ENTITY | T_SCORE_HOLDER => n.flags = r.u8()? as i32,
        T_TIME => n.flags = r.i32()?,
        44..=48 => {
            r.string()?;
        }
        _ => {}
    }
    Some(())
}

impl Tree {
    /// ClientboundCommandsPacket: the nodes and the root's index. None for a tree with a
    /// cycle or a reference past the end (validateEntries).
    pub fn read(r: &mut In) -> Option<Tree> {
        let n = r.varint()?.max(0) as usize;
        if n > r.left() {
            return None;
        }
        let mut nodes = Vec::with_capacity(n);
        for _ in 0..n {
            let flags = r.u8()?;
            let count = r.varint()?.max(0) as usize;
            if count > r.left() {
                return None;
            }
            let mut children = Vec::with_capacity(count);
            for _ in 0..count {
                children.push(r.varint()? as u32);
            }
            let redirect = if flags & 8 != 0 { Some(r.varint()? as u32) } else { None };
            let kind = flags & 3;
            let mut node = Node {
                kind,
                executable: flags & 4 != 0,
                children,
                redirect,
                name: String::new(),
                ty: 0,
                min: None,
                max: None,
                flags: 0,
                suggests: None,
            };
            if kind >= 1 {
                node.name = r.string()?;
            }
            if kind == 2 {
                node.ty = r.varint()? as u16;
                if node.ty as usize >= ARGUMENT_TYPES.len() {
                    return None; // a parser this client does not know
                }
                properties(r, node.ty, &mut node)?;
                if flags & 16 != 0 {
                    node.suggests = Some(r.string()?);
                }
            }
            nodes.push(node);
        }
        let root = r.varint()? as usize;
        let len = nodes.len() as u32;
        if root >= nodes.len() || nodes.iter().any(|x| x.children.iter().any(|c| *c >= len) || x.redirect.map_or(false, |d| d >= len)) {
            return None;
        }
        // No node may reach itself through children: the parser would recurse forever.
        let mut state = alloc::vec![0u8; nodes.len()];
        for s in 0..nodes.len() {
            if state[s] != 0 {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = alloc::vec![(s, 0)];
            state[s] = 1;
            while let Some(&mut (v, ref mut i)) = stack.last_mut() {
                if let Some(&c) = nodes[v].children.get(*i) {
                    *i += 1;
                    match state[c as usize] {
                        1 => return None,
                        0 => {
                            state[c as usize] = 1;
                            stack.push((c as usize, 0));
                        }
                        _ => {}
                    }
                } else {
                    state[v] = 2;
                    stack.pop();
                }
            }
        }
        Some(Tree { nodes, root })
    }

    /// Brigadier's usage text of a node: a literal's name, an argument's <name>.
    pub fn usage_text(&self, n: usize) -> String {
        let node = &self.nodes[n];
        match node.kind {
            1 => node.name.clone(),
            2 => alloc::format!("<{}>", node.name),
            _ => String::new(),
        }
    }

    fn smart(&self, n: usize, optional: bool, deep: bool) -> String {
        let node = &self.nodes[n];
        let me = if optional { alloc::format!("[{}]", self.usage_text(n)) } else { self.usage_text(n) };
        let child_optional = node.executable;
        let (open, close) = if child_optional { ("[", "]") } else { ("(", ")") };
        if deep {
            return me;
        }
        if let Some(r) = node.redirect {
            let to = if r as usize == self.root { String::from("...") } else { alloc::format!("-> {}", self.usage_text(r as usize)) };
            return alloc::format!("{} {}", me, to);
        }
        if node.children.len() == 1 {
            let u = self.smart(node.children[0] as usize, child_optional, child_optional);
            return alloc::format!("{} {}", me, u);
        }
        if node.children.len() > 1 {
            let mut uses: Vec<String> = Vec::new();
            for &c in &node.children {
                let u = self.smart(c as usize, child_optional, true);
                if !uses.contains(&u) {
                    uses.push(u);
                }
            }
            if uses.len() == 1 {
                let u = &uses[0];
                return alloc::format!("{} {}", me, if child_optional { alloc::format!("[{}]", u) } else { u.clone() });
            }
            let mut b = String::from(open);
            for (i, &c) in node.children.iter().enumerate() {
                if i > 0 {
                    b.push('|');
                }
                b.push_str(&self.usage_text(c as usize));
            }
            b.push_str(close);
            return alloc::format!("{} {}", me, b);
        }
        me
    }

    /// CommandDispatcher.getSmartUsage(node): each child's usage.
    pub fn smart_usage(&self, n: usize) -> Vec<(usize, String)> {
        let optional = self.nodes[n].executable;
        self.nodes[n].children.iter().map(|&c| (c as usize, self.smart(c as usize, optional, false))).collect()
    }

    /// The children a parse tries at `cursor`: a literal that matches the next word alone,
    /// else the arguments (CommandNode.getRelevantNodes).
    fn relevant(&self, n: usize, input: &str, cursor: usize) -> Vec<usize> {
        let rest = &input[cursor..];
        let word = rest.split(' ').next().unwrap_or("");
        let ch = &self.nodes[n].children;
        if let Some(&l) = ch.iter().find(|&&c| self.nodes[c as usize].kind == 1 && self.nodes[c as usize].name == word) {
            return alloc::vec![l as usize];
        }
        ch.iter().map(|&c| c as usize).filter(|&c| self.nodes[c].kind == 2).collect()
    }

    fn parse_nodes(&self, n: usize, input: &str, cursor: usize, so_far: &Parse, depth: usize) -> Parse {
        let mut errors = Vec::new();
        let mut best: Option<Parse> = None;
        if depth > 64 {
            return Parse { cursor, errors, ..so_far.clone() };
        }
        for child in self.relevant(n, input, cursor) {
            let node = &self.nodes[child];
            let start = cursor;
            let end = match node.kind {
                1 => {
                    let rest = &input[start..];
                    if rest.starts_with(node.name.as_str()) && rest[node.name.len()..].chars().next().map_or(true, |c| c == ' ') {
                        Ok(start + node.name.len())
                    } else {
                        Err((String::from("argument.literal.incorrect"), alloc::vec![node.name.clone()], start, true))
                    }
                }
                _ => read_argument(node, input, start).map_err(|(k, a, at)| (k, a, at, false)),
            };
            let end = match end {
                Ok(e) if e < input.len() && input.as_bytes()[e] != b' ' => {
                    errors.push(Error { key: String::from("command.expected.separator"), args: Vec::new(), at: e, literal: false });
                    continue;
                }
                Ok(e) => e,
                Err((key, args, at, literal)) => {
                    errors.push(Error { key, args, at, literal });
                    continue;
                }
            };
            let mut p = so_far.clone();
            {
                let ctx = p.contexts.last_mut().unwrap();
                ctx.nodes.push((child, start, end));
                ctx.range.1 = ctx.range.1.max(end);
                if ctx.nodes.len() == 1 {
                    ctx.range.0 = ctx.range.0.min(start);
                }
                ctx.executable = node.executable;
            }
            let more = if node.redirect.is_none() { 2 } else { 1 };
            let result = if end + more <= input.len() {
                let next = end + 1;
                if let Some(target) = node.redirect {
                    p.contexts.push(Context { root: target as usize, nodes: Vec::new(), range: (next, next), executable: false });
                    let r = self.parse_nodes(target as usize, input, next, &p, depth + 1);
                    return r;
                }
                self.parse_nodes(child, input, next, &p, depth + 1)
            } else {
                Parse { cursor: end, errors: Vec::new(), ..p }
            };
            best = Some(match best {
                None => result,
                Some(b) => {
                    let key = |x: &Parse| (x.cursor < input.len(), !x.errors.is_empty(), !x.valid(input));
                    if key(&result) < key(&b) {
                        result
                    } else {
                        b
                    }
                }
            });
        }
        match best {
            Some(b) => b,
            None => Parse { cursor, errors, ..so_far.clone() },
        }
    }

    /// CommandDispatcher.parse of `input` from `start` (after the slash).
    pub fn parse(&self, input: &str, start: usize) -> Parse {
        let p = Parse {
            contexts: alloc::vec![Context { root: self.root, nodes: Vec::new(), range: (start, start), executable: false }],
            cursor: start,
            errors: Vec::new(),
        };
        self.parse_nodes(self.root, input, start, &p, 0)
    }

    /// CommandContextBuilder.findSuggestionContext: the node whose children complete the text
    /// at `cursor`, and where that text starts.
    pub fn suggestion_context(&self, p: &Parse, cursor: usize) -> (usize, usize) {
        for (i, c) in p.contexts.iter().enumerate() {
            if c.range.0 > cursor {
                break;
            }
            if c.range.1 < cursor {
                if i + 1 < p.contexts.len() {
                    continue;
                }
                if let Some(&(n, _, e)) = c.nodes.last() {
                    return (n, e + 1);
                }
                return (c.root, c.range.0);
            }
            let mut prev = c.root;
            for &(n, s, e) in &c.nodes {
                if s <= cursor && cursor <= e {
                    return (prev, s);
                }
                prev = n;
            }
            return (prev, c.range.0);
        }
        let c = p.contexts.last().unwrap();
        (c.root, c.range.0)
    }

    /// SignableCommand.of: the `message` arguments with their text, not past a redirect back
    /// to the root (`execute ... run`).
    pub fn signable(&self, input: &str, p: &Parse) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (i, c) in p.contexts.iter().enumerate() {
            if i > 0 && c.root == self.root {
                break;
            }
            for &(n, s, e) in &c.nodes {
                let node = &self.nodes[n];
                if node.kind == 2 && node.ty == T_MESSAGE {
                    out.push((node.name.clone(), String::from(&input[s..e])));
                }
            }
        }
        out
    }

    /// The suggestions of `node`'s children for `text` (input up to the cursor) from
    /// `start`: literals that start with what is typed (case-insensitive), and what the
    /// argument types offer. The bool says a child asks the server.
    pub fn suggest(&self, parent: usize, text: &str, start: usize, players: &[String], items: &[String]) -> (Vec<String>, bool) {
        let typed = &text[start.min(text.len())..];
        let lower = typed.to_ascii_lowercase();
        let mut out: Vec<String> = Vec::new();
        let mut ask = false;
        for &c in &self.nodes[parent].children {
            let node = &self.nodes[c as usize];
            if node.kind == 1 {
                if node.name.to_ascii_lowercase().starts_with(&lower) {
                    out.push(node.name.clone());
                }
                continue;
            }
            if node.suggests.is_some() {
                ask = true;
                continue;
            }
            let before = out.len();
            let mut cand = |s: &str| {
                if s.to_ascii_lowercase().starts_with(&lower) && !out.iter().any(|o| o == s) {
                    out.push(String::from(s));
                }
            };
            match node.ty {
                T_BOOL => ["true", "false"].iter().for_each(|s| cand(s)),
                T_ENTITY | T_PROFILE | T_SCORE_HOLDER => {
                    if typed.is_empty() || typed.starts_with('@') {
                        let players_only = node.ty == T_PROFILE || node.flags & 2 != 0;
                        for s in ["@a", "@e", "@n", "@p", "@r", "@s"] {
                            if !players_only || s != "@e" && s != "@n" {
                                cand(s);
                            }
                        }
                    }
                    players.iter().for_each(|p| cand(p));
                }
                T_BLOCK_POS | T_VEC3 => coords(typed, 3).iter().for_each(|s| cand(s)),
                T_COLUMN_POS | T_VEC2 | T_ROTATION => coords(typed, 2).iter().for_each(|s| cand(s)),
                T_TEAM_COLOR => crate::text::NAMES.iter().chain(["reset"].iter()).for_each(|s| cand(s)),
                T_GAMEMODE => ["survival", "creative", "adventure", "spectator"].iter().for_each(|s| cand(s)),
                T_ANCHOR => ["eyes", "feet"].iter().for_each(|s| cand(s)),
                T_DIMENSION => ["minecraft:overworld", "minecraft:the_end", "minecraft:the_nether"].iter().for_each(|s| cand(s)),
                T_ITEM_STACK | T_BLOCK_STATE => {
                    for it in items {
                        let full = alloc::format!("minecraft:{}", it);
                        if full.starts_with(&lower) || it.starts_with(&lower) {
                            cand(&full);
                        }
                    }
                }
                _ => {}
            }
            // A registry argument this client has no list for: the server's suggestions
            // (the server has the registries ClientSuggestionProvider would use).
            if out.len() == before && matches!(node.ty, 12..=15 | 24 | 25 | 27 | 33 | 36 | 37 | 41 | 44..=48 | 52..=54 | 58 | 59) {
                ask = true;
            }
        }
        sort_suggestions(&mut out, typed);
        (out, ask)
    }
}

/// "~", "~ ~", "~ ~ ~" for coordinates (WorldCoordinates' suggestions while nothing is typed).
fn coords(typed: &str, n: usize) -> Vec<String> {
    if !typed.is_empty() && !typed.chars().all(|c| c == '~' || c == ' ') {
        return Vec::new();
    }
    let mut v = Vec::new();
    let mut s = String::new();
    for i in 0..n {
        if i > 0 {
            s.push(' ');
        }
        s.push('~');
        v.push(s.clone());
    }
    v.reverse();
    v
}

/// Suggestions.create's order (case-insensitive), then CommandSuggestions.sortSuggestions:
/// those starting with the typed word (or minecraft: and it) first.
pub fn sort_suggestions(v: &mut Vec<String>, typed: &str) {
    v.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    let word = typed.rsplit(' ').next().unwrap_or("").to_ascii_lowercase();
    let mc = alloc::format!("minecraft:{}", word);
    let (mut first, rest): (Vec<String>, Vec<String>) = v.drain(..).partition(|s| s.starts_with(&word) || s.starts_with(&mc));
    first.extend(rest);
    *v = first;
}

#[derive(Clone, Debug)]
pub struct Context {
    pub root: usize,
    /// (node, start, end) in the input.
    pub nodes: Vec<(usize, usize, usize)>,
    pub range: (usize, usize),
    pub executable: bool,
}

#[derive(Clone, Debug)]
pub struct Error {
    /// A translation key and its arguments.
    pub key: String,
    pub args: Vec<String>,
    pub at: usize,
    /// argument.literal.incorrect (counted, not shown, by CommandSuggestions).
    pub literal: bool,
}

/// ParseResults: the contexts (the first, then one per redirect), how far it read, and the
/// errors of the children that failed where it stopped.
#[derive(Clone, Debug)]
pub struct Parse {
    pub contexts: Vec<Context>,
    pub cursor: usize,
    pub errors: Vec<Error>,
}

impl Parse {
    /// ClientPacketListener's isValidCommand: all read, no errors, ending on a node that runs.
    pub fn valid(&self, input: &str) -> bool {
        self.cursor >= input.len() && self.errors.is_empty() && self.contexts.last().map_or(false, |c| c.executable)
    }
    /// The argument ranges of the last context (CommandSuggestions.formatText colours them).
    pub fn argument_ranges<'a>(&'a self, tree: &'a Tree) -> impl Iterator<Item = (usize, usize)> + 'a {
        let c = self.contexts.last().unwrap();
        c.nodes.iter().filter(move |(n, _, _)| tree.nodes[*n].kind == 2).map(|&(_, s, e)| (s, e))
    }
    /// Commands.getParseException: the one error, or "unknown command/argument".
    pub fn exception(&self, input: &str) -> Option<(String, Vec<String>, usize)> {
        if self.cursor >= input.len() {
            return None;
        }
        if self.errors.len() == 1 {
            let e = &self.errors[0];
            return Some((e.key.clone(), e.args.clone(), e.at));
        }
        let c = &self.contexts[0];
        if c.range.0 == c.range.1 && self.contexts.len() == 1 {
            Some((String::from("command.unknown.command"), Vec::new(), self.cursor))
        } else {
            Some((String::from("command.unknown.argument"), Vec::new(), self.cursor))
        }
    }
}

// ------------------------------------------------------------------ argument readers

type ReadResult = Result<usize, (String, Vec<String>, usize)>;

fn err(key: &str, at: usize) -> ReadResult {
    Err((String::from(key), Vec::new(), at))
}

/// StringReader.isAllowedInUnquotedString.
fn unquoted(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'+')
}

fn read_unquoted(b: &[u8], at: usize) -> usize {
    let mut e = at;
    while e < b.len() && unquoted(b[e]) {
        e += 1;
    }
    e
}

fn read_quoted(b: &[u8], at: usize) -> ReadResult {
    let q = b[at];
    let mut e = at + 1;
    while e < b.len() {
        if b[e] == b'\\' {
            e += 2;
            continue;
        }
        if b[e] == q {
            return Ok(e + 1);
        }
        e += 1;
    }
    err("parsing.quote.expected.end", at)
}

/// A token up to the next space at bracket depth 0 (quotes respected): SNBT, components,
/// selectors with their options, block states with properties.
fn read_balanced(b: &[u8], at: usize) -> ReadResult {
    let mut e = at;
    let mut depth = 0i32;
    while e < b.len() {
        match b[e] {
            b'"' | b'\'' => {
                e = read_quoted(b, e)?;
                continue;
            }
            b'[' | b'{' | b'(' => depth += 1,
            b']' | b'}' | b')' => {
                depth -= 1;
                if depth < 0 {
                    return err("argument.id.invalid", e);
                }
            }
            b' ' if depth == 0 => break,
            _ => {}
        }
        e += 1;
    }
    if depth > 0 {
        return err("parsing.expected", e);
    }
    if e == at {
        return err("argument.id.invalid", at);
    }
    Ok(e)
}

fn read_number(b: &[u8], at: usize, int: bool, node: &Node) -> ReadResult {
    let mut e = at;
    while e < b.len() && (b[e].is_ascii_digit() || b[e] == b'.' || b[e] == b'-') {
        e += 1;
    }
    let s = core::str::from_utf8(&b[at..e]).unwrap_or("");
    let kind = if int { "int" } else { "float" };
    if s.is_empty() {
        return Err((alloc::format!("parsing.{}.expected", kind), Vec::new(), at));
    }
    let v: f64 = if int {
        match s.parse::<i64>() {
            Ok(v) => v as f64,
            Err(_) => return Err((alloc::format!("parsing.{}.invalid", kind), alloc::vec![String::from(s)], at)),
        }
    } else {
        match s.parse::<f64>() {
            Ok(v) => v,
            Err(_) => return Err((alloc::format!("parsing.{}.invalid", kind), alloc::vec![String::from(s)], at)),
        }
    };
    let what = match node.ty {
        T_INT => "integer",
        T_LONG => "long",
        T_FLOAT => "float",
        _ => "double",
    };
    if let Some(m) = node.min {
        if v < m {
            return Err((alloc::format!("argument.{}.low", what), alloc::vec![number(m, int), String::from(s)], at));
        }
    }
    if let Some(m) = node.max {
        if v > m {
            return Err((alloc::format!("argument.{}.big", what), alloc::vec![number(m, int), String::from(s)], at));
        }
    }
    Ok(e)
}

fn number(v: f64, int: bool) -> String {
    if int {
        alloc::format!("{}", v as i64)
    } else {
        alloc::format!("{}", v)
    }
}

/// One coordinate: a number, "~" with an optional number, or "^" likewise.
fn read_coord(b: &[u8], at: usize) -> ReadResult {
    let mut e = at;
    if e < b.len() && (b[e] == b'~' || b[e] == b'^') {
        e += 1;
    }
    let s = e;
    while e < b.len() && (b[e].is_ascii_digit() || b[e] == b'.' || b[e] == b'-') {
        e += 1;
    }
    if e == at || (s == at && e == s) {
        return err("argument.pos.missing.double", at);
    }
    Ok(e)
}

fn read_coords(b: &[u8], at: usize, n: usize) -> ReadResult {
    let mut e = at;
    for i in 0..n {
        if i > 0 {
            if e >= b.len() || b[e] != b' ' {
                return err(if n == 3 { "argument.pos3d.incomplete" } else { "argument.pos2d.incomplete" }, e);
            }
            e += 1;
        }
        e = read_coord(b, e)?;
    }
    Ok(e)
}

/// How far an argument of `node`'s type reaches from `at`, or why it does not parse.
fn read_argument(node: &Node, input: &str, at: usize) -> ReadResult {
    let b = input.as_bytes();
    if at >= b.len() && node.ty != T_MESSAGE {
        return err(match node.ty {
            T_INT | T_LONG => "parsing.int.expected",
            T_FLOAT | T_DOUBLE => "parsing.float.expected",
            T_BOOL => "parsing.bool.expected",
            _ => "command.unknown.argument",
        }, at);
    }
    match node.ty {
        T_BOOL => {
            let e = read_unquoted(b, at);
            match &input[at..e] {
                "true" | "false" => Ok(e),
                v => Err((String::from("parsing.bool.invalid"), alloc::vec![String::from(v)], at)),
            }
        }
        T_INT | T_LONG => read_number(b, at, true, node),
        T_FLOAT | T_DOUBLE => read_number(b, at, false, node),
        T_STRING => match node.flags {
            2 => Ok(b.len()),
            1 if b[at] == b'"' || b[at] == b'\'' => read_quoted(b, at),
            _ => {
                let e = read_unquoted(b, at);
                if e == at {
                    err("parsing.quote.expected.start", at)
                } else {
                    Ok(e)
                }
            }
        },
        T_MESSAGE => Ok(b.len()),
        T_BLOCK_POS | T_VEC3 => read_coords(b, at, 3),
        T_COLUMN_POS | T_VEC2 | T_ROTATION => read_coords(b, at, 2),
        T_ENTITY | T_PROFILE | T_SCORE_HOLDER => {
            if b[at] == b'@' {
                let mut e = at + 1;
                if e < b.len() && matches!(b[e], b'p' | b'a' | b'r' | b's' | b'e' | b'n') {
                    e += 1;
                } else {
                    return err("argument.entity.selector.unknown", at);
                }
                if e < b.len() && b[e] == b'[' {
                    e = read_balanced(b, e)?;
                }
                Ok(e)
            } else if node.ty == T_SCORE_HOLDER {
                // ScoreHolderArgument: any name up to a space ("*" too).
                let mut e = at;
                while e < b.len() && b[e] != b' ' {
                    e += 1;
                }
                Ok(e)
            } else {
                // EntitySelectorParser.parseNameOrUUID: an unquoted string, a name or a UUID.
                let mut e = read_unquoted(b, at);
                if e == at {
                    return err("argument.entity.invalid", at);
                }
                if e - at > 16 && !(e - at == 36 && b[at..e].iter().filter(|c| **c == b'-').count() == 4) {
                    e = at;
                    return err("argument.entity.invalid", e);
                }
                Ok(e)
            }
        }
        T_TIME => {
            let e = read_number(b, at, false, &Node { min: None, max: None, ..node.clone() })?;
            Ok(if e < b.len() && matches!(b[e], b'd' | b's' | b't') { e + 1 } else { e })
        }
        _ => read_balanced(b, at),
    }
}

pub fn selftest() -> Vec<(&'static str, String, &'static str)> {
    // root -> "msg" -> <targets: entity> -> <message: message>; root -> "say" -> <message>;
    // root -> "tp" -> <pos: vec3>; root -> "execute" -> "run" -> redirect root.
    let mut o = crate::wire::Out::new();
    let lit = |o: &mut crate::wire::Out, children: &[i32], exec: bool, name: &str| {
        o.u8(1 | if exec { 4 } else { 0 }).varint(children.len() as i32);
        for c in children {
            o.varint(*c);
        }
        o.string(name);
    };
    o.varint(10);
    o.u8(0).varint(4).varint(1).varint(4).varint(6).varint(8); // 0 root
    lit(&mut o, &[2], false, "msg"); // 1
    o.u8(2).varint(1).varint(3).string("targets").varint(6).u8(0); // 2
    o.u8(2 | 4).varint(0).string("message").varint(20); // 3
    lit(&mut o, &[5], false, "say"); // 4
    o.u8(2 | 4).varint(0).string("message").varint(20); // 5
    lit(&mut o, &[7], false, "tp"); // 6
    o.u8(2 | 4 | 16).varint(0).string("location").varint(10).string("minecraft:ask_server"); // 7
    lit(&mut o, &[9], false, "execute"); // 8
    o.u8(1 | 8).varint(0).varint(0).string("run"); // 9 redirect to root
    o.varint(0);
    let mut out = Vec::new();
    let Some(t) = Tree::read(&mut In::new(&o.b)) else {
        out.push(("command tree", String::from("unreadable"), "read"));
        return out;
    };
    let cmd = "msg Chat hello there";
    let p = t.parse(cmd, 0);
    out.push(("command parse /msg", alloc::format!("{} {:?}", p.valid(cmd), t.signable(cmd, &p)), "true [(\"message\", \"hello there\")]"));
    let cmd = "execute run say hi";
    let p = t.parse(cmd, 0);
    out.push(("command redirect signs nothing past root", alloc::format!("{} {:?}", p.valid(cmd), t.signable(cmd, &p)), "true []"));
    let cmd = "tp ~ ~1 x";
    let p = t.parse(cmd, 0);
    out.push(("command parse error", alloc::format!("{} {:?}", p.valid(cmd), p.exception(cmd).map(|e| e.0)), "false Some(\"argument.pos.missing.double\")"));
    let (s, _) = t.suggest(t.root, "e", 0, &[], &[]);
    out.push(("command suggestions", alloc::format!("{:?}", s), "[\"execute\"]"));
    let cmd = "msg ";
    let p = t.parse(cmd, 0);
    let (n, start) = t.suggestion_context(&p, cmd.len());
    out.push(("command usage", alloc::format!("{} {:?}", start, t.smart_usage(n).into_iter().map(|u| u.1).collect::<Vec<_>>()), "4 [\"<targets> <message>\"]"));
    out
}
