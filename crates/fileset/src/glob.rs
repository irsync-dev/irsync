//! The pattern of `fileset list`: a glob matched against the whole of a stored path
//! (relative to the root, `/` between components).
//!
//! The wildcards are irsync's, from the `irsync-glob` crate, which has the rules: `*`
//! and `?` stop at `/`, `**` crosses it, `[...]` is a class, `\` takes the next byte
//! literally. Unlike irsync's `--exclude` patterns, a pattern here always covers the
//! whole path: `*.jpg` is the top level only, and a leading `/` has no special meaning.
//!
//! No pattern is an error. Trailing `/`s and one leading `./` are ignored, so `2026/`
//! and `./2026` mean `2026`.

use irsync_glob::Glob;

/// A parsed pattern; see the module docs.
#[derive(Debug, Clone)]
pub struct Pattern {
    glob: Glob,
}

impl Pattern {
    /// Never fails. Trailing `/`s and one leading `./` of `text` are ignored.
    pub fn new(text: &[u8]) -> Pattern {
        let mut text = text.strip_prefix(b"./").unwrap_or(text);
        while let Some(rest) = text.strip_suffix(b"/") {
            text = rest;
        }
        Pattern {
            glob: Glob::new(text),
        }
    }

    /// Whether the whole of `path` (a stored relative path) matches.
    pub fn matches(&self, path: &[u8]) -> bool {
        self.glob.matches(path)
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
