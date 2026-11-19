//! A fileset manifest from somewhere else: the input written as
//! `.fileset.sqlite` and opened the way `fileset list` and `check` open
//! it (`store::locate`, then `store::open` read-only), then every entry
//! read.
#![no_main]

use fileset::store::{self, OpenMode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(store::SQLITE_NAME), data).unwrap();
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
});
