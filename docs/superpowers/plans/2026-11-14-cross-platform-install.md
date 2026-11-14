# Cross-platform install over ssh: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A release build of irsync installs the matching release binary on
a server whose OS or CPU differs from the client's, instead of stopping.

**Architecture:** `ensure_installed` in `ssh/client.rs` keeps copying the
running binary when the platforms match. When they differ, a new module
`ssh/release.rs` downloads the release tarball with the client's `curl` or
`wget`, checks its SHA-256, unpacks the binary with `tar`, and the existing
upload sends it. The upload command gains a `--version` check on the server
before anything is replaced.

**Tech stack:** Rust, the system `curl`/`wget`/`tar`, the `sha2` crate
(already in `Cargo.lock`), the `fake-ssh` test fixture.

**Spec:** `docs/superpowers/specs/2026-11-14-cross-platform-install-design.md`

## Global constraints

- Never run `install.sh` or the release's installer anywhere.
- A failure changes nothing on the server: no new file, no replaced binary,
  no leftover temp file.
- The installed binary's `--version` must equal `irsync <LONG_VERSION>`
  exactly, commit included.
- Tag: `v<CARGO_PKG_VERSION>`. Files:
  `https://github.com/irsync-dev/irsync/releases/download/<tag>/irsync-<target>.tar.xz`
  and `.../<tag>/sha256.sum`. The binary inside is `irsync-<target>/irsync`.
- Targets: linux x86_64 → `x86_64-unknown-linux-musl`; linux aarch64 →
  `aarch64-unknown-linux-musl`; macos aarch64 → `aarch64-apple-darwin`.
  Nothing else.
- Dev builds (dirty, unknown commit, or the `netsim` feature) never
  download.
- No test touches the network.
- No new dependency beyond `sha2` as a direct one. Use the version already
  in `Cargo.lock` that needs no new crates (0.10.9 or 0.11.1; pick the one
  whose `cargo tree` adds nothing).
- Messages are plain, one line, and say what to do. Every user-facing
  message printed outside an error starts with `irsync: `.
- Commit messages: a short summary line, a blank line, a brief why. No
  attribution trailers of any kind.
- Run the full regression from `AGENTS.md` before the last commit.

## Review focus

1. `sha256.sum` written another way (uppercase hex, `  name` instead of
   ` *name`, CRLF line ends): the right line is still found. Test in Task 2.
2. The tarball's `irsync-<target>/irsync` is missing or is a symlink:
   refused with a clear message, nothing uploaded. Test in Task 2.
3. A failed download or checksum leaves nothing in the client's temp
   directory. Test in Task 2.
4. `TMPDIR` holds a space: the download and unpack still work. Test in
   Task 2.
5. The uploaded binary cannot run on the server at all (wrong libc, a
   `noexec` home): reported as "does not run", the temp file is removed,
   the old binary stays. Test in Task 1.

## File structure

- `crates/irsync/src/ssh/client.rs`: probe, upload, `ensure_installed`
  (modified).
- `crates/irsync/src/ssh/release.rs`: new. Which release file, download,
  checksum, unpack. Knows nothing about ssh.
- `crates/irsync/src/ssh/onboard.rs`: summary line for the new outcome.
- `crates/irsync/tests/common/release.rs`: new. Builds a fake release
  directory. Included with `#[path = "common/release.rs"] mod release;`
  so tests that don't use `common/mod.rs` stay light.
- `crates/irsync/tests/ssh_release.rs`: new. Tests for `release.rs`.
- `crates/irsync/tests/ssh_client.rs`, `tests/onboard_e2e.rs`: modified.

---

### Task 1: The server checks the uploaded binary's version

**Files:**
- Modify: `crates/irsync/src/ssh/client.rs` (`ensure_installed`, lines 157-203)
- Test: `crates/irsync/tests/ssh_client.rs`

**Interfaces:**
- Produces, in `client.rs`:

  ```rust
  /// What the upload command reported.
  #[derive(Debug, PartialEq, Eq)]
  enum Upload { Done, Short, NotThisVersion }

  /// Send `local` to the server as the new irsync. Replaces the old one
  /// only if the size matches and its `--version` is ours.
  fn upload(rsh: &Rsh, dest: &str, local: &Path) -> Result<Upload>
  ```

  `ensure_installed` keeps its signature and calls `upload`.

- [ ] **Step 1: Write the failing tests** in `tests/ssh_client.rs`

```rust
/// A stand-in binary that reports `version` (or fails when `None`).
fn fake_exe(f: &Fake, version: Option<&str>) -> PathBuf {
    let p = f.home.path().join("fake-irsync");
    let body = match version {
        Some(v) => format!("#!/bin/sh\necho '{v}'\n"),
        None => "#!/bin/sh\nexit 126\n".to_owned(),
    };
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn assert_nothing_installed(f: &Fake) {
    let bin = f.home.path().join(REMOTE_BIN);
    assert!(!bin.exists());
    assert_eq!(std::fs::read_dir(bin.parent().unwrap()).unwrap().count(), 0);
}

#[test]
fn install_rejects_a_copy_that_reports_another_version() {
    let f = fake(None);
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err().to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn install_rejects_a_copy_that_does_not_run() {
    let f = fake(None);
    set_exe_override(fake_exe(&f, None));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err().to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_rejected_copy_keeps_the_old_binary() {
    let f = fake(None);
    let dst = f.home.path().join(REMOTE_BIN);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    ensure_installed(&f.rsh, "myserver", &p).unwrap_err();
    assert!(std::fs::read_to_string(&dst).unwrap().contains("0.0.1 (old)"));
    assert_eq!(std::fs::read_dir(dst.parent().unwrap()).unwrap().count(), 1);
}
```

- [ ] **Step 2: Run them, expect failure**

Run: `cargo test -q -p irsync --all-features --test ssh_client`
Expected: the three new tests fail (the copy succeeds).

- [ ] **Step 3: Implement `upload`**

Move the upload out of `ensure_installed` into `upload`. In the remote
command, between `chmod 755` and `mv -f`, add: run the temp file with
`--version`, take the first line, compare with `irsync <LONG_VERSION>`; on
a mismatch or a failed run remove the temp file and `exit 4`. Exit 3 →
`Upload::Short`, exit 4 → `Upload::NotThisVersion`, any other failure → the
existing "copying irsync to {dest} failed" error. The version text sits
inside double quotes in the sh command; `sh_quote`'s rule (no `'` or `\`)
still holds, keep its `debug_assert`.

In `ensure_installed`, `NotThisVersion` on the same-platform path becomes:
"the copied irsync does not run on {dest}. Nothing was changed there.
Install irsync on the server and set it up by hand: <the existing URL>".

- [ ] **Step 4: Run the tests, expect pass**

Run: `cargo test -q -p irsync --all-features --test ssh_client --test onboard_e2e`
Expected: all pass, including the existing copy tests.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/ssh/client.rs crates/irsync/tests/ssh_client.rs
git commit -m "Check the copied irsync's version before it replaces anything

A copied binary that cannot run on the server (older glibc, NixOS) was
installed anyway and reinstalled on every run."
```

---

### Task 2: Download and unpack a release binary

**Files:**
- Create: `crates/irsync/src/ssh/release.rs`
- Modify: `crates/irsync/src/ssh/mod.rs` (add `pub mod release;`),
  `Cargo.toml` (workspace `sha2`), `crates/irsync/Cargo.toml` (`sha2`)
- Create: `crates/irsync/tests/common/release.rs`,
  `crates/irsync/tests/ssh_release.rs`

**Interfaces:**
- Produces, in `release.rs`:

  ```rust
  /// The release target for a server platform, as `parse_uname` names it.
  pub fn target_for(os: Option<&str>, arch: Option<&str>) -> Option<&'static str>

  /// `v<version>` if this build can have a release; `None` for a dev
  /// build: dirty, commit unknown, or built with `netsim`.
  pub fn release_tag(info: &BuildInfo) -> Option<String>

  /// The hex digest `sums` lists for `file_name`, lowercased.
  pub fn expected_sha256(sums: &str, file_name: &str) -> Option<String>

  /// A downloaded, checked and unpacked release binary. The directory is
  /// removed on drop.
  pub struct Fetched { /* tempfile::TempDir */ }
  impl Fetched { pub fn bin(&self) -> &Path }

  /// Download `irsync-<target>.tar.xz` and `sha256.sum` for `tag`, check
  /// the hash, unpack `irsync-<target>/irsync`.
  pub fn fetch(tag: &str, target: &str) -> Result<Fetched>

  /// Test builds only: read `IRSYNC_TEST_RELEASE_URL`.
  #[cfg(feature = "test-hooks")]
  pub fn test_url() -> Option<String>
  ```

  `IRSYNC_TEST_RELEASE_URL`, read only under `test-hooks`, replaces the
  whole `https://…/download/<tag>` prefix and may be a `file://` URL.

- Produces, in `tests/common/release.rs`:

  ```rust
  /// A supported platform that is not this machine's:
  /// (uname text, release target, "os cpu" as messages print it).
  /// Linux x86_64 hosts get ("Linux aarch64", "aarch64-unknown-linux-musl",
  /// "linux aarch64"); all others ("Linux x86_64",
  /// "x86_64-unknown-linux-musl", "linux x86_64").
  pub fn other_platform() -> (&'static str, &'static str, &'static str)

  /// Write `irsync-<target>.tar.xz` (holding a copy of `bin` as
  /// `irsync-<target>/irsync`) and a matching `sha256.sum` into `dir`.
  /// Returns the `file://` URL of `dir`.
  pub fn fake_release(dir: &Path, target: &str, bin: &Path) -> String
  ```

- [ ] **Step 1: Write the failing unit tests** at the bottom of `release.rs`

```rust
#[test]
fn targets() {
    assert_eq!(target_for(Some("linux"), Some("x86_64")), Some("x86_64-unknown-linux-musl"));
    assert_eq!(target_for(Some("linux"), Some("aarch64")), Some("aarch64-unknown-linux-musl"));
    assert_eq!(target_for(Some("macos"), Some("aarch64")), Some("aarch64-apple-darwin"));
    assert_eq!(target_for(Some("macos"), Some("x86_64")), None);
    assert_eq!(target_for(Some("linux"), None), None);
    assert_eq!(target_for(None, Some("x86_64")), None);
}

#[test]
#[cfg(not(feature = "netsim"))]
fn tags() {
    let b = |sha, dirty| BuildInfo::from_parts("0.1.0-beta.1", sha, dirty);
    assert_eq!(release_tag(&b(Some("ae5ebe5"), Some("false"))).as_deref(), Some("v0.1.0-beta.1"));
    assert_eq!(release_tag(&b(Some("ae5ebe5"), None)).as_deref(), Some("v0.1.0-beta.1"));
    assert_eq!(release_tag(&b(Some("ae5ebe5"), Some("true"))), None);
    assert_eq!(release_tag(&b(None, Some("false"))), None);
}

#[test]
#[cfg(feature = "netsim")]
fn a_netsim_build_has_no_release() {
    let b = BuildInfo::from_parts("0.1.0", Some("ae5ebe5"), Some("false"));
    assert_eq!(release_tag(&b), None);
}

#[test]
fn sums() {
    let s = "AAAA *irsync-a.tar.xz\r\nbbbb  irsync-b.tar.xz\ncccc *source.tar.gz\n";
    assert_eq!(expected_sha256(s, "irsync-a.tar.xz").as_deref(), Some("aaaa"));
    assert_eq!(expected_sha256(s, "irsync-b.tar.xz").as_deref(), Some("bbbb"));
    assert_eq!(expected_sha256(s, "irsync-c.tar.xz"), None);
    // A name that is only the tail of another does not match.
    assert_eq!(expected_sha256(s, "a.tar.xz"), None);
}
```

- [ ] **Step 2: Write the failing integration tests** in `tests/ssh_release.rs`

Each test takes a file-wide `Mutex` (as `ssh_client.rs` does with `ENV`),
sets `TMPDIR` to a fresh directory **whose name holds a space**, sets
`IRSYNC_TEST_RELEASE_URL`, and uses `other_platform()`'s target.

```rust
#[test]
fn fetch_unpacks_the_binary() {
    // fake_release(dir, target, CARGO_BIN_EXE_irsync)
    let got = fetch("v0.0.0", target).unwrap();
    assert_eq!(std::fs::read(got.bin()).unwrap(),
               std::fs::read(env!("CARGO_BIN_EXE_irsync")).unwrap());
    let path = got.bin().to_path_buf();
    drop(got);
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(&tmpdir).unwrap().count(), 0);
}

#[test]
fn fetch_refuses_a_damaged_download() {
    // after fake_release: append one byte to the tarball
    let e = fetch("v0.0.0", target).unwrap_err().to_string();
    assert!(e.contains("SHA-256 does not match"), "{e}");
    assert_eq!(std::fs::read_dir(&tmpdir).unwrap().count(), 0);
}

#[test]
fn fetch_refuses_a_tarball_missing_from_the_sums() {
    // after fake_release: overwrite sha256.sum with "abcd *other.tar.xz\n"
    let e = fetch("v0.0.0", target).unwrap_err().to_string();
    assert!(e.contains("sha256.sum does not list"), "{e}");
}

#[test]
fn fetch_reports_a_failed_download() {
    // an empty release directory
    let e = fetch("v0.0.0", target).unwrap_err().to_string();
    assert!(e.contains("could not download"), "{e}");
    assert!(e.contains(&format!("irsync-{target}.tar.xz")), "{e}");
    assert_eq!(std::fs::read_dir(&tmpdir).unwrap().count(), 0);
}

#[test]
fn fetch_refuses_a_tarball_without_the_binary() {
    // tarball built by hand holding only irsync-<target>/README.md,
    // with a correct sha256.sum
    let e = fetch("v0.0.0", target).unwrap_err().to_string();
    assert!(e.contains("does not hold"), "{e}");
}

#[test]
fn fetch_refuses_a_symlink_for_the_binary() {
    // tarball where irsync-<target>/irsync is a symlink to /bin/sh,
    // with a correct sha256.sum
    let e = fetch("v0.0.0", target).unwrap_err().to_string();
    assert!(e.contains("does not hold"), "{e}");
}
```

- [ ] **Step 3: Run them, expect failure**

Run: `cargo test -q -p irsync --all-features --test ssh_release --lib ssh::release`
Expected: does not compile (`release` module missing).

- [ ] **Step 4: Implement `release.rs` and the test helper**

- These tests need `curl` and a `tar` that handles `.tar.xz`.
  `tools/dev-setup.sh` already checks both as required, and
  `book/src/contributing/setup.md` lists them; check that CI's runners
  have them (`.github/workflows/ci.yml`) and add an install step if not.
- `fake_release` builds the tarball with the system `tar -cJf` and writes
  `<hex> *<file name>` using `sha2`.
- `fetch`: a `tempfile::TempDir` (prefix `irsync-release.`); download both
  files; hash the tarball in-process; unpack only the one member with
  `tar -xJf <tarball> -C <dir> irsync-<target>/irsync`; require the result
  to be a regular file (`symlink_metadata`). Arguments go to the tools as
  argv, never through a shell.
- Download: `curl -fsSL --proto =https -o <file> <url>` if `curl` is on
  `PATH`, else `wget -q -O <file> <url>`. With the test URL set, the
  protocol limit becomes `=file,https`. The tools' stderr stays on the
  terminal.
- Errors (the tests pin the quoted parts):
  - neither tool: "need curl or wget on this machine to download the
    release binary"
  - curl exit 22 or wget exit 8: "there is no release {tag} to download
    ({url}); this irsync looks like a development build"
  - any other download failure: "could not download {url}; check the
    network and try again"
  - "the download of {file} is damaged (SHA-256 does not match); try again"
  - "sha256.sum does not list {file}"
  - tar fails: "cannot unpack {file}: tar failed (it must be able to read
    .xz)"
  - "{file} does not hold irsync-{target}/irsync"
- Put the exit-code mapping in a small pure function and unit-test the
  22 / 8 / other cases there.

- [ ] **Step 5: Run the tests, expect pass**

Run: `cargo test -q -p irsync --all-features --test ssh_release --lib ssh::release`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/irsync/Cargo.toml crates/irsync/src/ssh crates/irsync/tests/common/release.rs crates/irsync/tests/ssh_release.rs
git commit -m "Download and check a release binary for another platform

Not used yet. The client fetches the tarball with curl or wget, checks
its SHA-256 and unpacks the irsync binary."
```

---

### Task 3: Install the release binary when the platforms differ

**Files:**
- Modify: `crates/irsync/src/ssh/client.rs` (`Install`, `ensure_installed`)
- Modify: `crates/irsync/src/ssh/onboard.rs` (`summary`, around line 116)
- Test: `crates/irsync/tests/ssh_client.rs`, `crates/irsync/tests/onboard_e2e.rs`,
  the unit tests in `onboard.rs`

**Interfaces:**
- Consumes: `upload`, `Upload` (Task 1); `target_for`, `release_tag`,
  `fetch`, `Fetched::bin`, `test_url` (Task 2); `other_platform`,
  `fake_release` (Task 2).
- Produces:

  ```rust
  pub enum Install {
      Current,
      Copied { replaced: Option<String> },
      /// The release binary for `platform` ("linux aarch64").
      Downloaded { replaced: Option<String>, platform: String },
  }
  ```

- [ ] **Step 1: Write the failing tests**

In `tests/ssh_client.rs`: `fake()` also removes `IRSYNC_TEST_RELEASE_URL`.
Replace `install_refuses_other_platform` with:

```rust
#[test]
fn install_refuses_a_platform_without_a_release() {
    let f = fake(Some("Linux riscv64"));
    let p = probe(&f.rsh, "myserver").unwrap();
    let msg = ensure_installed(&f.rsh, "myserver", &p).unwrap_err().to_string();
    assert!(msg.starts_with("myserver runs linux an unknown CPU, this irsync is "), "{msg}");
    assert!(msg.contains("there is no release binary for that platform"), "{msg}");
    assert!(msg.contains(
        "Install irsync on both machines and set the server up by hand: \
         https://irsync.org/book/tutorial/first-sync.html#set-the-server-up-by-hand"), "{msg}");
    assert!(!f.home.path().join(".local").exists());
}

#[test]
fn install_downloads_for_another_platform() {
    let (uname, target, platform) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let url = release::fake_release(rel.path(), target, Path::new(env!("CARGO_BIN_EXE_irsync")));
    unsafe { std::env::set_var("IRSYNC_TEST_RELEASE_URL", &url) }; // serialized by ENV
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p).unwrap();
    assert_eq!(r, Install::Downloaded { replaced: None, platform: platform.into() });
    assert_eq!(std::fs::read(f.home.path().join(REMOTE_BIN)).unwrap(),
               std::fs::read(env!("CARGO_BIN_EXE_irsync")).unwrap());
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(ensure_installed(&f.rsh, "myserver", &p).unwrap(), Install::Current);
}

#[test]
fn a_release_that_is_not_this_build_is_not_installed() {
    let (uname, target, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let other = fake_exe(&f, Some("irsync 9.9.9 (other)"));
    let url = release::fake_release(rel.path(), target, &other);
    unsafe { std::env::set_var("IRSYNC_TEST_RELEASE_URL", &url) };
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err().to_string();
    assert!(e.contains("development build"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_failed_download_changes_nothing() {
    let (uname, _, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap(); // empty
    unsafe { std::env::set_var("IRSYNC_TEST_RELEASE_URL", format!("file://{}", rel.path().display())) };
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err().to_string();
    assert!(e.contains("could not download"), "{e}");
    assert!(!f.home.path().join(".local").exists());
}
```

In `tests/onboard_e2e.rs`: `World` gains `release_url: Option<String>`,
passed as `IRSYNC_TEST_RELEASE_URL` (removed from the env when `None`).
In `other_platform_changes_nothing`, replace the "so it cannot copy itself
there" assertion with `err.contains("there is no release binary for that
platform")`. Add:

```rust
#[test]
fn other_platform_gets_the_release_binary() {
    let (uname, target, platform) = release::other_platform();
    let mut w = World::new();
    let rel = tempfile::tempdir().unwrap();
    w.uname = Some(uname.into());
    w.release_url = Some(release::fake_release(
        rel.path(), target, Path::new(env!("CARGO_BIN_EXE_irsync"))));
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = ok(&o); // if `ok` returns stdout, read stderr from `o` instead
    assert!(err.contains("irsync: downloading irsync v"), "{err}");
    assert!(err.contains(&format!("(release binary for {platform})")), "{err}");
    same_tree(w.src.path(), &dest);
}
```

In `onboard.rs` unit tests, next to `replaced_restarted_on_macos`:

```rust
#[test]
fn downloaded_says_which_platform() {
    let o = onboarded(
        Install::Downloaded { replaced: None, platform: "linux aarch64".into() },
        DaemonOutcome::Started, Some("linux"));
    let s = summary(&o);
    assert!(s.iter().any(|l| l == &format!(
        "  installed irsync {} at ~/{REMOTE_BIN} (release binary for linux aarch64)",
        *LONG_VERSION)), "{s:?}");
}
```

- [ ] **Step 2: Run them, expect failure**

Run: `cargo test -q -p irsync --all-features --test ssh_client --test onboard_e2e --lib ssh::onboard`
Expected: does not compile (`Install::Downloaded` missing).

- [ ] **Step 3: Implement**

In `ensure_installed`, when the server's OS or CPU differs:

1. `target_for(probe.os, probe.arch)` is `None` → the "by hand" error,
   worded "…, this irsync is {os} {arch}, and there is no release binary
   for that platform. Install irsync on both machines and set the server
   up by hand: <URL>".
2. Unless the test URL is set: `release_tag(&BuildInfo::current())` is
   `None` → "{dest} runs {os} {arch}, this irsync is {my_os} {my_arch} and
   a development build ({LONG_VERSION}). Only release builds install on
   another OS or CPU. Install irsync on both machines and set the server
   up by hand: <URL>". With the test URL set, the tag is
   `v<CARGO_PKG_VERSION>` regardless.
3. `eprintln!("irsync: downloading irsync {tag} for {os} {arch}")`.
4. `fetch(&tag, target)`, then `upload(rsh, dest, fetched.bin())`.
5. `Upload::NotThisVersion` → "release {tag} is not this build
   ({LONG_VERSION}): this irsync is a development build with a release's
   version number, so it cannot install on {dest}. Nothing was changed
   there." `Upload::Short` → the existing "cut short" error.
6. `Ok(Install::Downloaded { replaced: probe.installed.clone(), platform })`.

In `summary`, `Downloaded` prints the `Copied` lines with
` (release binary for {platform})` appended.

- [ ] **Step 4: Run the tests, expect pass**

Run: `cargo test -q -p irsync --all-features --test ssh_client --test ssh_release --test onboard_e2e --lib ssh`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/ssh crates/irsync/tests
git commit -m "Install the release binary on a server with another OS or CPU

A release build downloads the tarball for its own tag, checks it and
copies the binary over ssh. Dev builds still need the same platform."
```

---

### Task 4: Docs and TODO

**Files:**
- Modify: `TODO.md`, `docs/IRSYNC_DECISIONS.md` (R44, around line 337),
  `book/src/introduction.md:73-76`, `book/src/tutorial/first-sync.md:45-47`

No tests: a Markdown-only change. Build the book once (`mdbook build book`)
to catch a broken link.

- [ ] **Step 1: `TODO.md`**

- Add as the first section:

  ```markdown
  # Critical

  - [ ] Sign release files and verify them against a public key built into
        irsync. Today a release download is checked only by SHA-256 over
        HTTPS, which catches a damaged file but not a swapped one. Applies
        to the cross-platform install over ssh (`ssh/release.rs`) and to
        `site/install.sh`.
  ```

- Under "Auto-update irsync on the remote side": remove the "Other OS or
  CPU" sub-item; keep the reachable-older-daemon one.
- Remove the "Later item from the spec" section.
- Remove the "A copied binary with the same OS/CPU may still not run" item.
  Add in its place: "A same-platform copy that does not run could fall back
  to the release binary (static on Linux)."

- [ ] **Step 2: Decision record R44**

Replace the sentence that says other OS or CPU is open with: a release
build downloads the release binary for its own tag on the client, checks
its SHA-256 and copies it over; dev builds need the same OS and CPU; the
server checks the binary's version before replacing anything; signatures
are open (`TODO.md`). Add "Amended 2026-11-14" and the spec's path.

- [ ] **Step 3: The book**

Both pages: say that a released irsync also installs on a server with
another supported OS or CPU (it downloads the matching release on the
client, which needs `curl` or `wget`, and `tar`), that a build from source
needs the same OS and CPU, and that the by-hand setup remains for the rest
(for example an Intel Mac server). Keep the link to
`#set-the-server-up-by-hand`.

- [ ] **Step 4: Full regression**

Run the five commands under "Full regression" in `AGENTS.md`.
Expected: all pass, no clippy or fmt output.

- [ ] **Step 5: Commit**

```bash
git add TODO.md docs/IRSYNC_DECISIONS.md book/src
git commit -m "Document the cross-platform install and the open signature check"
```
