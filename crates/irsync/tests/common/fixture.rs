//! Config fixtures are written with Unix paths (`/srv/photos`). Those are
//! not absolute on Windows, where the loader rightly refuses them, so the
//! tests write and expect them through [`abs`]: on Unix the path as it is,
//! byte for byte; on Windows the same path on drive `C:`.
#![allow(dead_code)]

use std::path::PathBuf;

/// `unix` (an absolute Unix path) as an absolute path of this system: on
/// Unix `unix` itself; on Windows on drive `C:`, with `\` between the
/// components (`/srv/photos` is `C:\srv\photos`).
pub fn abs(unix: &str) -> String {
    assert!(unix.starts_with('/'), "not an absolute Unix path: {unix:?}");
    if cfg!(windows) {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_string()
    }
}

/// [`abs`] as a path.
pub fn abs_path(unix: &str) -> PathBuf {
    PathBuf::from(abs(unix))
}

/// [`abs`] as a TOML value, quoted as irsync's config writer quotes it
/// (`"/srv/photos"` on Unix; a literal string, `'C:\srv\photos'`, on
/// Windows, as a backslash makes the writer choose).
pub fn toml_value(unix: &str) -> String {
    toml_string(&abs(unix))
}

/// `s` as a TOML string, quoted as irsync's config writer quotes it: in
/// double quotes on Unix for the plain paths of the tests (as `{:?}` would
/// write them there), and a path with a backslash as a literal string.
pub fn toml_string(s: &str) -> String {
    toml_edit::Value::from(s).to_string()
}

/// TOML text of a fixture with each value that is a string starting with
/// `/` (a path: `path = "/p"`, `state_dir = "/var/lib/irsync"`, also in an
/// inline table) made absolute here by [`abs`] and quoted by
/// [`toml_value`]. On Unix the text as it is, byte for byte.
///
/// Only a value is rewritten: a string in double quotes, without escapes,
/// right after `=`, not in a comment and not inside a multi-line string
/// (where a written grant name or comment may hold `path = "/etc"` as
/// text). That is every path in the fixtures. Text already rewritten
/// stays as it is, so the writer's output may be read through it again.
pub fn toml(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut in_multiline = false;
    for line in text.split_inclusive('\n') {
        if in_multiline || line.contains("\"\"\"") {
            if line.matches("\"\"\"").count() % 2 == 1 {
                in_multiline = !in_multiline;
            }
            out.push_str(line);
            continue;
        }
        out.push_str(&values_in_line(line));
    }
    out
}

/// [`toml`] for one line outside any multi-line string.
fn values_in_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("\"/") {
        let (before, from) = rest.split_at(i);
        out.push_str(before);
        let after = &from[1..];
        let is_value = out.trim_end_matches([' ', '\t']).ends_with('=') && !out.contains('#');
        match after.find(['"', '\\', '\n']) {
            Some(end) if is_value && after.as_bytes()[end] == b'"' => {
                out.push_str(&toml_value(&after[..end]));
                rest = &after[end + 1..];
            }
            _ => {
                out.push('"');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}
