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
//! `dNN/fNNNNN`; every tenth has two extended attributes. The entries are
//! looked up in three phases, `REPS` times each:
//!
//! - `written`: on the store that has just written them;
//! - `reopened-rw`: after a close, opened for writing as `update` opens it;
//! - `reopened-ro`: after another close, opened read-only as `verify` does.
//!
//! The reopened phases are what a command on an existing manifest sees, and
//! the stores behave differently there: SQLite is back in rollback-journal
//! mode when opened read-only, and fjall reads from its table files instead
//! of from memory. fjall only writes table files once enough has built up:
//! a manifest of 300,000 entries is still answered from memory after a
//! reopen, one of 1,000,000 is not.
//!
//! Each rep looks every entry up three ways, in an order that jumps around
//! the tree, and prints one line of `key=value` fields: `get_us` on the
//! store itself, `reader_get_us` through a reader, and `get_many_us`
//! through a reader in batches, as the tree walk reads. All are
//! microseconds per entry.

use std::path::Path;
use std::time::Instant;

use fileset::model::{Entry, Flags, Kind, Timespec};
use fileset::store::{open, Backend, OpenMode, Store};

/// Entries written per transaction.
const WRITE_BATCH: usize = 1000;

/// Entries read per `get_many` call.
const READ_BATCH: usize = 64;

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
    // A stride coprime to n visits every index once, far from the last.
    let stride = (1..)
        .map(|k| n / 3 + k)
        .find(|s| gcd(*s, n) == 1)
        .unwrap_or(1);
    let order: Vec<&[u8]> = (0..n).map(|i| &items[(i * stride) % n].0[..]).collect();

    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("manifest");
    let run = Run {
        store,
        reps,
        order: &order,
    };

    let mut s = open(&manifest, backend, OpenMode::Create).unwrap();
    for batch in items.chunks(WRITE_BATCH) {
        s.put_batch(batch).unwrap();
    }
    run.phase("written", s);
    run.phase(
        "reopened-rw",
        reopen(&manifest, backend, OpenMode::ReadWrite),
    );
    run.phase(
        "reopened-ro",
        reopen(&manifest, backend, OpenMode::ReadOnly),
    );
}

fn reopen(manifest: &Path, backend: Backend, mode: OpenMode) -> Box<dyn Store> {
    open(manifest, backend, mode).unwrap()
}

/// What every phase shares.
struct Run<'a> {
    store: &'a str,
    reps: usize,
    /// Every path once, in the order to look them up.
    order: &'a [&'a [u8]],
}

impl Run<'_> {
    /// Looks every entry up `reps` times on `s` and through a reader of it,
    /// prints a line per rep, and closes `s`.
    fn phase(&self, phase: &str, s: Box<dyn Store>) {
        let n = self.order.len();
        let per_entry = |t: Instant| t.elapsed().as_secs_f64() * 1e6 / n as f64;
        let reader = s.reader().unwrap();
        for rep in 0..self.reps {
            let t = Instant::now();
            for path in self.order {
                assert!(s.get(path).unwrap().is_some());
            }
            let get = per_entry(t);
            let t = Instant::now();
            for path in self.order {
                assert!(reader.get(path).unwrap().is_some());
            }
            let reader_get = per_entry(t);
            let t = Instant::now();
            for batch in self.order.chunks(READ_BATCH) {
                let found = reader.get_many(batch).unwrap();
                assert!(found.len() == batch.len() && found.iter().all(Option::is_some));
            }
            let get_many = per_entry(t);
            println!(
                "case=lookup store={} n={n} phase={phase} rep={rep} get_us={get:.2} \
                 reader_get_us={reader_get:.2} get_many_us={get_many:.2}",
                self.store,
            );
        }
        drop(reader);
        s.close().unwrap();
    }
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
