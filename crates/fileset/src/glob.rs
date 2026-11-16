//! A glob over path bytes, for `fileset list`.
//!
//! A pattern is matched against the whole of a stored path (relative to the root, `/`
//! between components). The match is on bytes and is case-sensitive:
//!
//! - `*` matches any run of bytes except `/`
//! - `**` matches any run of bytes, `/` included
//! - `?` matches one byte except `/`
//! - `[abc]`, `[a-z]`, `[!a-z]` and `[^a-z]` match one byte except `/`
//! - `\` takes the next byte literally
//! - anything else matches itself; a `[` with no closing `]` is a plain `[`
//!
//! No pattern is an error. Trailing `/`s and one leading `./` are ignored, so `2026/`
//! and `./2026` mean `2026`.
//!
//! The wildcards are those of irsync's `--exclude` patterns (`crates/irsync/src/filter.rs`);
//! this is a second, smaller implementation so that fileset needs no dependency for it.
//! Unlike there, a pattern always covers the whole path: `*.jpg` is the top level only,
//! and a leading `/` has no special meaning.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(u8),
    /// `?`
    One,
    /// `*`
    Star,
    /// `**`
    StarStar,
    /// `[...]`: inclusive byte ranges, negated with a leading `!` or `^`.
    Class {
        neg: bool,
        items: Vec<(u8, u8)>,
    },
}

/// A parsed pattern; see the module docs for the rules.
#[derive(Debug, Clone)]
pub struct Pattern {
    toks: Vec<Tok>,
}

impl Pattern {
    /// Never fails. Trailing `/`s and one leading `./` of `text` are ignored.
    pub fn new(text: &[u8]) -> Pattern {
        let mut text = text.strip_prefix(b"./").unwrap_or(text);
        while let Some(rest) = text.strip_suffix(b"/") {
            text = rest;
        }
        Pattern { toks: tokens(text) }
    }

    /// Whether the whole of `path` (a stored relative path) matches.
    pub fn matches(&self, path: &[u8]) -> bool {
        // One cell per (token, position in the path) that is known not to match from
        // there on, so each pair is tried once: the work is bounded by pattern length
        // times path length, however many stars there are.
        let mut failed = vec![false; (self.toks.len() + 1) * (path.len() + 1)];
        glob(&self.toks, 0, path, 0, &mut failed)
    }
}

fn tokens(b: &[u8]) -> Vec<Tok> {
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
fn glob(toks: &[Tok], t: usize, s: &[u8], i: usize, failed: &mut [bool]) -> bool {
    let cell = t * (s.len() + 1) + i;
    if failed[cell] {
        return false;
    }
    let hit = match toks.get(t) {
        None => i == s.len(),
        Some(Tok::Lit(c)) => s.get(i) == Some(c) && glob(toks, t + 1, s, i + 1, failed),
        Some(Tok::One) => {
            matches!(s.get(i), Some(&c) if c != b'/') && glob(toks, t + 1, s, i + 1, failed)
        }
        Some(Tok::Class { neg, items }) => match s.get(i) {
            Some(&c) if c != b'/' => {
                let in_class = items.iter().any(|&(lo, hi)| lo <= c && c <= hi);
                in_class != *neg && glob(toks, t + 1, s, i + 1, failed)
            }
            _ => false,
        },
        Some(Tok::Star) => {
            // Every run up to the next `/`, shortest first.
            let limit = s[i..]
                .iter()
                .position(|&c| c == b'/')
                .map_or(s.len(), |n| i + n);
            (i..=limit).any(|j| glob(toks, t + 1, s, j, failed))
        }
        Some(Tok::StarStar) => (i..=s.len()).any(|j| glob(toks, t + 1, s, j, failed)),
    };
    if !hit {
        failed[cell] = true;
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Many stars on a long repetitive name: the work must grow with pattern length
    /// times path length, not double with every star.
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
            assert!(!Pattern::new(pat).matches(&name));
        }
        assert!(Pattern::new(b"*a*a*a*a*a*a*a*a*a*a*a*a*.jpg").matches(&name));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn rules() {
        // (pattern, path, matches)
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b"a.jpg", b"a.jpg", true),
            (b"a.jpg", b"A.jpg", false),
            (b"*.jpg", b"a.jpg", true),
            (b"*.jpg", b"d/a.jpg", false),
            (b"*", b"d/a", false),
            (b"**", b"d/a", true),
            (b"**/*.jpg", b"d/e/a.jpg", true),
            (b"**/*.jpg", b"a.jpg", false),
            (b"d/**", b"d/e/a.jpg", true),
            (b"d/**", b"d", false),
            (b"d", b"d/a", false),
            (b"a?c", b"abc", true),
            (b"a?c", b"a/c", false),
            (b"[abc].txt", b"b.txt", true),
            (b"[a-c].txt", b"d.txt", false),
            (b"[!a-c].txt", b"d.txt", true),
            (b"[^a-c].txt", b"a.txt", false),
            (b"[a-c]", b"/", false),
            (b"[!a]", b"/", false),
            (b"\\*", b"*", true),
            (b"\\*", b"x", false),
            (b"[ab", b"[ab", true),
            (b"a\xffb", b"a\xffb", true),
            (b"a?b", b"a\xffb", true),
            // What people type.
            (b"2026/", b"2026", true),
            (b"2026///", b"2026", true),
            (b"./2026", b"2026", true),
            (b"", b"a", false),
            (b"", b".", false),
            // The root is stored as `.`: one segment, so `*` matches it.
            (b"*", b".", true),
            (b".", b".", true),
            (b"**", b".", true),
            (b"*.jpg", b".", false),
        ];
        for (pat, path, want) in cases {
            assert_eq!(
                Pattern::new(pat).matches(path),
                *want,
                "{:?} on {:?}",
                String::from_utf8_lossy(pat),
                String::from_utf8_lossy(path)
            );
        }
    }
}
