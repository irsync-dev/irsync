//! A redb manifest from somewhere else: the input written as
//! `.fileset.redb` and opened the way `fileset list` and `check` open it
//! (`store::locate`, then `store::open` read-only), then every entry read.
//!
//! redb panics on some altered files. `fileset` catches a panic raised in
//! redb's own code during a read and reports a damaged manifest; its panic
//! hook keeps such a panic from the hook libFuzzer installed, which would
//! abort. So a panic that fails this target is one of `fileset`'s own, or
//! one redb raised outside a guarded read.
#![no_main]

use fileset::store::{self, OpenMode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(store::REDB_NAME), data).unwrap();
    let Ok(loc) = store::locate(dir.path(), None, None, false) else {
        return;
    };
    let Ok(s) = store::open(&loc.path, loc.backend, OpenMode::ReadOnly) else {
        return;
    };
    let _ = s.meta();
    let _ = s.for_each(&mut |path, _entry| {
        let _ = s.get(path);
        Ok(())
    });
    let _ = s.get(b"no/such/path");
});
