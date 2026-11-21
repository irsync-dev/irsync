//! Manifest store benchmark: how fast a store answers the per-entry lookup
//! that `update` and `verify` make for every file they walk.
//!
//! ```sh
//! cargo bench -p fileset --bench lookup --no-run
//! TMPDIR=/some/real/disk EXECUTABLE STORE N REPS
//! ```
//!
//! `EXECUTABLE` is the path the first command prints.
//!
//! `STORE` is `sqlite` or `fjall`. The manifest holds `N` entries named
//! `dNN/fNNNNN`; every tenth has two extended attributes. Each rep looks
//! every entry up once on the store itself and once through a reader, in an
//! order that jumps around the tree, and prints one line of `key=value`
//! fields.

use std::time::Instant;

use fileset::model::{Entry, Flags, Kind, Timespec};
use fileset::store::{open, Backend, OpenMode};

/// Entries written per transaction.
const BATCH: usize = 1000;

fn main() {
    // `cargo bench` passes `--bench`; nothing else starts with `-`.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let [store, n, reps] = args.as_slice() else {
        eprintln!("usage: lookup STORE N REPS");
        std::process::exit(2);
    };
    let backend = match store.as_str() {
        "sqlite" => Backend::Sqlite,
        "fjall" => Backend::Fjall,
        _ => panic!("STORE: sqlite or fjall"),
    };
    let n: usize = n.parse().expect("N");
    let reps: usize = reps.parse().expect("REPS");

    let items: Vec<(Vec<u8>, Entry)> = (0..n)
        .map(|i| {
            let mut e = entry();
            if i % 10 == 0 {
                e.xattrs = vec![
                    (b"user.a".to_vec(), b"1".to_vec()),
                    (b"user.b".to_vec(), b"2".to_vec()),
                ];
            }
            (format!("d{:02}/f{:05}", i % 20, i / 20).into_bytes(), e)
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut s = open(&dir.path().join("manifest"), backend, OpenMode::Create).unwrap();
    for batch in items.chunks(BATCH) {
        s.put_batch(batch).unwrap();
    }

    // A stride coprime to n visits every index once, far from the last.
    let stride = (1..)
        .map(|k| n / 3 + k)
        .find(|s| gcd(*s, n) == 1)
        .unwrap_or(1);
    let reader = s.reader().unwrap();
    for rep in 0..reps {
        let t = Instant::now();
        for i in 0..n {
            let (path, _) = &items[(i * stride) % n];
            assert!(s.get(path).unwrap().is_some());
        }
        let own = t.elapsed();
        let t = Instant::now();
        for i in 0..n {
            let (path, _) = &items[(i * stride) % n];
            assert!(reader.get(path).unwrap().is_some());
        }
        let read = t.elapsed();
        println!(
            "case=lookup store={store} n={n} rep={rep} get_us={:.2} reader_get_us={:.2}",
            own.as_secs_f64() * 1e6 / n as f64,
            read.as_secs_f64() * 1e6 / n as f64,
        );
    }
    drop(reader);
    s.close().unwrap();
}

/// A regular file's entry, with a hash.
fn entry() -> Entry {
    Entry {
        kind: Kind::File,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        size: 8,
        nlink: 1,
        ino: 1,
        dev: 1,
        rdev: 0,
        mtime: Timespec { sec: 1, nsec: 2 },
        ctime: Timespec { sec: 1, nsec: 2 },
        blake3: Some([7; 32]),
        target: None,
        xattrs: Vec::new(),
        flags: Flags::default(),
        generation: 1,
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}
