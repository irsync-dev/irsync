# File Operations Relative to Open Folders: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** No file operation of a session can leave its root through a symlink, whatever happens to the tree while the session runs.

**Architecture:** A new module `fs/beneath.rs` opens a root folder once and reaches every folder below it one name at a time with `O_NOFOLLOW`, keeping a bounded cache of open folders. Every read, write, rename, create and metadata call of a session is done relative to such an open folder plus one file name. A second pull request replaces the path-based directory walk with one built on the same handles.

**Tech Stack:** Rust, `nix` 0.31 (`openat`, `mkdirat`, `renameat`, `fstatat`, `unlinkat`, `symlinkat`, `readlinkat`, `nix::dir::Dir`), tokio, the existing test `Harness` in `crates/irsync/tests/common`.

**Spec:** `docs/superpowers/specs/2026-11-20-export-containment-design.md`

## Global Constraints

- One code path on Linux, macOS and FreeBSD. Per-system code only where the spec names it (chmod of a special file).
- Every `unsafe` block has a `// SAFETY:` comment.
- The root itself may be reached through symlinks; only what is below it is guarded.
- Folder cache: 64 entries per `Root`, least-recently-used, counted in the handle budget.
- Hot-path opens go through `crate::fs::transient::retry`.
- Never chmod through a name that could be a symlink.
- No protocol change. No new restriction on symlink targets.
- Honest use behaves as before, except: anything but a real folder at or on the way to a relative `--partial-dir` is an error.
- Speed on many small files stays within a few percent of `main`.
- Branch names, commit messages, PR titles and descriptions are worded as safety hardening and do not describe an attack. No attribution trailers of any kind.
- All work goes through pull requests against `main`. Before each push: `tools/ci.sh check`.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`.
- Two pull requests: Tasks 1-9 on branch `folder-handle-file-ops` (exists, holds the spec), Tasks 10-11 on branch `folder-handle-walk` cut from `main` after the first is merged.

## Review Focus

1. A folder deleted and made again at the same path during a session (by the session's own delete pass, or by another session): the next file in it must still be placed, not fail forever on a stale cached handle. Test in Task 1 and Task 5.
2. A low open-file limit (`ulimit -n 256`) with a wide tree: the session finishes, slower, without `EMFILE` errors. Test in Task 3.
3. A tree deeper than `PATH_MAX` allows as one string, and names that are not UTF-8 or are 255 bytes long: listed, sent and placed as today. Test in Task 1 (depth, bytes) and Task 5 (255-byte name with the shortened temp name).
4. The local folder or export path given through a symlink (`/tmp/x` on macOS): works unchanged on both sides. Test in Task 3.
5. `--partial-dir` on another filesystem than the destination (rename fails with `EXDEV`): the same error text as today, the part file stays. Test in Task 5.

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/irsync/src/fs/beneath.rs` (new) | `Root`, `Dir`, the cache, every folder-relative operation |
| `crates/irsync/src/fs/safe.rs` | keeps `errno`, `is_symlink_errno`, `nofollow_error`, `require_regular`, `split_last`; `open_dir_beneath` becomes a thin call into `beneath` and is removed once unused |
| `crates/irsync/src/fs/meta.rs` | entry-from-stat helpers take a `Dir`; path-based `apply_metadata` removed |
| `crates/irsync/src/fs/walk.rs` (new, Task 10) | the handle-based walker |
| `crates/irsync/tests/containment.rs` | all session-level swap tests |

---

## Pull request 1

### Task 1: `Root`, `Dir` and the cache

**Files:**
- Create: `crates/irsync/src/fs/beneath.rs`
- Modify: `crates/irsync/src/fs/mod.rs` (add `pub mod beneath;`)

**Interfaces:**
- Produces:

```rust
pub const DIR_CACHE: usize = 64;

/// What to do with a missing folder on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing { Refuse, Create }

pub struct Root { /* private */ }
impl Root {
    /// Opens `path` (symlinks on the way are followed) with a cache of DIR_CACHE.
    pub fn open(path: &Path) -> io::Result<Arc<Root>>;
    pub fn open_with_cache(path: &Path, cap: usize) -> io::Result<Arc<Root>>;
    pub fn path(&self) -> &Path;
    /// The folder `rel` below the root; `RelPath::root()` is the root itself.
    pub fn dir(&self, rel: &RelPath, missing: Missing) -> io::Result<Dir>;
    /// `rel`'s parent folder and final name.
    pub fn parent(&self, rel: &RelPath, missing: Missing) -> io::Result<(Dir, OsString)>;
    /// A new Root at the folder `rel`, with its own cache.
    pub fn sub(&self, rel: &RelPath, missing: Missing) -> io::Result<Arc<Root>>;
    /// Drops cached handles at or below `rel`.
    pub fn forget(&self, rel: &RelPath);
    /// Handles the cache holds now (tests, budget).
    pub fn cached(&self) -> usize;
}

#[derive(Clone)]
pub struct Dir { /* Arc<OwnedFd> + RelPath for messages */ }
impl AsFd for Dir {}
impl Dir { pub fn rel(&self) -> &RelPath; }
```

- Error for a symlink or non-folder on the way: `io::ErrorKind::InvalidInput`, message `"{rel}: a path component is a symlink or not a directory"` (the text `open_dir_beneath` uses today).

- [ ] **Step 1: Write the failing tests** (in `beneath.rs`, `#[cfg(test)]`)

```rust
#[test] fn dir_refuses_a_symlinked_component()        // root/esc -> outside; dir("esc"), dir("esc/x"), dir("a/esc") all InvalidInput
#[test] fn dir_create_makes_missing_folders_only()    // Create makes a/b/c (mode 0o777 & !umask); Refuse gives NotFound; a file on the way is InvalidInput
#[test] fn root_may_be_reached_through_a_symlink()    // Root::open(link_to_root) works; below it a symlink is still refused
#[test] fn cache_is_bounded_and_evicts_least_recent() // open_with_cache(.., 2); touch 3 folders; cached() == 2; the first is opened again on demand
#[test] fn a_held_dir_outlives_eviction()             // hold Dir for "a", evict it, fstatat through it still works
#[test] fn a_swapped_folder_stays_inside()            // hold Dir for "d"; rename d away, symlink d -> outside; create via held Dir lands in the renamed real folder, outside stays empty
#[test] fn forget_reopens_a_recreated_folder()        // Dir for "d"; rmdir d; mkdir d; forget("d"); dir("d") has the new inode
#[test] fn a_tree_deeper_than_path_max_and_raw_byte_names() // 300 levels of 20-byte names, one name b"\xff\xfe": dir() reaches the bottom
```

- [ ] **Step 2: Run** `cargo test -p irsync --lib fs::beneath` — fails to compile (module missing).
- [ ] **Step 3: Implement.** Walk from the deepest cached ancestor; each step is `openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` inside `transient::retry`; on `ENOENT` with `Missing::Create`, `mkdirat(parent, name, 0o777)` (accept `EEXIST`) then open again. Cache: `Mutex` over a small LRU keyed by `RelPath` holding `Arc<OwnedFd>`; write it by hand (no new dependency). The root handle is never evicted.
- [ ] **Step 4: Run** the same command — all pass.
- [ ] **Step 5: Commit** — `Add a module for folders opened below a root`.

### Task 2: Operations on a `Dir` and a name

**Files:**
- Modify: `crates/irsync/src/fs/beneath.rs`, `crates/irsync/src/fs/meta.rs`

**Interfaces:**
- Consumes: `Dir` from Task 1; `ApplyOpts`, `FileEntry`, `process_ids`, `may_chgrp` in `fs/meta.rs`; `PART_FLAGS` semantics from `recv.rs:290`.
- Produces (every `name` is one path component; a name containing `/` or equal to `.`/`..` is `InvalidInput`):

```rust
impl Dir {
    /// Read-only, O_NOFOLLOW | O_NONBLOCK, must be a regular file. Error text as open_regular_nofollow.
    pub fn open_regular(&self, name: &OsStr) -> io::Result<(File, FileStat)>;
    /// Read/write, O_NOFOLLOW | O_NONBLOCK; `create` adds O_CREAT (mode 0o666).
    pub fn open_rw(&self, name: &OsStr, create: bool) -> io::Result<File>;
    /// Read-only or write-only as the mode allows, no create (for restore_owner_rw).
    pub fn open_any(&self, name: &OsStr) -> io::Result<File>;
    /// lstat.
    pub fn stat(&self, name: &OsStr) -> io::Result<FileStat>;
    pub fn read_link(&self, name: &OsStr) -> io::Result<Vec<u8>>;
    /// The entry and ctime of `name`, as lstat_entry_with_ctime gives for `rel`.
    pub fn entry(&self, name: &OsStr, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))>;
    /// Makes `name` a folder, removing a non-folder in the way.
    pub fn mkdir_replacing(&self, name: &OsStr) -> io::Result<()>;
    pub fn symlink(&self, target: &OsStr, name: &OsStr) -> io::Result<()>;
    /// FIFO or device node from the entry, as fs::meta::create_special.
    pub fn create_special(&self, name: &OsStr, e: &FileEntry) -> io::Result<()>;
    pub fn rename(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()>;
    pub fn unlink(&self, name: &OsStr) -> io::Result<()>;
    /// Owner, mode, times of this folder itself, through its handle.
    pub fn apply_metadata(&self, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()>;
    /// Owner, mode, times of a symlink or special file at `name`; never follows.
    pub fn apply_node_metadata(&self, name: &OsStr, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()>;
}
```

- [ ] **Step 1: Write the failing tests** (in `beneath.rs`)

```rust
#[test] fn open_regular_refuses_symlink_fifo_and_folder()
#[test] fn open_rw_never_follows_a_symlink_at_the_name()       // symlink to an existing and to a missing outside file: error contains "symlink"; nothing created outside
#[test] fn names_with_a_slash_or_dots_are_refused()            // "a/b", "..", "." on every operation: InvalidInput
#[test] fn mkdir_replacing_replaces_a_symlink_without_following() // name -> outside dir; afterwards a real folder; outside dir untouched
#[test] fn rename_moves_between_two_dirs()
#[test] fn entry_matches_lstat_entry_with_ctime()              // for a file, folder, symlink, fifo
#[test] fn apply_metadata_sets_the_folder_through_its_handle() // mode 0o750, mtime 1_600_000_000
#[test] fn apply_node_metadata_never_reaches_a_symlink_target()
// name is a symlink to an outside file with mode 0o600 and mtime 1; entry is a FIFO with mode 0o644, mtime 2:
// the outside file still has mode 0o600 and mtime 1 afterwards.
```

- [ ] **Step 2: Run** `cargo test -p irsync --lib fs::beneath` — new tests fail to compile.
- [ ] **Step 3: Implement.** Owner and times for a node: `fchownat` / `utimensat` with the dir handle and `AT_SYMLINK_NOFOLLOW`. Mode for a node (skipped for a symlink, as today): on macOS and FreeBSD `fchmodat` with `AT_SYMLINK_NOFOLLOW`; on Linux open the node with `O_PATH | O_NOFOLLOW | O_CLOEXEC`, `fstat` it, refuse a symlink, then `chmod("/proc/self/fd/N")`; if `/proc` is not there, return the error (the entry fails; nothing is followed). Check each call against that system's man page before relying on it.
- [ ] **Step 4: Run** — all pass. On Linux this is first proven in CI; say so in the PR.
- [ ] **Step 5: Commit** — `Add folder-relative file operations`.

### Task 3: Roots in the session, and the handle budget

**Files:**
- Modify: `crates/irsync/src/session/engine.rs` (`Ctx`, `prepare_root`, both places that build `Ctx`, the `create_dir_all` at :753), `crates/irsync/src/daemon.rs` (`place_single`, `source_single`, `probe_kind`), `crates/irsync/src/session/planner.rs:83-117`
- Test: `crates/irsync/src/session/engine.rs` tests, `crates/irsync/tests/single_file.rs`, `crates/irsync/tests/containment.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:

```rust
// engine.rs, in Ctx:
/// The open folder `base`: index keys are relative to it.
pub base_root: Arc<Root>,
/// The open session root (`root`): listing paths are relative to it.
pub session_root: Arc<Root>,

impl Ctx {
    /// The parent folder and local name of the listing path `rel` (maps a single file's wire name).
    pub fn parent(&self, rel: &RelPath, missing: Missing) -> io::Result<(Dir, OsString)>;
    /// The same for an index key, below `base_root`.
    pub fn parent_key(&self, key: &RelPath, missing: Missing) -> io::Result<(Dir, OsString)>;
}

// engine.rs: replaces the lstat chain; the three modes stay.
pub(crate) fn prepare_root(export: &Arc<Root>, rel: &RelPath, missing: engine::Missing)
    -> io::Result<(PathBuf, Option<Arc<Root>>)>;   // None only for Missing::Allow with a missing folder

// planner.rs
const FDS_PER_INFLIGHT: usize = 4;   // .part, old file, and up to two folders no longer cached
// inflight_for_limit: spare = soft - NOFILE_RESERVE - 2 * DIR_CACHE
```

- `Ctx::full` and `Ctx::full_key` stay, with a doc comment: for messages and logging only.

- [ ] **Step 1: Write the failing tests**
  - `engine.rs`: keep `session_root_never_passes_through_a_symlink` passing against the new signature; add `prepare_root_creates_refuses_and_allows` (the three modes, same errors as today: `"export root is missing"`, `"{path} does not exist"`, `"... is not a directory"` or the component error).
  - `planner.rs`: `inflight_window_leaves_room_for_folder_handles`: `inflight_for_limit(256) == 1`, `inflight_for_limit(1024) == (1024 - 256 - 128) / 4`, `inflight_for_limit(u64::MAX) == MAX_INFLIGHT`.
  - `containment.rs`: `a_root_given_through_a_symlink_works_both_ways` (export path and local path are symlinks to the real folders; push and pull of a small tree succeed, trees equal).
  - `containment.rs`: `a_wide_tree_finishes_under_a_low_handle_limit` (300 folders of 3 files each, `hooks.max_inflight = Some(8)`, `Root::open_with_cache(.., 4)` through a test hook `SideHooks::dir_cache: Option<usize>`; no error contains "Too many open files"; trees equal).
- [ ] **Step 2: Run** `cargo test -p irsync --all-features engine:: planner:: && cargo test -p irsync --all-features --test containment --test single_file` — new tests fail.
- [ ] **Step 3: Implement.** The daemon opens the export `Root` once per session in `serve_daemon` and passes it down; the client opens its local folder. `session_root = base_root.sub(scope prefix)`.
- [ ] **Step 4: Run** the same, then the everyday run — pass.
- [ ] **Step 5: Commit** — `Open the session root once and keep it`.

### Task 4: The sending side reads through folders

**Files:**
- Modify: `crates/irsync/src/hasher.rs` (`Hasher` gets `root: Arc<Root>`; `Hasher::new(index, root: Arc<Root>, workers)`; the open at :466), `crates/irsync/src/send.rs` (`Sender::new` takes `Arc<Root>`; the open at :508), `crates/irsync/src/session/sender_role.rs` (`tail_digest`, `walk_admits`, the re-stat in `spawn_digest`), `crates/irsync/src/fs/meta.rs` (`lstat_entry_with_ctime`), every caller of `Hasher::new` / `Sender::new` (`session/engine.rs`, `session/mod.rs:1044`, `warm.rs`, tests)
- Test: `crates/irsync/tests/containment.rs`, `crates/irsync/tests/hasher.rs`, `crates/irsync/tests/send.rs`

**Interfaces:**
- Consumes: `Root::parent`, `Dir::open_regular`, `Dir::entry`, `Ctx::parent_key`.
- Produces:

```rust
// fs/meta.rs — replaces the (root: &Path, rel) form for session code
pub fn lstat_entry_with_ctime(root: &Root, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))>;
// sender_role.rs
fn walk_admits(root: &Root, path: &RelPath, filters: &FilterSet) -> bool;   // reaches the parent with Root::parent(Refuse), then Dir::stat
fn tail_digest(root: &Root, key: &RelPath, from: u64, check: bool) -> io::Result<Hashed>;
// New test hook points (SideHooks::event): "hash-open" before the hasher opens a file, "send-open" before send.rs opens it.
```

- The path-based walk in `scan.rs` keeps a private path-based stat until Task 11.

- [ ] **Step 1: Write the failing tests** (`containment.rs`; each makes `outside/secret` with content `b"private key"`, and a tree `d/secret` with other content of the same size and mtime; the swap is: rename `d` to `d.real`, symlink `d -> outside`)

```rust
#[tokio::test] async fn pull_swap_before_hashing_sends_nothing_from_outside()   // swap on event "hash-open" for d/secret
#[tokio::test] async fn pull_swap_before_sending_sends_nothing_from_outside()   // swap on event "send-open"
#[tokio::test] async fn resumed_pull_refuses_a_symlinked_parent()               // stored plan names d/secret; d is a symlink before the reconnect
```
Each asserts: the client's copy of `d/secret` is absent or holds the inside content, never `b"private key"`; `out.errors` has an entry for `d/secret`; exit code 23.

- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test containment pull_ resumed_pull` — fail (outside content arrives).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same, then `cargo test -p irsync --all-features` — pass.
- [ ] **Step 5: Commit** — `Read source files through open folders`.

### Task 5: The file writer and `--partial-dir`

**Files:**
- Modify: `crates/irsync/src/recv.rs`, `crates/irsync/src/session/recv_io.rs` (:435, :458-467, :850-853, callers of `copy_local`), `crates/irsync/src/session/engine.rs` (`partial_dir_from_base`)
- Test: `crates/irsync/tests/recv.rs`, `crates/irsync/tests/containment.rs`, `crates/irsync/tests/push_options.rs`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces:

```rust
// recv.rs
pub struct PartialDir {
    /// The root part files live under: the session's base root for a relative
    /// option, or a Root of its own for an absolute one (a pull to a client).
    pub root: Arc<Root>,
    /// The option's path below `root` (empty for an absolute one).
    pub dir: RelPath,
    pub skip: usize,
}
/// Where a file and its part file live.
pub struct Place { pub dest_dir: Dir, pub dest_name: OsString, pub part_dir_rel: RelPath, pub part_root: Arc<Root>, pub part_name: OsString }
fn place(root: &Arc<Root>, rel: &RelPath, partial_dir: Option<&PartialDir>) -> io::Result<Place>;
pub fn discard_part(root: &Arc<Root>, rel: &RelPath, partial_dir: Option<&PartialDir>, index: &IndexHandle) -> io::Result<()>;
impl FileWriter {
    pub fn open(root: &Arc<Root>, rel: &RelPath, expected: &FileDigest, partial_dir: Option<&PartialDir>, index: IndexHandle) -> io::Result<FileWriter>;
    // open_tracked, open_from, open_inplace: the same change of the first parameter
    pub async fn copy_local(&mut self, idx: u32, from: (Dir, OsString), offset: u64) -> io::Result<()>;
}
```

- `FileWriter` keeps `dest` and `part` as `PathBuf` for messages only. The part folder is opened (`Missing::Create`) at the first write, as the file is today.
- A create in a cached folder that fails with `ENOENT`: `forget` that folder, open it again, try once more.
- `still_same_file` / `names_file` become `Dir::stat` on the held part folder; the rename is `part_dir.rename(part_name, &dest_dir, dest_name)`, keeping today's `EISDIR` and `rename {part} -> {dest}: {e}` messages.
- New hook points: `"part-open"` just before the part file is opened, `"place"` just before the rename.

- [ ] **Step 1: Write the failing tests**

`containment.rs` (canary: `outside/` holds `keep` = `b"keep me"`, mode 0o600; after each test `outside` lists exactly `["keep"]`, content and mode unchanged):

```rust
#[tokio::test] async fn push_swap_before_the_first_write_stays_inside()     // swap d on "part-open" for d/x
#[tokio::test] async fn push_swap_before_placing_stays_inside()             // swap d on "place" for d/x
#[tokio::test] async fn push_inplace_swap_stays_inside()                    // --inplace, swap on "inplace"
#[tokio::test] async fn partial_dir_that_is_a_symlink_is_refused()          // export/.p -> outside; push --partial-dir .p; every file errors with "symlink or not a directory"
#[tokio::test] async fn a_symlink_below_the_partial_dir_is_refused()        // export/.p/sub -> outside; push sub/x
#[tokio::test] async fn a_folder_made_again_mid_session_still_takes_files() // on "part-open" for d/b: rmdir-and-mkdir d (after d/a was placed and removed); d/b is placed
```

`recv.rs`: port the existing tests to the new signatures; keep `part_symlink_is_refused` and `partial_dir_and_copy_local`; add
```rust
#[tokio::test] async fn a_255_byte_name_is_placed_through_its_shortened_part()
#[tokio::test] async fn a_partial_dir_on_another_filesystem_keeps_the_part_and_reports_the_rename() // skip unless a second filesystem is found (/dev/shm on Linux); error contains "rename"
```
`push_options.rs`: existing partial-dir tests pass unchanged; add `pull_with_an_absolute_partial_dir_through_a_symlink_works`.

- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test containment --test recv --test push_options` — new ones fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same, then `cargo test -p irsync --all-features` — pass.
- [ ] **Step 5: Commit** — `Write and place files through open folders`.

### Task 6: The rest of the receiving side

**Files:**
- Modify: `crates/irsync/src/session/planner.rs` (`mkdir_replacing`, `place_via_temp`, `temp_sibling`, the `MetaOnly`, `Symlink`, `Special` branches, :930, `place_hardlink`), `crates/irsync/src/session/recv_resume.rs:28`, `crates/irsync/src/session/receiver.rs` (`directory_pass`), `crates/irsync/src/session/recv_io.rs` (`unchanged_file`, :535, `read_local`, `still_indexed`, `settle_identical`), `crates/irsync/src/session/recv_links.rs:162`, `crates/irsync/src/delete.rs` (`DeletePass::execute/run`, `remove_one`, `ParentCache`), `crates/irsync/src/daemon.rs` (`remove_part`), `crates/irsync/src/fs/meta.rs` (remove path-based `apply_metadata` and `create_special`)
- Test: `crates/irsync/tests/containment.rs`, existing `delete.rs`, `gc.rs`, `posix_extras.rs`

**Interfaces:**
- Consumes: Tasks 1-3, `Dir` operations from Task 2.
- Produces:

```rust
// planner.rs
fn place_via_temp(dir: &Dir, name: &OsStr, make: impl FnOnce(&Dir, &OsStr) -> io::Result<()>, entry: &FileEntry, apply: &ApplyOpts) -> io::Result<()>;
pub(super) fn place_hardlink(root: &Root, target: &RelPath, path: &RelPath, link: bool, entry: &FileEntry, apply: &ApplyOpts) -> io::Result<()>;
// delete.rs
pub async fn execute(self, root: Arc<Root>) -> DeleteReport;      // other parameters as today
// daemon.rs
fn remove_part(root: &Root, rel: &RelPath, cutoff: i64, dry_run: bool) -> io::Result<Removal>;
```

- `MetaOnly` on a non-folder that is not a regular file uses `Dir::apply_node_metadata`; on a regular file it opens with `Dir::open_regular` and uses `apply_metadata_fd`.
- After the delete pass removes a folder: `Root::forget` it.
- New hook point: `"node"` just before a symlink or special file is made.

- [ ] **Step 1: Write the failing tests** (`containment.rs`, same canary as Task 5)

```rust
#[tokio::test] async fn symlink_placed_while_the_parent_is_swapped_stays_inside()   // swap on "node" for d/l
#[tokio::test] async fn fifo_placed_while_the_parent_is_swapped_stays_inside()
#[tokio::test] async fn folder_metadata_never_reaches_a_swapped_folder()            // d becomes a symlink to outside before the directory pass; outside's mode and mtime unchanged
#[tokio::test] async fn local_chunk_copies_never_read_through_a_swapped_folder()    // swap on "basis"/"open"; the pushed file fails or is fetched from the sender
#[tokio::test] async fn resumed_plan_does_not_make_folders_through_a_symlink()      // recv_resume path: a/ swapped before reconnect; outside gains no folder "b"
```

- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test containment` — new ones fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p irsync --all-features` — pass, including the unchanged `containment.rs` hard-link tests and `delete.rs`, `gc.rs`.
- [ ] **Step 5: Commit** — `Make folders, links and metadata through open folders`.

### Task 7: The `--paranoid` clone folder

**Files:**
- Modify: `crates/irsync/src/fs/clone.rs` (`Clones::clone_file`, `try_clone`, `area`, `area_dir`, `area_dir_below`), its callers in `crates/irsync/src/session/sender_role.rs` (`clone_of`)
- Test: `crates/irsync/src/fs/clone.rs` tests, `crates/irsync/tests/paranoid_clone.rs`

**Interfaces:**
- Produces:

```rust
impl Clones {
    /// Clones the regular file `key` below `root`.
    pub fn clone_file(&self, root: &Root, key: &RelPath) -> io::Result<Option<Cloned>>;
}
/// The clone folder's place for files on `dev`: looked for from the root's real
/// path (resolved once when the Root was opened) upward, never from a path below it.
pub fn area_dir(root: &Root, dev: u64) -> Option<PathBuf>;
```

- `Root` gains `pub fn real_path(&self) -> &Path` (canonicalized in `Root::open`).
- A file whose device differs from every folder from the root's real path up to its mount point gets no clone (`Attempt::NoClone`), as an unwritable area does today.

- [ ] **Step 1: Write the failing tests** (`clone.rs`)

```rust
#[test] fn the_clone_folder_is_never_placed_through_a_swapped_parent() // root/d -> outside (same filesystem); clone_file(root, "d/f") is an error or None; outside has no ".irsync-clones-*"
#[test] fn area_dir_is_chosen_from_the_root_not_the_file()             // root/a/b/f: area_dir is root's real path or above, never root/a
```
Port `area_dir_stays_within_its_bound` and the others to the new signatures.

- [ ] **Step 2: Run** `cargo test -p irsync --all-features fs::clone && cargo test -p irsync --all-features --test paranoid_clone` — new ones fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same — pass.
- [ ] **Step 5: Commit** — `Choose the clone folder from the session root`.

### Task 8: The daemon's work outside a session

**Files:**
- Modify: `crates/irsync/src/warm.rs` (`index_root`), `crates/irsync/src/daemon.rs` (the garbage-collection pass around :1395-1435 and its callers of `remove_part`)
- Test: `crates/irsync/tests/gc.rs`, `crates/irsync/tests/index.rs` (or where `index_root` is tested)

**Interfaces:**
- Consumes: `Root::open`, `Hasher::new(index, Arc<Root>, workers)`, `remove_part(&Root, ..)`, `lstat_entry_with_ctime(&Root, ..)`.
- Produces: `pub async fn index_root(root: Arc<Root>, ...)` (other parameters as today). Each pass opens one `Root` for its export and drops it at the end.

- [ ] **Step 1: Write the failing tests**

```rust
// gc.rs
#[tokio::test] async fn gc_never_stats_or_removes_through_a_swapped_folder()  // index row for d/f; d -> outside holding f and .irsync.f.part older than the cutoff; both remain, the row is dropped as "gone"
// warm test file
#[tokio::test] async fn warm_up_never_hashes_through_a_swapped_folder()       // swap on the hasher's hash hook for d/f; the index holds no digest of outside content
```

- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test gc --test index` — new ones fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same — pass.
- [ ] **Step 5: Commit** — `Use an open root for index warm-up and clean-up`.

### Task 9: Sweep, documentation, speed, pull request

**Files:**
- Modify: `crates/irsync/src/fs/safe.rs`, `book/src/reference/files-and-state.md`, `book/src/reference/command-line.md` (the `--partial-dir` row), `tools/ci.sh` (`miri_packages`, only if Task 1-2 added `unsafe`)

- [ ] **Step 1: Sweep.** Run
  `rg -n "std::fs::|OpenOptions|AT_FDCWD|\.join\(|full\(|full_key\(" crates/irsync/src/{recv.rs,send.rs,hasher.rs,delete.rs,warm.rs,fs/meta.rs,fs/clone.rs} crates/irsync/src/session`
  and account for every hit outside `#[cfg(test)]`: it is a message, the root itself, the walk (`scan.rs`, `control.rs`, until Task 11), or the clone area above the root. Anything else is converted now. Remove `open_dir_beneath`; `open_regular_nofollow(path)` stays only for callers that open a file the user named directly (list them in the PR).
- [ ] **Step 2: Book.** In "In the synced trees" add: irsync opens every folder below a session's root one name at a time and never through a symlink; a relative `--partial-dir` must be a real folder (or missing) all the way. Same sentence on the `--partial-dir` row. Build with `mdbook build book`.
- [ ] **Step 3: Speed.** With the stress crate's file generator make a tree of 100,000 files of 1-4 KiB in 1,000 folders. Time a fresh push and a fresh pull through a local daemon, three runs each, on `main` and on the branch (release builds). Expected: the branch's median within 5% of `main`. If not, profile before changing the cache size. Put the six numbers in the PR description.
- [ ] **Step 4: Full check.** `tools/ci.sh check` — passes. If `unsafe` was added: add `"irsync --lib -- fs::beneath::"` to `miri_packages` and run `tools/ci.sh miri`.
- [ ] **Step 5: Commit** — `Document folder handling and remove the path-based helpers`.
- [ ] **Step 6: Pull request.** Push `folder-handle-file-ops`, open a PR titled `Do file operations relative to open folders`; description: what changed, the speed numbers, that Linux-only code was first exercised in CI, FreeBSD still to be built by hand. Watch CI on Linux and macOS with `gh pr checks`.

---

## Pull request 2

### Task 10: A walker on open folders, beside the old one

**Files:**
- Create: `crates/irsync/src/fs/walk.rs`
- Modify: `crates/irsync/src/fs/mod.rs`, `crates/irsync/src/scan.rs`
- Test: `crates/irsync/src/scan.rs` tests, new `crates/irsync/tests/walk_compare.rs`

**Interfaces:**
- Consumes: `Root`, `Dir::entry`, `nix::dir::Dir::from_fd`.
- Produces:

```rust
// scan.rs
/// As walk(), reading every folder through a handle opened from its parent with O_NOFOLLOW.
pub fn walk_beneath(root: Arc<Root>, filters: Arc<FilterSet>, opts: WalkOpts, meter: Option<Arc<Meter>>)
    -> (mpsc::Receiver<ScanEvent>, oneshot::Receiver<ScanSummary>);
```

- Same contract as `walk`: the root first, entries in increasing `RelPath` order, a `Gap` where something could not be listed (same messages where the cause is the same), `max_depth`, clone folders and filtered entries pruned, `ScanSummary` filled the same way. `follow_root_symlink` is honoured by how the `Root` was opened.
- Parallelism: up to `opts.threads` folders are read ahead at once on `spawn_thread` workers; output order is restored before sending. The walk does not use the `Root`'s cache; it holds at most `opts.threads` folder handles plus one per level of the folder being emitted.
- A folder that turns out to be a symlink or gone when opened is a `Gap` at its path with the component error text; nothing below it is listed.

- [ ] **Step 1: Write the failing tests**
  - `scan.rs`: run every existing test in `mod tests` against both walkers (a helper `events_with(walker)`).
  - `walk_compare.rs`:

```rust
#[tokio::test] async fn both_walkers_list_generated_trees_identically()
// three filegen trees (seeds 1, 2, 3; symlinks, fifos, raw-byte names, a 0o000 folder, depth 12), with and without a filter set and max_depth Some(1):
// the two event sequences are equal (paths, entries, ctimes, gap paths), and the summaries are equal.
#[tokio::test] async fn a_folder_swapped_during_the_walk_lists_nothing_from_outside()
// test hook: a callback before each folder is opened; when it sees "d/sub", rename d away and symlink d -> outside (which has sub/secret).
// No entry or gap message contains "secret"; there is a Gap at "d/sub".
```

- [ ] **Step 2: Run** `cargo test -p irsync --all-features scan:: && cargo test -p irsync --all-features --test walk_compare` — fail to compile.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same — pass.
- [ ] **Step 5: Commit** — `Add a directory walk on open folders`.

### Task 11: Switch over and remove the old walk

**Files:**
- Modify: `crates/irsync/src/scan.rs` (remove `walk`, `run_walk`, `rel_of`, `sort_children` if unused), `crates/irsync/src/session/control.rs` (:520-540, :748-760), `crates/irsync/src/warm.rs`, any other caller of `scan::walk`, `crates/irsync/Cargo.toml` and `Cargo.toml` (drop `jwalk` if nothing else uses it), `crates/irsync/src/session/planner.rs` (`NOFILE_RESERVE` grows by `2 * WALK_THREADS`), `docs/IRSYNC_DECISIONS.md` (note at 2.1 item 7)
- Test: `crates/irsync/tests/containment.rs`, `crates/irsync/tests/walk_compare.rs`

**Interfaces:**
- Consumes: `walk_beneath` (renamed to `walk` here), `Ctx::session_root`.
- Produces: `ListingSpec.root: Arc<Root>` in place of `PathBuf`.

- [ ] **Step 1: Write the failing tests** (`containment.rs`)

```rust
#[tokio::test] async fn a_pull_listing_never_names_files_outside()   // daemon lists; d swapped mid-walk via the Task 10 hook; the client's listing events and out.errors never contain "secret"
#[tokio::test] async fn a_push_with_delete_never_lists_outside()     // receiver's own walk swapped; outside/sub/secret still exists, nothing reported deleted there
```
In `walk_compare.rs` keep `a_folder_swapped_during_the_walk_lists_nothing_from_outside`; remove the two-walker comparison together with the old walker in Step 3 (it has done its job in Task 10's CI run).

- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test containment a_pull_listing a_push_with_delete` — fail.
- [ ] **Step 3: Implement.** Repeat Task 9's sweep command; now no hit may be explained by "the walk".
- [ ] **Step 4: Verify.** `tools/ci.sh check` passes (GNU rsync 3.1+ on `PATH`, so `rsync_compare` really runs). Repeat Task 9 Step 3's timing for the listing-heavy case (a no-change push of the 100,000-file tree, cold and warm cache); expected within 5% of `main`. `tools/ci.sh deny` passes after the dependency change.
- [ ] **Step 5: Commit** — `Walk directories through open folders`.
- [ ] **Step 6: Pull request.** Branch `folder-handle-walk`, title `Walk directories through open folders`; description with the timing numbers. After it merges, both changes ship in the same release, and the advisory is published with it per `SECURITY.md`.
