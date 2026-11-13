# Single-File Sources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `irsync f.iso HOST:T` and `irsync HOST:dir/f.iso L` copy one file, placed by rsync's rules, with delta transfer and resume as for directories.

**Architecture:** A single-file session is still a directory session underneath. Its root is the directory that holds the file (on each side). Its listing holds exactly two entries: the root and the one file. The file's name on the wire is always the **receiver's** name for it (the "wire name"). Only the sender maps wire name → its own file name, at its filesystem boundary. So the receiver's write path (`.part` files, safe opens, renames, index keys) needs no renaming logic. On a push, the daemon decides placement and returns the wire name in `HelloAck`. On a pull, the client asks the daemon first with a new `PROBE` hello whether the remote path is a file, then decides placement locally and sends the wire name in `Hello.name`.

**Tech Stack:** Rust 2021, prost/protox (protos in `proto/irsync/v1/`), iroh, tokio.

**Spec:** `docs/superpowers/specs/2026-11-13-single-file-design.md`

## Global Constraints

- `PROTOCOL_VERSION` goes from 3 to 4 (`crates/irsync-proto/src/lib.rs:12`). There is no compatibility shim. A mismatch gives the existing "protocol version … unsupported" error.
- Placement table (the spec's Goals): `T` is a dir → `T/<name>`. `T` is missing → a file named `T`, or with `T/` the dir `T` is created and the file goes inside it. `T` is a file → `T` is updated, or with `T/` it's an error. Same for a pull into `L`.
- `T/<name>` (or `L/<name>`) being an existing directory is an error. No directory is ever replaced by a file.
- A single-file copy never deletes, renames or changes the metadata of any other entry. That includes the directory that holds the file. `--delete*` and `--max-delete` are a no-op for a single-file session.
- A source symlink is never followed. It is copied as a link with `-l`/`-a` and skipped otherwise. Devices and specials follow `-D`. This is the same as inside a directory copy. A source that is a symlink to a directory stays a directory copy, as today.
- Filters match the source file's name. An excluded file means nothing is copied, with exit 0.
- Automatic exports: a single-file push to a missing or file `T` exports `T`'s parent and creates nothing. `T` as a directory exports `T`. A destination ending in `/` keeps today's rule (create `T`, export it).
- Pushing several sources means the destination is always treated as `T/`.
- A trailing `/` on a file source is an error (per source, exit 23, as for any failed source).
- Commit messages: a short summary line, a blank line, then a plain-English "why" if not obvious. No `Co-Authored-By` or any attribution trailer.
- Before every commit: `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must be clean. Before the last commit: the full regression from `AGENTS.md`.

## Review Focus

- **The user's exact command form:** a bare filename in the current directory (`irsync f.iso HOST:tmp`), so the source's parent is `""`. It must use `.` as the root, not fail. Pinned in Task 5 (`push_bare_name_from_cwd`).
- **Pulling into `.` with `--delete -a`, where `.` holds other files and has its own mode and mtime:** the siblings and the directory's metadata must stay exactly as they were. Pinned in Task 4 (`pull_into_busy_dir_touches_nothing_else`).
- **Pushing onto a `T` that is a symlink on the daemon (to a file outside the export):** the link itself is replaced by the file, and nothing outside the export is written. Pinned in Task 3 (`push_onto_symlink_replaces_the_link`).
- **Running the same command a second time:** nothing is transferred. Updating a changed big file sends only the changed chunks. Pinned in Task 3 (`push_again_transfers_nothing`, `push_changed_file_uses_delta`).
- **File names with spaces and non-ASCII characters:** they must work on both sides and in the session key. Pinned in Task 5 (`names_with_spaces_and_unicode`).

---

### Task 1: Placement rules (pure)

**Files:**
- Create: `crates/irsync/src/session/single.rs` (add `pub mod single;` in `session/mod.rs`)
- Test: `crates/irsync/tests/single_place.rs`

**Interfaces:**
- Produces:
  - `pub struct SingleFile { pub wire: RelPath, pub local: RelPath }`. Each is exactly one path component. `wire` is the receiver's name for the file. `local` is this side's name. They are equal on the receiver.
  - `pub struct FilePlace { pub root: PathBuf, pub name: Vec<u8> }`
  - `pub enum PlaceError { NotADirectory(PathBuf), IsADirectory(PathBuf), Io(PathBuf, std::io::Error) }` (thiserror). Display: `"{path}: not a directory"`, `"{path}: is a directory"`, `"{path}: {err}"`.
  - `pub fn place_file(dest: &Path, contents_only: bool, src_name: &[u8], follow: bool) -> Result<FilePlace, PlaceError>`. `follow` picks `metadata` (client) or `symlink_metadata` (daemon) for `dest`. The check on `dest/src_name` always uses `symlink_metadata`. A missing root directory isn't checked here: the callers create it (the client with `create_dir_all`, the daemon with `prepare_root(.., Missing::Create)`).
  - `pub fn source_is_single(path: &Path, contents_only: bool) -> Result<bool, PlaceError>`. Returns false when `metadata(path)` is a dir, or when the path doesn't exist (today's error path stays). Otherwise, if `contents_only` → `NotADirectory`. Otherwise true.
  - `pub fn split_last(path: &Path) -> (PathBuf, Vec<u8>)`. Returns the parent (`.` for a bare name) and the last component.
  - `pub fn is_one_component(name: &[u8]) -> bool`. True for a non-empty name that has no `/` and no NUL and is not `.` or `..`.

- [ ] **Step 1: Write the failing tests.** These are `#[test]`s in `tests/single_place.rs`, on a tempdir. The `dest` column is the state of `T`.

| test | `T` state | `co` | expected |
| --- | --- | --- | --- |
| `dir_dest_takes_source_name` | dir | false | `root=T, name=b"f.iso"` |
| `dir_dest_with_slash` | dir | true | same |
| `missing_dest_is_the_file_name` | missing | false | `root=parent(T), name=b"T"` |
| `missing_dest_with_slash_is_a_dir` | missing | true | `root=T, name=b"f.iso"` |
| `file_dest_is_updated` | file | false | `root=parent(T), name=b"T"` |
| `file_dest_with_slash_is_an_error` | file | true | `Err(NotADirectory(T))` |
| `dir_in_the_way_is_an_error` | dir, holding the dir `f.iso` | false | `Err(IsADirectory(T/f.iso))` |
| `symlink_dest_followed_on_client` | symlink → dir | false, `follow=true` | `root=T, name=b"f.iso"` |
| `symlink_dest_is_a_file_on_daemon` | symlink → dir | false, `follow=false` | `root=parent(T), name=b"T"` |

  Also add these tests:
  - `source_is_single_rules`: dir → false; file → true; file with `co` → `NotADirectory`; symlink to file → true; symlink to dir → false; missing → false.
  - `split_last_bare_name`: `split_last("f.iso") == (".", b"f.iso")`.
- [ ] **Step 2: Run** `cargo test -p irsync --test single_place`. Expected: compile failure (`single` not found).
- [ ] **Step 3: Implement** the interfaces above in `session/single.rs`.
- [ ] **Step 4: Run** `cargo test -p irsync --test single_place`. Expected: all pass.
- [ ] **Step 5: Commit** with the message "Add placement rules for single-file copies".

### Task 2: Automatic export for a file destination

**Files:**
- Modify: `crates/irsync/src/autoexport.rs` (`auto_root`), and its caller `add_immediate` in `crates/irsync/src/daemon.rs` (~l.707). The caller passes `false` until Task 3.
- Test: `crates/irsync/tests/autoexport.rs`

**Interfaces:**
- Produces: `pub fn auto_root(abs: &Path, push: bool, file_dest: bool) -> Result<PathBuf, AutoRootError>`. With `file_dest` and a missing `abs`, it returns the parent (which must be a dir, else `NotFound`) and creates nothing. Every other case is unchanged.

- [ ] **Step 1: Write the failing tests:**
  - `missing_file_dest_exports_parent_and_creates_nothing`: `auto_root(dir/new.iso, true, true) == Ok(dir)` and `!dir/new.iso exists`.
  - `existing_dir_with_file_dest_is_itself`: `auto_root(dir, true, true) == Ok(dir)`.
  - Update the existing calls to pass `false`.
- [ ] **Step 2: Run** `cargo test -p irsync --test autoexport`. Expected: compile failure on the arity.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command. Expected: pass.
- [ ] **Step 5: Commit** with the message "Let automatic exports hold a single-file destination".

### Task 3: Single-file push, end to end through the engine

This is the core. It covers the protocol fields, the daemon's placement, the restricted listing, the sender's name mapping, the root-entry rule, deletes off, and the session keys.

**Files:**
- Modify: `proto/irsync/v1/common.proto`:
  - `Hello`: `bool single_file = 31; // name (21) is the source basename on a push and the receiver's file name on a pull`.
  - `HelloAck`: `bytes single_name = 5; // push: the daemon's name for the file (the wire name)`.
- Modify: `crates/irsync-proto/src/lib.rs`: `PROTOCOL_VERSION = 4`.
- Modify: `crates/irsync/src/daemon.rs`:
  - `admit` (l.729): when `hello.single_file`, don't join `name`. Validate it with `is_one_component`, or return `BadPath "invalid name"`.
  - `add_immediate`: `file_dest = single_file && push && !contents_only`.
  - Before `Place` is built: compute the placement and set `Place.single`.
- Modify: `crates/irsync/src/session/engine.rs`:
  - `Place` (l.856) gets `pub single: Option<SingleFile>`.
  - `Ctx` (l.343) gets `pub single: Option<SingleFile>`.
  - `Ctx::full`, `Ctx::key` and `Ctx::full_key` map `wire → local` when `rel == single.wire`.
  - `serve_daemon_inner`: set `HelloAck.single_name`, and force the delete mode to off when `single` is set.
  - `run_client_inner`: build `Ctx.single` from `LocalSide.single` and the ack.
- Modify: `crates/irsync/src/session/mod.rs`:
  - `LocalSide` gets `pub single: Option<Vec<u8>>` (this side's file name). `LocalSide::new` keeps its signature and sets `None`. Add `LocalSide::with_single(self, name: Vec<u8>) -> Self`.
  - `SessionKey::for_daemon` gets a `single: Option<&SingleFile>` parameter and mixes in `b"single\0" + local`.
- Modify: `client_session_key` (engine.rs:560): mix in `b"single\0" + LocalSide.single`.
- Modify: `crates/irsync/src/session/control.rs`: `ListingSpec` gets `pub single: Option<SingleFile>`. When it's set, `spawn_listing` doesn't call `scan::walk`. It emits:
  - the root entry (`lstat_entry_with_ctime(root, RelPath::root())`);
  - then, if `filters` admit `local` and `lstat(root/local)` succeeds, that entry with its path set to `wire`;
  - a missing file emits nothing; any other error emits a `Gap`.

  Set it from `ctx.single` at `sender_role.rs:330` and `receiver.rs:1614`.
- Modify: `crates/irsync/src/session/planner.rs` `Planner::one` (l.415): with `ctx.single`, the empty path must be a dir. Add it to `ok_dirs`, but never to `report.dirs`, so the holding directory's metadata is never applied.
- Modify: `sender_role.rs:1848`: with `ctx.single`, check that the root is a dir. The file is checked by the listing.
- Test: `crates/irsync/tests/single_file.rs` (Harness level, in-process). Add to `common/mod.rs`: `Harness::run_single_push(&self, opts: &SyncOptions, src_file: &Path, dest_path: &[u8], contents_only: bool) -> anyhow::Result<Outcome>`. It builds the `Hello` (`single_file`, `name` = basename, `path` = `dest_path`) and a `LocalSide` rooted at the file's parent `.with_single(basename)`, then calls `run_hello`.

**Mapping audit (part of Step 3):** every sender-side read of a listed path must go through `Ctx::full`, `Ctx::key` or `Ctx::full_key`. Check with `grep -n "root.join\|base.join\|scope.key(" crates/irsync/src/session/`. Each hit is either routed through those helpers or shown to be receiver-only. The hasher is reached through index keys, so mapping in `Ctx::key` covers it.

- [ ] **Step 1: Write the failing tests** in `tests/single_file.rs` (`#[tokio::test]`, `archive_opts()` unless stated). `m` is the export root.
  - `push_into_dir`: dest `m/T` is a dir → `m/T/f.iso` has the same bytes and mode.
  - `push_to_new_name`: dest `m/T` is missing → `m/T` is a file with the same bytes. `m/T/` doesn't exist as a dir, and `m/f.iso` doesn't exist.
  - `push_with_slash_creates_dir`: dest `m/T/`, missing → `m/T/f.iso`.
  - `push_with_slash_onto_file_fails`: `m/T` is a file, `co=true` → `Err` whose message contains "not a directory". `m/T` is unchanged.
  - `push_dir_in_the_way_fails`: `m/T/f.iso` is a dir → `Err` containing "is a directory".
  - `push_again_transfers_nothing`: run twice. The second `Outcome` has 0 bytes of file data sent.
  - `push_changed_file_uses_delta`: an 8 MiB file is pushed, then 4 KiB in its middle is changed and it is pushed again. Bytes sent < 1 MiB, and the content is equal.
  - `push_leaves_siblings_and_dir_metadata`: `m/T` is a dir holding `other` plus a mode of 0o700 and an old mtime. Push with `--delete`. Then `other` still exists, and `T`'s mode and mtime are unchanged.
  - `push_excluded_file_copies_nothing`: `--exclude f.iso` → `Ok`, and `m/T/f.iso` is absent.
  - `push_symlink_source_without_l_skips` and `push_symlink_source_with_l_copies_link`.
  - `push_onto_symlink_replaces_the_link`: `m/T` is a symlink to a file outside `m` → `m/T` is a regular file afterwards, and the outside file is unchanged.
  - `single_and_dir_sessions_have_different_keys`: `SessionKey::for_daemon` with `single` set vs `None` gives different keys.
- [ ] **Step 2: Run** `cargo test -p irsync --test single_file`. Expected: fail (compile, then the asserts).
- [ ] **Step 3: Implement** the file changes listed above, and do the mapping audit.
- [ ] **Step 4: Run** `cargo test -p irsync --test single_file`, then `cargo test -q --workspace --all-features --no-fail-fast`. Expected: all pass, and the directory tests are unchanged.
- [ ] **Step 5: Commit** with the message "Push a single file", explaining that the session root is the holding directory and the wire carries the receiver's name.

### Task 4: Single-file pull and the probe

**Files:**
- Modify: `proto/irsync/v1/common.proto`:
  - `Hello.Direction`: `PROBE = 3; // no session: the daemon answers with PathInfo for path, then closes`.
  - A new `message PathInfo { enum Kind { MISSING = 0; DIRECTORY = 1; FILE = 2; } Kind kind = 1; }`. `FILE` means anything that isn't a directory.
- Modify: `crates/irsync/src/daemon.rs` `handle`, so that `PROBE` does the following:
  1. `version_check`, then resolution as for `DAEMON_SENDS`, including `serve_immediate` for a pull.
  2. `prepare_root(export, parent(rel), Missing::Refuse)`. A symlinked or missing parent → `MISSING`.
  3. `symlink_metadata` of the last component, then reply `PathInfo`.
  
  Errors use the same codes as a pull.
- Modify: `daemon.rs`, for `DAEMON_SENDS` with `single_file`: split `rel` into the parent rel and the file name. Check that the file isn't a dir (otherwise `BadPath "… is a directory"`). `Place.rel` = the parent, and `single = SingleFile{wire: hello.name, local: file name}`.
- Create: `pub async fn probe(conn: &Connection, path: &[u8]) -> anyhow::Result<path_info::Kind>` in `crates/irsync/src/session/single.rs`. It uses the same stream framing as `list_exports_of` (`main.rs:745`).
- Test: extend `tests/single_file.rs`. Add `Harness::run_single_pull(&self, opts, remote_path: &[u8], dest: &Path, contents_only: bool) -> anyhow::Result<Outcome>`, which probes, runs `place_file(dest, co, basename, true)`, builds the `LocalSide` at `root.with_single(name)` and a `Hello` with `name` = that name, then runs.

- [ ] **Step 1: Write the failing tests:**
  - `probe_kinds`: probing `m/f.iso` → `FILE`, `m` → `DIRECTORY`, `m/nope` → `MISSING`.
  - `pull_into_dir`: `L` is a dir → `L/f.iso`.
  - `pull_to_new_name`: `L` is missing → the file `L`.
  - `pull_with_slash_creates_dir`: `L/` is missing → `L/f.iso`.
  - `pull_onto_file_updates_it`.
  - `pull_into_busy_dir_touches_nothing_else`: `L` holds `a`, `b/c`, and has mode 0o750 and an old mtime. Pull with `--delete -a`. Then everything else and `L`'s metadata are unchanged.
  - `pull_dry_run_creates_nothing`: `L/` is missing, `dry_run` → `L` still doesn't exist.
  - `pull_dir_in_the_way_fails`.
- [ ] **Step 2: Run** `cargo test -p irsync --test single_file pull_ probe_`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p irsync --test single_file`. Expected: pass.
- [ ] **Step 5: Commit** with the message "Pull a single file", explaining that the client asks the daemon what the path is before it picks a local root.

### Task 5: The command line

**Files:**
- Modify: `crates/irsync/src/main.rs`:
  - In the `sync` per-source loop (l.823-990), for a push, call `source_is_single(local, local_co)`. If true: the root is `split_last(local).0`, the local side is `.with_single(basename)`, and the `Hello` gets `single_file`, `name` = basename, and `contents_only = remote_co || srcs.len() > 1`. A `PlaceError` is a per-source failure, logged like the other source errors.
  - For a pull without `remote_co`, call `probe`. If it says `FILE`, do the same as `run_single_pull`. Otherwise use today's `resolve_roots`.
  - `session` (l.1002): skip `gc_if_due` (l.1034) when `LocalSide.single` is set.
  - `run_list` (l.699): give the destination as `<tmp>/list/`, so that a listed file keeps its own name.
  - The client always forces `delete_mode` to off on a single-file `SyncOptions` copy.
- Test: `crates/irsync/tests/single_file_e2e.rs`. It uses the binaries and copies the `listing_e2e.rs` `setup()` pattern, with a `photos` rw export.

- [ ] **Step 1: Write the failing tests:**
  - `push_bare_name_from_cwd`: run the binary with `current_dir(dir)` and `f.iso <id>:photos/tmp`, where `tmp` is missing → the file `photos/tmp` exists.
  - `pull_file_into_dot`: `current_dir(L)` with `<id>:photos/b.jpg .` → `L/b.jpg`.
  - `several_sources_mix_files_and_dirs`: `a.iso dir <id>:photos/T` → `T/a.iso` and `T/dir/...`.
  - `file_source_with_slash_fails`: `f.iso/` → exit status 23, and stderr contains "not a directory".
  - `listing_one_file`: `<id>:photos/b.jpg` → exit 0, stdout contains `b.jpg`, and nothing is written.
  - `names_with_spaces_and_unicode`: push and pull `"ä b.iso"`.
  - `matches_rsync_placement`: skipped without rsync, using the `rsync_present`/`skip_allowed` pattern from `rsync_compare.rs`. For each row of the placement table, run `rsync -a <src> <dst>` locally and the matching irsync push, then compare the resulting trees with the same `snapshot()` comparison.
- [ ] **Step 2: Run** `cargo build --workspace && cargo test -p irsync --test single_file_e2e`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same command. Expected: pass.
- [ ] **Step 5: Commit** with the message "Accept a single file as the source on the command line".

### Task 6: Resume after a dropped connection

**Files:**
- Test: `crates/irsync/tests/netsim.rs` (feature `netsim`). If it's needed, add a `Hello`-taking variant of `run_reconnect` to `common/mod.rs`: `run_reconnect_hello(&self, opts, hello: Hello, ropts: ReconnectOpts, local: LocalSide) -> (anyhow::Result<Outcome>, u32)`.

- [ ] **Step 1: Write the test** `single_file_push_resumes_after_blackout`: a `#[tokio::test(start_paused = true)]` using `with_netsim(ProfileName::TestBlackout, 1, …)`. Push a 16 MiB file to a new name. Assert `reconnects >= 1`, `h.daemon.session_ids().len() == 1`, and that the content is equal. Add `single_file_pull_resumes_after_blackout`, the same for a pull.
- [ ] **Step 2: Run** `cargo test -p irsync --features netsim --test netsim single_file`. Expected: pass if Tasks 3 and 4 got the keys right. A failure here is a bug in those tasks, so fix it there.
- [ ] **Step 3: Commit** with the message "Test resume for single-file copies".

### Task 7: Docs and decision record

**Files:**
- Modify: `book/src/reference/remote-addresses.md` (the trailing-slash section gets single-file rows and the placement table).
- Modify: `book/src/reference/protocol.md` (version 4, `single_file`, `single_name`, `PROBE`/`PathInfo`).
- Modify: `book/src/reference/command-line.md`, and the `SyncArgs` doc comment in `crates/irsync/src/cli/mod.rs:578`.
- Modify: `book/src/reference/limitations.md`: add "several sources with `--delete` can delete what an earlier source copied".
- Modify: `docs/IRSYNC_DECISIONS.md`: remove l.888 and add a §1 ruling covering the receiver's name on the wire, the holding directory as the root, the probe, and the parent export.

- [ ] **Step 1: Edit** the files above.
- [ ] **Step 2: Run** `mdbook build book`. Expected: no errors.
- [ ] **Step 3: Run** the full regression from `AGENTS.md` (`touch crates/irsync/build.rs`, `--include-ignored`, fmt, both clippy runs). Expected: all clean. On this Mac, check the APFS sparse-file note before blaming irsync for any sparse-test failure.
- [ ] **Step 4: Commit** with the message "Document single-file copies".
