//! rsync-style include/exclude filters, matched on raw bytes.

use crate::cli::FilterRule;
use crate::fs::paths::RelPath;
use irsync_glob::Glob;
use std::io;

/// Outcome of filtering a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Include,
    Exclude,
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
    /// The wildcards, matched by the matcher `fileset list` uses too.
    glob: Glob,
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
        let glob = Glob::new(b);
        // rsync: a pattern with a `/` (not counting a trailing one) or a
        // `**` is matched against the full path, not the last component.
        let full_path = b.contains(&b'/') || glob.crosses_separators();
        Pattern {
            text,
            anchored,
            dir_only,
            full_path,
            glob,
        }
    }

    fn matches(&self, path: &[u8], is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if self.anchored {
            return self.glob.matches(path);
        }
        if !self.full_path {
            let last = path.rsplit(|&c| c == b'/').next().unwrap_or(path);
            return self.glob.matches(last);
        }
        // Unanchored, on the full path: match any trailing run of whole components.
        let mut start = 0;
        loop {
            if self.glob.matches(&path[start..]) {
                return true;
            }
            match path[start..].iter().position(|&c| c == b'/') {
                Some(i) => start += i + 1,
                None => return false,
            }
        }
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

/// The longest pattern a rule may have, in bytes: as long as the longest
/// path it could name (`PATH_MAX`).
pub const MAX_PATTERN: usize = 4096;

/// The most rules a session takes.
pub const MAX_RULES: usize = 16 * 1024;

/// The most bytes all patterns of a session may have together. A compiled
/// pattern takes about 32 bytes a pattern byte, so the rules of a session
/// stay below about 32 MiB.
pub const MAX_PATTERN_BYTES: usize = 1024 * 1024;

/// Checks a session's patterns against the limits above. Both sides check:
/// the client its own rules before it connects, the daemon the rules in a
/// `Hello` before it compiles them, since they come from the peer.
pub fn check_limits<'a>(patterns: impl IntoIterator<Item = &'a [u8]>) -> Result<(), String> {
    let (mut rules, mut bytes) = (0usize, 0usize);
    for p in patterns {
        if p.len() > MAX_PATTERN {
            return Err(format!(
                "a filter pattern of {} bytes is longer than the limit of {MAX_PATTERN}",
                p.len()
            ));
        }
        rules += 1;
        bytes += p.len();
    }
    if rules > MAX_RULES {
        return Err(format!(
            "{rules} filter rules are more than the limit of {MAX_RULES}"
        ));
    }
    if bytes > MAX_PATTERN_BYTES {
        return Err(format!(
            "filter patterns of {bytes} bytes together are more than the limit of \
             {MAX_PATTERN_BYTES}"
        ));
    }
    Ok(())
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
        check_limits(out.iter().map(Rule::pattern))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
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
