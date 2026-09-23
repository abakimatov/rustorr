//! `x/net/webdav`'s in-memory lock system (`memLS`) and `If` header parser.

use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockError {
    ConfirmationFailed,
    Locked,
    NoSuchLock,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Condition {
    pub not: bool,
    pub token: String,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockDetails {
    pub root: String,
    /// `None` is an infinite lock.
    pub duration: Option<Duration>,
    pub owner_xml: String,
    pub zero_depth: bool,
}

#[derive(Debug)]
struct Node {
    details: LockDetails,
    token: String,
    ref_count: usize,
    expiry: Option<Instant>,
    held: bool,
}

/// `memLS`: locks by name and token, expiring lazily on each call.
#[derive(Debug)]
pub(crate) struct MemLs {
    by_name: HashMap<String, Node>,
    by_token: HashMap<String, String>,
    generation: u64,
}

impl MemLs {
    /// Tokens count up from the creation time in Unix seconds.
    pub(crate) fn new() -> Self {
        Self {
            by_name: HashMap::new(),
            by_token: HashMap::new(),
            generation: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        }
    }

    fn collect_expired(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .by_name
            .values()
            .filter(|node| !node.held && !node.token.is_empty())
            .filter(|node| node.expiry.is_some_and(|expiry| now >= expiry))
            .map(|node| node.details.root.clone())
            .collect();
        for root in expired {
            self.remove(&root);
        }
    }

    fn lookup(&self, name: &str, conditions: &[Condition]) -> Option<String> {
        for condition in conditions {
            let Some(root) = self.by_token.get(&condition.token) else {
                continue;
            };
            let node = &self.by_name[root];
            if node.held {
                continue;
            }
            if name == node.details.root {
                return Some(root.clone());
            }
            if node.details.zero_depth {
                continue;
            }
            if node.details.root == "/" || name.starts_with(&format!("{}/", node.details.root)) {
                return Some(root.clone());
            }
        }
        None
    }

    /// `Confirm`: holds the locks the conditions name for `name0` and
    /// `name1`; the returned roots release them.
    pub(crate) fn confirm(
        &mut self,
        now: Instant,
        name0: &str,
        name1: &str,
        conditions: &[Condition],
    ) -> Result<Vec<String>, LockError> {
        self.collect_expired(now);
        let mut held = Vec::new();
        for name in [name0, name1] {
            if name.is_empty() {
                continue;
            }
            let root = self
                .lookup(&slash_clean(name), conditions)
                .ok_or(LockError::ConfirmationFailed)?;
            if !held.contains(&root) {
                held.push(root);
            }
        }
        for root in &held {
            if let Some(node) = self.by_name.get_mut(root) {
                node.held = true;
            }
        }
        Ok(held)
    }

    pub(crate) fn release(&mut self, roots: &[String]) {
        for root in roots {
            if let Some(node) = self.by_name.get_mut(root) {
                node.held = false;
            }
        }
    }

    fn can_create(&self, name: &str, zero_depth: bool) -> bool {
        walk_to_root(name, |name, first| {
            let Some(node) = self.by_name.get(name) else {
                return true;
            };
            if first {
                node.token.is_empty() && zero_depth
            } else {
                node.token.is_empty() || node.details.zero_depth
            }
        })
    }

    pub(crate) fn create(
        &mut self,
        now: Instant,
        mut details: LockDetails,
    ) -> Result<String, LockError> {
        self.collect_expired(now);
        details.root = slash_clean(&details.root);
        if !self.can_create(&details.root, details.zero_depth) {
            return Err(LockError::Locked);
        }
        walk_to_root(&details.root.clone(), |name, _| {
            self.by_name
                .entry(name.to_owned())
                .or_insert_with(|| Node {
                    details: LockDetails {
                        root: name.to_owned(),
                        duration: None,
                        owner_xml: String::new(),
                        zero_depth: false,
                    },
                    token: String::new(),
                    ref_count: 0,
                    expiry: None,
                    held: false,
                })
                .ref_count += 1;
            true
        });
        self.generation += 1;
        let token = self.generation.to_string();
        let node = self.by_name.get_mut(&details.root).expect("just created");
        node.token.clone_from(&token);
        node.expiry = details.duration.map(|duration| now + duration);
        node.details = details;
        self.by_token
            .insert(token.clone(), node.details.root.clone());
        Ok(token)
    }

    pub(crate) fn refresh(
        &mut self,
        now: Instant,
        token: &str,
        duration: Option<Duration>,
    ) -> Result<LockDetails, LockError> {
        self.collect_expired(now);
        let root = self.by_token.get(token).ok_or(LockError::NoSuchLock)?;
        let node = self.by_name.get_mut(root).ok_or(LockError::NoSuchLock)?;
        if node.held {
            return Err(LockError::Locked);
        }
        node.details.duration = duration;
        node.expiry = duration.map(|duration| now + duration);
        Ok(node.details.clone())
    }

    pub(crate) fn unlock(&mut self, now: Instant, token: &str) -> Result<(), LockError> {
        self.collect_expired(now);
        let root = self
            .by_token
            .get(token)
            .cloned()
            .ok_or(LockError::NoSuchLock)?;
        if self.by_name.get(&root).is_some_and(|node| node.held) {
            return Err(LockError::Locked);
        }
        self.remove(&root);
        Ok(())
    }

    fn remove(&mut self, root: &str) {
        if let Some(node) = self.by_name.get_mut(root) {
            self.by_token.remove(&node.token);
            node.token.clear();
            node.expiry = None;
        }
        walk_to_root(root, |name, _| {
            if let Some(node) = self.by_name.get_mut(name) {
                node.ref_count -= 1;
                if node.ref_count == 0 {
                    self.by_name.remove(name);
                }
            }
            true
        });
    }
}

fn walk_to_root(name: &str, mut visit: impl FnMut(&str, bool) -> bool) -> bool {
    let mut name = name.to_owned();
    let mut first = true;
    loop {
        if !visit(&name, first) {
            return false;
        }
        if name == "/" {
            return true;
        }
        let cut = name.rfind('/').unwrap_or(0);
        name.truncate(cut);
        if name.is_empty() {
            name.push('/');
        }
        first = false;
    }
}

/// `slashClean`.
pub(crate) fn slash_clean(name: &str) -> String {
    if name.starts_with('/') {
        rustorr_vfs::clean(name)
    } else {
        rustorr_vfs::clean(&format!("/{name}"))
    }
}

/// `parseTimeout`: `None` for an infinite (or absent) timeout.
pub(crate) fn parse_timeout(header: &str) -> Result<Option<Duration>, ()> {
    if header.is_empty() {
        return Ok(None);
    }
    let first = header.split(',').next().unwrap_or_default().trim();
    if first == "Infinite" {
        return Ok(None);
    }
    let seconds = first.strip_prefix("Second-").ok_or(())?;
    if !seconds.starts_with(|character: char| character.is_ascii_digit()) {
        return Err(());
    }
    let seconds: i64 = seconds.parse().map_err(|_| ())?;
    if seconds > i64::from(u32::MAX) {
        return Err(());
    }
    Ok(Some(Duration::from_secs(
        u64::try_from(seconds).map_err(|_| ())?,
    )))
}

/// One list of an `If` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IfList {
    pub resource_tag: String,
    pub conditions: Vec<Condition>,
}

#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    Eof,
    Error,
    Str(&'a str),
    Not,
    Angle(&'a str),
    Square(&'a str),
    Char(char),
}

fn lex(s: &str) -> (Token<'_>, &str) {
    let s = s.trim_start_matches([' ', '\t']);
    if s.is_empty() {
        return (Token::Eof, "");
    }
    let end = s
        .find(['\t', ' ', '(', ')', '<', '>', '[', ']'])
        .unwrap_or(s.len());
    if end != 0 {
        let (text, rest) = s.split_at(end);
        return if text == "Not" {
            (Token::Not, rest)
        } else {
            (Token::Str(text), rest)
        };
    }
    let first = s.chars().next().expect("not empty");
    let close = match first {
        '<' => '>',
        '[' => ']',
        other => return (Token::Char(other), &s[1..]),
    };
    match s[1..].find(close) {
        Some(at) => {
            let inner = &s[1..=at];
            let rest = &s[at + 2..];
            if first == '<' {
                (Token::Angle(inner), rest)
            } else {
                (Token::Square(inner), rest)
            }
        }
        None => (Token::Error, ""),
    }
}

fn parse_condition(s: &str) -> Option<(Condition, &str)> {
    let (mut token, mut rest) = lex(s);
    let mut condition = Condition::default();
    if token == Token::Not {
        condition.not = true;
        (token, rest) = lex(rest);
    }
    match token {
        Token::Str(text) | Token::Angle(text) => condition.token = text.into(),
        Token::Square(text) => condition.etag = text.into(),
        _ => return None,
    }
    Some((condition, rest))
}

fn parse_list(s: &str) -> Option<(IfList, &str)> {
    let (token, mut s) = lex(s);
    if token != Token::Char('(') {
        return None;
    }
    let mut list = IfList::default();
    loop {
        let (token, rest) = lex(s);
        if token == Token::Char(')') {
            if list.conditions.is_empty() {
                return None;
            }
            return Some((list, rest));
        }
        let (condition, rest) = parse_condition(s)?;
        list.conditions.push(condition);
        s = rest;
    }
}

/// `parseIfHeader`: no-tag or tagged lists.
pub(crate) fn parse_if(header: &str) -> Option<Vec<IfList>> {
    let s = header.trim();
    let mut lists = Vec::new();
    match lex(s).0 {
        Token::Char('(') => {
            let mut s = s;
            loop {
                let (list, rest) = parse_list(s)?;
                lists.push(list);
                if rest.is_empty() {
                    return Some(lists);
                }
                s = rest;
            }
        }
        Token::Angle(_) => {
            let mut s = s;
            let (mut tag, mut count, mut first) = (String::new(), 0, true);
            loop {
                let (token, rest) = lex(s);
                match token {
                    Token::Angle(text) => {
                        if !first && count == 0 {
                            return None;
                        }
                        tag = text.into();
                        count = 0;
                        s = rest;
                    }
                    Token::Char('(') => {
                        count += 1;
                        let (mut list, rest) = parse_list(s)?;
                        list.resource_tag.clone_from(&tag);
                        lists.push(list);
                        if rest.is_empty() {
                            return Some(lists);
                        }
                        s = rest;
                    }
                    _ => return None,
                }
                first = false;
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(root: &str, zero_depth: bool) -> LockDetails {
        LockDetails {
            root: root.into(),
            duration: None,
            owner_xml: String::new(),
            zero_depth,
        }
    }

    #[test]
    fn locks_exclude_their_subtree_until_unlocked() {
        let mut locks = MemLs::new();
        let now = Instant::now();
        let token = locks.create(now, details("/a", false)).unwrap();
        assert_eq!(
            locks.create(now, details("/a/b", true)),
            Err(LockError::Locked)
        );
        // A zero-depth lock on an ancestor is allowed, as in memLS.
        assert!(locks.create(now, details("/", true)).is_ok());
        let held = locks
            .confirm(
                now,
                "/a/b",
                "",
                &[Condition {
                    token: token.clone(),
                    ..Condition::default()
                }],
            )
            .unwrap();
        assert_eq!(locks.unlock(now, &token), Err(LockError::Locked));
        locks.release(&held);
        assert_eq!(locks.unlock(now, &token), Ok(()));
        assert_eq!(locks.unlock(now, &token), Err(LockError::NoSuchLock));
        assert!(locks.create(now, details("/a/b", true)).is_ok());
    }

    #[test]
    fn expired_locks_disappear() {
        let mut locks = MemLs::new();
        let now = Instant::now();
        let mut expiring = details("/x", false);
        expiring.duration = Some(Duration::from_secs(1));
        let token = locks.create(now, expiring).unwrap();
        let later = now + Duration::from_secs(2);
        assert_eq!(locks.unlock(later, &token), Err(LockError::NoSuchLock));
        assert!(locks.create(later, details("/x", false)).is_ok());
    }

    #[test]
    fn timeouts_and_if_headers_parse_like_go() {
        assert_eq!(parse_timeout(""), Ok(None));
        assert_eq!(parse_timeout("Infinite, Second-5"), Ok(None));
        assert_eq!(parse_timeout("Second-5"), Ok(Some(Duration::from_secs(5))));
        assert_eq!(parse_timeout("Second-x"), Err(()));
        let lists = parse_if("(<1> [\"e\"]) (Not <2>)").unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].conditions[0].token, "1");
        assert_eq!(lists[0].conditions[1].etag, "\"e\"");
        assert!(lists[1].conditions[0].not);
        let tagged = parse_if("<http://h/dav/a> (<1>)").unwrap();
        assert_eq!(tagged[0].resource_tag, "http://h/dav/a");
        assert!(parse_if("").is_none());
        assert!(parse_if("(<1>").is_none());
    }
}
