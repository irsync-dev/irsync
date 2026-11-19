//! `--exclude` and `--include` patterns: `Glob::new`, then `matches`.
//!
//! Input: the pattern, a NUL byte, then the path it is matched against (a
//! path never holds a NUL). Without a NUL the input is both. libFuzzer's
//! -timeout catches a match that does not end.
#![no_main]

use irsync_glob::Glob;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (pattern, subject) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, data),
    };
    let g = Glob::new(pattern);
    g.matches(subject);
    g.crosses_separators();
    // A pattern without wildcards or escapes matches itself.
    if !pattern.iter().any(|b| b"*?[\\".contains(b)) {
        assert!(g.matches(pattern));
    }
});
