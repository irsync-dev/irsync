# Design: keep every file operation inside the export

Date: 2026-11-20. Status: approved by Mike on 2026-11-20.

Source: a privately reported finding, to be published with the release.

## Why

irsync promises that a client granted an export can only touch files inside it.
Today the daemon checks folders once and then works with full path strings, so a
folder swapped for a symlink in between leads the daemon outside the export.
Reading the code confirmed all three vectors in the advisory:

1. Push: part files, their rename, symlinks, special files and folder metadata
   are all done by path (`recv.rs`, `session/planner.rs`, `fs/meta.rs`,
   `session/receiver.rs`).
2. Pull: files are opened by path with only the last name guarded
   (`fs/safe.rs::open_regular_nofollow`, used by `hasher.rs`, `send.rs`,
   `session/sender_role.rs`, `session/recv_io.rs`).
3. `--partial-dir`: only the spelling of the name is checked
   (`session/engine.rs`), never what is at that name, and the receiver's
   listing skips it.

Deletes (`delete.rs`), hard links (`planner.rs::place_hardlink`) and part
cleanup (`daemon.rs::remove_part`) are already safe: they use
`fs/safe.rs::open_dir_beneath`, which opens each folder in turn with
`O_NOFOLLOW` and then works relative to the open folder.

## Goals

- No file operation of a session can leave its root through a symlink, whatever
  happens to the tree while the session runs. This holds against other irsync
  sessions and against any other local program that can write in the tree.
- One code path on Linux, macOS and FreeBSD.
- One change, one release.

## Non-goals

- The per-system "stay below this folder" calls (Linux `openat2`, FreeBSD
  `O_RESOLVE_BENEATH`, macOS `O_NOFOLLOW_ANY`). The new interface must allow
  adding them later without touching callers.
- Any protocol change. None is needed.
- Restricting symlink targets a client may upload. A symlink that points outside
  the export is legitimate data; the daemon just must never walk through it.

## Anti-goals

- A visible slowdown on trees with many small files. Target: within a few
  percent of today on the existing benchmarks/stress runs.
- Behaviour changes for honest use. The one intended change: a symlink (or
  anything that is not a real folder) at the `--partial-dir` name, or on the way
  to it, is now an error for that session.
- Running out of file handles because of the folder cache.

## Constraints

- Every `unsafe` block needs a `// SAFETY:` comment (clippy enforces it).
- macOS-only code only compiles on the Mac; FreeBSD is built by hand. So keep
  per-system code to the minimum.
- The receiver already budgets file handles per file in flight
  (`planner.rs::FDS_PER_INFLIGHT`, `fs/rlimit.rs`). The cache must be counted
  in that budget.
- `fs/transient.rs::retry` already waits out a momentary handle shortage; new
  opens on the hot path should go through it.
- The root itself may be reached through symlinks the user chose (a client's
  local folder, an export path in the config). Only what is below the root is
  guarded. This matches `open_dir_beneath` today.

## Approaches considered

- **A. Folder handles everywhere, walked step by step (chosen).** Open the root
  once, reach each folder one name at a time refusing symlinks, do every
  operation relative to the open folder plus one file name, and cache open
  folders per session. One code path everywhere, reuses a tested pattern. Cost:
  many call sites change; the cache needs a limit.
- **B. Same, with each system's one-step call.** Fewer system calls, but three
  security-critical code paths plus a fallback, each testable only on its own
  system. Kept as a possible later speed-up behind A's interface.
- **Lock an export to one session.** Rejected: does not stop other local
  programs, does not fix `--partial-dir`, removes parallel sessions.
- **Re-check parents before each open.** Rejected: narrows the window, never
  closes it.

Chosen: A, picked by Mike on 2026-11-20.

## Design

### A new small module, `fs/beneath.rs` (name is the builder's call)

It grows out of `fs/safe.rs`. Two types:

- **Root**: the open root folder of a session side, plus its path for messages.
  Shared (`Arc`).
- **Dir**: an open folder below the root, shared (`Arc<OwnedFd>`), with the
  relative path it was opened as (for messages only).

`Root::dir(rel)` returns the `Dir` for a relative path. It walks from the
nearest cached ancestor, one name at a time, with
`O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`. A symlink or non-folder on the way is
the existing "a path component is a symlink or not a directory" error.
`Root::dir_create(rel)` does the same but makes missing folders with
`mkdirat` (then opens them with `O_NOFOLLOW`).

**Cache.** Per session side, bounded, least-recently-used, keyed by relative
path. My call: 64 entries by default, counted in the handle budget next to
`FDS_PER_INFLIGHT`. A cached handle may outlive its folder (deleted or
swapped): operations then act on the old, real folder or fail with "not found".
Both are harmless and stay inside the tree. Operations that hold a `Dir` keep
it alive past eviction because it is reference-counted.

**Operations on a `Dir` and one name** (the name is a single path component,
never containing `/`):

| Need | How |
| --- | --- |
| open a regular file to read | `openat` with `O_NOFOLLOW | O_NONBLOCK`, then the existing regular-file check on the handle |
| open/create a part file | `openat` with the current `PART_FLAGS` plus create |
| look at an entry | `fstatat` with `AT_SYMLINK_NOFOLLOW`; `readlinkat` for a symlink's target |
| make a folder, replacing a non-folder | `fstatat`, `unlinkat`, `mkdirat` |
| make a symlink / FIFO / device | `symlinkat`, `mkfifoat`, `mknodat` on the temp name |
| rename into place | `renameat(dir_a, name_a, dir_b, name_b)` |
| remove | `unlinkat` |

**Metadata.**
- Regular files: already through the open file. No change.
- Folders: open the folder itself via `Root::dir` and use `fchown`,
  `fchmod`, `futimens` on the handle.
- Symlinks and special files: `fchownat` and `utimensat` with
  `AT_SYMLINK_NOFOLLOW` relative to the `Dir`. Mode is the hard one:
  today `fs/meta.rs::apply_metadata` calls `fchmodat` in follow mode. Rule
  for the builder: **never chmod through a name that could be a symlink.**
  Suggested (builder to confirm against each system's man pages): macOS and
  FreeBSD accept `AT_SYMLINK_NOFOLLOW` in `fchmodat`; on Linux open the node
  with `O_PATH | O_NOFOLLOW`, check its type with `fstat`, and chmod through
  that handle (`/proc/self/fd/N`, or `fchmodat2` where the kernel has it).
  If neither works, create the node with its final mode under a zero umask
  scope is NOT acceptable (umask is process-wide); fail the entry instead.

### Where the root is opened

- Daemon: open the export root once per session. Resolve the session root from
  it with `Root::dir` / `dir_create`. This replaces the `lstat` chain in
  `engine.rs::prepare_root` (keep its three modes: refuse, create, allow for a
  dry run). The single-file paths in `daemon.rs` (`place_single`,
  `source_single`, `probe_kind`) use the same.
- Client: open the local folder normally, then everything below it the same way.
- `Ctx` gets the `Root` for `base` (the index root; index keys are relative
  to it). `Ctx::full` and `Ctx::full_key` stay only for messages and for the
  walk; nothing may open, create, rename or stat through them after this change.

### Call sites to convert

Receiver (push to the daemon, pull to a client):
- `recv.rs`: `paths`, `open_rw`, `open_part`, `restore_owner_rw`,
  `part_exists`, `still_same_file`, `names_file`, `open_part_in_dir`
  (`create_dir_all`), `discard_part`, the rename and the remove in
  `Verified::commit` / `discard`, `open_inplace`, `Source::Local`.
  `FileWriter` holds two `Dir`s (destination parent, part parent) and two
  names instead of two paths. Opening stays lazy as today.
- `--partial-dir`: resolved from the session root with `dir_create`, then the
  file's parent folders are mirrored below it the same way. The spelling check
  in `engine.rs` stays.
- `session/planner.rs`: `mkdir_replacing`, `place_via_temp`,
  `temp_sibling`, the `MetaOnly` branch. `place_hardlink` moves onto the
  shared cache.
- `session/recv_resume.rs`: `mkdir_replacing`.
- `session/receiver.rs::directory_pass`.
- `session/recv_io.rs`: `unchanged_file`, the open near line 535,
  `read_local`, `still_indexed`, `settle_identical`.
- `delete.rs` and `daemon.rs::remove_part`: already safe; move them onto the
  shared `Root` so there is one implementation.

Sender (pull from the daemon, push from a client):
- `hasher.rs` (open before hashing), `send.rs` (open before sending chunks),
  `session/sender_role.rs` (`tail_digest`, `walk_admits`, the re-stat in
  `spawn_digest`), `fs/meta.rs::lstat_entry_with_ctime`, and the source open
  in `fs/clone.rs` near line 195.

After the change, `fs/safe.rs::open_regular_nofollow(path)` should have no
caller left that passes a path below a session root. A grep for `std::fs::`,
`OpenOptions`, `AT_FDCWD` and `.join(` in the files above is the checklist.

### The directory walk (in scope, second pull request, same release)

Decided by Mike on 2026-11-20: in.

The listing walk (`scan.rs`, jwalk) reads folders by path with
`follow_links(false)`. If a folder is swapped for a symlink while the walk is
inside it, the walk can list names and sizes from outside the root. Replace it
with a walker built on open folder handles: each folder is opened from its
parent's handle with `O_NOFOLLOW`, read through that handle
(`fdopendir` / `nix::dir::Dir`), and its entries are looked at with
`fstatat` / `readlinkat` relative to it. The walker must keep what callers
rely on today: the same entries in the same order, gaps reported for unreadable
folders, parallel threads (`WALK_THREADS`), `max_depth`,
`follow_root_symlink` for the root only, pruning of clone folders and
irsync's temp names, and the same statistics. Handles held by the walk are
bounded (one per folder being read per thread) and counted in the handle
budget.

This lands as its own pull request after the first, and both ship in the same
release. The first is complete on its own for file contents; the second closes
the listing leak.

### Calls made after the reviewer's read (Claude's, for Mike to check)

- **Absolute `--partial-dir` on a pull** (client side, the user's own machine):
  kept. It is a second root the user chose: opened normally, symlinks allowed on
  the way to it (`/tmp` on macOS is one), guarded below it like any root. On a
  push the daemon still refuses an absolute one, as today.
- **`--paranoid` clone folder** (`fs/clone.rs::area_dir_below`): in scope. It
  canonicalizes the file's parent by path and then makes
  `.irsync-clones-<uid>` above it, so a swapped parent could place that folder
  elsewhere. Fix: choose the folder from the session root's real path (resolved
  once, when the root is opened) and never from a path below the root. Clones
  for files on another filesystem below the root (a mount inside the tree) then
  fall back to no clone, as an unwritable area does today. This can change
  where clones live for such trees.
- **Daemon background work outside a session** (the index warm-up in
  `warm.rs`, the stat near `daemon.rs:1422`, garbage collection): covered.
  Each gets its own `Root` for the export and its own tests.

## Tests

- Unit tests for the new module: every operation refuses a symlinked component,
  cache eviction, an evicted-but-held `Dir` still works, a swapped folder after
  the handle is held stays inside.
- Session tests in `crates/irsync/tests/containment.rs`, in-process on the
  existing `Harness`, made deterministic with the existing test hooks (add a
  hook where one is missing) rather than by timing:
  - push: a destination folder becomes a symlink to an outside folder between
    planning and the first write, and between the last write and the rename;
    nothing appears outside, the file fails with a clear error;
  - pull: the same swap between listing and hashing, and between hashing and
    sending; nothing from outside is sent;
  - resumed pull from a stored plan with a symlinked parent;
  - `--partial-dir` naming a symlink, and a symlink below a real partial dir;
  - symlink, FIFO and folder metadata placed while the parent is swapped.
  Each test asserts on an outside "canary" folder: no new entries, contents and
  modes unchanged.
- The full run before pushing: `tools/ci.sh check`. Also `tools/ci.sh miri`
  if the module adds `unsafe`.
- Walker (second pull request): the existing `scan.rs` tests and
  `rsync_compare` pass unchanged; new tests swap a folder for a symlink
  mid-walk (with a hook) and assert nothing from outside is listed; a
  comparison test runs old and new walkers over generated trees (filegen) and
  asserts identical output before the old one is removed.
- Speed: compare a many-small-files push and pull before and after (the stress
  crate has the tooling); record the numbers in the PR.

## Rollout

- Decided by Mike on 2026-11-20: the work goes through the public repository
  as normal pull requests, so the usual CI runs on Linux and macOS. Usage is
  still small, so nobody is put at risk. Titles, branch names, commit messages
  and descriptions are worded as safety fixes / hardening (for example "Do
  file operations relative to open folders") and do not spell out the attack.
  The advisory text stays out of the repository until release; this design is
  in it (Mike's decision).
- FreeBSD is built and tested by hand, as today.
- Book: update "In the synced trees" in
  `book/src/reference/files-and-state.md` and the `--partial-dir` notes to
  say what is refused. Add the security note to the release notes.
- Publish the advisory with the release, per `SECURITY.md`.

## Left to the builder

- Names and exact shape of the types and functions.
- Whether the cache lives in `Root` or beside it; its exact size, as long as it
  is bounded and budgeted.
- The order of converting call sites, as long as the result lands as one change.
- Which new test hooks are needed.
