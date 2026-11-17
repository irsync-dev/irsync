# The ssh install matches install.sh: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A first sync over ssh leaves the server as `install.sh` would: `irsync`, `fileset` and the man pages under `~/.local`, with the same `PATH` advice.

**Architecture:** `irsync-installer.sh` gets a `--from DIR` mode and a login-shell `PATH` probe. The irsync binary carries the installer script. The client stages the files (release tarball, or its own files), streams them and the script to the server as a plain tar in one ssh call, and the server checks the version and runs the script. `ssh-setup` on the server deals with the old `~/.local/share/irsync/bin`.

**Tech Stack:** POSIX sh (must pass under dash and BusyBox sh), Rust, the system `tar`, Python `unittest` for the installer tests.

**Spec:** `docs/superpowers/specs/2026-11-17-ssh-install-matches-installer-design.md`. Read it first; this plan argues from it.

## Global Constraints

- The installer never edits shell startup files. `ssh-setup` never edits systemd or launchd files.
- The server needs no internet, no `xz`, no `sha256sum`, no `curl`: only `sh`, plain `tar`, `cat`, `wc`, `mktemp`.
- Nothing is installed or replaced on the server before the uploaded `irsync --version` printed exactly `irsync {LONG_VERSION}`.
- The installer's download mode (flags, checksum, `--uninstall`, messages) does not change, apart from the `PATH` check of Task 2.
- New path: `~/.local/bin/irsync`. Old path: `~/.local/share/irsync/bin/irsync`. The two service files: `~/.config/systemd/user/irsync.service`, `~/Library/LaunchAgents/org.irsync.daemon.plist`.
- User-facing text: short plain sentences, as in the existing messages. Every installer line starts with `irsync: `.
- Commits: summary line, blank line, short reason. No attribution trailers of any kind.
- Before the last commit of each Rust task: `cargo fmt --all` and `cargo clippy --workspace --all-features -- -D warnings`.
- Installer tests: `python3 -m unittest discover -s packaging/tests`. Rust tests: `cargo test -q --workspace --all-features --no-fail-fast`. Before the final commit: `tools/ci.sh check`.

## Review Focus

Conditions the spec implies but does not spell out. Each has a test in the task named.

1. **A login shell whose startup file hangs or is slow** (starts tmux, waits for input). The installer must not hang: the probe is cut off after 5 seconds and counts as failed. (Task 2)
2. **A server home directory with a space in it.** The install works. (Task 5)
3. **A server without `tar`, or a home where the work directory cannot be made.** A clear message, nothing changed, no leftover directory. (Task 5)
4. **The old daemon's recorded path reaches the old binary through a symlink** (a symlinked home). It is still recognised as the old daemon. (Task 7)
5. **A service file that exists but cannot be read.** Treated as pointing at the old binary: warn, keep the binary, leave the daemon. (Task 7)

## File Structure

| File | Role |
|---|---|
| `packaging/installer/irsync-installer.sh.in` | Modify: `--from DIR`, login-shell probe, nushell advice |
| `packaging/tests/test_installer.py` | Modify: tests for both |
| `crates/irsync/src/ssh/installer.rs` | Create: the built-in installer script |
| `crates/irsync/src/ssh/payload.rs` | Create: stage the files to send and pack them |
| `crates/irsync/src/ssh/release.rs` | Modify: `fetch` unpacks the whole tarball |
| `crates/irsync/src/ssh/client.rs` | Modify: new path, new upload, fallback, `--no-install` |
| `crates/irsync/src/ssh/legacy.rs` | Create: the old directory and the service files |
| `crates/irsync/src/ssh/setup.rs` | Modify: adopt the old daemon, report lines |
| `crates/irsync/src/ssh/onboard.rs` | Modify: flags struct, summary, snippets |
| `crates/irsync/src/cli/mod.rs`, `src/main.rs`, `src/config/settings.rs`, `src/config/file.rs` | Modify: `--no-install`, `[client] ssh_install` |
| `crates/irsync/tests/ssh_client.rs`, `tests/onboard_e2e.rs` | Modify |
| `book/src/…`, `docs/IRSYNC_DECISIONS.md`, `TODO.md` | Modify |

---

### Task 1: Installer `--from DIR`

**Files:**
- Modify: `packaging/installer/irsync-installer.sh.in`
- Test: `packaging/tests/test_installer.py`

**Interfaces:**
- Produces: `sh irsync-installer.sh --from DIR`. `DIR` holds `irsync` (required) and optionally `fileset`, `man/man1/irsync.1`, `man/man1/fileset.1`, `man/man5/irsync.toml.5`. Exit 0 on success, 1 on failure, 2 on bad usage. Later tasks call exactly this.

- [ ] **Step 1: Write the failing tests** in a new class `FromDirTests(Base)`. Add a helper `make_dir(self, names)` that writes the named files under `self.tmp/from`, and run the installer with `self.install("--from", d, archives=False)`.

```python
ALL = ["irsync", "fileset", "man/man1/irsync.1", "man/man1/fileset.1", "man/man5/irsync.toml.5"]

def test_from_installs_five_files(self):
    r = self.install("--from", self.make_dir(ALL), archives=False)
    self.assertEqual(r.returncode, 0, r.stderr)
    self.assertEqual(self.under_home(), {
        ".local/bin/irsync", ".local/bin/fileset",
        ".local/share/man/man1/irsync.1", ".local/share/man/man1/fileset.1",
        ".local/share/man/man5/irsync.toml.5"})
    self.assertTrue(os.access(os.path.join(self.home, ".local/bin/irsync"), os.X_OK))

def test_from_irsync_only(self):
    r = self.install("--from", self.make_dir(["irsync"]), archives=False)
    self.assertEqual(r.returncode, 0, r.stderr)
    self.assertEqual(self.under_home(), {".local/bin/irsync"})
    self.assertIn("fileset", r.stderr)          # says what it skipped
    self.assertIn("manual pages", r.stderr)

def test_from_some_pages_installs_no_pages(self):
    r = self.install("--from", self.make_dir(["irsync", "man/man1/irsync.1"]), archives=False)
    self.assertEqual(self.under_home(), {".local/bin/irsync"})

def test_from_without_irsync_changes_nothing(self):
    r = self.install("--from", self.make_dir(["fileset"]), archives=False)
    self.assertEqual(r.returncode, 1)
    self.assertEqual(self.under_home(), set())

def test_from_needs_no_tools_and_no_platform(self):
    # No curl, wget, tar, sha256sum on PATH, and an unsupported platform.
    r = self.install("--from", self.make_dir(ALL), archives=False,
                     uname=("SunOS", "sparc"), bindir=self.make_bindir("bare", sums=()))
    self.assertEqual(r.returncode, 0, r.stderr)

def test_from_usage_errors(self):
    for args in (["--from"], ["--from", "/nonexistent"], ["--from", self.make_dir(ALL), "--uninstall"]):
        self.assertEqual(self.install(*args, archives=False).returncode,
                         1 if args == ["--from", "/nonexistent"] else 2, args)

```

Also `test_from_unwritable_bin_changes_nothing` in `UnwritableTests`: the setup of `test_unwritable_bin_changes_nothing`, run with `--from`; exit 1 and `self.snapshot()` is the same before and after.

`make_bindir("bare", …)` must also leave out `curl`, `wget`, `tar`: add a `without=()` parameter to `make_bindir` for that.

- [ ] **Step 2: Run** `python3 -m unittest discover -s packaging/tests -k FromDir`. Expected: failures, `--from` gives usage error 2.

- [ ] **Step 3: Implement in the template.**
  - Argument loop: `--from` takes the next argument as `DIR` (the loop becomes a `while [ $# -gt 0 ]` with `shift`). Missing value, or combined with `--uninstall` or `--help`: usage, exit 2. Usage text gains `[--from DIR]`; help text gains one sentence: "`--from DIR` installs from an unpacked release directory instead of downloading."
  - With `--from`: skip the platform `case`, the checksum-tool check, the wget check, the download, the checksum and `tar`. Set `src=$DIR`. `[ -f "$src/irsync" ]` else die `"$DIR has no irsync; nothing was installed"`.
  - `os=$(uname -s)` still runs in both modes (the `PATH` advice needs it).
  - The five-file requirement stays for the download mode. In `--from` mode: `fileset` is placed if present; the pages are placed only if all three are present and the man directories are usable.
  - Closing lines in `--from` mode name what was skipped: `installed without fileset: DIR has none`, `installed without the manual pages: DIR has none`.
  - Header comment: document `--from`.

- [ ] **Step 4: Run** `python3 -m unittest discover -s packaging/tests`. Expected: all pass, including the old tests unchanged.

- [ ] **Step 5: Commit** `Let the installer install from an unpacked directory`.

---

### Task 2: Login-shell `PATH` probe and nushell advice

**Files:**
- Modify: `packaging/installer/irsync-installer.sh.in` (the `on_path` block, lines 241-288 today)
- Test: `packaging/tests/test_installer.py`

**Interfaces:**
- Produces: no new interface. Advice is printed only when neither the installer's own `PATH` nor the login shell finds `$HOME/.local/bin/irsync`.

- [ ] **Step 1: Write the failing tests.** Add a helper `fake_shell(self, name, body)` that writes an executable `self.tmp/shells/<name>` running `body` and logging its arguments to `self.log`. Pass it as `env={"SHELL": path}`. The installed path is `f"{self.home}/.local/bin/irsync"`.

```python
def test_login_shell_finds_it_no_advice(self):
    sh = self.fake_shell("zsh", f'echo banner; echo "{self.home}/.local/bin/irsync"')
    r = self.install(env={"SHELL": sh})
    self.assertNotIn("not on your PATH", r.stderr)
    self.assertIn("-l -i -c command -v irsync", open(self.log).read())

def test_login_shell_misses_it_advice(self):
    for body in ("exit 1", "echo /usr/bin/irsync", "echo 'irsync: aliased to x'"):
        r = self.install(env={"SHELL": self.fake_shell("zsh", body)})
        self.assertIn("not on your PATH", r.stderr, body)
        self.assertIn("~/.zshrc", r.stderr, body)

def test_own_path_is_enough(self):
    # The shell misses it, but the installer's PATH has ~/.local/bin.
    r = self.install(env={"SHELL": self.fake_shell("zsh", "exit 1"),
                          "PATH": f"{self.bindir}:{self.home}/.local/bin"})
    self.assertNotIn("not on your PATH", r.stderr)

def test_slow_login_shell_is_cut_off(self):
    start = time.monotonic()
    r = self.install(env={"SHELL": self.fake_shell("bash", "sleep 60")})
    self.assertLess(time.monotonic() - start, 20)
    self.assertEqual(r.returncode, 0)
    self.assertIn("not on your PATH", r.stderr)

def test_nushell_probe_and_advice(self):
    sh = self.fake_shell("nu", "exit 1")
    r = self.install(env={"SHELL": sh})
    self.assertIn("-l -c which irsync | get path.0", open(self.log).read())
    self.assertIn('$env.PATH = ($env.PATH | prepend ($env.HOME | path join ".local" "bin"))', r.stderr)
    self.assertIn("$nu.config-path", r.stderr)

def test_csh_and_unknown_shells_are_not_run(self):
    for name in ("tcsh", "xonsh"):
        self.install(env={"SHELL": self.fake_shell(name, "exit 0")})
    self.assertNotIn("shells/", open(self.log).read() if os.path.exists(self.log) else "")

def test_missing_shell_binary_falls_back(self):
    r = self.install(env={"SHELL": "/nonexistent/zsh"})
    self.assertEqual(r.returncode, 0)
    self.assertIn("not on your PATH", r.stderr)
```

Existing tests that set `SHELL` to a bare name (`test_path_hint_names_the_shells_startup_file`) keep passing: a shell that cannot be run counts as "probe failed".

- [ ] **Step 2: Run** `python3 -m unittest discover -s packaging/tests -k login -k nushell -k slow -k own_path -k csh -k missing_shell`. Expected: FAIL.

- [ ] **Step 3: Implement** `login_finds()` in the template, called only when the installer's own `PATH` check missed:
  - By `${SHELL##*/}`: `sh dash ash ksh ksh93 mksh bash zsh fish` run `"$SHELL" -l -i -c 'command -v irsync'`; `nu` runs `"$SHELL" -l -c 'which irsync | get path.0'`; anything else, or `SHELL` unset or not executable: return 1 without running it.
  - stdin `/dev/null`, stderr dropped, stdout to a file in `$tmp` (in `--from` mode `$tmp` must exist too: create the work directory in both modes).
  - 5-second cap, POSIX only: run the shell in the background, a second background job sleeps 5 and kills it, `wait` for the shell, then kill the sleeper. A killed or failed probe returns 1.
  - Found only if the last line of stdout equals `$bin/irsync` exactly.
  - Nushell advice, a new `nu)` arm: `$missing put this line in your config.nu (nu prints its place with: $nu.config-path):` then the line asserted above.
  - Everything else in the block stays, including the Linux "log out and in again" wording.

- [ ] **Step 4: Run** the whole installer suite. Then add the new probe cases to the dash run: `ShellTests.test_posix_sh` shows how the suite runs the installer under dash (`make_bindir(..., shell=...)`); run `test_login_shell_finds_it_no_advice` and `test_slow_login_shell_is_cut_off` that way too. Expected: PASS.

- [ ] **Step 5: Commit** `Ask the login shell whether it finds irsync`.

---

### Task 3: The built-in installer script

**Files:**
- Create: `crates/irsync/src/ssh/installer.rs`
- Modify: `crates/irsync/src/ssh/mod.rs` (add `pub mod installer;`)

**Interfaces:**
- Produces: `pub fn script() -> String`: the template with every placeholder filled.

- [ ] **Step 1: Write the failing tests** (unit tests in `installer.rs`):

```rust
#[test]
fn no_placeholder_is_left() {
    let s = script();
    assert!(!s.contains("@VERSION@") && !s.contains("@BASE_URL@") && !s.contains("@SHA256_"));
}
#[test]
fn names_this_version() {
    assert!(script().contains(&format!("version='{}'", env!("CARGO_PKG_VERSION"))));
}
#[test]
fn download_mode_can_never_pass_the_checksum() {
    // Not hex, so no real digest equals it.
    assert_eq!(script().matches("sha=built-in-copy-has-no-checksum").count(), 3);
}
#[test]
fn is_valid_sh() {
    // Write script() to a temp file; `sh -n FILE` exits 0.
}
```

- [ ] **Step 2: Run** `cargo test -p irsync --lib ssh::installer`. Expected: does not compile.

- [ ] **Step 3: Implement** `script()`: `include_str!("../../../../packaging/installer/irsync-installer.sh.in")`, then replace `@VERSION@` with `CARGO_PKG_VERSION`, `@BASE_URL@` with `https://github.com/irsync-dev/irsync/releases/download/v{version}`, and the three `@SHA256_…@` placeholders with `built-in-copy-has-no-checksum`. The module doc says why the checksums are a non-value. (The path leaves the crate directory. That is fine for git and dist builds; it would have to change before a `cargo publish`. Say so in the doc comment.)

- [ ] **Step 4: Run** the tests. Expected: PASS.

- [ ] **Step 5: Commit** `Build the installer script into irsync`.

---

### Task 4: Stage and pack the files to send

**Files:**
- Create: `crates/irsync/src/ssh/payload.rs`
- Modify: `crates/irsync/src/ssh/release.rs`, `crates/irsync/src/ssh/mod.rs`
- Test: unit tests in `payload.rs`; `crates/irsync/tests/common/release.rs` users keep passing

**Interfaces:**
- Consumes: `installer::script()` (Task 3).
- Produces:

```rust
// release.rs — replaces `Fetched::bin()`
impl Fetched { pub fn dir(&self) -> &Path }   // the unpacked irsync-<target>/ directory
// `fetch` now unpacks the whole tarball; it fails unless dir()/irsync is a regular file.

// payload.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contents { pub fileset: bool, pub man: bool }

/// The files to install, in the tarball's layout, at `dir`.
pub struct Staged { /* dir + what owns it */ }
impl Staged {
    pub fn from_release(fetched: release::Fetched) -> Staged;
    /// This machine's files: `exe` as `irsync`; `fileset` beside it if a regular
    /// file; the three man pages from `<exe dir>/man`, else `<exe dir>/../share/man`,
    /// all three or none.
    pub fn from_local(exe: &Path) -> anyhow::Result<Staged>;
    pub fn contents(&self) -> Contents;
    /// A plain tar holding `files/…` (the staged files) and `installer.sh`.
    pub fn pack(&self) -> anyhow::Result<Packed>;
}
pub struct Packed { /* temp file */ }
impl Packed { pub fn path(&self) -> &Path; pub fn len(&self) -> u64; }
```

- [ ] **Step 1: Write the failing tests** in `payload.rs`. Each builds a temp tree and unpacks the result with the system `tar -xf` to look inside.

```rust
#[test] fn local_alone()            // exe only -> Contents{fileset:false, man:false}; tar lists files/irsync, installer.sh
#[test] fn local_with_fileset()     // fileset beside exe -> fileset:true; files/fileset is in the tar
#[test] fn local_pages_beside()     // <dir>/man/man1/irsync.1, man1/fileset.1, man5/irsync.toml.5 -> man:true
#[test] fn local_pages_in_share()   // <dir>/../share/man/... -> man:true
#[test] fn local_two_pages_is_none()// one page missing -> man:false, no man/ in the tar
#[test] fn exe_with_another_name()  // exe named "irsync-abc123" still lands as files/irsync
#[test] fn packed_len_is_file_size()// Packed::len() == metadata(path).len()
#[test] fn packed_irsync_is_the_file_not_a_link() // unpacked files/irsync is a regular file with exe's bytes, mode has 0o111
```

- [ ] **Step 2: Run** `cargo test -p irsync --lib ssh::payload`. Expected: does not compile.

- [ ] **Step 3: Implement.**
  - `release::fetch`: unpack without naming a member; keep the "is a regular file, not a symlink" check on `irsync`, and the two error messages.
  - `from_local`: a `tempfile::TempDir` with symlinks to the real files under the tarball's names.
  - `pack`: write `installer.sh` into a second temp dir, then run the system `tar -chf OUT -C <stage parent> files -C <script dir> installer.sh` (`-h` follows the symlinks; both GNU tar and bsdtar take it). For `from_release` the staged directory is the unpacked one; arrange the names so the tar member prefix is `files/` in both cases (a symlink named `files` pointing at it is enough).
  - On macOS set `COPYFILE_DISABLE=1` for the `tar` call so no `._` entries are written.

- [ ] **Step 4: Run** `cargo test -p irsync --lib ssh::` . Expected: PASS. (`client.rs` still calls `Fetched::bin()`: keep a private shim or adjust the call to `dir().join("irsync")` so the crate compiles; Task 5 rewrites that code.)

- [ ] **Step 5: Commit** `Stage and pack what the ssh install sends`.

---

### Task 5: Install through the installer, at the new path

**Files:**
- Modify: `crates/irsync/src/ssh/client.rs`, `crates/irsync/src/ssh/onboard.rs` (only what no longer compiles)
- Test: `crates/irsync/tests/ssh_client.rs`, `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `payload::{Staged, Packed, Contents}` (Task 4).
- Produces:

```rust
pub const REMOTE_BIN: &str = ".local/bin/irsync";
pub const OLD_REMOTE_BIN: &str = ".local/share/irsync/bin/irsync";

pub enum Install {
    Current,
    Copied { replaced: Option<String>, contents: Contents },
    Downloaded { replaced: Option<String>, platform: String, contents: Contents },
}
// signature unchanged in this task:
pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe) -> Result<Install>
```

- [ ] **Step 1: Update and add tests in `ssh_client.rs`.** Existing tests: every `REMOTE_BIN` assertion now means `~/.local/bin/irsync`; `Install::Copied { replaced: None }` gains `contents`. New tests:

```rust
#[test] fn install_lands_in_local_bin_with_no_leftovers() {
    // after ensure_installed: ~/.local/bin/irsync is the test binary, executable;
    // no entry in the fake home starts with ".irsync-install"; ~/.local/share/irsync does not exist.
}
#[test] fn install_brings_fileset_and_pages_when_beside_the_exe() {
    // set_exe_override to a temp dir holding a copy of the binary, a `fileset` file and man/…;
    // -> Copied{contents: Contents{fileset:true, man:true}}; the four extra files exist under ~/.local.
}
#[test] fn release_install_brings_everything() {
    // other_platform() + release::pack with irsync (a script printing the right version),
    // fileset and man pages -> Downloaded{contents: {true,true}}; files under ~/.local.
}
#[test] fn same_platform_release_build_downloads_first() {
    // IRSYNC_TEST_RELEASE_URL set, uname = this platform, a full fake release for this target
    // -> Install::Downloaded.
}
#[test] fn same_platform_falls_back_when_the_download_fails() {
    // IRSYNC_TEST_RELEASE_URL points at an empty dir -> Install::Copied; stderr line not checked here.
}
#[test] fn same_platform_falls_back_when_the_release_is_another_build() {
    // fake release whose irsync prints "irsync 0.0.0 (other)" -> Install::Copied, ~/.local/bin/irsync is ours.
}
#[test] fn other_platform_has_no_fallback() {
    // other_platform(), release irsync prints another version -> Err containing "is not this build";
    // fake home has no .local and no .irsync-install*.
}
#[test] fn home_with_a_space() {
    // FAKE_SSH_HOME = <tmp>/my home -> install succeeds.
}
#[test] fn server_without_tar_changes_nothing() {
    // A fake-ssh PATH without tar (extend the fixture: FAKE_SSH_HIDE=tar puts a failing
    // `tar` first on PATH) -> Err containing "could not unpack"; nothing under the fake home.
}
```

  The existing three rejection tests (cut short, another version, does not run) keep their messages and now also assert no `.irsync-install*` entry and no `.local` in a fresh home.

- [ ] **Step 2: Run** `cargo test -p irsync --features test-hooks --test ssh_client`. Expected: FAIL.

- [ ] **Step 3: Implement.**
  - The probe reads `"$HOME/{REMOTE_BIN}" --version` at the new path.
  - `upload` becomes `fn install_remote(rsh, dest, packed: &Packed) -> Result<Upload>`. One remote command, with stdin the packed tar:

    ```sh
    w=$(mktemp -d "$HOME/.irsync-install.XXXXXX") || exit 7
    trap 'rm -rf "$w"' EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM
    cat > "$w/in.tar" || exit 7
    [ "$(wc -c < "$w/in.tar")" -eq LEN ] || exit 3
    tar -xf "$w/in.tar" -C "$w" || exit 7
    v=$("$w/files/irsync" --version 2>/dev/null | head -n 1)
    [ -n "$v" ] || exit 5
    [ "$v" = "WANT" ] || exit 4
    sh "$w/installer.sh" --from "$w/files" || exit 6
    ```

    Exit codes: 0 `Done`, 3 `Short`, 4 `NotThisVersion`, 5 `DoesNotRun`, new 6 `InstallerFailed`, new 7 `CannotUnpack`. Messages for the new two: `"the installer failed on {dest}; see its message above"` and `"could not unpack irsync on {dest} (it needs tar and a writable home directory). Nothing was changed there."` ssh's stderr is inherited, so the installer's lines reach the terminal.
  - `ensure_installed`:
    1. Version at `REMOTE_BIN` is ours: `Current`.
    2. `release::target_for(server)` is `Some` and `release_tag()` is `Some`: fetch, stage, install → `Downloaded`. If the server is this OS and CPU and either the fetch failed or the result was `NotThisVersion`: print `irsync: no release files for this build ({reason}); installing this machine's own files` and go to 3. Otherwise today's errors.
    3. Server is this OS and CPU: `Staged::from_local(exe_path())` → `Copied`.
    4. Else today's two "no release binary" / "development build" errors.
  - Fix `onboard.rs` so it compiles (the summary wording is Task 8).

- [ ] **Step 4: Run** `cargo test -q -p irsync --all-features --no-fail-fast`. Fix `onboard_e2e.rs` expectations that name the old path. Expected: PASS.

- [ ] **Step 5: Commit** `Install over ssh through the installer, into ~/.local/bin`.

---

### Task 6: `--no-install` and `[client] ssh_install`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (next to `no_remote_dht`, both structs and the mapping near line 1048), `crates/irsync/src/main.rs` (both `onboard` call sites), `crates/irsync/src/config/settings.rs` (`ClientSettings`), `crates/irsync/src/config/file.rs`, `crates/irsync/src/ssh/onboard.rs`, `crates/irsync/src/ssh/client.rs`
- Modify: `book/src/reference/command-line.md`, `book/src/reference/configuration.md`
- Test: `crates/irsync/tests/ssh_client.rs`, `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Produces:

```rust
// onboard.rs — replaces the three trailing bools of `onboard` and of `client::run_setup`
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupFlags { pub recheck: bool, pub no_dht: bool, pub no_install: bool }
pub fn onboard(paths: &Paths, rsh: &Rsh, alias: &str, dest: &str, client: EndpointId, flags: SetupFlags) -> anyhow::Result<Onboarded>
// client.rs
pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe, no_install: bool) -> Result<Install>
pub fn run_setup(rsh: &Rsh, dest: &str, client: EndpointId, flags: SetupFlags) -> Result<SetupReport>
// settings.rs
pub struct ClientSettings { /* … */ pub ssh_install: Option<bool> }   // [client] ssh_install, default true
```

  `no_install = opts.no_install || settings.client.ssh_install == Some(false)`. The setting lives in `[client]` only, not per host. `--no-install` does not force a run through ssh.

- [ ] **Step 1: Write the failing tests.**

```rust
// ssh_client.rs
#[test] fn no_install_with_the_same_build_is_current()      // install once, then no_install=true -> Install::Current
#[test] fn no_install_with_nothing_there_fails() {
    // fresh home, no_install=true -> Err; message contains "--no-install" and "~/.local/bin/irsync"
    // and "Nothing was changed"; fake home has no .local and no .irsync-install*.
}
#[test] fn no_install_with_another_version_fails()          // a script at ~/.local/bin/irsync printing "irsync 0.0.1 (old)"; file unchanged after
// onboard_e2e.rs
#[test] fn no_install_flag_stops_a_first_push()             // `--no-install` on a fresh server: exit != 0, no alias saved
#[test] fn ssh_install_false_in_config_does_the_same()      // [client] ssh_install = false
```

- [ ] **Step 2: Run** the two test files. Expected: FAIL (no such flag).

- [ ] **Step 3: Implement.** Message: `--no-install: {dest} has no irsync {LONG_VERSION} at ~/.local/bin/irsync ({found}). Install that version there, or run without --no-install. Nothing was changed.` where `{found}` is `found none` or `found {version line}`. With `no_install`, `run_setup` adds `--keep-old` to the `ssh-setup` command line (Task 7 gives it meaning; add the hidden CLI flag on `SshSetup` now, unused).

- [ ] **Step 4: Document.** `command-line.md`: a row after `--no-remote-dht`: "`--no-install` | When going through ssh, copy nothing to the server: use the irsync already at `~/.local/bin/irsync` there, and stop if it is missing or another version. Default: `ssh_install` in the config file, else install." `configuration.md`: `ssh_install = true` in the `[client]` sample and a table row. Run `python3 -m unittest discover -s tools/tests`.

- [ ] **Step 5: Run** `cargo test -q -p irsync --all-features --no-fail-fast`. Expected: PASS.

- [ ] **Step 6: Commit** `Add --no-install and the ssh_install setting`.

---

### Task 7: The old directory, on the server

**Files:**
- Create: `crates/irsync/src/ssh/legacy.rs`
- Modify: `crates/irsync/src/ssh/setup.rs` (the daemon step at lines 317-345, `SetupReport`, `to_lines`, `parse`), `crates/irsync/src/cli/mod.rs` and `main.rs` (`ssh-setup --keep-old`)
- Test: unit tests in `legacy.rs` and `setup.rs`; `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `client::OLD_REMOTE_BIN` (Task 5), `--keep-old` (Task 6).
- Produces:

```rust
// legacy.rs
/// The service files under `home` that mention ".local/share/irsync/bin",
/// or that exist and cannot be read.
pub fn stale_services(home: &Path) -> Vec<PathBuf>;
/// Whether `exe` is the old binary under `home`, comparing canonical paths
/// when both resolve, else the paths as written.
pub fn is_old_bin(home: &Path, exe: &Path) -> bool;
/// Remove the old binary, then its `bin` directory if empty. Missing is fine.
pub fn remove_old_bin(home: &Path) -> std::io::Result<bool>;   // true: something was removed

// setup.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OldBin { Absent, Kept, Removed }
pub struct SetupReport { /* … */ pub stale_services: Vec<PathBuf>, pub old_bin: OldBin }
pub fn run(paths: &Paths, client: EndpointId, recheck: bool, no_dht: bool, keep_old: bool) -> anyhow::Result<SetupReport>
```

  Report lines: one `stale_service=<path>` per file, and `old_bin=absent|kept|removed`. `parse` treats missing lines as none and `absent`.

- [ ] **Step 1: Write the failing tests.**

```rust
// legacy.rs
#[test] fn finds_the_systemd_unit()        // ExecStart=%h/.local/share/irsync/bin/irsync daemon -vv
#[test] fn finds_the_launchd_plist()       // <string>/Users/x/.local/share/irsync/bin/irsync</string>
#[test] fn ignores_a_unit_with_the_new_path()
#[test] fn unreadable_unit_counts_as_stale()   // mode 000 (skip when running as root)
#[test] fn no_files_no_findings()
#[test] fn old_bin_through_a_symlinked_home()  // home is a symlink to real; exe given via real path -> true
#[test] fn another_exe_is_not_the_old_bin()
#[test] fn remove_takes_the_empty_bin_dir_but_not_a_full_one()
// setup.rs
#[test] fn report_round_trips_stale_and_old_bin()
#[test] fn report_without_the_new_lines_parses()   // -> vec![], OldBin::Absent
// onboard_e2e.rs (the fake server is this machine; plant files in the server HOME first)
#[test] fn old_daemon_is_moved_to_the_new_path() {
    // Copy the test binary to <server>/.local/share/irsync/bin/irsync, start its daemon,
    // then push. Summary says restarted; the lock's exe is <server>/.local/bin/irsync;
    // the old binary and its bin directory are gone.
}
#[test] fn service_file_keeps_the_old_binary_and_daemon() {
    // Same, plus a systemd unit naming the old path. The old daemon still holds the lock,
    // the old binary exists, and stderr has a line naming the unit file and ~/.local/bin/irsync.
}
#[test] fn stale_old_binary_without_a_daemon_is_removed()
#[test] fn no_install_keeps_the_old_binary() {
    // New build already at ~/.local/bin, old binary present, `--no-install --ssh`:
    // old binary still there; a daemon that ran from it is restarted from the new path.
}
```

- [ ] **Step 2: Run** `cargo test -p irsync --all-features legacy` and the e2e file. Expected: FAIL.

- [ ] **Step 3: Implement** in `setup::run`, with `home = dirs::home_dir()`:
  1. `stale = legacy::stale_services(home)` before the daemon step.
  2. Daemon step: the "ours" arm also matches when `stale.is_empty() && legacy::is_old_bin(home, &held)`. For that case always stop and start (never `Running` or `Reloaded`), reported as `Restarted { old_version }`. With `stale` non-empty the old daemon falls through to `Foreign`, as today.
  3. After the daemon step: old binary missing → `Absent`; `!stale.is_empty() || keep_old` → `Kept`; else `remove_old_bin` → `Removed`.

- [ ] **Step 4: Run** `cargo test -q -p irsync --all-features --no-fail-fast`. Expected: PASS.

- [ ] **Step 5: Commit** `Move a server off ~/.local/share/irsync/bin`.

---

### Task 8: What the user reads

**Files:**
- Modify: `crates/irsync/src/ssh/onboard.rs` (`summary`, `howto`, their tests)
- Modify: `book/src/how-to/run-the-daemon.md`, `book/src/how-to/ssh-setup.md`, `book/src/tutorial/first-sync.md`, `book/src/reference/files-and-state.md`, `book/src/reference/command-line.md` ("Setup over ssh"), `docs/IRSYNC_DECISIONS.md`, `TODO.md`

**Interfaces:**
- Consumes: `Install` with `Contents` (Task 5), `SetupReport::{stale_services, old_bin}` (Task 7).

- [ ] **Step 1: Update the unit tests in `onboard.rs`** to the new lines, with `v = LONG_VERSION`:

```
  installed irsync {v} in ~/.local/bin, with fileset and the manual pages (release files for linux aarch64)
  installed irsync {v} in ~/.local/bin, with fileset and the manual pages
  installed irsync {v} in ~/.local/bin (no fileset, no manual pages: this machine has none beside its irsync)
  replaced irsync 0.0.1 (old) with {v} in ~/.local/bin, with fileset and the manual pages
```

  Mixed cases read `with fileset (no manual pages: …)` and `with the manual pages (no fileset: …)`. New lines, after the install line:

```
  removed the old copy in ~/.local/share/irsync/bin
  warning: ~/.config/systemd/user/irsync.service still starts irsync from ~/.local/share/irsync/bin.
    That copy is no longer updated. Change the path in that file to ~/.local/bin/irsync, then restart the service.
```

  One warning per stale file, the path shown with `~` for the home part (existing `shell_path` helper). `howto` snippets: `ExecStart=%h/.local/bin/irsync daemon -vv` and `<string>$HOME/.local/bin/irsync</string>`. Add tests: `warns_per_stale_service_file`, `says_when_the_old_copy_was_removed`, `kept_old_copy_says_nothing_extra`.

- [ ] **Step 2: Run** `cargo test -p irsync --lib ssh::onboard`. Expected: FAIL.

- [ ] **Step 3: Implement** the wording in `summary` and `howto`.

- [ ] **Step 4: Update the docs.**
  - The four book chapters: every `~/.local/share/irsync/bin/irsync` becomes `~/.local/bin/irsync`; sample output matches Step 1. `files-and-state.md` says what the ssh setup installs (the five files) and that it is the same as `install.sh`. `run-the-daemon.md` gets a short note: a service file written from an older irsync names `~/.local/share/irsync/bin/irsync`; change it to the new path.
  - `command-line.md`, "Setup over ssh": it installs irsync, fileset and the manual pages; a release build downloads the release files on this machine.
  - `docs/IRSYNC_DECISIONS.md`: amend R44, dated 2026-11-17: the ssh install runs the built-in installer script on files the client sends; lands in `~/.local/bin`; release builds download the tarball for every platform, with the same-platform fallback; the server still needs no internet; `--no-install`; link the spec.
  - `TODO.md`: tick or remove the two "upload command on the server" items now fixed; add: the installer's login-shell probe is not run for csh/tcsh; a service file under another name is not found.

- [ ] **Step 5: Run** `python3 -m unittest discover -s tools/tests` (man pages), then `tools/ci.sh check`. Expected: PASS.

- [ ] **Step 6: Commit** `Describe the new ssh install in the summary and the book`.
