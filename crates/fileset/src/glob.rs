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
//! The rules are those of irsync's `--exclude` patterns (`crates/irsync/src/filter.rs`);
//! this is a second, smaller implementation so that fileset needs no dependency for it.

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
        glob(&self.toks, path)
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
            // Every run up to the next `/`, shortest first.
            let limit = s.iter().position(|&c| c == b'/').unwrap_or(s.len());
            (0..=limit).any(|n| glob(rest, &s[n..]))
        }
        Some((Tok::StarStar, rest)) => (0..=s.len()).any(|n| glob(rest, &s[n..])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
