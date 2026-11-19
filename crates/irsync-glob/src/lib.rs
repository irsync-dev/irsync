//! The wildcard matcher that irsync's `--exclude` and `--include` patterns and
//! `fileset list` share: a glob over raw bytes, with rsync's wildcards.
//!
//! A [`Glob`] is matched against the whole of a byte string. The match is on bytes
//! and is case-sensitive:
//!
//! - `*` matches any run of bytes except `/`
//! - `**` matches any run of bytes, `/` included, wherever it stands
//! - `?` matches one byte except `/`
//! - `[abc]`, `[a-z]`, `[!a-z]` and `[^a-z]` match one byte except `/`
//! - `\` takes the next byte literally, also inside a class
//! - anything else matches itself; a `[` with no closing `]`, and a `\` at the very
//!   end, are plain bytes
//!
//! No pattern is an error. What a pattern is matched against (the whole path, its
//! last component, a leading `/` that anchors it) is the caller's business.
//!
//! The work of one match is bounded by pattern length times input length, however
//! many stars the pattern has.
//!
//! # Why this is hand-written
//!
//! irsync's patterns have to mean what rsync's mean: filter files written for rsync
//! are used with irsync, and the test suite compares the two. The `globset` crate
//! was tried for this (2026-11-16, version 0.4.21) and differs from rsync 3.4.1 in
//! ways that cannot be configured away:
//!
//! - `**` crosses `/` only as a whole path component, so `a/**.o` misses `a/b/x.o`
//! - `**/` also matches no directory at all, so `a/**/b` matches `a/b`
//! - `{a,b}` means "a or b" instead of those five bytes
//! - a negated class such as `[!x]` matches `/`
//! - an unclosed `[`, a lone `{` or `}` and a trailing `\` are errors
//! - a pattern must be UTF-8, so one that names a file with other bytes cannot be
//!   written
//!
//! Any change in what a pattern matches is a breaking change for users' filter
//! files and scripts.

#![forbid(unsafe_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(u8),
    /// `?`: one byte except `/`.
    One,
    /// `*`: any run of bytes without `/`.
    Star,
    /// `**`: any run of bytes.
    StarStar,
    /// `[...]`: inclusive byte ranges, negated with a leading `!` or `^`.
    Class {
        neg: bool,
        items: Vec<(u8, u8)>,
    },
}

/// A compiled pattern; see the crate docs for the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    toks: Vec<Tok>,
}

impl Glob {
    /// Compiles `pattern`. Never fails.
    pub fn new(pattern: &[u8]) -> Glob {
        Glob {
            toks: compile(pattern),
        }
    }

    /// Whether the pattern has a `**`, the one wildcard that matches `/`.
    pub fn crosses_separators(&self) -> bool {
        self.toks.contains(&Tok::StarStar)
    }

    /// Whether the whole of `s` matches.
    pub fn matches(&self, s: &[u8]) -> bool {
        // One cell per (token, position in `s`) known not to match from there on, so
        // each pair is tried once.
        let mut failed = vec![false; (self.toks.len() + 1) * (s.len() + 1)];
        matches_from(&self.toks, 0, s, 0, &mut failed)
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

/// Parses a class body (what follows `[`): the token and the bytes used through `]`.
/// `None` when there is no closing `]`.
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

/// Whether `toks[t..]` matches `s[i..]`. `failed` remembers the (t, i) pairs already
/// found not to match; a pair that matches ends the whole search at once.
fn matches_from(toks: &[Tok], t: usize, s: &[u8], i: usize, failed: &mut [bool]) -> bool {
    let cell = t * (s.len() + 1) + i;
    if failed[cell] {
        return false;
    }
    let hit = match toks.get(t) {
        None => i == s.len(),
        Some(Tok::Lit(c)) => s.get(i) == Some(c) && matches_from(toks, t + 1, s, i + 1, failed),
        Some(Tok::One) => {
            matches!(s.get(i), Some(&c) if c != b'/') && matches_from(toks, t + 1, s, i + 1, failed)
        }
        Some(Tok::Class { neg, items }) => match s.get(i) {
            Some(&c) if c != b'/' => {
                let in_class = items.iter().any(|&(lo, hi)| lo <= c && c <= hi);
                in_class != *neg && matches_from(toks, t + 1, s, i + 1, failed)
            }
            _ => false,
        },
        Some(Tok::Star) => {
            // Every run up to the next `/`, shortest first.
            let limit = s[i..]
                .iter()
                .position(|&c| c == b'/')
                .map_or(s.len(), |n| i + n);
            (i..=limit).any(|j| matches_from(toks, t + 1, s, j, failed))
        }
        Some(Tok::StarStar) => (i..=s.len()).any(|j| matches_from(toks, t + 1, s, j, failed)),
    };
    if !hit {
        failed[cell] = true;
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &[u8], s: &[u8]) -> bool {
        Glob::new(pat).matches(s)
    }

    #[test]
    fn rules() {
        // (pattern, input, matches)
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b"a.jpg", b"a.jpg", true),
            (b"a.jpg", b"A.jpg", false),
            (b"a.jpg", b"a.jpgx", false),
            (b"", b"", true),
            (b"", b"a", false),
            (b"*", b"", true),
            (b"*.jpg", b"a.jpg", true),
            (b"*.jpg", b"d/a.jpg", false),
            (b"*", b"d/a", false),
            (b"**", b"d/a", true),
            (b"***", b"d/a", true),
            (b"**/*.jpg", b"d/e/a.jpg", true),
            (b"**/*.jpg", b"a.jpg", false),
            (b"d/**", b"d/e/a.jpg", true),
            (b"d/**", b"d", false),
            (b"a/**/b", b"a/x/y/b", true),
            (b"a/**/b", b"a/b", false),
            // `**` crosses `/` wherever it stands, as in rsync.
            (b"a/**.o", b"a/b/x.o", true),
            (b"a/**.o", b"a/x.o", true),
            (b"**.jpg", b"d/e/a.jpg", true),
            (b"d**", b"d/e", true),
            (b"a**b", b"a/x/b", true),
            (b"d", b"d/a", false),
            (b"a?c", b"abc", true),
            (b"a?c", b"a/c", false),
            (b"?", b"\xff", true),
            (b"[abc].txt", b"b.txt", true),
            (b"[a-c].txt", b"d.txt", false),
            (b"[!a-c].txt", b"d.txt", true),
            (b"[^a-c].txt", b"a.txt", false),
            (b"[^a-c].txt", b"d.txt", true),
            (b"[a-c]", b"/", false),
            (b"[!a]", b"/", false),
            (b"[]]", b"]", true),
            (b"[a\\]]", b"]", true),
            (b"[a\\-c]", b"-", true),
            (b"[a\\-c]", b"b", false),
            (b"\\*", b"*", true),
            (b"\\*", b"x", false),
            (b"a\\", b"a\\", true),
            // Plain bytes, not errors and not alternatives.
            (b"[ab", b"[ab", true),
            (b"x{", b"x{", true),
            (b"a}b", b"a}b", true),
            (b"{a,b}", b"{a,b}", true),
            (b"{a,b}", b"a", false),
            (b"a\xffb", b"a\xffb", true),
            (b"a?b", b"a\xffb", true),
        ];
        for (pat, s, want) in cases {
            assert_eq!(
                m(pat, s),
                *want,
                "{:?} on {:?}",
                String::from_utf8_lossy(pat),
                String::from_utf8_lossy(s)
            );
        }
    }

    #[test]
    fn crosses_separators_only_with_a_double_star() {
        assert!(Glob::new(b"a/**").crosses_separators());
        assert!(Glob::new(b"**.o").crosses_separators());
        assert!(!Glob::new(b"a/*").crosses_separators());
        assert!(!Glob::new(b"a\\**").crosses_separators());
        assert!(!Glob::new(b"[*][*]").crosses_separators());
    }

    #[test]
    fn equal_patterns_compile_to_equal_globs() {
        assert_eq!(Glob::new(b"a*[x-z]"), Glob::new(b"a*[x-z]"));
        assert_ne!(Glob::new(b"a*"), Glob::new(b"a**"));
    }

    /// Many stars on a long repetitive input: the work must grow with pattern length
    /// times input length, not multiply with every star.
    #[test]
    fn many_stars_do_not_take_exponential_time() {
        let mut name = vec![b'a'; 200];
        name.extend_from_slice(b".jpg");
        let started = std::time::Instant::now();
        for pat in [
            &b"*a*a*a*a*a*a*a*a*a*a*a*a*b"[..],
            b"**a**a**a**a**a**a**a**a**a**a**a**a**b",
            b"*a*a*a*a*a*a*a*a*a*a*a*a?b",
        ] {
            assert!(!m(pat, &name));
        }
        assert!(m(b"*a*a*a*a*a*a*a*a*a*a*a*a*.jpg", &name));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
