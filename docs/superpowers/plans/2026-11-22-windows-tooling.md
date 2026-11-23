# filegen and fileset on Windows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `filegen`, `fileset` and `irsync-glob` build and pass their tests on Windows (x86_64 and ARM64, NTFS), with no change of behaviour on Linux, macOS and FreeBSD.

**Architecture:** Each tool gets a `platform` module (`mod.rs`, `unix.rs`, `windows.rs`) of plain functions; no other file names a platform API. Names stay bytes outside it: raw on Unix, WTF-8 on Windows, `/` between components. The manifest and the state file record their platform, and one function refuses a manifest from the other family.

**Tech Stack:** Rust 1.100.0; `std::os::windows`; `winapi-util`, `windows-sys`, `cpu-time`, `ctrlc` (Windows only); cargo-nextest; Git Bash for `tools/ci.sh`; GitHub runners `windows-latest` and `windows-11-arm`.

**Spec:** `docs/superpowers/specs/2026-11-22-windows-tooling-design.md`. Read it first. Also read `AGENTS.md`.

## Global Constraints

- All work goes through a pull request against `main`. Never push to `main`.
- Six pull requests in this order, each merged before the next starts: PR 0 is a check with no code; PR 1 = Task 1; PR 2 = Task 2; PR 3 = Tasks 3 to 5; PR 4 = Tasks 6 to 8; PR 5 = Tasks 9 to 11.
- `filegen`, `fileset` and `irsync` share no code. Where both tools need the same thing (the WTF-8 conversion, `Platform`), each has its own copy. Do not create a shared crate or `include!` a file.
- Outside `src/platform/`, no file of either tool names `nix::`, `libc::`, `xattr::`, `std::os::unix`, `std::os::windows`, `std::os::fd`, `windows_sys`, `winapi_util`, `cpu_time` or `ctrlc`. `#[cfg(unix)]` and `#[cfg(windows)]` attributes are allowed anywhere.
- `unsafe`: use a wrapper crate where one does the job. Otherwise call Win32 through `windows-sys` inside `platform/windows.rs` only, each block with a `// SAFETY:` comment.
- Behaviour on Unix does not change: same output, same exit codes, and for `filegen` the same run from the same seed. No new random draw may happen on Unix.
- Nothing on Windows opens, walks or hashes through a reparse point of any kind.
- Bytes that are not a valid stored name are an error, never repaired.
- `fileset`'s `SCHEMA_VERSION` stays 1. The protobuf `Entry` is unchanged. `filegen`'s `FORMAT_VERSION` goes from 3 to 4.
- Targets: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`. No GNU toolchain, no 32-bit.
- A new dependency is the newest release at least 7 days old, gets an entry in `docs/SUPPLY_CHAIN_DECISIONS.md` in the pull request that adds it, and is declared in `[workspace.dependencies]`.
- A new dependency must not bring a second version of any crate: `deny.toml` has `multiple-versions = "deny"`. Check with `tools/ci.sh deny`.
- After any change to `Cargo.lock`, run `tools/sync-fuzz-lock.sh` and commit both lock files; `tools/ci.sh check` fails if they differ.
- Checks live in `tools/ci.sh`, and a change there is tested in `tools/tests/test_ci_script.py`. A change under `.github/` passes `tools/ci.sh workflows` and the policy tests in `.github/scripts/tests/`.
- Windows builds use a short target directory (`CARGO_TARGET_DIR=C:\t`): the MSVC linker fails on a path over 260 characters.
- Commit messages: a short summary line, a blank line, then why, in plain English.
- No attribution of any kind in commits, branches, pull requests or comments (no `Co-Authored-By`, no "Generated with").
- Book and Markdown text: short sentences, plain words, in the style of `book/src/contributing/supply-chain.md`.

## Where to work

- The development machine is Windows 11 ARM64. Until Task 4, `fileset` does not compile there, and until Task 7 neither does `filegen`.
- **Tasks 1 and 2 are checked on Unix.** Work on the Mac host if it is available. If not, check `filegen` here with `cargo check --target x86_64-unknown-linux-gnu -p filegen --all-targets` (after `rustup target add x86_64-unknown-linux-gnu`; it needs no linker), and rely on CI for `fileset`, whose C dependencies do not cross-compile.
- **From Task 4 on, every task is run on Windows for both architectures:**

  ```sh
  export CARGO_TARGET_DIR='C:\t'
  cargo nextest run -p fileset -p filegen -p irsync-glob
  cargo nextest run -p fileset -p filegen -p irsync-glob --target x86_64-pc-windows-msvc
  cargo clippy -p fileset -p filegen -p irsync-glob --all-targets -- -D warnings
  ```

  Before Task 7 leave `-p filegen` out. Called "the Windows run" below. The Unix side of these tasks is checked by CI on the pull request.

## How every task ends (the pull request loop)

Referred to below as "Ship it". Only the last task of a pull request ships; the others end with a commit.

1. `git switch main && git pull`, then `git switch -c <branch>` before the first edit of the pull request.
2. Before pushing: `cargo fmt --all --check`; the Windows run if the task has Windows code; `tools/ci.sh check` when working on Unix.
3. `git push -u origin <branch>`, then `gh pr create --base main` with a description that says what and why.
4. `gh pr checks --watch`. If `CI result` fails and the branch is behind `main`, follow the rule in `AGENTS.md`. Otherwise fix it on the branch.
5. When `CI result` is green: `gh pr merge --merge --delete-branch`, `git switch main && git pull`, delete the local branch.
6. Stop and ask the maintainer instead of merging when the task says "Needs the maintainer".

## Refinements of the spec

Decisions the spec left open or that reading the code forced. The maintainer sees these when reviewing the plan.

- **Reparse tag.** The standard library reports a junction as a symlink. Telling them apart needs the reparse tag, which only `GetFileInformationByHandleEx` gives. So that call is used for the tag as well as the change time in `fileset`, and `filegen` gets it too, for the tag alone. The spec counted two `unsafe` places; there are three blocks: `fileset` file information, `filegen` file information, `filegen` `DeviceIoControl`.
- **Edge names.** `filegen` names are Rust strings today; it makes no non-UTF-8 names on Unix. The `awkward` class already makes a trailing dot and a trailing space, so those need no new code. The new rate, `names.edge_rate` (default 0.02, Windows only), adds unpaired surrogates and reserved device names.
- **Path length** stays counted in stored bytes. A path of N bytes never has more than N 16-bit units, so the 32,767 limit holds. Name length is counted in units on Windows.
- **Read-only files.** On Windows the start-up walk puts a file with the read-only attribute off limits, as it does on Unix for a file without owner write.
- **mtime floor.** NTFS cannot hold a time before 1601. A drawn mtime is clamped to 1601-01-01 on Windows.
- **`rename_over` on Windows** takes only regular files on both sides, and not two names of one file.
- **Dry run.** A dry run probes the capabilities in the output directory, since it must not write to the tree.
- **Smoke run** checks that the tools work together and exit 0; it does not assert that `check` finds the changes. The tools' own tests do that.
- **No fuzz target** for the name conversion; seeded random round trips and Miri cover it.
- **Supply-chain entries** arrive with the dependency (pull requests 3 and 4), not in pull request 5.

## Review Focus

Inputs the spec implies but its test list does not name. Each has a test in the task shown.

1. **A read-only file already in the tree `filegen` is given.** Expected: off limits, never a target; the run does not fail on it. Task 8.
2. **A root given as a relative path, with `/`, or with `..`** (`fileset init a/b/../b`, `filegen run .`). Expected: resolved to the extended form; works as an absolute `\`-path does. Tasks 5 and 8.
3. **A junction that points at its own ancestor.** Expected: both walks end; `fileset` records `other`, `filegen` marks it off limits. Tasks 5 and 8.
4. **A manifest `platform` value that is nearly right** (`Windows`, `unix ` with a space, empty). Expected: refused as unknown, in every store. Task 3.
5. **A config whose mtime range reaches before 1601** (`mtime.distant_years: 1000`). Expected on Windows: the step succeeds with the time clamped; the log and the disk agree. Task 8.

---

### Task 0: Dependency check with the Windows targets (no pull request)

**Files:** none kept.

- [ ] **Step 1: Install cargo-deny at CI's version**

Read the pin from `.github/workflows/ci.yml` (the `deny` job), then `cargo install --locked cargo-deny@<that version>`.

- [ ] **Step 2: Run the check with both Windows targets**

Add `"x86_64-pc-windows-msvc"` and `"aarch64-pc-windows-msvc"` to `targets` in `deny.toml` (working tree only), then:

Run: `cargo deny --locked check`
Expected: `advisories ok, bans ok, licenses ok, sources ok`.

- [ ] **Step 3: Restore and report**

Run: `git restore deny.toml`

If the check passed, go on to Task 1. If it reported anything, **stop: needs the maintainer.** Give them the full output and the three choices in the spec ("Needs the maintainer").

**Done 2026-11-23**, on `main` at `6479896`, with CI's flags (`-D advisory-not-detected -D unmatched-skip -D unnecessary-skip`): bans, licenses and sources pass; one advisory, `chrono` RUSTSEC-2026-0543 (unmaintained), through `netwatch` from iroh. The maintainer accepted it for the Windows build; Task 10 records that in `deny.toml`.

---

### Task 1: fileset platform module, Unix only (PR 1)

A pure move. No test changes its expected values.

**Files:**
- Create: `.gitattributes`
- Create: `crates/fileset/src/platform/mod.rs`, `crates/fileset/src/platform/unix.rs`
- Create: `crates/fileset/tests/platform_rule.rs`
- Modify: `crates/fileset/src/lib.rs`, `model.rs`, `hash.rs`, `cpu.rs`, `walk.rs`, `main.rs`, `pipeline.rs`, `engine/mod.rs`, `engine/check.rs`, `store/mod.rs`, `store/sqlite.rs`, `store/fjall.rs`
- Modify: `crates/fileset/tests/common/mod.rs`, `tests/untrusted_manifest.rs`
- Modify: `crates/fileset/Cargo.toml`

**Interfaces:**
- Produces, in `fileset::platform` (`mod.rs` declares them by `pub use unix::*` under `#[cfg(unix)]`):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform { Unix, Windows }
impl Platform {
    pub const CURRENT: Platform;                 // by cfg, defined in mod.rs
    pub fn as_str(self) -> &'static str;         // "unix" | "windows"
    pub fn parse(s: &str) -> Option<Platform>;   // exact match only
}

pub struct Stat {
    pub kind: Kind, pub mode: u32, pub uid: u32, pub gid: u32, pub size: u64,
    pub nlink: u64, pub ino: u64, pub dev: u64, pub rdev: u64,
    pub mtime: Timespec, pub ctime: Timespec,
}
pub type Xattrs = Vec<(Vec<u8>, Vec<u8>)>;

pub fn lstat(abs: &Path) -> io::Result<Stat>;            // never follows
pub fn read_link(abs: &Path) -> io::Result<Vec<u8>>;     // the target as stored bytes
pub fn read_xattrs(abs: &Path) -> (Xattrs, Option<io::Error>);
pub fn is_link(md: &std::fs::Metadata) -> bool;          // md from symlink_metadata
pub fn open_nofollow(abs: &Path) -> io::Result<File>;    // read-only; never follows, never blocks
pub fn os_to_bytes(name: &OsStr) -> Vec<u8>;
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString>;
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf>;  // `rel` is `/`-separated
pub fn path_to_rel(rel: &Path) -> Vec<u8>;               // a relative path as stored bytes
pub fn thread_cpu_time() -> Option<Duration>;
pub fn core_split() -> Option<(usize, usize)>;

pub mod testutil {
    pub fn symlink(target: &Path, link: &Path) -> io::Result<()>;
    /// False when this user may not create symlinks. Panics instead when
    /// the environment variable `CI` is set.
    pub fn symlinks_available(dir: &Path) -> bool;
    pub struct Unreadable;                                // restores on drop
    /// `None` when the file cannot be made unreadable here (root on Unix).
    pub fn make_unreadable(path: &Path) -> Option<Unreadable>;
    pub fn mkfifo(path: &Path) -> bool;                   // false where there are none
    #[cfg(unix)] pub fn set_mode(path: &Path, mode: u32);
}
```

`Kind` and `Timespec` stay in `model.rs`. `testutil` is `#[doc(hidden)] pub`, so the integration tests can use it.

- [ ] **Step 1: Add `.gitattributes`**

```
* text=auto eol=lf
```

Run: `git add .gitattributes && git add --renormalize . && git status --short`
Expected: only `A  .gitattributes`. If other files show as modified, they hold CRLF in the repository; stop and list them for the maintainer.

- [ ] **Step 2: Write the failing rule test** in `crates/fileset/tests/platform_rule.rs`

```rust
const BANNED: [&str; 10] = [
    "nix::", "libc::", "xattr::", "std::os::unix", "std::os::windows", "std::os::fd",
    "windows_sys", "winapi_util", "cpu_time", "ctrlc",
];

#[test]
fn only_the_platform_module_names_a_platform_api() {
    // Every .rs file under src/, tests/ and benches/ of this crate, except
    // src/platform/, tests/common/ and this file. The failure message lists
    // each offending `path:line: text`.
}
```

- [ ] **Step 3: Run it to see it fail**

Run: `cargo test -p fileset --test platform_rule`
Expected: FAIL, listing the lines in `model.rs`, `hash.rs`, `cpu.rs`, `walk.rs` and the others.

- [ ] **Step 4: Create `platform/mod.rs` and `platform/unix.rs`** with the interface above

Move, do not rewrite:
- `lstat`: the body of `Entry::stat` in `model.rs` up to the `Entry` literal.
- `read_xattrs`, `collect_xattrs`, `is_unsupported` and their unit tests: from `model.rs`.
- `open_nofollow`: the `OpenOptions` call in `hash.rs`. `hash_file` keeps its `is_file` check.
- `thread_cpu_time` and the macOS and Linux `split` (as `core_split`): from `cpu.rs`. `cpu_list_len` stays in `cpu.rs`; `core_split` on Linux calls `crate::cpu::cpu_list_len`.
- `is_link` is `md.is_symlink()`.
- `rel_to_path` is `root.join(OsStr::from_bytes(rel))`; `path_to_rel` is the bytes of the path.

- [ ] **Step 5: Change the call sites to use `platform`**

| File | Change |
| --- | --- |
| `model.rs` | `Entry::stat` builds the `Entry` from `platform::lstat` and `platform::read_link`; `load_xattrs` calls `platform::read_xattrs` |
| `walk.rs` `list_dir` | convert each name with `os_to_bytes` first, then sort the `(bytes, OsString)` pairs by the bytes |
| `walk.rs` `is_sidecar` | compare `os_to_bytes` of the names |
| `engine/check.rs:143` | `platform::rel_to_path(root, path)`; an error counts as "not there" exactly as a failed `symlink_metadata` does today |
| `engine/mod.rs` `store_parent` | `platform::path_to_rel(rel)` |
| `main.rs:141` | `platform::os_to_bytes(&p)` |
| `store/mod.rs:192`, `store/fjall.rs` (the symlink-inside check), `store/sqlite.rs:304` | `platform::is_link(&md)`; in `sqlite.rs` the test becomes `!m.is_file() \|\| platform::is_link(&m)` |
| unit tests in `src/` | `platform::testutil::{symlink, make_unreadable, mkfifo, set_mode}` instead of `std::os::unix` and `libc`; a test of sockets or of a non-UTF-8 name keeps its `#[cfg]` and moves into `platform/unix.rs` if it must name `std::os::unix` |
| `tests/common/mod.rs`, `tests/untrusted_manifest.rs` | `fileset::platform::testutil::symlink` |

- [ ] **Step 6: Make the Unix crates target-specific** in `crates/fileset/Cargo.toml`

```toml
[target.'cfg(unix)'.dependencies]
xattr = { workspace = true }
libc = { workspace = true }
```

and remove both from `[dependencies]`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -q -p fileset --all-features --no-fail-fast` (on Unix)
Expected: PASS, including `only_the_platform_module_names_a_platform_api`, with the same number of tests as on `main` plus one.

- [ ] **Step 8: Commit and ship**

Branch `fileset-platform-module`. Ship it. The description says: a pure move, no behaviour change, first of five pull requests of the spec.

After the merge, refresh the line endings of the Windows checkout (clean tree only): `git rm -r --cached -q . && git reset --hard`. Check: `file tools/ci.sh` no longer says "CRLF".

---

### Task 2: filegen platform module, Unix only (PR 2)

A pure move. The same seed must give the same log as on `main`.

**Files:**
- Create: `crates/filegen/src/platform/mod.rs`, `crates/filegen/src/platform/unix.rs`
- Create: `crates/filegen/tests/platform_rule.rs`
- Delete: `crates/filegen/src/signals.rs`
- Modify: `crates/filegen/src/lib.rs`, `log.rs`, `model.rs`, `run.rs`, `walk.rs`, `ops/mod.rs`, `ops/file.rs`, `ops/link.rs`, `ops/meta.rs`, `ops/tree.rs`
- Modify: `crates/filegen/tests/common/mod.rs`, `tests/cli.rs`, `tests/pace.rs`, `tests/signals.rs`
- Modify: `crates/filegen/Cargo.toml`

**Interfaces:**
- Produces, in `filegen::platform`:

```rust
pub enum Platform { Unix, Windows }      // as in fileset, its own copy

pub const PATH_MAX: usize;               // libc::PATH_MAX
pub const MTIME_STEP_NS: u32;            // 1
pub const MIN_MTIME_SEC: i64;            // i64::MIN
pub const MAX_LINKS: u64;                // u64::MAX
pub const TARGET_SEP: u8;                // b'/'
pub const RENAME_OVER_DIRS: bool;        // true
pub const RENAME_OVER_SAME_FILE: bool;   // true
pub const EDGE_NAMES: bool;              // false

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind { File, Dir, Symlink, Fifo, CharDev, BlockDev, Socket, Other }
pub struct Stat {
    pub kind: FileKind, pub size: u64, pub mode: u32, pub mtime_ns: i128,
    pub ino: u64, pub dev: u64, pub nlink: u64, pub uid: u32,
}
pub fn lstat(path: &Path) -> io::Result<Stat>;
/// Whether filegen may pick this entry as a target (today's `Guard`).
pub fn may_change(stat: &Stat) -> bool;
pub fn is_root() -> bool;

pub fn os_to_bytes(name: &OsStr) -> Vec<u8>;
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString>;
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf>;
pub fn path_to_bytes(path: &Path) -> Vec<u8>;       // any path, for the log
pub fn name_ok(name: &str) -> bool;                 // true
pub fn name_units(name: &str) -> usize;             // name.len()

pub fn set_mtime(path: &Path, sec: i64, nsec: u32) -> io::Result<()>;   // never follows
#[derive(Clone, Copy)] pub enum LinkTarget { File, Dir }
pub fn symlink(target: &[u8], link: &Path, kind: LinkTarget) -> io::Result<()>;
pub fn remove_symlink(link: &Path) -> io::Result<()>;
pub fn read_link(path: &Path) -> io::Result<Vec<u8>>;
pub fn punch_hole(f: &File, offset: u64, len: u64) -> io::Result<bool>; // false: cannot here
pub fn sync_dir(path: &Path) -> io::Result<()>;

// POSIX-only. Plain std types in the signatures; no nix type crosses.
pub fn chmod(..), chown(..), mkfifo(..), mknod(..), set_xattr(..), euid() -> u32, egid() -> u32;

pub fn install_interrupt() -> Arc<AtomicI32>;       // today's signals::install
pub fn interrupt_name(sig: i32) -> String;          // today's signals::name

pub mod testutil { /* symlink, symlinks_available, make_unreadable, mkfifo, set_mode,
                      raise_interrupt(sig: i32) (unix), as in fileset */ }
```

`PostStat::of(&Metadata)` in `log.rs` becomes `PostStat::of(&platform::Stat)`; the kind letters are unchanged (`f d l p c b s ?`).

- [ ] **Step 1: Record the reference log** (on Unix, on `main`, before any edit)

Run a fixed seed for 3000 steps with the command form in `book/src/contributing/filegen.md`, `--dry-run` off, into a temp tree, and keep the log's step lines without their timing fields. Run it again after Step 5; the two must be equal.

- [ ] **Step 2: Write the failing rule test** in `crates/filegen/tests/platform_rule.rs`

The same test as Task 1 Step 2, a copy, scanning this crate.

Run: `cargo test -p filegen --test platform_rule`
Expected: FAIL, listing lines in 11 source files.

- [ ] **Step 3: Create `platform/mod.rs` and `platform/unix.rs`**

Move, do not rewrite:
- `signals.rs` whole, as `install_interrupt` and `interrupt_name`.
- the Linux and macOS `punch` and the fallback from `ops/file.rs`, as `punch_hole`.
- `Guard` from `walk.rs`, as `may_change` and `is_root`. `may_change` returns true for root.
- every `nix`, `libc` and `xattr` call in `ops/meta.rs`, `ops/tree.rs`, `ops/file.rs` and `ops/link.rs`, each behind the function named above.
- `symlink` ignores `kind`. `remove_symlink` is `fs::remove_file`. `sync_dir` is what the code does today when it syncs a directory; if nothing does, leave the function out.

- [ ] **Step 4: Change the call sites**

| File | Change |
| --- | --- |
| `model.rs:188` | `platform::rel_to_path`; `abs_path` keeps its signature, so a failure panics with the path (cannot happen on Unix) |
| `log.rs` | `PostStat::of(&Stat)`, `PostStat::lstat` through `platform::lstat`, `path_bytes` through `platform::path_to_bytes` |
| `walk.rs` | `platform::lstat`, `may_change`, `is_root`; listing sorted by `os_to_bytes` |
| `run.rs` | `platform::PATH_MAX`, `install_interrupt`, `interrupt_name` |
| `ops/link.rs` | targets joined with `platform::TARGET_SEP`; `platform::symlink` with the drawn target's kind (`File` for a dangling one); `platform::remove_symlink` |
| `ops/meta.rs` `set_mtime` | clamp the drawn seconds to `MIN_MTIME_SEC`, round the drawn nanoseconds down to a multiple of `MTIME_STEP_NS`, then `platform::set_mtime` |
| `ops/tree.rs` `hardlink` | no target when the file already has `MAX_LINKS` names, counted in the model with `Model::linked` |
| `ops/tree.rs` `rename_over` | when `!RENAME_OVER_DIRS`, source and destination are regular files only; when `!RENAME_OVER_SAME_FILE`, never two names of one file |
| `names.rs` `acceptable` | `platform::name_units(name) <= limit && platform::name_ok(name)` |
| tests | as in Task 1; `tests/signals.rs` and the two interrupt tests in `tests/cli.rs` become `#[cfg(unix)]` and use `testutil::raise_interrupt` |

On Unix each constant makes its new branch a no-op, and no branch draws from the random generator.

- [ ] **Step 5: Target-specific dependencies** in `crates/filegen/Cargo.toml`

```toml
[target.'cfg(unix)'.dependencies]
nix = { workspace = true, features = ["fs", "signal", "user"] }
libc = { workspace = true }
xattr = { workspace = true }
```

- [ ] **Step 6: Run the tests and compare the log**

Run: `cargo test -q -p filegen --no-fail-fast`, then `cargo test -q --workspace --all-features --no-fail-fast` (stress drives filegen).
Expected: PASS. The reference log of Step 1 equals the new one.

- [ ] **Step 7: Commit and ship**

Branch `filegen-platform-module`. Ship it.

---

### Task 3: fileset: the manifest records its platform (PR 3, part 1)

Runs on every platform. No Windows code yet.

**Files:**
- Modify: `crates/fileset/src/store/mod.rs`, `store/sqlite.rs`, `store/fjall.rs`, `store/redb.rs`
- Modify: `crates/fileset/src/engine/mod.rs`, `engine/check.rs`, `engine/update.rs`, `engine/list.rs`
- Test: unit tests in the three store files; `crates/fileset/tests/engine.rs`

**Interfaces:**
- Consumes: `platform::Platform` (Task 1).
- Produces:

```rust
// store/mod.rs
pub struct Meta { /* existing fields */ pub platform: Platform }

// engine/mod.rs
/// Whether a manifest made on `manifest` can be used on `running`.
/// The one place that decides; step 3 of the Windows work replaces the refusal.
pub fn usable_here(manifest: Platform, running: Platform) -> anyhow::Result<()>;
```

Error text of `usable_here`, exact: `this manifest was made on {manifest}, and this is {running}; fileset cannot use a manifest across the two yet`. Error text for an unknown value, exact: `manifest platform '{value}' is not known to this fileset`.

- [ ] **Step 1: Write the failing store tests** (one set in each of `sqlite.rs`, `fjall.rs`, `redb.rs`, writing the raw key with that store's own API)

```rust
#[test] fn a_new_manifest_records_this_platform()   // meta().platform == Platform::CURRENT
#[test] fn a_manifest_without_the_key_is_unix()     // delete the raw key, reopen ReadOnly: Platform::Unix
#[test] fn an_unknown_platform_is_refused()
// for v in ["Windows", "unix ", "", "macos"]: write raw `platform` = v, reopen:
// Err whose text contains "manifest platform '" and "is not known to this fileset"
#[test] fn set_meta_round_trips_the_platform()      // write Windows on any machine, read Windows
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p fileset --lib store::`
Expected: FAIL to compile: `Meta` has no field `platform`.

- [ ] **Step 3: Add `platform` to `Meta` and to the three stores**

Each store's meta reader maps a missing `platform` key to `Platform::Unix`, a known value through `Platform::parse`, anything else to the error above. Each writer writes `platform.as_str()`. The initial `Meta` of a new manifest has `Platform::CURRENT`.

- [ ] **Step 4: Write the failing engine tests** in `tests/engine.rs`, in the per-backend macro

```rust
fn a_manifest_of_the_other_family_is_refused(b: Backend)
// init a tree; open the store ReadWrite, set_meta with the other Platform, close.
// check, update and list each fail (exit code 2) with the usable_here text, naming both platforms.
fn update_keeps_the_platform(b: Backend)
// init, change a file, update: meta().platform is still Platform::CURRENT.
```

and a unit test in `engine/mod.rs`:

```rust
#[test] fn usable_here_allows_the_same_family_only()
// Ok for (Unix, Unix) and (Windows, Windows); Err with the exact text for the two mixed pairs.
```

- [ ] **Step 5: Implement `usable_here` and call it**

Call it right after the manifest opens in `check`, in `list`, and in `update` for `Mode::Update` (not for `Mode::Init`, which makes a new manifest). `update`'s two `set_meta` calls carry the old meta's `platform` over.

- [ ] **Step 6: Run the tests**

Run: `cargo test -q -p fileset --all-features --no-fail-fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git commit -am "fileset: record the platform in the manifest"
```

---

### Task 4: fileset: names and the Windows platform file (PR 3, part 2)

**Files:**
- Create: `crates/fileset/src/wtf8.rs`
- Create: `crates/fileset/src/platform/windows.rs`
- Modify: `crates/fileset/src/lib.rs`, `platform/mod.rs`, `Cargo.toml`
- Modify: `Cargo.toml` (workspace), `docs/SUPPLY_CHAIN_DECISIONS.md`

**Interfaces:**
- Produces:

```rust
// wtf8.rs: platform-neutral, safe, compiled everywhere
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid { pub at: usize }                  // byte offset of the first bad sequence
pub fn encode(units: &[u16]) -> Vec<u8>;
pub fn decode(bytes: &[u8]) -> Result<Vec<u16>, Invalid>;
```

- `platform/windows.rs` implements every function of Task 1's interface, `testutil` included, plus `#[cfg(windows)] pub fn testutil::junction(target: &Path, link: &Path)` (runs `cmd /c mklink /J`).

- [ ] **Step 1: Write the failing conversion tests** in `wtf8.rs`

```rust
#[test] fn fixed_vectors() {
    let v: [(&[u16], &[u8]); 6] = [
        (&[0x61], b"a"),
        (&[0xE9], &[0xC3, 0xA9]),
        (&[0xD83D, 0xDE00], &[0xF0, 0x9F, 0x98, 0x80]),          // a pair: four bytes
        (&[0xD800], &[0xED, 0xA0, 0x80]),                         // lone high
        (&[0xDFFF], &[0xED, 0xBF, 0xBF]),                         // lone low
        (&[0xDE00, 0xD83D], &[0xED, 0xB8, 0x80, 0xED, 0xA0, 0xBD]), // low then high: two lone
    ];
    // encode(units) == bytes and decode(bytes) == Ok(units) for each
}
#[test] fn invalid_bytes_are_rejected() {
    // each is Err(Invalid { at: 0 }):
    // [0xFF]; [0xC0, 0x80] (overlong); [0xE2, 0x82] (cut short);
    // [0xF4, 0x90, 0x80, 0x80] (above U+10FFFF);
    // [0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80] (a pair written as two three-byte forms)
    // and decode(b"ab\xFF") == Err(Invalid { at: 2 })
}
#[test] fn random_units_round_trip() {
    // 10_000 sequences of 0..=40 units from a fixed-seed xorshift written in the test
    // (no new dev-dependency), a quarter of the units drawn from 0xD800..=0xDFFF:
    // decode(&encode(&u)) == Ok(u)
}
#[test] fn well_formed_names_are_plain_utf8() {
    // for s in ["a", "é", "日本", "😀"]: encode(&s.encode_utf16().collect::<Vec<_>>()) == s.as_bytes()
}
```

- [ ] **Step 2: Run to see them fail, implement `encode` and `decode`, run to see them pass**

Run: `cargo test -p fileset --lib wtf8::`
Expected: first FAIL (not defined), then PASS. This step works on Unix and Windows.

- [ ] **Step 3: Add the Windows dependencies**

Workspace `Cargo.toml`, `[workspace.dependencies]`: `winapi-util`, `windows-sys`, `cpu-time`. Use the `winapi-util` and newest `windows-sys` versions already in `Cargo.lock` if they are a week old. `crates/fileset/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
winapi-util = { workspace = true }
cpu-time = { workspace = true }
windows-sys = { workspace = true, features = ["Win32_Foundation", "Win32_Storage_FileSystem"] }
```

Add an entry for each of the three to `docs/SUPPLY_CHAIN_DECISIONS.md`, in the form of the entries there: what it is used for, why this crate, who maintains it.

- [ ] **Step 4: Write `platform/windows.rs`**

| Function | How |
| --- | --- |
| `os_to_bytes` | `wtf8::encode` of `encode_wide()` |
| `bytes_to_os` | `wtf8::decode`, then `OsString::from_wide`; `Invalid` becomes `io::ErrorKind::InvalidData` |
| `rel_to_path` | split `rel` at `/`, `bytes_to_os` each component, push each onto `root` |
| `path_to_rel` | `os_to_bytes` of each component, joined with `/` |
| handle for metadata | `OpenOptions` with `access_mode(FILE_READ_ATTRIBUTES)` and `custom_flags(FILE_FLAG_BACKUP_SEMANTICS \| FILE_FLAG_OPEN_REPARSE_POINT)` |
| `lstat` | one handle. `winapi_util::file::information`: `ino` = file index, `dev` = volume serial number, `nlink` = link count, size, attributes. `file_info::<FILE_BASIC_INFO>`: `mtime` from `LastWriteTime`, `ctime` from `ChangeTime`. If the attributes have `FILE_ATTRIBUTE_REPARSE_POINT`, `file_info::<FILE_ATTRIBUTE_TAG_INFO>` gives the tag |
| kind | reparse point with tag `IO_REPARSE_TAG_SYMLINK` (`0xA000_000C`): `Symlink`; any other reparse point: `Other`; else directory attribute: `Dir`; else `File`. The reparse test comes first: a junction has the directory attribute |
| other `Stat` fields | `mode`, `uid`, `gid`, `rdev` are 0; a directory's `size` is 0 |
| time conversion | `fn filetime_to_timespec(ft: i64) -> Timespec`: `sec = ft.div_euclid(10_000_000) - 11_644_473_600`, `nsec = ft.rem_euclid(10_000_000) * 100` |
| `read_link` | `os_to_bytes` of `std::fs::read_link` |
| `read_xattrs` | `(Vec::new(), None)` |
| `is_link` | `md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0` |
| `open_nofollow` | read access, `custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)`; then if the opened file's attributes have the reparse bit, `io::ErrorKind::InvalidInput`, "not a regular file" |
| `thread_cpu_time` | `cpu_time::ThreadTime` |
| `core_split` | `None` |
| `testutil::symlink` | `symlink_dir` when the target, resolved against the link's directory, is a directory; else `symlink_file` |
| `testutil::make_unreadable` | holds the file open with `share_mode(0)` until dropped |
| `testutil::mkfifo` | `false` |

The one `unsafe` block is in:

```rust
/// Reads one fixed-size information class of an open file.
fn file_info<T: Copy>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T>
```

called only with `(FILE_BASIC_INFO, FileBasicInfo)` and `(FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo)`. The `// SAFETY:` comment states: the handle is open for the borrow; the buffer is a zeroed `T` of exactly `size_of::<T>()` bytes; both `T` are plain integer structs valid for any bit pattern; the call writes at most that many bytes. Check each constant's and struct's module path against the `windows-sys` documentation; do not guess.

Unit tests in `windows.rs`:

```rust
#[test] fn filetime_conversion()
// 116_444_736_000_000_000 -> Timespec { sec: 0, nsec: 0 }
// 116_444_736_000_000_001 -> { 0, 100 };  0 -> { -11_644_473_600, 0 }
#[test] fn names_round_trip_through_the_os()
// for units in [[0x61], [0xD800], [0x61, 0xDFFF, 0x62]]:
// os_to_bytes(&OsString::from_wide(units)) decodes back to units via bytes_to_os
#[test] fn invalid_stored_bytes_are_an_error()   // bytes_to_os(b"a\xFF") is InvalidData
```

- [ ] **Step 5: Wire the cfg switch** in `platform/mod.rs`

`#[cfg(windows)] mod windows; #[cfg(windows)] pub use windows::*;` and `Platform::CURRENT`.

- [ ] **Step 6: Build and run on Windows**

Run: the Windows run, with `-p fileset -p irsync-glob`.
Expected: both targets compile; clippy is clean. Tests that create symlinks or use Unix-only things may still fail; Task 5 deals with them. List the failures in the commit message.

- [ ] **Step 7: Commit**

```bash
git commit -am "fileset: WTF-8 names and the Windows platform file"
```

---

### Task 5: fileset: the tests on Windows (PR 3, part 3)

**Files:**
- Modify: unit tests in `crates/fileset/src/{model,hash,walk,pipeline}.rs`, `store/mod.rs`
- Modify: `crates/fileset/tests/{engine,list,cli,untrusted_manifest,damaged_manifest,redb_manifest,store_suite}.rs`, `tests/common/mod.rs`
- Create: `crates/fileset/tests/windows.rs` (whole file `#![cfg(windows)]`)

**Interfaces:**
- Consumes: `platform::testutil` (Tasks 1 and 4).

- [ ] **Step 1: Sort the existing failures into the spec's three groups**

| Group | Treatment |
| --- | --- |
| Uses a symlink | starts with `if !testutil::symlinks_available(dir) { return; }` |
| chmod, FIFO, socket, xattr, non-UTF-8 name, uid or mode in the expected output | `#[cfg(unix)]` |
| Needs an unreadable file | `testutil::make_unreadable`; the test returns early on `None` |

A test whose expected text holds a root path compares against the canonical root, which on Windows starts with `\\?\`.

- [ ] **Step 2: Write the Windows tests** in `tests/windows.rs`

```rust
#[test] fn a_tree_deeper_than_260_characters()
// 30 nested directories with 20-character names and a file at the bottom:
// init is Clean, list shows the file, check is Clean.
#[test] fn names_the_shell_refuses()
// in a canonical temp root create `CON`, `trail.`, `trail ` and a name with the lone unit 0xD800:
// init records four entries whose stored names are b"CON", b"trail.", b"trail " and
// [b'x', 0xED, 0xA0, 0x80]; check is Clean.
#[test] fn hard_links_share_an_identity()
// two names of one file: same ino and dev, nlink == 2 for both.
#[test] fn the_three_symlink_kinds_are_recorded_and_not_followed()
// file symlink, directory symlink, dangling: kind symlink, target as on disk,
// and nothing below the directory symlink is listed.
#[test] fn a_junction_is_other_and_not_entered()
// d\f exists; junction j -> d: j has kind other; no entry j/f.
#[test] fn a_junction_to_its_own_parent_ends_the_walk()
// junction d\loop -> d: init returns; d/loop has kind other; no entry below it.
#[test] fn a_manifest_that_is_a_junction_is_refused()
// .fileset.fjall in the tree is a junction to a directory elsewhere: update and check fail,
// with the text that names --manifest.
#[test] fn a_fjall_manifest_with_a_junction_inside_is_refused()
// as the symlink cases in untrusted_manifest.rs, with testutil::junction.
#[test] fn hashing_a_file_swapped_for_a_symlink_fails()
// hash_file on a file symlink is an error, not the target's hash.
#[test] fn a_file_held_without_sharing_is_unreadable_not_fatal()
// make_unreadable on one file: init finishes, warns, and flags that entry UNREADABLE.
#[test] fn a_relative_root_with_slashes_and_dots_works()
// current directory set to a temp dir by running the binary with current_dir:
// `fileset init a/b/../b` then `fileset check a\b` are both Clean (exit 0).
#[test] fn posix_fields_are_zero()
// every entry of a small tree has mode, uid, gid, rdev == 0 and no xattrs.
```

- [ ] **Step 3: Run on Windows, fix what the tests find, run again**

Run: the Windows run, with `-p fileset -p irsync-glob`.
Expected: PASS on both targets. With Developer Mode off the symlink tests print that they skipped; run once with it on.

- [ ] **Step 4: Commit and ship**

Branch `fileset-on-windows`. Ship it. The description lists what `fileset` records on Windows and the blind spot (read-only attribute, security descriptor).

---

### Task 6: filegen: state version 4, the log header, capabilities (PR 4, part 1)

Runs on every platform. On Unix the capability set is always full and no operation is disabled.

**Files:**
- Create: `crates/filegen/src/caps.rs`
- Modify: `crates/filegen/src/lib.rs`, `state.rs`, `log.rs`, `run.rs`, `config.rs`, `platform/mod.rs`, `platform/unix.rs`
- Modify: `crates/stress/src/concurrent.rs` (a test only, unless its parser needs a fix)
- Test: unit tests in `caps.rs`, `state.rs`, `log.rs`, `run.rs`

**Interfaces:**
- Produces:

```rust
// caps.rs
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps { pub symlinks: bool, pub sparse: bool }
impl Caps {
    pub const ALL: Caps;
    pub fn log_value(self) -> String;   // "symlinks,sparse" | "symlinks" | "sparse" | "-"
}
/// The kinds that cannot run on `platform` with `caps`, in OpKind::ALL order.
pub fn disabled(platform: Platform, caps: Caps) -> Vec<OpKind>;
/// A copy of `weights` with the weight of every transition into a disabled kind set to 0.
pub fn without(weights: &Weights, disabled: &[OpKind]) -> Weights;

// platform
pub fn probe_caps(dir: &Path) -> io::Result<Caps>;   // Unix: Ok(Caps::ALL), touches nothing

// state.rs
pub const FORMAT_VERSION: u32 = 4;
pub struct State { /* existing */ pub platform: Platform, pub caps: Caps }

// log.rs
pub struct Header { /* existing */ pub platform: Platform, pub caps: Caps }

// config.rs
pub struct NamesConfig { /* existing */ pub edge_rate: f64 }   // default 0.02, 0.0..=1.0
```

`Platform` gains `Serialize` and `Deserialize` as `unix` / `windows`.

- [ ] **Step 1: Write the failing tests**

```rust
// caps.rs
#[test] fn nothing_is_disabled_on_unix()        // disabled(Unix, Caps::ALL) is empty
#[test] fn windows_disables_the_posix_kinds()
// disabled(Windows, Caps::ALL) == [Mkfifo, Chmod, SetXattr, Chown, Mknod]
#[test] fn missing_capabilities_disable_their_kinds()
// Windows, symlinks false: also Symlink, RepointSymlink, DeleteSymlink
// Windows, sparse false: also PunchHole
#[test] fn without_zeroes_only_the_columns()
// for every row: weight into a disabled kind is 0.0, every other weight unchanged
#[test] fn log_values()   // the four strings above

// state.rs
#[test] fn a_version_3_state_reads_as_unix_with_every_capability()
// serialise sample(), remove the `platform` and `caps` keys, set format_version: 3:
// read() gives platform Unix, caps Caps::ALL, and the rest equal to sample()
#[test] fn a_version_5_state_is_refused()       // StateError::Version { found: 5 }

// log.rs: the existing header test, extended
// the `# run` line ends with ` platform=unix caps=symlinks,sparse`, after `started=`

// run.rs
#[test] fn resume_refuses_a_state_from_the_other_platform()
#[test] fn resume_refuses_a_state_with_other_capabilities()
// write a state, flip the field in the YAML, resume: RunError::Usage whose text names
// both values and says the run would no longer be the one the seed gave

// config.rs
#[test] fn edge_rate_must_be_a_probability()    // -0.1 and 1.5 are ConfigError::Invalid
```

Fix the text of `StateError::Version` to `(expected 3 or 4)`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p filegen --lib`
Expected: FAIL to compile (`caps` not defined).

- [ ] **Step 3: Implement**

- `state::read` accepts versions 3 and 4. For 3 it parses a version-3 shape and fills `platform: Unix, caps: Caps::ALL`. A state is always written as version 4.
- `run`: after the config is loaded, `platform::probe_caps` in the root, or in the output directory for a dry run. On a resume, compare the state's platform and capabilities with the current ones and refuse a difference. Build the chain from `caps::without(&cfg.weights, &disabled)`; the state keeps the config as the user gave it.
- When `disabled` is not empty and the run is not `--quiet`, print once to stderr: `filegen: not available here, weight set to 0: {names joined with ", "}`.
- `log.header` appends ` platform={} caps={}`.

- [ ] **Step 4: Check the stress harness reads the longer header**

Read `run_summary` in `crates/stress/src/concurrent.rs` (near line 495). Add to its tests a `# run` line that ends with ` platform=unix caps=symlinks,sparse` and assert the same result as without. If the parser rejects it, make it ignore fields it does not know.

- [ ] **Step 5: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast` (Unix)
Expected: PASS. The reference log of Task 2 Step 1 still equals a new run's step lines.

- [ ] **Step 6: Commit**

```bash
git commit -am "filegen: record platform and capabilities in state and log"
```

---

### Task 7: filegen: names and the Windows platform file (PR 4, part 2)

**Files:**
- Create: `crates/filegen/src/wtf8.rs` (its own copy; same interface and the same tests as Task 4 Steps 1 and 2)
- Create: `crates/filegen/src/platform/windows.rs`
- Modify: `crates/filegen/src/lib.rs`, `platform/mod.rs`, `Cargo.toml`
- Modify: `Cargo.toml` (workspace), `docs/SUPPLY_CHAIN_DECISIONS.md`

**Interfaces:**
- Consumes: the interface of Task 2 and `probe_caps` of Task 6.
- Produces: `platform/windows.rs` implementing all of it.

- [ ] **Step 1: `wtf8.rs` with its tests, failing then passing**

Run: `cargo test -p filegen --lib wtf8::`

- [ ] **Step 2: Add the dependencies**

Workspace: `ctrlc` (default features). `crates/filegen/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
winapi-util = { workspace = true }
ctrlc = { workspace = true }
windows-sys = { workspace = true, features = [
    "Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_IO", "Win32_System_Ioctl",
] }
```

An entry for `ctrlc` in `docs/SUPPLY_CHAIN_DECISIONS.md`; extend the `windows-sys` and `winapi-util` entries to name `filegen`.

- [ ] **Step 3: Write `platform/windows.rs`**

Constants: `PATH_MAX = 32_767`, `MTIME_STEP_NS = 100`, `MIN_MTIME_SEC = -11_644_473_600`, `MAX_LINKS = 1023`, `TARGET_SEP = b'\\'`, `RENAME_OVER_DIRS = false`, `RENAME_OVER_SAME_FILE = false`, `EDGE_NAMES = true`.

| Function | How |
| --- | --- |
| names and paths | as Task 4; `path_to_bytes` is `os_to_bytes` of the whole path, separators untouched |
| `name_ok` | false if the name holds any of `< > : " \| ? * \` or a character below U+0020 |
| `name_units` | `name.encode_utf16().count()` |
| `lstat` | one handle as in Task 4; `mode` 0, `uid` 0; kind as in Task 4, with `Symlink` only for the symlink tag |
| `may_change` | false when the attributes have `FILE_ATTRIBUTE_READONLY` (keep the attributes in a private field of `Stat` or re-read them) |
| `is_root`, `euid`, `egid` | `false`, `0`, `0` |
| `set_mtime` | handle with `FILE_WRITE_ATTRIBUTES`, `FILE_FLAG_OPEN_REPARSE_POINT \| FILE_FLAG_BACKUP_SEMANTICS`; `File::set_times` with only the modified time |
| `symlink` | `symlink_dir` for `LinkTarget::Dir`, else `symlink_file`; the target is `bytes_to_os(target)` unchanged |
| `remove_symlink` | `remove_dir` when `lstat` shows the directory attribute, else `remove_file` |
| `punch_hole` | `FSCTL_SET_SPARSE`, then `FSCTL_SET_ZERO_DATA` with `FILE_ZERO_DATA_INFORMATION { FileOffset: offset, BeyondFinalZero: offset + len }`; `Ok(false)` when the first fails with `ERROR_INVALID_FUNCTION` or `ERROR_NOT_SUPPORTED` |
| `sync_dir` (if it exists) | `Ok(())` |
| `chmod`, `chown`, `mkfifo`, `mknod`, `set_xattr` | `Err(io::ErrorKind::Unsupported.into())` |
| `probe_caps(dir)` | `symlinks`: create and remove a file symlink `.filegen-probe-{pid}` in `dir`; false on `ERROR_PRIVILEGE_NOT_HELD` (1314), any other error is returned. `sparse`: create a file of that name, `FSCTL_SET_SPARSE`, remove it; false as for `punch_hole` |
| `install_interrupt` | registers `ctrlc::set_handler` once per process (`OnceLock`), clears the flag on every call. The handler sets the flag from 0 to 2; if it was already set, `std::process::exit(130)` |
| `interrupt_name` | `"Ctrl-C"` |

Two `unsafe` blocks, each in one private function:

```rust
fn file_info<T: Copy>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T>;  // tag only
/// DeviceIoControl with an input buffer and no output.
fn ioctl_in<T: Copy>(file: &File, code: u32, input: Option<&T>) -> io::Result<()>;
```

`ioctl_in`'s `// SAFETY:` comment states: the handle is open for the borrow; the input pointer is null with length 0, or points to a live `T` with `size_of::<T>()`; no output buffer; `lpOverlapped` is null and the handle was not opened for overlapped I/O; `lpBytesReturned` points to a local.

Read the `ctrlc` documentation for which console events its Windows handler receives, and note the answer for Task 11's book text.

- [ ] **Step 4: Build on Windows**

Run: the Windows run (all three crates).
Expected: both targets compile; clippy clean. Failing tests are Task 8's.

- [ ] **Step 5: Commit**

```bash
git commit -am "filegen: WTF-8 names and the Windows platform file"
```

---

### Task 8: filegen: edge names and the tests on Windows (PR 4, part 3)

**Files:**
- Modify: `crates/filegen/src/names.rs`, `ops/mod.rs` (the `new_name` helper), `ops/*.rs` call sites of `new_name`
- Modify: unit tests across `crates/filegen/src/`, `tests/cli.rs`, `tests/pace.rs`, `tests/common/mod.rs`
- Create: `crates/filegen/tests/windows.rs` (`#![cfg(windows)]`)

**Interfaces:**
- Changes: `names::draw_name` returns `Option<(Vec<u8>, NameClass)>`; `NameClass` gains `Edge`. Call sites build paths with `platform::bytes_to_os` and stop calling `into_bytes`.

- [ ] **Step 1: Write the failing name tests** in `names.rs`

```rust
#[test] fn edge_names_are_valid_stored_names_of_the_two_kinds()
// draw 2000 names with edge_rate 1.0 through the edge builder directly (pub(crate) fn edge):
// every one decodes with wtf8::decode; each either holds exactly one unpaired surrogate,
// or its part before the first '.' is one of CON PRN AUX NUL COM1 LPT1; both kinds occur.
#[test] fn no_edge_draw_where_edge_names_are_off()
// with platform::EDGE_NAMES false (Unix): two generators with the same seed, one config with
// edge_rate 0.0 and one with 1.0, give the same 500 names.
#[cfg(windows)] #[test] fn windows_names_avoid_the_forbidden_characters()
// 5000 default draws: none holds < > : " | ? * \ or a control character,
// and none is longer than 255 UTF-16 units.
```

- [ ] **Step 2: Implement**

In `draw_name`, at the top of each attempt: `if platform::EDGE_NAMES && rng.random::<f64>() < cfg.edge_rate`, build an edge name; otherwise today's path. The short-circuit keeps Unix runs unchanged.

Edge builder, drawn uniformly between two kinds:
- a `word`, with one unit from `0xD800..=0xDFFF` inserted at a drawn character position, WTF-8 encoded;
- one of `CON`, `PRN`, `AUX`, `NUL`, `COM1`, `LPT1`, plus a drawn extension from the config with probability one half.

Uniqueness among siblings uses the model's fold of the lossy text of the bytes, as the walk does for a name read from disk.

- [ ] **Step 3: Sort the existing test failures** into the three groups of Task 5 Step 1

Tests of `chmod`, `chown`, `mkfifo`, `mknod`, `set_xattr` and of the `Guard` rules become `#[cfg(unix)]`. `tests/common/mod.rs` reads trees through `filegen::platform::lstat`.

- [ ] **Step 4: Write the Windows tests** in `tests/windows.rs`

```rust
#[test] fn a_default_run_finishes_and_resumes()
// 3000 steps from seed 1, then --resume for 1000 more: both exit 0; the log's `# run` lines
// say platform=windows; stderr names the five POSIX kinds once per run.
#[test] fn the_same_seed_gives_the_same_run()
// two fresh trees, same seed, 2000 steps: equal step lines (timing fields removed).
#[test] fn punch_hole_makes_a_real_hole()
// a 1 MiB file of non-zero bytes; platform::punch_hole(&f, 262_144, 524_288) is Ok(true);
// the range reads as zeros and the rest is unchanged; lstat size is still 1 MiB;
// `fsutil sparse queryflag` succeeds and `fsutil sparse queryrange` prints two ranges
// (two lines holding "0x").
#[test] fn hard_links_share_an_identity()         // lstat: same ino, nlink == 2
#[test] fn set_mtime_does_not_follow_and_rounds_to_100ns()
// set_mtime(file, 1_700_000_000, 123_456_700): lstat mtime_ns == 1_700_000_000_123_456_700.
// On a symlink (if available): the link's mtime changes, the target's does not.
#[test] fn an_mtime_before_1601_is_clamped()
// run with `--set mtime.distant_years=1000 --set mtime.distant=1` and only set_mtime weighted:
// the run exits 0, and every post-step mtime in the log is >= -11_644_473_600 s and equals the disk.
#[test] fn edge_names_exist_on_disk()
// run with `--set names.edge_rate=1`, 300 steps: exit 0; the tree holds a name with an
// unpaired surrogate and one reserved device name; a second start (--resume) walks them.
#[test] fn symlinks_of_the_three_kinds()          // needs symlinks_available
// only the symlink kinds weighted, 300 steps: file, directory and dangling targets occur;
// read_link of each equals the target in the log; targets use `\`.
#[test] fn without_the_symlink_capability_the_kinds_are_off()
// caps::disabled(Windows, Caps { symlinks: false, sparse: true }) drives a run built in-process:
// no symlink step in 2000 steps.
#[test] fn a_read_only_file_is_off_limits()
// a tree with one file whose read-only attribute is set (Permissions::set_readonly):
// walk::from_disk marks it OffLimits; a 500-step run exits 0 and the file is unchanged.
// Clear the attribute at the end so the temp dir can be removed.
#[test] fn a_junction_is_off_limits_and_not_entered()
// d\f and junction d\loop -> d: from_disk ends; loop is OffLimits; d/f appears once.
#[test] fn a_relative_root_works()
// `filegen run . --seed 3 --steps 200` with current_dir set to the tree: exit 0.
#[test] fn rename_over_takes_files_only()
// only rename_over weighted, in a tree of files, empty directories and hard links:
// 500 steps exit 0 and no step line names a directory as destination.
```

- [ ] **Step 5: Run on Windows, fix, run again**

Run: the Windows run (all three crates).
Expected: PASS on both targets.

- [ ] **Step 6: Check Ctrl-C by hand** (no automated test: sending a console event needs `unsafe`)

Start `filegen run <tree> --seed 5 --steps 100000000` in a console and press Ctrl-C once.
Expected: the log ends with an outcome of `interrupted: Ctrl-C`, the state file exists, `echo $?` prints 130, and `--resume` continues. Start again and press Ctrl-C twice quickly: the process exits at once with 130. Put both results in the pull request description.

- [ ] **Step 7: Commit and ship**

Branch `filegen-on-windows`. Ship it.

---

### Task 9: `tools/ci.sh check` on Windows (PR 5, part 1)

**Files:**
- Modify: `tools/ci.sh`
- Test: `tools/tests/test_ci_script.py`

**Interfaces:**
- Produces: in `tools/ci.sh`, `windows_packages=(filegen fileset irsync-glob)`; `os` is `Windows` when `uname -s` starts with `MINGW`, `MSYS` or `CYGWIN`; `fileset --lib wtf8::` in `miri_packages`.

- [ ] **Step 1: Write the failing tests**

Give `run_with` an optional `uname` argument that puts a fake `uname` on the path instead of the real one. The fake cargo appends its arguments to a file, one call per line.

```python
WINDOWS = 'echo MINGW64_NT-10.0-26100'

def test_windows_check_runs_the_three_crates(self):
    # ci.sh check with uname=WINDOWS: exit 0. The cargo calls, in order, start with:
    #   fmt --all --check
    #   clippy --locked -p filegen -p fileset -p irsync-glob --all-targets --all-features -- -D warnings
    #   nextest run --locked -p filegen -p fileset -p irsync-glob --all-features --profile ci --run-ignored all
    #   test --locked -p filegen -p fileset -p irsync-glob --all-features --doc
    # No call holds "--workspace". python3, pandoc, mandoc and rsync are not on the path.

def test_windows_check_ends_with_the_smoke_run(self):
    # after the doc tests: calls starting "run --locked -q -p filegen --" and
    # "run --locked -q -p fileset --"; the last three fileset calls hold init, update, check in that order.

def test_bare_run_on_windows_is_check_only(self):
    # ci.sh with no job, uname=WINDOWS: stdout holds
    # "ci.sh: on Windows only check runs, for filegen, fileset and irsync-glob"
    # and no cargo call starts with "deny".

def test_check_elsewhere_still_takes_the_workspace(self):
    # uname "Linux" (fake), check with fake python3/rsync/pandoc/mandoc on the path:
    # the clippy and nextest calls hold "--workspace".
```

Update `test_miri_runs_each_listed_crate_with_the_nightly` for the new entry.

- [ ] **Step 2: Run to see them fail**

Run: `python3 -m unittest tools.tests.test_ci_script -k windows` (on Unix)
Expected: FAIL.

- [ ] **Step 3: Implement in `tools/ci.sh`**

- `needs_check` on Windows needs only cargo-nextest.
- `job_check` on Windows: skips the `touch` of `crates/irsync/build.rs`, the `rustfmt` of the fuzz targets and the Python tests; passes `-p` for each of `windows_packages` where the others pass `--workspace`; runs one clippy (these crates have no OpenTelemetry feature); then calls `smoke_tools`.
- `smoke_tools`: in a `mktemp -d` directory, removed on exit: `filegen` 2000 steps from seed 1 into `tree` with its output directory beside it; `fileset init tree`; `fileset check tree`; `filegen --resume` for 500 steps; `fileset update tree`; `fileset check tree`. Use the command forms in `book/src/contributing/filegen.md` and `book/src/fileset/reference.md`. Every command must exit 0; if `update` reports what it changed with status 1, accept 0 or 1 for that one command. It must take under 20 seconds on the development machine.
- Update the header comment of the script: what `check` does on Windows.

- [ ] **Step 4: Run the tests and the real thing**

Run on Unix: `python3 -m unittest discover -s tools/tests` and `tools/ci.sh shell-scripts`. Expected: PASS.
Run on Windows, in Git Bash: `CARGO_TARGET_DIR='C:\t' tools/ci.sh check`. Expected: every step prints and passes, ending with the smoke run.
Run on Linux: `tools/ci.sh miri`. Expected: PASS with the `fileset` entry. If `fileset` does not build under Miri, stop: needs the maintainer.

- [ ] **Step 5: Commit**

```bash
git commit -am "ci.sh: check filegen, fileset and irsync-glob on Windows"
```

---

### Task 10: The Windows CI job and `deny.toml` (PR 5, part 2)

**Files:**
- Modify: `.github/workflows/ci.yml`, `deny.toml`
- Test: `.github/scripts/tests/test_workflow_policy.py`

- [ ] **Step 1: Write the failing policy tests**

```python
NON_BLOCKING = {"windows"}

def test_ci_result_needs_every_job(self):
    # as today, but: set(res["needs"]) == set(j) - {"ci-result"} - NON_BLOCKING

def test_windows_job_reports_but_does_not_block(self):
    # jobs has "windows"; its matrix os list is ["windows-latest", "windows-11-arm"];
    # fail-fast is False; it needs "changes" and has the same `if` as the check job;
    # a step runs "tools/ci.sh check"; "windows" is not in ci-result's needs.

def test_deny_covers_windows(self):
    # deny.toml [graph].targets holds x86_64-pc-windows-msvc and aarch64-pc-windows-msvc.
```

Run: `python3 -m unittest discover -s .github/scripts/tests`
Expected: the two new tests FAIL.

- [ ] **Step 2: Add the job to `ci.yml`**

```yaml
  # filegen, fileset and irsync-glob on Windows. Reports on every pull
  # request and does not block: `CI result` does not need it. It joins the
  # required jobs when irsync itself builds on Windows.
  windows:
    name: ${{ matrix.os }}
    needs: changes
    if: needs.changes.outputs.code == 'true'
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, windows-11-arm]
    defaults:
      run:
        shell: bash
    env:
      # The MSVC linker fails on a path over 260 characters.
      CARGO_TARGET_DIR: C:\t
    steps:
      # checkout, "Install Rust", rust-cache and "Install cargo-nextest":
      # copy the four steps from the check job, same pins, without the target lines.
      - name: Format, clippy, tests and the smoke run
        run: tools/ci.sh check
```

GitHub sets `CI=true`, so a runner that cannot create symlinks fails the symlink tests instead of skipping them.

- [ ] **Step 3: Add the targets to `deny.toml`**

Add both Windows targets to `targets`, and rewrite the comment above the list: it now says Windows is covered because `filegen` and `fileset` are built for it.

Task 0 found one advisory with the Windows targets, and the maintainer accepted it (2026-11-23). Add it to `[advisories].ignore`, following "How to accept an advisory" in `book/src/contributing/supply-chain.md`:

```toml
{ id = "RUSTSEC-2026-0543", reason = "chrono is unmaintained; it is only in the Windows build, through netwatch (iroh); tracked in #174" },
```

Add `test_deny_accepts_chrono_for_windows_only` to the policy tests: the `ignore` list holds exactly that ID, and its reason names `#174`.

- [ ] **Step 4: Run the checks**

Run: `python3 -m unittest discover -s .github/scripts/tests`, `tools/ci.sh workflows`, `tools/ci.sh deny`.
Expected: PASS. If actionlint does not know the `windows-11-arm` label, add it to actionlint's configured runner labels rather than removing the runner.

If the `windows-11-arm` job never starts on the pull request (no runner for the organisation), stop: needs the maintainer.

- [ ] **Step 5: Commit**

```bash
git commit -am "CI: test filegen and fileset on Windows, x86_64 and ARM64"
```

---

### Task 11: Documentation (PR 5, part 3)

**Files:**
- Modify: `book/src/fileset/reference.md`, `book/src/fileset/how-it-works.md`
- Modify: `book/src/contributing/filegen.md`, `book/src/contributing/filegen-internals.md`
- Modify: `book/src/contributing/setup.md`, `book/src/contributing/testing.md`
- Modify: `AGENTS.md`
- Modify: `docs/FILESET_DECISIONS.md`, `docs/FILEGEN_DECISIONS.md`
- Do not change: `book/src/reference/limitations.md` ("Linux and macOS only" is about irsync)

- [ ] **Step 1: fileset pages**

State, in the reference and in "How it works":
- the table "Manifest field / Value on Windows" from the spec;
- names are stored as WTF-8 with `/`; patterns for `list` use `/`;
- the `platform` key, and that `check`, `update` and `list` refuse a manifest from the other family, with the message;
- use a current `fileset` on both sides: an older one on Unix does not refuse a Windows manifest and reports false differences in `mode`, `uid` and `gid`;
- reparse points are never followed; a junction is `other`; `--cross-mount-points` has no effect on Windows;
- the blind spot: the read-only attribute and the security descriptor are not recorded;
- not supported: ReFS, FAT, exFAT, network shares, folders shared from a virtual machine's host.

- [ ] **Step 2: filegen pages**

State: the operations table from the spec; the capabilities and how they are found; the startup notice; reproducibility per platform family and capability set; names on Windows and `names.edge_rate`; state version 4 and what a resume refuses; `platform=` and `caps=` in the log; Ctrl-C on Windows, worded from what Task 7 Step 3 found about `ctrlc`, and that there is no polite stop from another process yet.

- [ ] **Step 3: Setup and testing pages, `AGENTS.md`**

`setup.md`, a section "Windows": the MSVC build tools, Git Bash for `tools/ci.sh`, Developer Mode for the symlink tests, a short checkout path and `CARGO_TARGET_DIR`, trees on an NTFS drive, and that `tools/dev-setup.sh` does not run there.

`AGENTS.md`, under "Tests": on Windows only `filegen`, `fileset` and `irsync-glob` build; the everyday run is `cargo test -q -p filegen -p fileset -p irsync-glob --no-fail-fast`; the full run is `tools/ci.sh check` in Git Bash. Under "Things that trip people up": the `windows` CI job reports and does not block.

- [ ] **Step 4: Decision records**

`docs/FILESET_DECISIONS.md`: WTF-8 names; no schema bump for the `platform` key and its cost; refusal across platforms in one function; any reparse point refused for an in-tree manifest. `docs/FILEGEN_DECISIONS.md`: the platform module instead of `cfg` at call sites; capabilities and weight zero; state version 4; the reparse tag read as a third `unsafe` place; `rename_over` on Windows. Each in the form of the entries there.

- [ ] **Step 5: Build the book**

Run: `mdbook build book`
Expected: no warning about a broken link.

- [ ] **Step 6: Commit and ship**

Branch `windows-tooling-ci-docs`. Before merging: **needs the maintainer.** Ask them to start the FreeBSD workflow on this branch (the Unix regression the spec asks for) and to confirm both `windows` jobs ran. Merge after their go-ahead.
