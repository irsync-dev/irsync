//! rsync-style include/exclude filters, matched on raw bytes.

use crate::cli::FilterRule;
use crate::fs::paths::RelPath;
use std::io;

/// Outcome of filtering a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Include,
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(u8),
    /// `?`: one byte except `/`.
    One,
    /// `*`: any run of bytes without `/`.
    Star,
    /// `**`: any run of bytes.
    StarStar,
    Class {
        neg: bool,
        items: Vec<(u8, u8)>,
    },
}

/// A compiled rsync pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// As typed (raw bytes: a pattern file may name non-UTF-8 files), so
    /// the rule can travel to the daemon.
    text: Vec<u8>,
    anchored: bool,
    dir_only: bool,
    /// Matched against the full path (a `/` or `**` in it), not the last
    /// component.
    full_path: bool,
    toks: Vec<Tok>,
}

impl Pattern {
    pub fn new(pat: &[u8]) -> Pattern {
        let text = pat.to_vec();
        let mut b = pat;
        let anchored = b.first() == Some(&b'/');
        if anchored {
            b = &b[1..];
        }
        let dir_only = b.len() > 1 && b.last() == Some(&b'/');
        if dir_only {
            b = &b[..b.len() - 1];
        }
        let toks = compile(b);
        // rsync: a pattern with a `/` (not counting a trailing one) or a
        // `**` is matched against the full path, not the last component.
        let full_path = b.contains(&b'/') || toks.contains(&Tok::StarStar);
        Pattern {
            text,
            anchored,
            dir_only,
            full_path,
            toks,
        }
    }

    fn matches(&self, path: &[u8], is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if self.anchored {
            return glob(&self.toks, path);
        }
        if !self.full_path {
            let last = path.rsplit(|&c| c == b'/').next().unwrap_or(path);
            return glob(&self.toks, last);
        }
        // Unanchored, on the full path: match any trailing run of whole components.
        let mut start = 0;
        loop {
            if glob(&self.toks, &path[start..]) {
                return true;
            }
            match path[start..].iter().position(|&c| c == b'/') {
                Some(i) => start += i + 1,
                None => return false,
            }
        }
    }
}

fn compile(b: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                out.push(Tok::Lit(b[i + 1]));
                i += 2;
            }
            b'?' => {
                out.push(Tok::One);
                i += 1;
            }
            b'*' => {
                if b.get(i + 1) == Some(&b'*') {
                    out.push(Tok::StarStar);
                    while b.get(i) == Some(&b'*') {
                        i += 1;
                    }
                } else {
                    out.push(Tok::Star);
                    i += 1;
                }
            }
            b'[' => match class(&b[i + 1..]) {
                Some((tok, used)) => {
                    out.push(tok);
                    i += 1 + used;
                }
                None => {
                    out.push(Tok::Lit(b'['));
                    i += 1;
                }
            },
            c => {
                out.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    out
}

/// Parse a class body (after `[`); returns the token and bytes consumed through `]`.
fn class(b: &[u8]) -> Option<(Tok, usize)> {
    let mut i = 0;
    let neg = matches!(b.first(), Some(b'!') | Some(b'^'));
    if neg {
        i = 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *b.get(i)?;
        if c == b']' && !first {
            return Some((Tok::Class { neg, items }, i + 1));
        }
        first = false;
        let lo = if c == b'\\' {
            i += 1;
            *b.get(i)?
        } else {
            c
        };
        i += 1;
        if b.get(i) == Some(&b'-') && b.get(i + 1).is_some_and(|&n| n != b']') {
            let mut hi = b[i + 1];
            i += 2;
            if hi == b'\\' {
                hi = *b.get(i)?;
                i += 1;
            }
            items.push((lo, hi));
        } else {
            items.push((lo, lo));
        }
    }
}

fn glob(toks: &[Tok], s: &[u8]) -> bool {
    match toks.split_first() {
        None => s.is_empty(),
        Some((Tok::Lit(c), rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
        Some((Tok::One, rest)) => matches!(s.first(), Some(&c) if c != b'/') && glob(rest, &s[1..]),
        Some((Tok::Class { neg, items }, rest)) => match s.first() {
            Some(&c) if c != b'/' => {
                let hit = items.iter().any(|&(lo, hi)| lo <= c && c <= hi);
                hit != *neg && glob(rest, &s[1..])
            }
            _ => false,
        },
        Some((Tok::Star, rest)) => {
            let mut n = 0;
            loop {
                if glob(rest, &s[n..]) {
                    return true;
                }
                if n < s.len() && s[n] != b'/' {
                    n += 1;
                } else {
                    return false;
                }
            }
        }
        Some((Tok::StarStar, rest)) => (0..=s.len()).any(|n| glob(rest, &s[n..])),
    }
}

/// One filter rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    Include(Pattern),
    Exclude(Pattern),
}

impl Rule {
    pub fn is_include(&self) -> bool {
        matches!(self, Rule::Include(_))
    }

    /// The pattern as typed.
    pub fn pattern(&self) -> &[u8] {
        match self {
            Rule::Include(p) | Rule::Exclude(p) => &p.text,
        }
    }

    /// A rule from its wire form (see [`Rule::pattern`]).
    pub fn from_parts(include: bool, pattern: &[u8]) -> Rule {
        if include {
            Rule::Include(Pattern::new(pattern))
        } else {
            Rule::Exclude(Pattern::new(pattern))
        }
    }
}

/// Ordered rules; the first match wins, the default is include.
#[derive(Debug, Clone, Default)]
pub struct FilterSet {
    rules: Vec<Rule>,
}

/// The patterns of a `--exclude-from`/`--include-from` file, as bytes.
fn read_patterns(path: &std::path::Path) -> io::Result<Vec<Vec<u8>>> {
    let data = std::fs::read(path)?;
    Ok(data
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty() && !l.starts_with(b"#") && !l.starts_with(b";"))
        .map(<[u8]>::to_vec)
        .collect())
}

impl FilterSet {
    pub fn new(rules: Vec<Rule>) -> FilterSet {
        FilterSet { rules }
    }

    /// The rules in order, every `*From` file already expanded.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Build from command-line rules, expanding `*From` files in place.
    pub fn from_rules(rules: &[FilterRule]) -> io::Result<FilterSet> {
        let mut out = Vec::new();
        for r in rules {
            match r {
                FilterRule::Include(p) => out.push(Rule::Include(Pattern::new(p.as_bytes()))),
                FilterRule::Exclude(p) => out.push(Rule::Exclude(Pattern::new(p.as_bytes()))),
                FilterRule::IncludeFrom(f) => {
                    for p in read_patterns(f)? {
                        out.push(Rule::Include(Pattern::new(&p)));
                    }
                }
                FilterRule::ExcludeFrom(f) => {
                    for p in read_patterns(f)? {
                        out.push(Rule::Exclude(Pattern::new(&p)));
                    }
                }
            }
        }
        Ok(FilterSet { rules: out })
    }

    pub fn matches(&self, rel: &RelPath, is_dir: bool) -> Verdict {
        let p = rel.as_bytes();
        if p.is_empty() {
            return Verdict::Include;
        }
        for r in &self.rules {
            match r {
                Rule::Include(pat) if pat.matches(p, is_dir) => return Verdict::Include,
                Rule::Exclude(pat) if pat.matches(p, is_dir) => return Verdict::Exclude,
                _ => {}
            }
        }
        Verdict::Include
    }
}
