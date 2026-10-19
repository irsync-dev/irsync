# fileset — design

Date: 2026-10-19
Status: draft, awaiting approval

## Intent

`fileset` is a standalone command-line tool that records the state of a
directory tree in a manifest stored at the root of that tree, and later
verifies the tree against it. It has two verbs sharing one engine:

- `fileset update <PATH>` scans the tree and brings the manifest up to date.
- `fileset check <PATH>` scans the tree and reports every difference
  between it and the manifest.

It serves two jobs.

1. **Archival integrity.** Run `update` once, run `check` months or years
   later, and learn whether any file was lost, any byte flipped, or any
   metadata drifted. Bit rot, filesystem corruption and accidental edits all
   show up as differences.
2. **Independent validation of irsync.** Run `update` on the source tree,
   let irsync carry the tree (manifest included) to the destination, run
   `check` there. A clean check is evidence, produced by code that shares
   nothing with irsync, that the sync delivered what it promised.

The second job is why the manifest lives inside the tree and travels with
it, and why `check` has to be strict enough that passing means something.

## Goals

- Record every POSIX filesystem object under the root: directories, regular
  files, symlinks, FIFOs, sockets, character and block devices.
- Record enough metadata that a later check can detect content changes
  (BLAKE3), permission changes, timestamp changes, ownership changes,
  extended attribute changes and type changes.
- Scale to millions of entries and terabytes of content: memory use must
  not grow with the size of the tree, and a routine `update` must not
  re-read every byte.
- Deterministic output across runs, independent of the order the
  filesystem happens to return directory entries.
- Work on Linux (ext4, btrfs, ZFS) and macOS (APFS, ZFS).
- Share no code with irsync.

## Non-goals

- Nested filesets. There is exactly one manifest, at the root. Manifests
  found deeper in the tree are warned about and otherwise treated as
  ordinary files.
- Following symlinks. Symlinks are recorded as symlinks, never traversed.
- Repairing anything. `check` reports; it never writes to the tree.
- macOS ACLs. They need a separate API and are out of scope for v1. Linux
  POSIX ACLs are covered because the kernel exposes them as xattrs.
- Windows.
- A human-readable or text export of the manifest. Could come later as a
  separate verb; not part of this design.
- Comparing two manifests to each other, or two trees to each other.

## Anti-goals

Things that would make this a failure even if it technically works:

- A `check` that passes while content differs.
- Silently weakening the manifest: an unreadable file must produce a
  warning at `update` time, not a quiet entry without a checksum.
- Memory proportional to the number of entries.
- Output that differs between two runs on an unchanged tree.
- Depending on irsync crates, or on irsync being installed.

## Constraints and decisions already made

These were decided in conversation and are not open to the builder.

### Commands and flags

```
fileset update [OPTIONS] <PATH>
    --store <sqlite|fjall>   backend to create if no manifest exists yet (default: sqlite)
    --rehash                 re-hash every regular file even if size, mtime and inode are unchanged
    --cross-mount-points     descend into directories on other filesystems
    --no-dir-sort            do not sort directory entries before iterating

fileset check [OPTIONS] <PATH>
    --relaxed                compare only type, content and symlink target
    --paranoid               compare everything that is recorded and comparable
    --ignore <FIELD>         drop one field from the comparison (repeatable)
    --require <FIELD>        add one field to the comparison (repeatable)
    --cross-mount-points     as above
    --no-dir-sort            as above
```

Both verbs also take:

```
    --manifest <MPATH>       location of the manifest instead of <PATH>/.fileset.sqlite
                             or <PATH>/.fileset.fjall; may be outside the tree
    --fail-on-warnings       exit 1 instead of 0 when the run produced warnings
    -j, --jobs <N>           hashing threads (default: CPU count)
    -q, --quiet              suppress progress
```

Names of `--relaxed`, `--paranoid`, `--ignore`, `--require`, `--manifest`,
`-j`, `-q` are the designer's choice; the rest are the user's.

### Manifest storage

- The manifest is a storage abstraction (`Store` trait) with two
  implementations: **SQLite** (default) and **fjall**. The two will be
  benchmarked later for disk size, read speed and write speed; the design
  must keep them interchangeable.
- SQLite: file `<PATH>/.fileset.sqlite`. Key is the relative path. Every
  recorded property is a real column. Extended attributes live in a
  child table keyed by (path, name).
- fjall: directory `<PATH>/.fileset.fjall/`. Key is the relative path.
  Value is a custom protobuf message (`Entry`) holding all properties
  including xattrs.
- `update` on SQLite implicitly upgrades the schema to the current version
  before doing anything else. `check` never writes; on an older schema it
  exits with code 2 and tells the user to run `update`. Both backends
  refuse a manifest written by a newer schema version.
- `--store` only matters when no manifest exists yet. If one exists and
  `--store` names a different backend, that is an error. If both backends'
  files exist at the root, that is an error.
- `check` auto-detects the backend by which of the two exists. With
  `--manifest`, a directory is fjall and a regular file is SQLite.
- `--manifest <MPATH>` moves the manifest anywhere. The point is read-only
  media: to check a read-only mounted backup or a snapshot, copy the
  manifest off it and point `--manifest` at the copy. Neither backend is
  required to open without writing to its own files, so `check` with the
  manifest inside a read-only tree may fail with exit 2 and a message
  suggesting `--manifest`. When MPATH lies inside the tree, that object
  gets the `store` entry exactly as the default location would; when it
  lies outside, nothing in the tree is flagged `store`.
- `update` finishes by checkpointing and truncating the SQLite WAL and
  closing cleanly, so a single `.fileset.sqlite` file is self-contained
  and can be copied or synced on its own. Designer's call.
- The manifest's own files are recorded as present but are never hashed,
  and their metadata is exempt from comparison. Concretely: the
  `.fileset.sqlite` file or the `.fileset.fjall` directory gets one entry
  flagged `store`; the contents of `.fileset.fjall/` are not walked at all;
  SQLite's transient sidecars (`-wal`, `-shm`, `-journal`) are not recorded.
- No YAML or other in-memory manifest. It was dropped because it cannot be
  updated in place and does not fit in RAM at the target scale.

### Scanning

- Directory entries are sorted by name (byte-wise) before iteration unless
  `--no-dir-sort`. This makes walk order, and therefore output order,
  deterministic regardless of on-disk directory ordering.
- Symlinks are never followed. Every stat is an `lstat`.
- Mount points are not crossed unless `--cross-mount-points`. A directory
  whose device differs from the root's is recorded (its own metadata) but
  not descended, with a warning naming it. The warning is emitted by both
  verbs. Known limitation, acknowledged: on btrfs every subvolume reports
  its own device number, so a tree containing subvolumes or snapshots is
  cut short by default and needs `--cross-mount-points`. Consulting the
  real mount table instead is an open point.
- `update` skips re-hashing a regular file when its size, mtime
  (nanosecond) and inode all match the stored entry, and reuses the stored
  checksum. `--rehash` forces hashing of every regular file. `check`
  always hashes every regular file it can read.
- A nested `.fileset.sqlite` or `.fileset.fjall` anywhere below the root
  produces a warning; the object is then tracked like any other file or
  directory (a nested fjall directory is walked and its contents hashed).

### Comparison

- Default compares: type, content (BLAKE3), symlink target, device numbers
  (for device nodes), mode, mtime, size.
- `--relaxed` compares: type, content, symlink target, device numbers.
- `--paranoid` compares everything in default plus: uid, gid, hard link
  count, extended attributes.
- Hard link count is compared only in paranoid mode (or with
  `--require nlink`), because it differs legitimately between a source and
  a copy.
- Extended attributes are recorded on all supported filesystems and
  compared only in paranoid mode (or `--require xattr`). Designer's call;
  the user asked for xattr support without placing it in a tier.
- `--ignore` and `--require` adjust whichever tier is in effect, so
  "default plus owner" is `--require uid --require gid`.

### Output

Human-readable, one line per differing object, to stdout:

```
ADD <path>
DEL <path>
CHG <path> <field>=<old>→<new> [<field>=<old>→<new> ...]
```

- `ADD`: on disk, not in the manifest.
- `DEL`: in the manifest, not on disk.
- `CHG`: in both, with at least one compared field differing. Every
  differing field is listed as `field=value`; the designer's addition is to
  show both the stored and the observed value, so the line stands alone.
  If the type differs, only `kind=` is shown.
- Warnings go to stderr prefixed `WARN `. The progress indicator goes to
  stderr only when stderr is a terminal and `-q` is not given.
- A summary line goes to stderr at the end: counts of added, deleted,
  changed, unchanged and warnings.
- Exit codes: 0 clean, 1 differences found, 2 the run itself failed
  (manifest unreadable, root not a directory, schema too new, ...).
  Warnings alone do not change the exit code unless `--fail-on-warnings`
  is given, in which case a run that would exit 0 exits 1. This applies to
  both verbs, so `update --fail-on-warnings` fails on an unreadable file.

## Data model

One entry per object, keyed by its path relative to the root. The root
directory itself is recorded under the path `.`.

Paths are byte strings, not UTF-8. Keys in both backends are raw bytes. For
display, non-printable and non-UTF-8 bytes are escaped; the builder picks
the escape syntax (something like `\xNN`) and uses it consistently.

| Field | Source | Compared in |
|---|---|---|
| kind | `st_mode` type bits: dir, file, symlink, fifo, socket, chardev, blockdev, other | always |
| mode | permission bits including suid, sgid, sticky (`st_mode & 07777`) | default |
| uid, gid | `st_uid`, `st_gid`, numeric only | paranoid |
| size | `st_size` | default, regular files and symlinks only |
| nlink | `st_nlink` | paranoid, never for directories |
| ino, dev | `st_ino`, `st_dev` | never (used for rehash skipping and mount detection) |
| rdev | `st_rdev`, meaningful for chardev and blockdev | default, device nodes only |
| mtime | `st_mtim`, seconds and nanoseconds | default |
| ctime | `st_ctim`, seconds and nanoseconds | never (cannot be set, recorded for forensics) |
| blake3 | 32-byte BLAKE3 of content; regular files only; absent if unreadable or store file | relaxed and up |
| target | symlink target bytes | relaxed and up |
| xattrs | sorted list of (name, value) byte pairs, read without following symlinks | paranoid |
| flags | bit set: `store` (manifest's own file), `unreadable` (hash could not be computed) | n/a |
| generation | update counter, see below | never |

Names of users and groups are deliberately not recorded; they are a
lookup, not a property of the object.

atime is not recorded: it is noise and the tool itself changes it.

Timestamps are compared exactly at nanosecond precision. A tolerance flag
for filesystems with coarser timestamp granularity is a plausible later
addition and is not in v1.

### Special cases in comparison

- **Root directory** (`.`): mtime is exempt, because writing the manifest
  changes it. Everything else is compared normally.
- **Store entries** (`store` flag): kind is compared; every other field is
  exempt.
- **Unreadable at update time** (`unreadable` flag, no hash): `check` emits
  a warning that no stored checksum exists for the path and compares the
  remaining fields. If the file is readable at check time it is still not
  hashed (nothing to compare against).
- **Unreadable at check time** with a stored hash: reported as
  `CHG <path> blake3=<hex>→unreadable`.
- **Directories**: `size` and `nlink` are never compared for directories.
  Both are filesystem-specific (ext4, btrfs, ZFS and APFS all report
  different directory sizes for the same contents) and would make every
  cross-filesystem check noisy. They are still recorded. Fields therefore
  have a per-kind applicability in addition to the tier, as `rdev` already
  does.
- **Symlink mode**: Linux reports 0777 for every symlink, macOS allows
  setting it. Compared like any other mode. A Linux-to-macOS sync will show
  this as a difference; `--ignore mode` or `--relaxed` is the remedy.
  Documented limitation.

## Engine

One engine, two drivers. The engine walks the tree and produces a stream
of observed entries; `update` reconciles the stream into the store,
`check` compares the stream against the store.

### Walk

- Single walker thread. `lstat` the root, remember its `st_dev`. Depth-first
  from the root. In each directory, read all names, sort them byte-wise
  (unless `--no-dir-sort`), then for each: `lstat`, read xattrs, read
  symlink target if applicable, classify. Emit an observed entry.
- Directories are emitted before their contents.
- The walker never allocates per-tree state beyond the current directory's
  entry list and the recursion stack.

### Hashing

- Regular files that need hashing are sent through a bounded channel to a
  pool of `-j` hasher threads. Bounding the channel caps memory. Results
  flow through a second channel to the single consumer (store writer for
  `update`, comparator for `check`).
- Entries that need no hashing (directories, symlinks, files whose stored
  size/mtime/ino match on `update`) bypass the pool.
- Order of results is not the order of the walk. Output order for `check`
  must nonetheless be deterministic: the consumer reorders results back
  into walk order before printing (a sequence number on each entry and a
  small reorder buffer; the buffer is bounded by the channel capacity).
  Builder's choice of mechanism, but the property is required.

### update

1. Open or create the store. Run schema migrations. Read the current
   `generation` counter from store metadata and increment it to `G`.
2. Walk. For each observed entry:
   - Look up the stored entry by path.
   - If regular file, not `--rehash`, stored entry exists, is a regular
     file, has a hash, and size/mtime/ino match: reuse the stored hash.
     Otherwise hash (or mark `unreadable` and warn if it cannot be opened).
   - Upsert the entry with `generation = G`.
3. After the walk: delete every entry whose generation is not `G`. These
   are the objects that disappeared.
4. Write `generation = G` and the tool version into store metadata, commit.
5. Writes are batched into transactions of a few thousand entries. If the
   run is interrupted the store may contain a mix of generations; the next
   `update` repairs it, and `check` against an interrupted store is still
   meaningful because every entry is either current or stale-but-valid.

The generation counter is what makes deletion detection work without a
set of visited paths in memory and without requiring the walk order to
match the store's key order.

### check

1. Open the store read-only. Refuse an unknown or newer schema.
2. Walk. For each observed entry, look up the stored entry:
   - absent: `ADD`.
   - present: compare the fields selected by the tier and `--ignore` /
     `--require`; any difference: `CHG`.
3. Second pass: iterate the store in key order; `lstat` each path; if it
   does not exist on disk, `DEL`. This is O(entries) `lstat` calls, which
   is cheap next to hashing terabytes, and it keeps `check` from writing
   anything. A merge-join during the first pass would avoid the second
   pass but requires the store's key order to equal walk order; the builder
   may do that as an optimization later, it is not required.
4. Print the summary, exit 0/1/2.

## Storage

### `Store` trait

Roughly:

```rust
trait Store {
    fn open(root: &Path, mode: OpenMode) -> Result<Self>;   // Create | ReadWrite | ReadOnly
    fn meta(&self) -> Result<Meta>;                          // schema version, generation, tool version, timestamps
    fn get(&self, path: &[u8]) -> Result<Option<Entry>>;
    fn iter(&self) -> Result<impl Iterator<Item = Result<(Vec<u8>, Entry)>>>; // key order
    fn begin(&mut self) -> Result<Txn<'_>>;                  // put / delete / set_meta / commit
    fn delete_not_in_generation(&mut self, g: u64) -> Result<u64>;
}
```

Exact shape is the builder's. The requirements: point lookup by path,
streaming iteration, batched writes, atomic metadata update, and the
generation sweep.

### SQLite

- `rusqlite` with the bundled feature.
- `PRAGMA journal_mode=WAL`, `synchronous=NORMAL` during update.
- `meta(key TEXT PRIMARY KEY, value TEXT)` holding `schema_version`,
  `generation`, `tool_version`, `created_at`, `updated_at`.
- `entries(path BLOB PRIMARY KEY, kind, mode, uid, gid, size, nlink, ino,
  dev, rdev, mtime_sec, mtime_nsec, ctime_sec, ctime_nsec, blake3 BLOB NULL,
  target BLOB NULL, flags, generation) WITHOUT ROWID`, index on
  `generation`.
- `xattrs(path BLOB, name BLOB, value BLOB, PRIMARY KEY (path, name))
  WITHOUT ROWID`. Deleting an entry deletes its xattr rows in the same
  transaction.
- Migrations: an ordered list of SQL scripts, one per schema version,
  applied in a single transaction by `update` when `schema_version` is
  behind. Version 1 is the schema above.

### fjall

- One keyspace `entries`: key = relative path bytes, value = prost-encoded
  `Entry`.
- One keyspace `meta`: the same keys as the SQLite meta table.
- The protobuf lives at `crates/fileset/proto/fileset.proto` and is
  compiled at build time with `prost-build` + `protox` (no system protoc).
  Field numbers are stable forever; new fields are added, never renumbered.
- Schema version is still recorded in `meta` so a future semantic change
  can be detected, even though protobuf makes most changes
  backward-compatible without migration.
- The generation sweep is a full scan of `entries`, deleting as it goes.

## Crate layout

- `crates/fileset/`, a workspace member, binary target `fileset`.
- May use the workspace dependency table for version pinning only. Must
  not depend on `irsync` or `irsync-proto`.
- Suggested modules (builder may reorganize): `cli`, `walk`, `hash`,
  `model` (Entry, Kind, Flags), `compare` (tiers, field selection,
  diff), `report` (line formatting, escaping, summary), `store` (trait),
  `store::sqlite`, `store::fjall`, `engine` (update and check drivers).
- Libraries assumed: `clap` (derive), `blake3`, `rusqlite` (bundled),
  `fjall`, `prost` + `prost-build` + `protox`, `xattr`, `nix` or `libc` for
  `lstat` details not exposed by std, `anyhow`/`thiserror`. Thread pool and
  progress-bar crates are the builder's choice.

## Testing expectations

Left to the builder in detail, but the plan should cover at least:

- Unit tests for the comparator: each tier, `--ignore`/`--require`, the
  root/store/unreadable exemptions.
- Unit tests for line formatting including escaped paths.
- Integration tests on temp directories: create files, directories,
  symlinks, FIFOs; `update`; mutate (content, mode, mtime, rename, delete,
  add, retarget a symlink); `check`; assert exact output lines and exit
  code. Run the same suite against both backends through the trait.
- A test that two consecutive `check` runs on an unchanged tree produce
  byte-identical output, and that `update` after a shuffle of directory
  creation order produces an identical manifest iteration order.
- A test that a second `update` with unchanged files does not re-hash
  (observable via timing or a counter exposed for tests) and that
  `--rehash` does.
- A test for `--manifest` outside the tree (no `store` entry, check
  passes) and `--fail-on-warnings`.
- A test for the nested-manifest warning and the SQLite schema migration
  path (open a v1 database, run `update`, confirm the version bumps once a
  v2 exists; until then, confirm a too-new version is refused).
- Mount-point and device-node tests require privileges and may be marked
  ignored by default.

## Left to the builder

- Thread pool mechanism, channel sizes, reorder buffer implementation.
- BLAKE3 read strategy (buffered read vs mmap; blake3's own multi-threaded
  hashing for large files is allowed but must respect `-j`).
- Progress indicator design.
- Escape syntax for non-printable path bytes.
- Error types and how warnings are collected and counted.
- Exact timestamp rendering in `CHG` lines (must include nanoseconds).
- Whether `ADD` lines show the object's kind. Designer's preference: yes,
  `ADD <path> kind=file`.
- Migration framework details.
- Whether to merge-join instead of the second `lstat` pass in `check`.

## Open points, not blocking v1

- Timestamp tolerance for coarse-granularity filesystems.
- macOS ACLs.
- A text export verb.
- Allowing nested filesets behind a flag.
- Detecting mount points via the mount table instead of `st_dev`, for
  btrfs subvolumes.
- Whether fjall or SQLite wins the benchmark and becomes the sole backend.

## Amendments made during implementation

Rulings taken during the build, recorded here so the spec matches the
shipped behaviour. The ledger line for each is in git history of the
implementation branch.

- **Unverifiable content is a difference.** Stored entries that `check`
  cannot observe, because their directory could not be read or was not
  descended (mount point, see `--cross-mount-points`), or because their
  own `lstat` fails with an error other than "not found", are reported as
  `CHG <path> kind=<kind>→unverified` and counted as changed, so the run
  exits 1. The anti-goal "a check that passes while content differs"
  outranks the rule that warnings alone never change the exit code.
- **The store's parent directory is mtime-exempt**, exactly as the root
  is, because writing the manifest and its sidecars changes that
  directory's mtime. The root is simply the default store's parent.
- **No nested-manifest warning at the root level.** A `.fileset.sqlite`
  or `.fileset.fjall` that is a direct child of the root is not "below
  the root"; it is tracked as an ordinary object without a warning. This
  keeps the documented read-only workflow (`--manifest` pointing at a
  copy) free of warnings.
- **`update` claims its generation before writing entries**, so an
  interrupted run cannot leave entries that the next run would mistake
  for current.
- **fjall manifests are never created by `check`**: opening in read-only
  or read-write mode requires fjall's own marker files to exist.
- **xattr and symlink-target read failures** keep the entry (with a
  warning) instead of dropping it and its subtree.
