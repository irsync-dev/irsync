# irsync on Windows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `irsync` builds and passes its tests on Windows (x86_64 and ARM64, NTFS) as client and daemon, with containment as strong as on Unix and simple rules for a session between a Unix and a Windows side; behaviour on Linux, macOS and FreeBSD does not change.

**Architecture:** A `platform` module in `crates/irsync` (`mod.rs`, `unix.rs`, `windows.rs`) holds every operating-system call. Its centre is `Dir`, a directory held open, with single-name operations that never follow a link: the `openat` family on Unix, handle-relative native calls (`NtCreateFile` and rename, delete and link by handle) on Windows. Operating-system errors are turned into a small set of meanings. Mixed-session rules live in one platform-neutral module keyed on the two sides' platform families.

**Tech Stack:** Rust 1.100.0; `nix` (Unix only); `windows-sys` with `Wdk_*` and `Win32_*` features, `std::os::windows`, tokio's Windows signals (Windows only); cargo-nextest; Git Bash for `tools/ci.sh`.

**Spec:** `docs/superpowers/specs/2026-11-23-windows-irsync-port-design.md`. Read it first. Also read `AGENTS.md`, and for the patterns already proven: `crates/fileset/src/platform/` and `crates/fileset/src/wtf8.rs`.

## Global Constraints

- All work goes through a pull request against `main`. Never push to `main`.
- Six pull requests in this order, each merged before the next starts: PR 1 = Tasks 1 to 5; PR 2 = Tasks 6 to 8; PR 3 = Task 9; PR 4 = Tasks 10 and 11; PR 5 = Task 12; PR 6 = Tasks 13 and 14.
- Behaviour on Linux, macOS and FreeBSD does not change: same output, same messages, same exit codes, same bytes on the wire to an existing peer. PR 1 is a pure move; no test changes its expected values.
- Outside `crates/irsync/src/platform/`, no file of the crate names `nix::`, `libc::`, `std::os::unix`, `std::os::windows`, `std::os::fd`, `windows_sys` or `winapi_util`. `#[cfg(unix)]` and `#[cfg(windows)]` attributes are allowed anywhere.
- `unsafe` only inside `platform/`: each native call in one small private function with a safe signature and a `// SAFETY:` comment in which every sentence is true of that signature. A generic helper is sealed so it compiles only for the types it is sound for. The crate carries `#![deny(unsafe_code)]`; `platform/` is the one `#[allow]`.
- No code shared with `filegen` or `fileset`: no shared crate, no `include!`, no dependency on either. Where irsync needs what they have (the WTF-8 conversion), it gets its own copy.
- Below a root, nothing follows a link: a symlink on Unix; a reparse point of any kind on Windows.
- Bytes that are not a valid name on this platform fail that entry. They are never repaired.
- The wire format changes only by one optional field in `Hello` and `HelloAck`. `PROTOCOL_VERSION` stays 5.
- Targets: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`. Minimum Windows 10 version 1809.
- No new dependency is expected. If one is needed: the newest release at least 7 days old, declared in `[workspace.dependencies]`, an entry in `docs/SUPPLY_CHAIN_DECISIONS.md`, and no second version of any crate (`tools/ci.sh deny`).
- After any change to `Cargo.lock`, run `tools/sync-fuzz-lock.sh` and commit both lock files.
- Checks live in `tools/ci.sh`, and a change there is tested in `tools/tests/test_ci_script.py`. A change under `.github/` passes `tools/ci.sh workflows` and the policy tests.
- Windows builds use a short target directory (`CARGO_TARGET_DIR=C:\t`).
- Commit messages: a short summary line, a blank line, then why, in plain English.
- No attribution of any kind in commits, branches, pull requests or comments (no `Co-Authored-By`, no "Generated with").
- Book and Markdown text: short sentences, plain words, in the style of `book/src/contributing/supply-chain.md`.

## Where to work

- **PR 1 (Tasks 1 to 5) is a refactor of working Unix code and must be compiled and tested on Unix.** `irsync` has C dependencies, so it cannot be cross-checked from Windows, and it does not build on Windows until Task 8. Do PR 1 on the Mac. If that is not possible, every task of PR 1 is checked only by CI on the pull request, one round trip per mistake; say so before starting.
- **From Task 8 on, every task is run on Windows for both architectures:**

  ```sh
  export CARGO_TARGET_DIR='C:\t'
  cargo nextest run -p irsync -p irsync-proto --all-features --no-fail-fast
  cargo nextest run -p irsync -p irsync-proto --all-features --no-fail-fast --target x86_64-pc-windows-msvc
  cargo clippy -p irsync -p irsync-proto --all-targets --all-features -- -D warnings
  ```

  Called "the Windows run" below. The Unix side of these tasks is checked by CI on the pull request.
- Tests that create symlinks need Developer Mode, which is on. With `CI` set they fail instead of skipping.

## How every task ends (the pull request loop)

Referred to below as "Ship it". Only the last task of a pull request ships; the others end with a commit.

1. `git switch main && git pull`, then `git switch -c <branch>` before the first edit of the pull request.
2. Before pushing: `cargo fmt --all --check`; `tools/ci.sh check` on the machine the task was done on.
3. `git push -u origin <branch>`, then `gh pr create --base main` with a description that says what and why.
4. `gh pr checks --watch`. If `CI result` fails and the branch is behind `main`, follow the rule in `AGENTS.md`. A job that never got a runner is rerun, not debugged.
5. When `CI result` is green: `gh pr merge --merge --delete-branch`, `git switch main && git pull`.
6. Stop and ask the maintainer instead of merging when the task says "Needs the maintainer".

## Review Focus

Inputs the spec implies but its test list does not name. Each has a test in the task shown.

1. **A source file another program holds open for writing, or locked** (a mail store, a database). Expected: that entry fails with a message that says the file is in use; the session goes on. Task 9.
2. **A sync stopped with Ctrl-C in the middle of a file.** Expected: the lock on the part file is gone with the process; the next run resumes from it or removes it; no part file is left that nothing will ever clean. Task 12.
3. **A root that is the top of a drive** (`E:\`) or given relative, with `/`, or with `..`. Expected: works as any other root; `System Volume Information` and `$RECYCLE.BIN` at a drive's top do not make every run warn. Task 9.
4. **An mtime NTFS cannot hold** (before 1601) arriving from a Unix peer. Expected: the file is placed; its time is set to the earliest NTFS holds; the next run does not send it again. Task 11.
5. **A destination directory that is read-only, or a destination file with the read-only attribute.** Expected: the file is replaced or the entry fails with a clear message; the session goes on. Task 9.

---

### Task 1: The platform skeleton: families, error meanings, names (PR 1)

**Files:**
- Create: `crates/irsync/src/platform/mod.rs`, `crates/irsync/src/platform/unix.rs`
- Create: `crates/irsync/tests/platform_rule.rs`
- Modify: `crates/irsync/src/lib.rs`, `fs/safe.rs`, `fs/paths.rs`, `fs/transient.rs`, `resolve.rs`, `session/receiver.rs`, `session/single.rs`, `session/mod.rs`

**Interfaces:**
- Produces, in `irsync::platform`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform { Unix, Windows }
impl Platform {
    pub const CURRENT: Platform;
    pub fn as_str(self) -> &'static str;          // "unix" | "windows"
}

/// What an operating-system error means to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    LinkRefused, NotThere, AlreadyThere, NotADirectory, IsADirectory, NotEmpty,
    CrossDevice, OutOfHandles, InUse, NotSupported, Other,
}
pub fn cause(e: &io::Error) -> Cause;

pub fn os_to_bytes(name: &OsStr) -> Vec<u8>;
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString>;   // Unix: never fails
```

Unix mapping of `cause`, from the raw error number: `ELOOP`, and `EMLINK` on FreeBSD → `LinkRefused`; `ENOENT` → `NotThere`; `EEXIST` → `AlreadyThere`; `ENOTDIR` → `NotADirectory`; `EISDIR` → `IsADirectory`; `ENOTEMPTY` → `NotEmpty`; `EXDEV` → `CrossDevice`; `EMFILE`, `ENFILE` → `OutOfHandles`; `EOPNOTSUPP`, `ENOTSUP`, `ENOSYS` → `NotSupported`; anything else → `Other`. `InUse` never occurs on Unix.

- [ ] **Step 1: Write the rule test with a shrinking list**

`crates/irsync/tests/platform_rule.rs`: the test from `crates/fileset/tests/platform_rule.rs`, a copy, with one addition:

```rust
/// Files not moved yet. Each task of this pull request removes the files it
/// converts. Task 5 empties the list and deletes this constant.
const PENDING: &[&str] = &[ /* every file under src/ and tests/ that names a banned API today */ ];
```

The test fails when a file outside `platform/`, `tests/common/` and `PENDING` names a banned API, and also when a file in `PENDING` no longer does (so the list cannot rot).

- [ ] **Step 2: Write the failing tests for `cause`** (in `platform/unix.rs`, `#[cfg(test)]`)

```rust
#[test] fn causes_from_error_numbers()
// for each pair in the mapping above: cause(&io::Error::from_raw_os_error(n)) == expected;
// cause(&io::Error::other("x")) == Cause::Other
```

- [ ] **Step 3: Implement, then move the byte and name conversions**

Every `OsStrExt`/`OsStringExt` use in `fs/paths.rs` (`RelPath::to_os`), `resolve.rs`, `session/receiver.rs`, `session/single.rs` and `session/mod.rs` goes through `os_to_bytes`/`bytes_to_os`. `RelPath::to_os` changes to return an owned `OsString`; a failure there is a bug on Unix (`expect`), and Task 9 gives it a checked sibling for Windows. `fs/transient.rs` `is_transient` matches `Cause::OutOfHandles` plus the error numbers it has no meaning for (`EAGAIN`, `ENOMEM`, `ENOBUFS`), which move to `platform::is_transient_extra(e)`.

- [ ] **Step 4: Run**

Run: `cargo test -q -p irsync --all-features --no-fail-fast`
Expected: PASS, same count as on `main` plus the new tests.

- [ ] **Step 5: Commit**

```bash
git commit -am "irsync: a platform module with error meanings and name bytes"
```

---

### Task 2: `Dir`, and the containment layer on top of it (PR 1)

**Files:**
- Modify: `crates/irsync/src/platform/mod.rs`, `platform/unix.rs`
- Modify: `crates/irsync/src/fs/safe.rs`
- Create: `crates/irsync/tests/containment.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { File, Dir, Symlink, Fifo, CharDev, BlockDev, Socket, Other }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind, pub mode: u32 /* & 0o7777 */, pub uid: u32, pub gid: u32,
    pub size: u64, pub nlink: u64, pub ino: u64, pub dev: u64, pub rdev: u64,
    pub mtime: (i64, u32), pub ctime: (i64, u32),
    pub sparse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create { No, IfMissing, New }
#[derive(Clone, Copy, Debug)]
pub struct Open { pub read: bool, pub write: bool, pub create: Create, pub truncate: bool, pub mode: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind { File, Dir }

/// A directory held open. Every operation takes one name (stored bytes,
/// one component) and never follows a link.
pub struct Dir(/* private */);
impl Dir {
    /// Opens `path` by name. Links in `path` are followed: the caller trusts it.
    pub fn open_root(path: &Path) -> io::Result<Dir>;
    pub fn open_dir(&self, name: &[u8]) -> io::Result<Dir>;
    pub fn create_dir(&self, name: &[u8], mode: u32) -> io::Result<()>;
    pub fn open_file(&self, name: &[u8], how: Open) -> io::Result<File>;
    pub fn stat(&self, name: &[u8]) -> io::Result<Stat>;
    pub fn stat_self(&self) -> io::Result<Stat>;
    pub fn read_link(&self, name: &[u8]) -> io::Result<Vec<u8>>;
    pub fn list(&self) -> io::Result<Vec<Vec<u8>>>;            // no `.` or `..`, unsorted
    pub fn remove_file(&self, name: &[u8]) -> io::Result<()>;
    pub fn remove_dir(&self, name: &[u8]) -> io::Result<()>;   // empty only
    /// Replaces what is at `to_name`, as `rename` does.
    pub fn rename(&self, name: &[u8], to: &Dir, to_name: &[u8]) -> io::Result<()>;
    pub fn hard_link(&self, name: &[u8], from: &Dir, from_name: &[u8]) -> io::Result<()>;
    pub fn symlink(&self, name: &[u8], target: &[u8], kind: LinkKind) -> io::Result<()>;
    pub fn set_mtime(&self, name: &[u8], mtime: (i64, u32)) -> io::Result<()>;
    pub fn set_mode(&self, name: &[u8], mode: u32) -> io::Result<()>;
    pub fn set_owner(&self, name: &[u8], uid: Option<u32>, gid: Option<u32>) -> io::Result<()>;
    pub fn make_fifo(&self, name: &[u8], mode: u32) -> io::Result<()>;
    pub fn make_device(&self, name: &[u8], kind: Kind, mode: u32, rdev: u64) -> io::Result<()>;
}
pub fn fstat(file: &File) -> io::Result<Stat>;
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize>;
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize>;
```

Error contract, on every platform: a link where a directory or file is opened → `Cause::LinkRefused`; a missing name → `NotThere`; an existing name on create → `AlreadyThere`; a non-directory where one is opened → `NotADirectory`.

- `fs/safe.rs` keeps its public functions and types with the same meaning, rewritten on `Dir`: `open_dir_beneath` and `create_dir_all_beneath` return `Dir`; `open_parent_beneath` returns `(Dir, Vec<u8>)`; `DirStack::dir` returns `&Dir`; `Beneath::open` takes `platform::Open` instead of `OFlag`; `Beneath::lstat` and `open_regular_*_stat` return `platform::Stat`. The messages (`a path component is a symlink or not a directory`, `refusing to follow symlink …`, `… is not a regular file`) stay byte for byte.

- [ ] **Step 1: Write the failing `Dir` tests** (in `platform/unix.rs` now; Task 7 moves them to a file both platforms compile)

```rust
#[test] fn every_operation_takes_one_name_and_follows_no_link()
// tree: d/f, d/sub/g, link_to_dir -> outside_dir, link_to_file -> outside/f
// open_dir("link_to_dir") is LinkRefused; open_file("link_to_file", read) is LinkRefused;
// stat("link_to_file").kind == Symlink; read_link gives the target bytes;
// remove_file("link_to_file") removes the link, outside/f still exists;
// rename("f", &sub, "g") replaces sub/g; hard_link gives nlink 2 and the same ino;
// set_mtime on a link changes the link's time, not the target's;
// list() on d returns exactly the names, without "." and "..".
#[test] fn error_meanings()
// open_dir("missing") NotThere; open_dir("f") NotADirectory; create_dir existing AlreadyThere;
// open_file(Create::New) existing AlreadyThere; remove_dir non-empty NotEmpty;
// open_file("sub", write) IsADirectory
#[test] fn a_name_with_a_separator_or_nul_is_refused()
// each of b"a/b", b"", b".", b"..", b"a\0" as a name is io::ErrorKind::InvalidInput, for every operation
```

- [ ] **Step 2: Write the failing attack tests** in `tests/containment.rs`

Each test starts a helper thread that swaps a directory on the path for a link to a directory outside the root, back and forth, as fast as it can, for the length of the test (`testutil::swap_for_link`), while the main thread runs the operation 2000 times. It asserts that the directory outside holds exactly what it held before and that every result is success or an error, never a write or removal outside.

```rust
#[test] fn open_regular_beneath_never_reads_outside()
#[test] fn beneath_open_create_never_writes_outside()
#[test] fn beneath_unlink_and_rename_never_touch_outside()
#[test] fn create_dir_all_beneath_never_creates_outside()
#[test] fn dir_stack_never_descends_outside()
```

- [ ] **Step 3: Implement `Dir` on Unix and rewrite `fs/safe.rs` on it**

Move, do not rewrite: every `openat`, `fstatat`, `unlinkat`, `renameat`, `mkdirat`, `linkat`, `symlinkat`, `readlinkat`, `utimensat`, `fchmodat`, `fchownat`, `mkfifoat`, `mknodat` call now in `fs/` goes behind the `Dir` method named above, with the same flags. `set_mode` keeps the fallback for kernels without no-follow `fchmodat`. `symlink` ignores `kind`.

- [ ] **Step 4: Run**

Run: `cargo test -q -p irsync --all-features --no-fail-fast`
Expected: PASS. The four existing tests at the end of `fs/safe.rs` pass with only their `OFlag` arguments changed to `Open`.

- [ ] **Step 5: Commit**

```bash
git commit -am "irsync: a directory held open, and containment on top of it"
```

---

### Task 3: Metadata, entry kinds and the callers (PR 1)

**Files:**
- Modify: `crates/irsync/src/fs/meta.rs`, `fs/ids.rs`, `recv.rs`, `send.rs`, `delete.rs`, `scan.rs`, `index.rs`, `hasher.rs`, `daemon.rs`, `session/planner.rs`, `session/engine.rs`, `session/recv_io.rs`, `session/sender_role.rs`, `session/control.rs`, `session/reconnect.rs`, `session/listing_stream.rs`, `session/limits.rs`
- Modify: `crates/irsync/src/platform/mod.rs`, `platform/unix.rs`

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces, in `platform`:

```rust
pub fn set_mtime_file(file: &File, mtime: (i64, u32)) -> io::Result<()>;
pub fn set_mode_file(file: &File, mode: u32) -> io::Result<()>;
pub fn set_owner_file(file: &File, uid: Option<u32>, gid: Option<u32>) -> io::Result<()>;
pub fn euid() -> u32;  pub fn is_root() -> bool;
pub fn groups() -> Vec<u32>;                       // effective and supplementary
pub fn user_name(uid: u32) -> Option<String>;  pub fn group_name(gid: u32) -> Option<String>;
pub fn user_id(name: &str) -> Option<u32>;     pub fn group_id(name: &str) -> Option<u32>;
pub fn local_time(sec: i64) -> /* what session/reconnect.rs needs today */;
```

- `fs/meta.rs`: `entry_of` builds a `FileEntry` from `platform::Stat`; `apply_metadata_at` takes `&Dir` and a name; `create_special_at` takes `&Dir` and a name. `FileStat` no longer appears outside `platform/`.

- [ ] **Step 1: Convert the callers**

Every place listed in the files above that holds an `OwnedFd`, calls an `*at` function, reads a `FileStat`, matches an error number, or uses `MetadataExt`, `PermissionsExt`, `FileExt` or `OsStrExt` goes through `platform`. Error-number matches become matches on `platform::cause(&e)`:

| Today | Becomes |
| --- | --- |
| `ELOOP`, `EMLINK`, `ENOTDIR` meaning "symlink refused" | `LinkRefused \| NotADirectory` |
| `ENOENT` | `NotThere` |
| `EEXIST` in a mkdir race | `AlreadyThere` |
| `EISDIR` on the final rename (`recv.rs`) | `IsADirectory` |
| `EOPNOTSUPP`, `ENOSYS`, `EINVAL` in the chmod fallback | stays inside `Dir::set_mode` |

`hasher.rs` and `recv.rs` use `platform::read_at`/`write_at`.

- [ ] **Step 2: Remove the converted files from `PENDING`** in `tests/platform_rule.rs`

- [ ] **Step 3: Run**

Run: `cargo test -q -p irsync --all-features --no-fail-fast`, then `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS, no expected value changed. `rsync_compare` must have run against a real rsync (see `AGENTS.md`).

- [ ] **Step 4: Commit**

```bash
git commit -am "irsync: metadata and the session code go through the platform module"
```

---

### Task 4: Process-level code, clones and the rest (PR 1)

**Files:**
- Create: `crates/irsync/src/platform/unix/clone.rs` (so `platform/unix.rs` becomes `platform/unix/mod.rs`)
- Delete: `crates/irsync/src/fs/clone.rs`, `fs/rlimit.rs`
- Modify: `crates/irsync/src/main.rs`, `fs/private.rs`, `fs/mod.rs`, `identity.rs`, `manage/mod.rs`, `telemetry/process.rs`, `net/tap/capture.rs`, `ssh/client.rs`, `ssh/setup.rs`, `ssh/payload.rs`, `ssh/legacy.rs`, `session/sender_role.rs`

**Interfaces:**
- Produces, in `platform`:

```rust
pub fn umask() -> u32;                                   // read once, as main.rs does today
pub fn raise_handle_limit() -> io::Result<u64>;          // today's raise_nofile
pub fn handle_limit() -> u64;                            // today's nofile_soft
pub fn create_private_dir_all(dir: &Path) -> io::Result<()>;         // 0700
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()>; // 0600
pub fn open_private(path: &Path, how: Open) -> io::Result<File>;      // 0600, no follow
/// Why this file must not be trusted (others can read it, or it is another user's), if so.
pub fn open_file_problem(meta: &std::fs::Metadata) -> Option<String>;
pub fn key_file_warning(meta: &std::fs::Metadata) -> Option<String>;  // identity.rs:57
pub fn host_name() -> Option<String>;
pub struct Usage { pub cpu: Duration, pub max_rss: u64 }
pub fn process_usage() -> Option<Usage>;

/// What can stop or signal a running irsync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal { Stop, Reload }
/// Resolves each time one arrives. Unix: SIGINT, SIGTERM → Stop; SIGHUP → Reload
/// where the caller asks for reloads, else Stop (as main.rs does today).
pub async fn next_signal(reloads: bool) -> Signal;

pub mod clone { /* today's fs/clone.rs, moved whole: same public items */ }
pub mod daemonize { /* setsid, kill and the pid handling ssh/setup.rs uses today */ }
```

- [ ] **Step 1: Move** `fs/clone.rs` whole to `platform/unix/clone.rs` and re-export it as `platform::clone`; its callers change only their path. Its tests move with it.

- [ ] **Step 2: Convert** `main.rs` (signals, `geteuid`, limit, umask), `fs/private.rs`, `identity.rs`, `manage/mod.rs` (`saved_mode`), `telemetry/process.rs`, `net/tap/capture.rs` and `ssh/*` to the functions above. The three tokio signal sites in `main.rs` become loops over `platform::next_signal`.

- [ ] **Step 3: Remove the converted files from `PENDING`**

- [ ] **Step 4: Run**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -am "irsync: process-level code and clones move into the platform module"
```

---

### Task 5: The tests, the rule and the lint (PR 1)

**Files:**
- Modify: `crates/irsync/src/platform/mod.rs`, `platform/unix/mod.rs` (a `testutil` module)
- Modify: `crates/irsync/tests/common/mod.rs` and every file under `crates/irsync/tests/` that names a banned API
- Modify: `crates/irsync/tests/platform_rule.rs`, `crates/irsync/src/lib.rs`, `crates/irsync/src/main.rs`, `crates/irsync/Cargo.toml`

**Interfaces:**
- Produces, in `irsync::platform::testutil` (`#[doc(hidden)] pub`):

```rust
pub fn symlink(target: &Path, link: &Path) -> io::Result<()>;
/// False when this user may not create symlinks: prints "skipping: …". Panics instead when `CI` is set.
pub fn symlinks_available(dir: &Path) -> bool;
/// A thread that swaps `dir` for a link to `outside` and back until dropped.
pub fn swap_for_link(dir: &Path, outside: &Path) -> impl Drop;
pub struct Unreadable;   pub fn make_unreadable(path: &Path) -> Option<Unreadable>;
pub fn lstat(path: &Path) -> io::Result<Stat>;      // for tree comparison in tests
#[cfg(unix)] pub fn set_mode(path: &Path, mode: u32);
#[cfg(unix)] pub fn mkfifo(path: &Path);
#[cfg(unix)] pub fn send_signal(pid: u32, sig: Signal);
#[cfg(unix)] pub fn non_utf8_name() -> OsString;
```

- [ ] **Step 1: Convert the tests.** `tests/common/mod.rs` (`assert_trees_equal`, `try_create_non_utf8`, the daemon helpers) and each test file go through `platform::testutil`. No assertion changes. A test that is inherently Unix (modes, owners, FIFOs, devices, root, the rsync comparison, ssh onboarding, signals to a daemon) keeps running on Unix; do not gate anything yet.

- [ ] **Step 2: Empty `PENDING`** and delete the constant and its second check.

Run: `cargo test -p irsync --test platform_rule`
Expected: PASS.

- [ ] **Step 3: The lint and the dependencies**

`#![deny(unsafe_code)]` at the top of `lib.rs` and `main.rs`; `#[allow(unsafe_code)]` on `mod platform`. In `crates/irsync/Cargo.toml`, `nix` moves under `[target.'cfg(unix)'.dependencies]`.

- [ ] **Step 4: Full regression**

Run: `tools/ci.sh check`
Expected: PASS. The number of tests equals `main`'s plus the new ones of Tasks 1 and 2.

- [ ] **Step 5: Commit and ship**

Branch `irsync-platform-module`. Ship it. The description says: a pure move, no behaviour change, first of six pull requests of the spec; and lists any place where behaviour could differ, however small.

**Needs the maintainer** before merging: start the FreeBSD workflow on the branch. This pull request moves FreeBSD-specific code (`EMLINK`, the clone and `mknod` variants).

---

### Task 6: irsync's own name conversion (PR 2)

**Files:**
- Create: `crates/irsync/src/wtf8.rs`
- Modify: `crates/irsync/src/lib.rs`, `tools/ci.sh` (Miri list), `tools/tests/test_ci_script.py`

**Interfaces:**
- Produces: the interface and the four tests of `crates/fileset/src/wtf8.rs` (`encode`, `decode`, `Invalid { at }`), as irsync's own file.

- [ ] **Step 1: Tests first, failing; then the implementation; then passing**

Run: `cargo test -p irsync --lib wtf8::`

- [ ] **Step 2: Miri**

Add `"irsync --lib wtf8::"` to `miri_packages` in `tools/ci.sh`, update the comment block and `test_miri_runs_each_listed_crate_with_the_nightly`.

- [ ] **Step 3: Commit**

```bash
git commit -am "irsync: WTF-8 names, its own copy"
```

---

### Task 7: `Dir` on Windows, on native calls (PR 2)

**Files:**
- Create: `crates/irsync/src/platform/windows/mod.rs`, `platform/windows/native.rs`
- Create: `crates/irsync/src/platform/dir_tests.rs` (compiled on both platforms)
- Modify: `crates/irsync/src/platform/mod.rs`, `crates/irsync/Cargo.toml`, `Cargo.toml` (workspace features of `windows-sys`), `docs/SUPPLY_CHAIN_DECISIONS.md`

**Interfaces:**
- Consumes: the `Dir` interface of Task 2 and `Cause` of Task 1.
- Produces: `Dir`, `Stat`, `fstat`, `read_at`, `write_at`, `cause`, `os_to_bytes`, `bytes_to_os` on Windows, and `testutil::{symlink, symlinks_available, swap_for_link, junction, make_unreadable, lstat}`.

`native.rs` holds every `unsafe` block and nothing else. Its functions have safe signatures:

```rust
pub(super) fn open_relative(dir: Option<&OwnedHandle>, name: &[u16], access: u32, share: u32,
    disposition: u32, options: u32, attributes: u32) -> io::Result<OwnedHandle>;   // NtCreateFile
pub(super) fn basic_info(h: &OwnedHandle) -> io::Result<FILE_BASIC_INFO>;
pub(super) fn standard_info(h: &OwnedHandle) -> io::Result<FILE_STANDARD_INFO>;
pub(super) fn id_info(h: &OwnedHandle) -> io::Result<(u64 /*volume*/, u64 /*index*/)>;
pub(super) fn attribute_tag(h: &OwnedHandle) -> io::Result<(u32 /*attributes*/, u32 /*tag*/)>;
pub(super) fn set_basic_info(h: &OwnedHandle, info: &FILE_BASIC_INFO) -> io::Result<()>;
pub(super) fn rename_to(h: &OwnedHandle, to_dir: &OwnedHandle, to_name: &[u16], replace: bool) -> io::Result<()>;
pub(super) fn link_to(h: &OwnedHandle, to_dir: &OwnedHandle, to_name: &[u16]) -> io::Result<()>;
pub(super) fn delete_on_close(h: &OwnedHandle) -> io::Result<()>;          // POSIX semantics, ignore read-only
pub(super) fn list(h: &OwnedHandle) -> io::Result<Vec<Vec<u16>>>;
pub(super) fn get_reparse(h: &OwnedHandle) -> io::Result<Vec<u8>>;         // the raw reparse buffer
pub(super) fn set_reparse(h: &OwnedHandle, buffer: &[u8]) -> io::Result<()>;
```

Rules for the Windows half:

| Operation | How |
| --- | --- |
| every relative open | `open_relative` with `FILE_OPEN_REPARSE_POINT`; the name is one component, never containing `\` or `/` |
| `open_dir` | then `attribute_tag`: a reparse point of any kind is `LinkRefused`; not a directory is `NotADirectory` |
| `open_file` | then `attribute_tag`: a reparse point is `LinkRefused`; share read, write and delete |
| `stat` | open for attributes only; `kind` is `Symlink` only for the symlink tag, `Other` for any other reparse point; `ino` = file index, `dev` = volume serial; `mode`, `uid`, `gid`, `rdev` are 0; `sparse` is false; times converted from 100 ns units since 1601 |
| `rename` | `rename_to` with POSIX semantics and ignore-read-only; replaces |
| `remove_file`, `remove_dir` | open the name without following, then `delete_on_close`; `remove_dir` on a non-empty directory is `NotEmpty` |
| `symlink`, `read_link` | build and parse the symlink reparse buffer in safe code; `native` only passes bytes. A relative target is stored relative |
| `set_mode`, `set_owner`, `make_fifo`, `make_device` | `io::ErrorKind::Unsupported`, which `cause` reports as `NotSupported` |
| `cause` | from the Win32 or NT status: path or file not found → `NotThere`; already exists → `AlreadyThere`; sharing violation, lock violation, delete pending → `InUse`; directory not empty → `NotEmpty`; not same device → `CrossDevice`; plus `LinkRefused`, `NotADirectory`, `IsADirectory`, which this module raises itself |

Check every constant, struct layout and field name against the `windows-sys` source in the cargo registry. Do not guess.

- [ ] **Step 1: Move the `Dir` tests of Task 2 to `platform/dir_tests.rs`** so both platforms compile them, and add the Windows cases:

```rust
#[cfg(windows)] #[test] fn a_junction_is_refused_like_a_symlink()
// open_dir on a junction is LinkRefused; stat gives Other; remove_dir removes the junction, not its target's contents
#[cfg(windows)] #[test] fn names_win32_refuses_work()
// create, stat, list and remove files named "CON", "trail.", "trail ", and a name with the lone unit 0xD800
#[cfg(windows)] #[test] fn rename_replaces_a_read_only_file_and_one_open_elsewhere()
// destination has the read-only attribute: rename succeeds. Destination held open by another handle
// that shares delete: rename succeeds. Held without sharing delete: Cause::InUse
#[cfg(windows)] #[test] fn a_path_deeper_than_260_characters()
#[cfg(windows)] #[test] fn file_identity_and_times()
// two names of one file: same ino and dev, nlink 2; set_mtime(…, (1_700_000_000, 123_456_700)) reads back exactly
```

- [ ] **Step 2: Write `native.rs` and the Windows `Dir`**

Each `native` function gets its `// SAFETY:` comment and, where it parses a variable-length buffer (`list`, `get_reparse`), bounds checks on every length read from the buffer, with a unit test that feeds it a truncated and an oversized buffer through a safe parsing function that takes `&[u8]`.

- [ ] **Step 3: Run `Dir`'s tests on Windows**

`irsync` does not build on Windows until Task 8. For this step add a temporary `#[cfg(windows)]` path in `lib.rs` that compiles only `platform`, `wtf8` and what they need, and remove it in Task 8.

Run: `cargo test -p irsync --lib platform:: wtf8::` on both Windows targets.
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git commit -am "irsync: a directory held open on Windows, on handle-relative calls"
```

---

### Task 8: The rest of the Windows platform file; irsync builds (PR 2)

**Files:**
- Modify: `crates/irsync/src/platform/windows/mod.rs`, `platform/mod.rs`, `crates/irsync/src/lib.rs`
- Modify: test files only as far as needed to compile on Windows

**Interfaces:**
- Produces on Windows, each with the stated behaviour:

| Function | On Windows |
| --- | --- |
| `set_mtime_file` | by handle |
| `set_mode_file`, `set_owner_file` | `Unsupported` |
| `euid`, `is_root`, `groups` | `0`, `false`, empty |
| `user_name`, `group_name`, `user_id`, `group_id` | `None` |
| `umask` | `0` |
| `raise_handle_limit`, `handle_limit` | `Ok(8192)`, `8192` |
| `create_private_dir_all`, `write_private`, `open_private` | plain creation inside the given path; `open_private` never follows a reparse point |
| `open_file_problem`, `key_file_warning` | `None` |
| `host_name` | the `COMPUTERNAME` environment variable |
| `process_usage` | from `sysinfo` if it has the figures, else `None` |
| `next_signal` | tokio's `ctrl_c`, `ctrl_break` and `ctrl_close` → `Stop`; never `Reload` |
| `clone` | every entry point returns `Unsupported`, as on a Unix without reflinks |
| `daemonize` | every entry point returns `Unsupported` |
| `is_transient_extra` | false |
| `local_time` | through the standard library or an existing dependency; no new one |

- [ ] **Step 1: Implement**, remove the temporary path of Task 7 Step 3, and add `#[cfg(unix)]` to tests only where they cannot compile on Windows (they call Unix-only `testutil` items).

- [ ] **Step 2: Build and lint on Windows**

Run: `cargo clippy -p irsync -p irsync-proto --all-targets --all-features -- -D warnings` on both Windows targets.
Expected: clean.

- [ ] **Step 3: Run the tests and record what fails**

Run: the Windows run.
Expected: it compiles and runs. Many tests fail; write the list, grouped by reason, into the pull request description. It is the input of Task 9.

- [ ] **Step 4: Commit and ship**

Branch `irsync-windows-platform`. Ship it. The Unix build is checked by CI.

---

### Task 9: Receiver, deleter and scanner on Windows (PR 3)

**Files:**
- Create: `crates/irsync/src/fs/names.rs` (platform-neutral name rules), `crates/irsync/tests/windows.rs` (`#![cfg(windows)]`)
- Modify: `crates/irsync/src/fs/paths.rs`, `fs/safe.rs`, `fs/transient.rs`, `recv.rs`, `delete.rs`, `scan.rs`, `session/planner.rs`, `session/engine.rs`, `session/recv_io.rs`, `daemon.rs`
- Modify: `crates/irsync/src/platform/windows/mod.rs`, and the tests

**Interfaces:**
- Produces:

```rust
// fs/names.rs
/// Why a stored name cannot exist on `platform`, if it cannot.
pub fn refused(name: &[u8], platform: Platform) -> Option<&'static str>;
// Windows: "not a valid name" for bytes wtf8::decode rejects;
//          "holds a character Windows does not allow" for < > : " | ? * \ or a byte below 0x20.
// Unix: None.
/// Whether two names are one name to a case-insensitive directory: equal after
/// simple Unicode uppercasing of each character (the table Rust's `char::to_uppercase` gives,
/// first character of the result), compared per UTF-16 unit for names with lone surrogates.
pub fn same_ignoring_case(a: &[u8], b: &[u8]) -> bool;

// platform
/// Fails unless `root` is on a local NTFS volume. Unix: always Ok.
pub fn check_root(root: &Path) -> io::Result<()>;
pub fn is_builtin_exclude(name: &[u8]) -> bool;     // Windows: System Volume Information, $RECYCLE.BIN, any case
```

Exact messages: `{path}: not a valid name on this system`, `{path}: holds a character Windows does not allow`, `{path}: differs only by case from {existing}`, `{root} is not on a local NTFS volume`, `{path}: in use by another program`.

- [ ] **Step 1: Write the failing tests**

Platform-neutral, in `fs/names.rs`:

```rust
#[test] fn windows_refuses_what_ntfs_cannot_hold()
// refused(b"a:b", Windows), b"a*b", b"a\x01", b"a\xff" are Some; b"CON", b"trail.", b"a b", "é" are None;
// every one of them is None for Unix
#[test] fn case_only_differences()
// same_ignoring_case(b"A", b"a"); ("É", "é"); not (b"a", b"b"); not ("ß", "SS"); a name is the same as itself
```

In `tests/windows.rs`, each through a real sync between two directories in one process (the helpers in `tests/common`):

```rust
#[test] fn a_tree_syncs_and_syncs_again_with_nothing_to_do()
// files, nested directories, a hard link pair, an empty file, a 1 GiB sparse-looking file of zeros:
// after the first run the trees are equal; the second run transfers nothing
#[test] fn long_paths_and_awkward_names_sync()
// a path over 260 characters; "CON"; "trail."; "trail "; a lone surrogate
#[test] fn a_name_windows_cannot_hold_fails_that_entry_only()
// the sender side is fed an entry named "a:b" (through the test seam of Task 10 if needed, else a
// crafted listing): exit status says some entries failed; the message is the exact text; other files arrive
#[test] fn a_case_only_clash_fails_the_second_entry()
// destination holds "Readme"; the source has "README": the entry fails with the exact text; "Readme" is unchanged
#[test] fn a_destination_in_use()
// held open sharing delete: replaced. Held without sharing delete: that entry fails with "in use by another program",
// after the retries; the session goes on
#[test] fn a_source_file_held_by_a_writer()
// a file opened for writing by another handle that shares read: it is sent.
// One opened with no sharing: that entry fails with the in-use text; the session goes on
#[test] fn read_only_destinations()
// a destination file with the read-only attribute is replaced; a delete of one succeeds
#[test] fn delete_removes_links_not_what_they_point_at()
// --delete with a junction and a directory symlink in the destination: the links go, their targets stay
#[test] fn roots()
// a root given relative, with `/`, with `..`: works. A root on a drive top (use `subst` or skip with a
// printed line if no spare drive letter): System Volume Information and $RECYCLE.BIN are not walked and not warned about
#[test] fn a_root_that_is_not_ntfs_is_refused()
// unit-test check_root's decision function on volume information given as data
#[test] fn a_junction_swapped_in_during_a_sync_redirects_nothing()
// tests/containment.rs runs on Windows too, with testutil::swap_for_link making junctions
```

- [ ] **Step 2: Implement**

- `RelPath` gains `to_os_checked(&self) -> io::Result<OsString>`; the receiving side checks each incoming path with `names::refused(…, Platform::CURRENT)` once, where entries enter the plan, and fails that entry.
- The case check runs on Windows where a new name is about to be created in a directory: list the directory once per directory, cached for the session's pass over it.
- `fs/transient.rs`: `Cause::InUse` is retried with waits that sum to about one second (`[10, 30, 60, 100, 200, 300, 300]` ms), then reported with the in-use text.
- `platform::check_root` is called where roots are opened (`engine.rs` `prepare_root`, the daemon's exports, the sender's source).
- The scanner applies `platform::is_builtin_exclude` at the top of a root that is the top of a volume. Unix: the function is false for every name; nothing changes.
- Sort the tests that still fail on Windows: platform-neutral behaviour is made to run there; POSIX-only behaviour, the rsync comparison, ssh onboarding and signal tests are `#[cfg(unix)]`. No test is deleted; no assertion changes on Unix.

- [ ] **Step 3: Run**

Run: the Windows run. Expected: 0 failed on both targets. Run it once with `CI=1`: no symlink test skips.

- [ ] **Step 4: Commit and ship**

Branch `irsync-sync-on-windows`. Ship it.

---

### Task 10: Each side says what it is (PR 4)

**Files:**
- Modify: `crates/irsync-proto/proto/irsync/v1/common.proto`, `crates/irsync-proto/src/lib.rs`
- Create: `crates/irsync/src/session/family.rs`
- Modify: `crates/irsync/src/session/mod.rs`, `session/control.rs`, `session/engine.rs`, `daemon.rs`

**Interfaces:**
- Produces:

```proto
// common.proto
enum PlatformFamily { PLATFORM_FAMILY_UNSPECIFIED = 0; PLATFORM_FAMILY_UNIX = 1; PLATFORM_FAMILY_WINDOWS = 2; }
// Hello:    PlatformFamily platform = 33;
// HelloAck: PlatformFamily platform = 6;
```

```rust
// session/family.rs: platform-neutral, tested everywhere
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Families { pub local: Platform, pub peer: Platform, pub peer_said_so: bool }
impl Families {
    pub fn from_wire(local: Platform, peer: PlatformFamily) -> Families;  // UNSPECIFIED → Unix, peer_said_so false
    pub fn mixed(self) -> bool;
}
```

`Families` is carried in the session state on both roles. Nothing reads `Platform::CURRENT` for a session decision after this task; it reads `Families::local`, so a test can pretend.

- [ ] **Step 1: Write the failing tests**

```rust
// irsync-proto: a Hello encoded without the field decodes with platform UNSPECIFIED;
// a Hello with platform = WINDOWS decoded by a struct without the field loses nothing else (round trip of the other fields)
// session/family.rs
#[test] fn an_absent_family_is_unix()
#[test] fn mixed_only_when_the_two_differ()
// an in-process session test: both sides end up with the other's family in their state
```

- [ ] **Step 2: Implement.** `PROTOCOL_VERSION` stays 5. Check the protocol documentation page (`book/src/reference/protocol.md`) and the `pbwalk` field tables for a list of `Hello` fields that must gain the new one.

- [ ] **Step 3: Run** `cargo test -q --workspace --all-features --no-fail-fast` (Unix, in CI) and the Windows run.

- [ ] **Step 4: Commit**

```bash
git commit -am "irsync: each side of a session says its platform family"
```

---

### Task 11: The mixed-session rules (PR 4)

**Files:**
- Modify: `crates/irsync/src/session/family.rs`, `session/planner.rs`, `session/control.rs`, `session/engine.rs`, `session/sender_role.rs`, `fs/meta.rs`, `recv.rs`, `scan.rs`, `daemon.rs`
- Test: `crates/irsync/tests/mixed.rs` (runs on every platform)

**Interfaces:**
- Consumes: `Families` (Task 10).
- Produces, in `session/family.rs`, all pure functions:

```rust
impl Families {
    /// Whether owners and groups may be applied or sent at all in this session.
    pub fn owners(self) -> bool;                       // false when mixed, or when local is Windows
    pub fn modes(self) -> bool;                        // false when local is Windows
    pub fn specials(self) -> bool;                     // false when local is Windows
    /// The mode a Windows sender reports.
    pub fn sent_mode(kind: Kind) -> u32;               // File 0o644, Dir 0o755, Symlink 0o777
    /// The grain at which this session compares and stores mtimes, in nanoseconds.
    pub fn mtime_grain(self) -> u32;                   // 100 when either side is Windows, else 1
    /// A symlink target as the local side must write it, from the peer's.
    pub fn target_in(self, target: &[u8]) -> Vec<u8>;
    /// A symlink target as the peer must get it, from the local one.
    pub fn target_out(self, target: &[u8]) -> Vec<u8>;
}
/// The earliest and latest time NTFS holds, as (sec, nsec).
pub const NTFS_MIN: (i64, u32) = (-11_644_473_600, 0);
pub fn clamp_to_ntfs(mtime: (i64, u32)) -> (i64, u32);
```

Target translation: only when `mixed()`, and only for a relative target (Unix: does not start with `/`; Windows: does not start with `\`, `/` or a drive letter and `:`). Unix to Windows: every `/` becomes `\`. Windows to Unix: every `\` becomes `/`. An absolute target is unchanged. All translation is done by the Windows side.

Exact messages: `owners and groups cannot be kept with a Windows peer; drop -o and -g, or update irsync on this side` (the refusal of an older client), `skipped {n} special files: this system has none`, `skipped {n} links of a kind irsync does not copy` (reparse points other than symlinks).

- [ ] **Step 1: Write the failing tests**

Pure, in `session/family.rs`:

```rust
#[test] fn owners_only_between_two_unix_sides()
#[test] fn grain()            // (Unix,Unix) 1; any pair with Windows 100
#[test] fn targets()
// local Windows, peer Unix:  in  b"../a/b" -> b"..\\a\\b";  b"/abs/x" unchanged;  out b"..\\a\\b" -> b"../a/b";  b"C:\\x" unchanged
// local Unix or not mixed: unchanged both ways
#[test] fn clamp()            // (-20_000_000_000, 0) -> NTFS_MIN; (0, 123_456_789) -> (0, 123_456_700)
```

In `tests/mixed.rs`, in-process sessions where `Families::local` of one side is set by the test:

```rust
#[test] fn a_windows_receiver_ignores_modes_and_owners()
// a side acting as Windows receives entries with mode 0600, uid 1234: no chmod or chown is attempted (count the calls through a test hook), no error
#[test] fn a_unix_root_receiver_does_not_apply_a_windows_senders_owner()
// the planner's metadata options have owner and group off when Families::mixed(), whatever -o and -g say
#[test] fn an_older_client_asking_a_windows_daemon_for_owners_is_refused()
// peer_said_so false, Preserve has owner or group, local Windows: the daemon ends the session with the exact
// text, sent to the client the way the daemon reports a refused request today (find that path in daemon.rs and reuse it)
#[test] fn mtime_is_compared_at_the_session_grain()
// source mtime (1_700_000_000, 123_456_789), destination (…, 123_456_700): nothing is transferred when grain is 100
#[test] fn an_mtime_before_1601_is_clamped_and_not_sent_again()
#[test] fn special_files_are_skipped_with_one_warning()
// two FIFOs and a device in the listing, local Windows: none planned; one warning with the count 3; exit status 0
#[test] fn symlinks_are_translated_and_looked_at_again()
// listing order: link "a" -> "dir" (a directory that arrives later). After the session the link is a directory link.
// (On Unix the kind is not observable: assert the second-look list holds the path and is processed.)
```

In `tests/windows.rs`: `reparse_points_other_than_symlinks_are_skipped_in_a_source` (a junction in the source: not sent, counted in the one warning), and `a_file_symlink_and_a_directory_symlink_arrive_as_such` (Windows to Windows).

- [ ] **Step 2: Implement**

- Sender on Windows: reports `sent_mode`, uid and gid 0, `sparse` false; a symlink with `target_out`; any other reparse point is counted and skipped.
- Receiver: `ApplyOpts` is built from the `Preserve` flags AND the `Families` answers. Incoming mtimes go through `clamp_to_ntfs` on Windows and are compared at `mtime_grain()`; the index stores what is on disk.
- Symlinks on Windows: `LinkKind` from a stat of the resolved target at creation, `File` if it does not resolve; such paths go on a list; at the end of the session each is looked at once more and recreated as a directory link if the target is now a directory.
- The daemon on Windows checks `Preserve` against `Families` right after `Hello`.

- [ ] **Step 3: Run** the Windows run; CI for Unix.

- [ ] **Step 4: Commit and ship**

Branch `irsync-mixed-sessions`. Ship it.

---

### Task 12: Process handling, locations and refused features on Windows (PR 5)

**Files:**
- Modify: `crates/irsync/src/main.rs`, `config/paths.rs`, `config/mod.rs`, `identity.rs`, `daemon.rs`, `daemon_lock.rs`, `ssh/mod.rs` (or where the ssh commands are dispatched), `cli/mod.rs`
- Modify: `crates/irsync/src/platform/mod.rs`, `platform/unix/mod.rs`, `platform/windows/mod.rs`
- Test: `crates/irsync/tests/windows.rs`, unit tests in `config/paths.rs`

**Interfaces:**
- Produces, in `platform`:

```rust
pub struct Locations { pub config: PathBuf, pub state: PathBuf, pub key: PathBuf }
/// Default locations from the environment, given as a lookup so tests can supply one.
pub fn default_locations(env: &dyn Fn(&str) -> Option<OsString>) -> Option<Locations>;
```

| | Unix (unchanged) | Windows |
| --- | --- | --- |
| config | `~/.config/irsync/irsync.toml` | `%APPDATA%\irsync\irsync.toml` |
| state | `$XDG_STATE_HOME/irsync` or `~/.local/state/irsync` | `%LOCALAPPDATA%\irsync` |
| key | `~/.ssh/id_ed25519` | `%USERPROFILE%\.irsync\id_ed25519` |

Exact message for a refused feature: `{what} is not available on Windows yet`, exit code 2, where `{what}` is `ssh setup`, `ssh onboarding` or the name of the legacy service command.

- [ ] **Step 1: Write the failing tests**

```rust
// config/paths.rs, every platform, through default_locations with a supplied environment
#[test] fn windows_locations()   // #[cfg(windows)]: the three paths above from APPDATA, LOCALAPPDATA, USERPROFILE
#[test] fn unix_locations_are_unchanged()   // #[cfg(unix)]: with and without XDG_STATE_HOME
// tests/windows.rs, spawning the binary
#[test] fn the_key_is_made_under_dot_irsync_and_dot_ssh_is_not_touched()
#[test] fn ssh_commands_are_refused()                 // exact text, exit 2
#[test] fn ctrl_break_stops_a_daemon_cleanly()
// spawn the daemon in a new process group (std::os::windows::process::CommandExt::creation_flags,
// from a testutil helper), send Ctrl-Break with testutil::send_ctrl_break(pid) if that needs no new unsafe
// (GenerateConsoleCtrlEvent is unsafe: if no safe route exists, drive it with `taskkill` without /F... which sends WM_CLOSE
// and does not reach a console program; in that case test `next_signal` in-process and leave the end-to-end stop to the manual check)
#[test] fn an_interrupted_sync_leaves_a_part_file_the_next_run_deals_with()
// kill a client mid-file (Child::kill); the part file exists and is not locked; the next run completes and no .part remains
#[test] fn a_second_daemon_on_the_same_state_is_refused()   // daemon.lock
#[test] fn the_daemon_does_not_reload()
// editing irsync.toml while the daemon runs changes nothing until it is restarted; `irsync` prints no reload hint on Windows
```

- [ ] **Step 2: Implement**

- `config/paths.rs` and `expand_tilde` use `platform::default_locations`. `~` in a configured path stays the home directory on both platforms.
- `main.rs`: the ssh commands and the legacy service handling return the refusal on Windows before doing anything. The root check and the private-file checks call the platform functions, which answer "no problem" on Windows.
- Any message or help text that tells the user to send SIGHUP says, on Windows, to restart the daemon.

- [ ] **Step 3: Run** the Windows run; CI for Unix.

- [ ] **Step 4: Check stopping by hand** (if Step 1 could not automate it)

Start `irsync daemon` in a console, press Ctrl-C: it logs a clean stop, removes `daemon.addr`, exits 0 or the code Unix uses for SIGINT. Close the console window on another run: the lock is released. Put the results in the pull request description.

- [ ] **Step 5: Commit and ship**

Branch `irsync-process-on-windows`. Ship it.

---

### Task 13: CI covers irsync on Windows (PR 6)

**Files:**
- Modify: `tools/ci.sh`, `tools/tests/test_ci_script.py`, `.config/nextest.toml` if a Windows profile is needed

**Interfaces:**
- Produces: on Windows `tools/ci.sh check` runs clippy, nextest and the doc tests with `--workspace --exclude stress --exclude netstress` in place of the three `-p` flags; the smoke run gains a third part: an `irsync` copy of the filegen tree into a second directory, then `fileset check` of the copy against the manifest of the source.

- [ ] **Step 1: Change the tests first** (`test_windows_check_runs_the_three_crates` becomes `test_windows_check_runs_the_workspace_without_the_harnesses`; the smoke test gains the two new calls), trace them by hand if no Python is at hand, then change the script.

- [ ] **Step 2: Run** `tools/ci.sh check` on Windows to the end; time it. If the Windows jobs would exceed 20 minutes, add a `ci-windows` nextest profile that leaves out the same slow binaries the `ci-macos` profile leaves out, and say so in the script's comment.

- [ ] **Step 3: Commit**

```bash
git commit -am "ci.sh: check irsync on Windows"
```

---

### Task 14: Documentation (PR 6)

**Files:**
- Modify: `book/src/reference/limitations.md`, `book/src/reference/files-and-state.md`, `book/src/reference/configuration.md`, `book/src/reference/command-line.md`, `book/src/reference/protocol.md`
- Modify: `book/src/contributing/setup.md`, `book/src/contributing/testing.md`, `AGENTS.md`
- Create or modify: the decision record for irsync (find where irsync's decisions are kept under `docs/`; if there is none, `docs/WINDOWS_DECISIONS.md`)

- [ ] **Step 1: Write**, checking every statement against the code:

- **Limitations:** remove "Linux and macOS only"; add a "Windows" section: the minimum version; local NTFS only; what a Windows side keeps and does not keep (the table of the spec's "Mixed sessions"); owners never transferred in a mixed session and the refusal an older client gets; symlinks need Developer Mode, elevation or the privilege; special files and reparse points other than symlinks are skipped; a OneDrive folder with files kept online cannot be copied; case-only clashes fail; no reload, restart the daemon; the two unchecked gaps (who can read the key and config files; an elevated daemon); what is refused (ssh onboarding).
- **Files and state:** the Windows locations, and that `.ssh` is not used there.
- **Protocol:** the platform field.
- **Contributor pages and `AGENTS.md`:** the platform module, `Dir`, the attack tests, the Windows test commands, that the Windows CI job now covers irsync.
- **Decision records:** handle-relative native calls and why not the alternatives; the platform field and all adaptation on the Windows side; the owner rule; the key location; no reload in this step.

Several of these chapters are man-page sources: read `tools/build-man.sh` and `tools/tests/test_build_man.py` first and use only constructs the chapter already uses.

- [ ] **Step 2: Commit and ship**

Branch `irsync-windows-ci-docs`. **Needs the maintainer** before merging:

1. A real run between a Mac and a Windows machine, both ways, push and pull, with `-a`: the trees are equal by `fileset` as far as the spec says they should be.
2. The FreeBSD workflow on the branch.
3. Whether the `windows` job joins the jobs `CI result` needs. If yes, that is one more commit here: `ci.yml` and the policy test.
