//! `RelPath::new`: the check every path from a peer goes through before
//! it is joined to the destination.
#![no_main]

use irsync::fs::paths::RelPath;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(p) = RelPath::new(data) else {
        return;
    };
    assert_eq!(p.as_bytes(), data);
    if data.is_empty() {
        return;
    }
    // An accepted path stays inside the tree it is joined to.
    assert!(!data.starts_with(b"/"), "absolute path accepted");
    for c in data.split(|&b| b == b'/') {
        assert!(!c.is_empty(), "empty component accepted");
        assert_ne!(c, b"..", "`..` accepted");
        assert_ne!(c, b".", "`.` accepted");
        assert!(!c.contains(&0), "NUL accepted");
    }
    // Rebuilt one component at a time, it is the same path.
    let mut q = RelPath::root();
    for c in p.components() {
        q = q.join(c).expect("a component of an accepted path joins");
    }
    assert_eq!(q, p);
});
