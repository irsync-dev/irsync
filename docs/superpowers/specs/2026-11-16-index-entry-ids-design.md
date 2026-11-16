# Index rows keyed by an entry id instead of the path

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

Giving each file's row a number, and keying its chunks by that number,
stores the path twice instead of five times.

## Goals

- The chunk rows and the chunk-hash index no longer hold the path.
- For the long-path case above, the flush on ZFS takes at most half its
  time today.
- No slower for short paths: flush, lookups and a whole sync within 10% of
  today's.
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

- A row must never describe another path's file. The sender trusts a row
  whose stat matches, so a mixed-up row would fail that file's transfer.
- A new build must never change a file an older build may have open.
- No upgrade may stop a sync or a daemon export with an error, as a schema
  version mismatch does today (`daemon.rs:394`, `main.rs:1152`,
  `main.rs:1275`).

## Approaches

Both were built and measured with `benches/index.rs` (see Results).

1. **A counter id, with a unique index on `path`.** Chosen. The path is
   stored twice: in the row and in its unique index. New files get
   increasing ids in the order they are written, which is the walk's
   sorted order, so a batch of writes lands on a few neighbouring pages, as
   today. It is faster than today for every tree measured, and for long
   paths the flush takes 30% of today's time.
2. **A 64-bit hash of the path as the id.** Rejected after measuring. The
   path is stored once, and a lookup needs no index on `path`, so it saves
   more space: 36 MB for the long-path tree, against 45 MB with a counter.
   But a hash puts every row on a random page. For short and typical paths
   the flush got 92–122% slower, and a first pull of the typical tree 36%
   slower. A collision also needed handling (the path compared on every
   read and delete, and colliding files pushing each other out of the
   cache). The cause is set out under Results.
3. **`entries` and `chunks` as `WITHOUT ROWID` tables keyed by path.**
   Rejected without building. It drops only the two key-index copies:
   three remain.

## Design

### Schema

```sql
CREATE TABLE entries (
  id INTEGER PRIMARY KEY,         -- SQLite's row number: the next one up
  path BLOB NOT NULL UNIQUE,
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

`chunks` is `WITHOUT ROWID`, so its rows are stored in `(entry_id, idx)`
order and SQLite keeps no separate key index. A new file's chunks are
therefore written after the last file's.

### Writing and reading a row

- `write_entry` is an upsert: `INSERT ... ON CONFLICT (path) DO UPDATE SET
  ... RETURNING id`. A path already indexed keeps its row and its id, so
  its chunk rows stay in place; a new path takes the next id. It then
  deletes the chunk rows under that id and writes the new ones, all in one
  transaction. It is not `INSERT OR REPLACE`: a replace deletes the row and
  inserts it with a new id, which would scatter the rows of files written
  again, and whether a replace fires the `ON DELETE CASCADE` depends on
  SQLite settings.
- `get_entry(path)` finds the row through the unique index on `path`, then
  its chunks by `entry_id`.
- `remove_entry` and `remove_entry_if` delete the chunks under the id of
  the matching row (`entry_id = (SELECT id FROM entries WHERE path = ...)`),
  then the row. They do not rely on the cascade either.

The upsert's `RETURNING` needs SQLite 3.35 or newer. The bundled SQLite of
`libsqlite3-sys` 0.38 is newer.

### Finding the files that hold a chunk

`find_chunks` pages through the files holding a chunk. Today it pages by
path (`c.path > ?2 ... ORDER BY c.path`). With ids, the index
`chunks_hash_entry` is ordered by `(hash, entry_id)`, so it pages by id
(`c.entry_id > ?2 ORDER BY c.entry_id`), and `ChunkHolder` carries the
`entry_id` for the next page's cursor. The caller, `local_sources` in
`session/recv_io.rs`, keeps that id as its cursor instead of the last path.
It needs only a cursor that moves forward. Candidates are tried in id order
instead of path order. Both orders are arbitrary as far as which copy is
better.

`find_paths_by_whole_hash` and `entry_identities` (for `irsync gc`) are
unchanged.

### Upgrade: one file per schema version

The index file name gets the schema version:
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

- **`partials` and `sessions` rows are copied** from the old file when a
  new one is created (`ATTACH`, then `INSERT OR IGNORE ... SELECT`, in the
  transaction that creates the new file). Without that, a transfer cut off
  just before the upgrade would start over instead of resuming. The tables
  are small and their columns are unchanged. A failed copy is logged, and
  the new index works without it.
- **`irsync gc` removes old-version files**, with their `-wal` and `-shm`,
  but only when no process has them open. It opens the file in SQLite's
  exclusive locking mode and begins an exclusive transaction. In WAL mode
  that is refused while any other connection has the file open, idle ones
  included (checked with SQLite 3.46). It holds the lock while it deletes
  the files. Old-version files no longer feed gc's list of roots, which
  would otherwise create new-version files for them. A dry run lists them.
  The summary line gains ", N old index file(s)" only when there are any.
- **A version mismatch inside a file stays an error.** With versioned
  names it can only mean a damaged or hand-copied file.

## Benchmark

`crates/irsync/benches/index.rs`, a Cargo bench target (`harness = false`).
It is a bench and not an example because the whole-sync case uses the test
harness in `tests/common`, which needs `CARGO_BIN_EXE_irsync`, and Cargo
sets that only for tests and benches. A bench is also built with the
release settings, as users run it, and `clippy --all-targets` in CI keeps
it compiling. It is not a test: AGENTS.md allows no ignored tests besides
the slow netsim ones, and its timings would fail a normal test run on a
slow machine. It uses only index calls that exist with either schema, so
the same source builds against `main` and the branch.

### What is measured

1. **Writer throughput.** N files, each with its stat and digest, queued at
   once, then `flush()`. It reports the time and the database and WAL
   sizes. This is what the end-of-session flush costs.
2. **Lookups.** After the flush, `get_entry` for every path in an order
   that jumps around the tree, and `find_chunk` for every tenth file's
   chunk, each its own `read` on a reader connection.
3. **A whole sync.** A pull of the same tree through the in-process
   harness: once into an empty destination, and once again with nothing
   changed (the sender's lookups decide everything). Each ends once the
   daemon's side has too, its index flush included.

`tests/blocking_pool.rs` on CI is worth a look after the merge as well.
CI timings vary a lot (23 to 63 s on Ubuntu so far), so it is a check, not
a pass/fail number.

### Trees

| tree    | files  | paths                                                   |
|---------|--------|---------------------------------------------------------|
| short   | 20,000 | `dNN/fNNNNN`, about 10 bytes                            |
| typical | 58,520 | those of a `~/.cargo/registry/src`, 79 bytes on average, in sorted order as the walk visits them |
| long    | 20,000 | the 783-byte paths of `tests/blocking_pool.rs`          |

Every file holds 8 bytes: one chunk each. Files with more chunks would
favour the new schema more, since each chunk row loses a path copy.

### How it runs

- A release build, on ZFS (`TMPDIR` on `/home`).
- Five rounds, alternating today's schema and the branch, one run of each
  tree per round. The median and the range are reported.
- On the same machine, otherwise idle.

### Decision rule

Merge if, in release on the real disk:

- for `long`, the database is at most a third of today's size and the flush
  takes at most half today's time;
- for `short` and `typical`, the flush, the lookups and both syncs are no
  more than 10% slower than today.

## Results (2026-11-16)

Release build, ZFS, an 8-core Linux machine that was otherwise idle. Each
schema against today's (`main` at 32f1ecc), five alternating rounds each.
Each cell is the median, with the range of the five runs:

| tree | measure | today | counter ids | change | hash ids | change |
|---|---|---|---|---|---|---|
| short | flush (s) | 0.39 (0.37–0.41) | 0.35 (0.35–0.39) | −10% | 0.86 (0.84–0.92) | +122% |
| short | database + WAL (MB) | 9.9 | 8.6 | −13% | 12.1 | +22% |
| short | `get_entry` (µs) | 7.60 (7.24–9.32) | 7.20 (7.13–7.66) | −5% | 7.30 (7.03–8.12) | −4% |
| short | `find_chunk` (µs) | 6.45 (6.27–7.37) | 5.59 (5.55–5.85) | −13% | 6.03 (5.91–6.57) | −5% |
| short | first pull (s) | 2.95 (2.91–3.15) | 3.09 (2.92–3.14) | +5% | 3.31 (3.23–3.48) | +7% |
| short | pull, nothing changed (s) | 0.41 (0.40–0.41) | 0.39 (0.38–0.40) | −6% | 0.39 (0.39–0.39) | −4% |
| typical | flush (s) | 3.46 (3.43–3.59) | 1.82 (1.73–1.88) | −47% | 6.62 (6.58–6.65) | +92% |
| typical | database + WAL (MB) | 43.4 | 26.5 | −39% | 27.3 | −37% |
| typical | `get_entry` (µs) | 8.45 (8.30–8.72) | 7.51 (7.46–7.57) | −11% | 7.33 (7.22–7.51) | −14% |
| typical | `find_chunk` (µs) | 7.26 (6.93–7.87) | 5.57 (5.48–5.65) | −23% | 6.20 (6.03–6.36) | −12% |
| typical | first pull (s) | 9.97 (9.73–10.43) | 9.42 (9.15–9.59) | −5% | 13.43 (13.13–13.73) | +36% |
| typical | pull, nothing changed (s) | 1.40 (1.35–1.43) | 1.25 (1.21–1.33) | −11% | 1.22 (1.19–1.29) | −11% |
| long | flush (s) | 4.66 (4.46–4.98) | 1.39 (1.37–1.43) | −70% | 2.70 (2.63–2.72) | −42% |
| long | database + WAL (MB) | 111.7 | 45.1 | −60% | 35.5 | −68% |
| long | `get_entry` (µs) | 13.72 (13.55–14.14) | 10.53 (9.96–11.25) | −23% | 8.35 (8.23–8.80) | −39% |
| long | `find_chunk` (µs) | 9.02 (8.76–9.76) | 5.98 (5.84–6.11) | −34% | 6.02 (5.83–6.09) | −32% |
| long | first pull (s) | 7.23 (6.96–7.53) | 3.93 (3.88–4.01) | −46% | 5.59 (5.57–5.80) | −23% |
| long | pull, nothing changed (s) | 0.68 (0.67–0.70) | 0.56 (0.55–0.58) | −17% | 0.48 (0.43–0.52) | −30% |

The two runs measured today's schema separately (five rounds each); their
medians agree within a few percent.

Counter ids against the decision rule:

- `long`: the flush takes 30% of today's time (met). The database is 40%
  of today's size, short of the third the rule asked for. That figure
  came from the hash ids' estimate (23 MB); with a counter the path is
  stored twice, and 45 MB was measured against the 38 MB estimated.
- `short`, `typical`: nothing is more than 10% slower. The one slower
  figure, the first pull of `short` (+5%), lies within the ranges of both.

Hash ids miss the rule for `long` (the flush takes 58% of today's time)
and by far for `short` and `typical`.

**Why hash ids lose: they throw away the order the writes come in.** The
walk sorts each directory (`scan.rs`), so files are hashed, and their rows
queued, in path order. Today's tables are keyed by path or by an increasing
row number, and so are those with a counter id: a batch of 1,024 writes
lands on a few neighbouring pages, and those pages end up full. A hash id
puts each file's row, and its chunks, on a random page: a batch touches
about 1,024 pages, each written to the WAL in full, and pages split at
random positions are left partly empty. Shuffling the input order shows
it. For `typical`, today's schema takes 3.5 s in walk order and 7.7 s
shuffled; hash ids take 6.5 s and 6.6 s.

**Why counter ids beat today's schema even for short paths:** each chunk
row and each entry of the chunk-hash index holds an 8-byte id instead of
the path, and `chunks` has no separate key index any more.

## Tests

- Index: a path written again keeps its id, and its old chunk rows are
  gone. Paging `find_chunks` by id lists each file once, in id order, and
  a page picks up after the last holder's id.
- `find_chunks` paging with more holders than a page holds, the first ones
  gone from disk, still finds a live one (the existing `local_sources`
  tests, now paging by id).
- Upgrade:
  - With a version 1 file, opening the root creates `.v2.sqlite`, takes its
    `partials` and `sessions` (not its entries), copies only on creation,
    and leaves the old file as it was.
  - `Index::remove_unused` keeps a file another connection has open, idle,
    and removes it with its WAL files once that connection is closed.
  - `irsync gc` lists an old file on a dry run, keeps it while it is open,
    and removes it otherwise, leaving the version 2 file.
- The existing index tests, rewritten for the new statements, all pass.
