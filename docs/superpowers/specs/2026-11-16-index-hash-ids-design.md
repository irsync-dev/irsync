# Index rows keyed by a hash of the path

Date: 2026-11-16

## Intent

The index stores each file's path five times: in `entries`, in the hidden
index SQLite keeps for its `path` key, in every `chunks` row of the file, in
the `chunks` key index `(path, idx)`, and in `chunks_hash_path`. With short
paths that costs little. With long ones it is most of the database, and
writing it is what makes the end-of-session flush slow on a real disk.

Measured with a throwaway benchmark: 20,000 one-chunk files queued at once,
then `flush()`, on ZFS (`/home`) and tmpfs (`/tmp`):

| build   | paths     | tmpfs  | ZFS    | database + WAL |
|---------|-----------|--------|--------|----------------|
| release | short     | 0.16 s | 0.28 s | 8 MB           |
| release | 783 bytes | 0.86 s | 3.9 s  | 112 MB         |
| debug   | 783 bytes | 1.6 s  | 4.7 s  | 112 MB         |

At 783 bytes a path, SQLite's `dbstat` puts 95 of the 99 MB database in the
five path copies: `chunks_hash_path` 23 MB, `entries` 20 MB, `chunks` 20 MB,
and the two key indexes 16 MB each.

The 783-byte paths are those of `tests/blocking_pool.rs`, whose daemon
flush took over 10 s on a loaded CI runner (fixed in #42 by giving the test
more time). Real trees with paths of hundreds of bytes exist (deep
`node_modules`, generated build trees, mail stores), but they are not the
common case.

Keying a file's rows by a 64-bit hash of its path stores the path once.

## Goals

- Each indexed path is stored once.
- For the long-path case above, the database is at most a third of its
  size today, and the flush on ZFS at most half its time.
- No slower for short paths: flush, lookups and a whole sync within 10% of
  today's.
- A hash collision costs a cache miss, never a wrong answer.
- Upgrading needs nothing from the user, and an old and a new build on one
  machine never break each other.

## Non-goals

- `partials`, `sessions` and `meta`. They stay keyed as they are.
- The writer thread, its batching, and WAL settings.
- Compiling the bundled SQLite with optimisation in test builds. That
  helps only on tmpfs (1.6 s to 1.06 s above); on disk it saves almost
  nothing.
- How files are cut into chunks, and the chunk hashes.

## Anti-goals

- A collision must never make the sender describe a file by another file's
  row. The receiver checks every chunk against its hash (`put_chunk`,
  `recv.rs`), but the sender trusts a row whose stat matches, so a mixed-up
  row would fail that file's transfer.
- A new build must never change a file an older build may have open.
- No upgrade may stop a sync or a daemon export with an error, as a schema
  version mismatch does today (`daemon.rs:394`, `main.rs:1152`,
  `main.rs:1275`).

## Approaches

1. **A 64-bit hash of the path as the row id.** Chosen. The path is stored
   once, in its `entries` row. A lookup by path hashes it and finds the row
   by id, so no index on `path` is needed. The id is known before the
   database is touched, and it never changes for a path.
2. **A counter id, with a unique index on `path`.** Rejected. The path is
   still stored twice (about 38 MB instead of 23 MB in the case above). A
   write needs an upsert and then a lookup of the id, and an id changes
   when a row is replaced.
3. **`entries` and `chunks` as `WITHOUT ROWID` tables keyed by path.**
   Rejected. It drops only the two key-index copies: three remain.

## Design

### Schema

Decided:

```sql
CREATE TABLE entries (
  id INTEGER PRIMARY KEY,         -- hash of path (see below)
  path BLOB NOT NULL,             -- no index: checked on every read
  kind INTEGER NOT NULL, size INTEGER NOT NULL,
  ... the same columns as today ...,
  whole_hash BLOB, has_digest INTEGER NOT NULL);
CREATE TABLE chunks (
  entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL, offset INTEGER NOT NULL, len INTEGER NOT NULL,
  hash BLOB NOT NULL,
  PRIMARY KEY (entry_id, idx)) WITHOUT ROWID;
CREATE INDEX chunks_hash_entry ON chunks(hash, entry_id);
```

`partials`, `sessions` and `meta` are unchanged. `SCHEMA_VERSION` becomes 2.

My calls, not yet confirmed:

- `chunks` is `WITHOUT ROWID`, so its rows are stored in `(entry_id, idx)`
  order and SQLite keeps no separate key index. The key is two integers,
  which is what `WITHOUT ROWID` suits.

### The id

Decided: the first 8 bytes of a BLAKE3 hash of the path bytes, read as a
little-endian signed 64-bit integer. SQLite takes any such value as a row
id, negative ones included. Rust's `DefaultHasher` is not an option: its
output may change between Rust versions, and every existing id would then
point at the wrong row.

My calls, not yet confirmed:

- The hash is BLAKE3's key derivation with the context string
  `"irsync index entry id v1"` rather than a plain hash, so the id can
  never be mistaken for, or match, a content hash.
- All 64 bits, not 63: there is nothing to gain from avoiding negative ids.

### Collisions

With 64 bits, the chance that any two paths in one tree share an id is
about n² / 2⁶⁵: 1 in 37 million for a million files, 1 in 3,700 for a
hundred million. Each tree has its own index file.

Decided: every statement that finds a row by id also compares `path`:

- `get_entry(path)`: `WHERE id = ?1 AND path = ?2`. A row with another path
  is "not indexed": the file is hashed again.
- `write_entry`: deletes every chunk under the id, as it deletes the
  path's chunks today, then `INSERT OR REPLACE` by id writes this path's
  row over whatever row had the id, then writes its chunks. It does not
  rely on `ON DELETE CASCADE` to remove the old chunks, since whether a
  REPLACE fires foreign-key actions depends on SQLite settings. All of it
  happens in one transaction, so the chunks under an id always belong to
  the path in its row.
- `remove_entry`, `remove_entry_if`: `WHERE id = ?1 AND path = ?2 ...`, so
  removing one path never removes the other's row.
- `find_chunks` joins chunks to `entries` by id and returns the row's path.
  It is the path of the file the chunks were written for.

Two colliding files therefore push each other out of the cache: each time
one is hashed, it replaces the other's row. Both still sync correctly.

### Finding the files that hold a chunk

`find_chunks` pages through the files holding a chunk. Today it pages by
path (`c.path > ?2 ... ORDER BY c.path`). With ids, the index
`chunks_hash_entry` is ordered by `(hash, entry_id)`.

Decided: it pages by id (`c.entry_id > ?2 ORDER BY c.entry_id`), and
`ChunkHolder` carries the `entry_id` for the next page's cursor. The caller,
`local_sources` in `session/recv_io.rs`, keeps that id as its cursor instead
of the last path. It needs only a cursor that moves forward. Candidates are
tried in id order instead of path order. Both orders are arbitrary as far
as which copy is better.

### Other queries

- `find_paths_by_whole_hash` is unchanged: it scans `entries` today.
- `entry_identities` (for `irsync gc`) still returns rows by path, now
  sorted by SQLite. It runs only during cleanup.

### Upgrade: one file per schema version

Decided: the index file name gets the schema version:
`index/<root hash>.v2.sqlite` instead of `index/<root hash>.sqlite`.

The client, `irsync export update`, `irsync gc` and the daemon share one
state directory by default (`client_state_dir` falls back to the daemon's
`paths::state_dir()`). `export update` and `gc` open the daemon's indexes
while it runs. An old build keeps using its own file, and the new build
starts a new one, so neither ever sees the other's schema. Nothing is
upgraded in place.

The new file starts empty. The first session on a tree hashes every file
again, which is the one-time cost of the upgrade. On a daemon export with
`warm_index`, that happens at start. `irsync export update` can do it ahead
of time.

My calls, not yet confirmed:

- **`partials` and `sessions` rows are copied** from the old file the first
  time a new one is created (`ATTACH`, then `INSERT ... SELECT` in one
  transaction). Without that, a transfer cut off just before the upgrade
  starts over instead of resuming. The tables are small and their format
  is unchanged.
- **`irsync gc` removes old-version files**, with their `-wal` and `-shm`,
  but only when it can take SQLite's exclusive lock on them, which an old
  process holding the file open prevents. Today it opens every `*.sqlite`
  file by working out its name again from the root, which would open the
  new file and never remove the old one.
- **A version mismatch inside a file stays an error.** With versioned
  names it can only mean a damaged or hand-copied file.

## Benchmark

The numbers above came from a throwaway test. Before and after the change,
the same benchmark runs on `main` and on the branch, on the same machine.

### What is measured

1. **Writer throughput.** N files, each with its stat and digest, queued at
   once, then `flush()`. It reports the time, the time per file, and the
   database and WAL sizes. This is what the end-of-session flush costs.
2. **Lookups.** After the flush, `get_entry` for every path in random
   order, and `find_chunks` for a random 10% of chunk hashes, on a reader
   connection. This guards against the extra path comparison or the join
   costing more than they save.
3. **A whole sync.** A pull of the same tree through the in-process
   harness: once into an empty destination (the receiver writes the index),
   and once again with nothing changed (the sender's lookups decide
   everything). This checks that the change shows up where it matters.
4. **`tests/blocking_pool.rs` duration on CI**, from the test log, for the
   five CI runs before and the five after the merge. CI timings vary a lot
   (23 to 63 s on Ubuntu so far), so this is a check, not a pass/fail
   number.

### Trees

| tree    | files   | paths                                                              |
|---------|---------|--------------------------------------------------------------------|
| short   | 20,000  | `dNN/fNNNN`, about 10 bytes                                        |
| typical | 200,000 | lengths drawn from a real tree: this repo's `target/`, about 100 bytes on average |
| long    | 20,000  | the 783-byte paths of `tests/blocking_pool.rs`                     |

`typical` matters most: it shows whether ordinary users gain anything, or
only pay for the upgrade.

### How it runs

- A release build (`--release`), because that is what users run. The debug
  numbers are reported too, since that is what the tests see.
- On a real disk (`TMPDIR` on the home file system), and on tmpfs as a
  reference.
- Five runs of each case, with `main` and the branch alternating. The
  median and the spread are reported.
- On the same machine, otherwise idle. The page cache is not dropped
  between runs: that needs root, and the flush writes rather than reads.

My calls, not yet confirmed:

- The benchmark is a Cargo example (`crates/irsync/examples/index_bench.rs`)
  taking the tree, the file count and the repetitions as arguments, and
  printing one line per run. It is not a test: AGENTS.md allows no ignored
  tests besides the slow netsim ones, and its timings would fail a normal
  test run on a slow machine. `cargo test` still builds it, so it keeps
  compiling. It needs `test-hooks` only if the whole-sync case uses the
  harness.
- Results go into `docs/IRSYNC_DECISIONS.md`, next to the earlier
  blocking-pool measurements.

### Decision rule

Merge if, in release on the real disk:

- for `long`, the database is at most a third of today's size and the flush
  takes at most half today's time;
- for `short` and `typical`, the flush, the lookups and both syncs are no
  more than 10% slower than today.

If `typical` shows no gain and `long` is the only win, we decide again
whether long paths are worth an upgrade that makes every user's first sync
afterwards hash everything again.

## Tests

- Index unit tests with a test hook that cuts the id to 4 bits, so
  collisions are certain:
  - Put A, then B with the same id: `get_entry(A)` is none, `get_entry(B)`
    is B's row, and `find_chunks` of A's chunk hashes finds nothing.
  - `remove_entry(A)` and `remove_entry_if(A, ...)` leave B's row.
  - A put after a collision brings the file back.
- A sync of a few hundred files with the 4-bit hook. Every file arrives
  correct and the session reports no errors.
- `find_chunks` paging: more holders of one chunk than a page holds, with
  the first ones gone from disk, still finds a live one. The existing
  `local_sources` tests cover this, rewritten to page by id.
- Upgrade:
  - With an existing v1 file, opening the root creates `.v2.sqlite`, leaves
    the v1 file untouched, and copies its `partials` and `sessions`.
  - `irsync gc` removes an unlocked v1 file and leaves one another
    connection holds open.
- The existing index tests, rewritten for the new statements, all pass.

## Left to the builder

- Whether the id is computed in Rust and bound as a parameter, or by a
  custom SQLite function. Computing it in Rust is simpler.
- How the 4-bit test hook is wired in (`test-hooks` feature, as the other
  hooks are).
- How `local_sources` keeps an id cursor and its existing path-keyed
  `live` map side by side.
