# SSH Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `irsync -av ~/Photos/ myserver:/srv/photos` works when irsync is installed only on the client and `ssh myserver` works: the first run installs, grants and starts irsync on the server over SSH, saves a host alias, and later runs dial the daemon over iroh with no SSH.

**Architecture:** A new hidden server-side subcommand, `irsync ssh-setup`, does every server-side step in one process (identity, grant, daemon start) and prints a key=value report. The client drives it through the system `ssh` (module `ssh/`): probe, copy the binary if needed, run `ssh-setup`, save the alias with the SSH destination. The daemon gets an "immediate" grant (`[immediate]` table) that reaches any path. It turns each new path into an automatic export (`auto = true`), so immediate transfers keep their index, resume and GC. A lock file in the state dir tells whether a daemon is running, and which binary and version started it.

**Tech Stack:** Rust (stable), tokio, iroh, toml_edit, clap; `std::fs::File::try_lock` for the daemon lock; the system `ssh`.

**Spec:** `docs/superpowers/specs/2026-11-10-ssh-onboarding-design.md`

## Global Constraints

- Everything on the server runs as the SSH user. No sudo, no root paths.
- irsync's own copy on the server is `~/.local/share/irsync/bin/irsync`. It never touches any other irsync binary.
- It never installs a systemd unit, launchd agent or cron entry. It only prints a how-to.
- The first run never asks for confirmation. It acts, then reports.
- Only the system `ssh` is used (overridable with `--rsh`/`IRSYNC_RSH`, split on whitespace like rsync's `-e`). irsync has no SSH code of its own.
- An installed copy counts as current only if its `--version` line equals the client's `LONG_VERSION` exactly.
- Copying the client binary happens only when the server's `uname -sm` matches the client (`Linux`→`linux`, `Darwin`→`macos`; `x86_64`/`amd64`→`x86_64`; `aarch64`/`arm64`→`aarch64`). Otherwise it stops with a message to install irsync on the server by hand, and changes nothing.
- A refusal from the daemon (unauthorized, no access) never triggers SSH. Only "never reached" (dial gave up) and "no identity saved for the host" do.
- irsync stops or restarts only a daemon started from its own copy. It never stops any other daemon.
- No backward compatibility: config and protocol may change freely.
- Commit messages: short summary line, blank line, short why. No attribution trailers of any kind.

## Review Focus

1. **Push to a directory that doesn't exist yet** (`myserver:/srv/new`). The parent exists, so the auto export goes on the parent and the receiver creates `new`, as rsync does. Pinned in Task 2 `auto_root_missing_push_uses_parent`.
2. **Pull of a single file** (`myserver:/etc/hostname`). The auto export goes on the parent directory, never on the file itself. Pinned in Task 2 `auto_root_file_uses_parent`.
3. **Second push to a subdirectory of an earlier auto export.** It reuses the outer export and adds no new one. Pinned in Task 2 `immediate_reuses_containing_auto_export`.
4. **`daemon.immediate = false` with an `[immediate]` grant present.** The grant is ignored and the client gets the ordinary no-access error. Pinned in Task 1 `immediate_off_ignores_grant` and Task 2 `immediate_off_rejects_unexported_path`.
5. **Upgraded client against a server running a daemon from irsync's older copy.** Setup replaces the copy, stops that daemon and starts the new one. A daemon started by a different binary is left running and reported. Pinned in Task 4 `setup_restarts_own_stale_daemon` and `setup_leaves_foreign_daemon`.

---

### Task 1: Config schema: immediate grants, `daemon.immediate`, `auto` exports, `hosts.*.ssh`

**Files:**
- Modify: `crates/irsync/src/config/exports.rs` (add `"immediate"` to `RESERVED`, `Export.auto`, `Exports.immediate_rw`, accept top-level `[immediate]` in the shape pass)
- Modify: `crates/irsync/src/config/settings.rs` (`DaemonSettings.immediate: bool`, default `true`)
- Modify: `crates/irsync/src/config/hosts.rs` (`HostEntry.ssh: Option<String>`, `RawHost.ssh`)
- Modify: `crates/irsync/src/authz.rs` (`Grants.immediate`, `Snapshot.immediate_enabled`, `Snapshot::from_files` gains the flag; `is_empty` counts `immediate`)
- Modify: `crates/irsync/src/manage/grants.rs` (grant target `immediate`), `crates/irsync/src/manage/exports.rs` (`add_auto_export`), `crates/irsync/src/manage/hosts.rs` (`HostFlags.ssh`)
- Modify: `crates/irsync/src/main.rs` `run_grant` so `grant add immediate WHO` / `grant remove immediate WHO` work (the word `immediate` works like `all`; read-write only, so `--ro` with `immediate` is a usage error)
- Test: `crates/irsync/tests/config_files.rs`, `crates/irsync/tests/authz_snapshot.rs`, `crates/irsync/tests/manage_grants.rs`, `crates/irsync/tests/manage_exports.rs`, `crates/irsync/tests/manage_hosts.rs`

**Interfaces:**
- Produces:
  - `pub struct Grants { pub exports, pub all, pub immediate: bool }`. `immediate` is true only when the key is in `[immediate].rw` *and* the daemon has `immediate` on.
  - `Snapshot::from_files(hosts: &Hosts, exports: &Exports, immediate_enabled: bool) -> Snapshot`. `FileAuthorizer` passes `config.settings.daemon.immediate`.
  - `pub struct Export { …, pub auto: bool }` (serde default `false`).
  - `pub struct Exports { …, pub immediate_rw: Vec<String> }`.
  - `pub struct HostEntry { …, pub ssh: Option<String> }`.
  - `pub fn grant_immediate(doc: &mut DocumentMut, who: &PeerSpec, resolve: &dyn Fn(&str) -> Option<EndpointId>) -> Result<bool, ManageError>` in `manage/grants.rs`. It returns `false` when the peer was already granted. Same entry-matching rules as `grant_add`.
  - `pub fn add_auto_export(doc: &mut DocumentMut, path: &Path, peer: EndpointId) -> Result<String, ManageError>` in `manage/exports.rs`. It returns the name it picked: `auto-` + path components joined by `-`, every byte outside `[A-Za-z0-9._-]` replaced by `_`, `/` becomes `auto-root`, and `-2`, `-3`, … is appended if the name is taken. It writes `path`, `auto = true` and `rw = ["<peer id>"]`.
  - `HostFlags { …, pub ssh: Option<String> }` writes `ssh = "…"`.

Config shape (copy into the reference docs in Task 7):

```toml
[daemon]
immediate = true          # false: [immediate] grants are ignored, no auto exports

[immediate]
rw = ["<client endpoint id>"]

[exports.auto-srv-photos]
path = "/srv/photos"
auto = true
rw = ["<client endpoint id>"]

[hosts."user@myserver"]
endpoint_id = "<daemon endpoint id>"
ssh = "user@myserver"
```

- [ ] **Step 1: Write the failing tests**
  - `config_files.rs::immediate_table_parses`: a file with `[immediate] rw = [ID]` → `exports.immediate_rw == [ID]`. A file with `[immediate] ro = [ID]` → error mentioning `immediate`.
  - `config_files.rs::daemon_immediate_defaults_on`: an empty file gives `settings.daemon.immediate == true`, and `immediate = false` parses.
  - `config_files.rs::export_named_immediate_is_reserved`: `[exports.immediate]` → error.
  - `authz_snapshot.rs::immediate_grant_reaches`: `from_files(.., true)` → `grants(&id).immediate == true` and `!grants(&id).is_empty()` with no exports.
  - `authz_snapshot.rs::immediate_off_ignores_grant`: `from_files(.., false)` → `grants(&id).immediate == false` and `is_empty()`.
  - `manage_grants.rs::grant_immediate_adds_once`: the first call returns `true`, the second `false`, and the doc has exactly one entry.
  - `manage_exports.rs::auto_export_names`: `/srv/photos` → `auto-srv-photos`; `/` → `auto-root`; `/a b/ç` → `auto-a_b-__` (one `_` per non-allowed byte); a second `/srv/photos` export added under the same name → `auto-srv-photos-2`; the written table has `auto = true` and `rw = [id]`.
  - `manage_hosts.rs::host_add_writes_ssh`: `HostFlags { ssh: Some("u@h".into()), .. }` → `ssh = "u@h"`, read back by `Hosts::parse` as `Some("u@h")`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test config_files --test authz_snapshot --test manage_grants --test manage_exports --test manage_hosts`. They should fail to compile on the missing fields and functions.
- [ ] **Step 3: Implement the fields, parsing, `from_files` flag, `grant_immediate`, `add_auto_export`, `HostFlags.ssh`, and the `grant add|remove immediate` CLI path.** Update every existing `Export { … }` literal (tests' `common/mod.rs` included) with `auto: false`, and every `from_files` caller.
- [ ] **Step 4: Run the tests and see them pass.** Same command, plus `cargo test -p irsync` to catch broken callers.
- [ ] **Step 5: Commit.** `git commit -m "Add immediate grants, auto exports and ssh host entries to the config"`

---

### Task 2: Daemon serves immediate paths through automatic exports

**Files:**
- Create: `crates/irsync/src/autoexport.rs` (root choice, pure)
- Modify: `crates/irsync/src/lib.rs` (`pub mod autoexport;`)
- Modify: `crates/irsync/src/authz.rs` (`Authorizer::add_auto_export`, implemented by `FileAuthorizer` and `MemoryAuthorizer`)
- Modify: `crates/irsync/src/daemon.rs` (`validate` / `handle`: on `Ok(None)` with `grants.immediate`, add the export, take a fresh snapshot, resolve again)
- Modify: `crates/irsync/src/daemon.rs` `run_from_settings`: don't warn "no exports defined" when `[immediate]` has entries
- Test: `crates/irsync/tests/autoexport.rs` (new), `crates/irsync/tests/daemon_auth.rs`, `crates/irsync/tests/sync_e2e.rs`

**Interfaces:**
- Consumes: `Grants.immediate`, `add_auto_export` (Task 1), `resolve::resolve_path`, `resolve::containing`, `ConfigLock`.
- Produces:
  - `pub fn auto_root(abs: &Path, push: bool) -> Result<PathBuf, AutoRootError>` in `autoexport.rs`. An existing directory → `abs`. An existing non-directory → its parent. Missing with `push` and the parent an existing directory → the parent. Anything else → `AutoRootError::NotFound`.
  - `trait Authorizer { fn snapshot(&self) -> …; fn add_auto_export(&self, root: &Path, peer: EndpointId) -> Result<(), AuthzError>; }`. `FileAuthorizer` takes `ConfigLock` on the config file, calls `manage::exports::add_auto_export` and saves. `ManageError::Locked` maps to an `AuthzError` that says the config is locked. `MemoryAuthorizer` keeps its snapshot in a `Mutex` and inserts the export there.
  - On the wire: a missing path for an immediate-granted key replies `Code::BadPath` with `no such file or directory: <escaped path>`. Such a key can reach the server over SSH anyway, so there's nothing to hide. Keys without the immediate grant keep today's uniform `no access` reply.

- [ ] **Step 1: Write the failing tests**
  - `autoexport.rs::auto_root_dir_is_itself`, `auto_root_file_uses_parent`, `auto_root_missing_push_uses_parent`, `auto_root_missing_pull_is_not_found`, `auto_root_missing_parent_is_not_found` (tempdir fixtures).
  - `daemon_auth.rs::immediate_push_creates_auto_export`: a `MemoryAuthorizer` with no exports and `Grants { immediate: true, .. }`, then a push hello to `<tmp>/dest`. The reply is `HelloAck`, and the authorizer's snapshot now has one export with `auto == true`, `path == <tmp>/dest`, granted rw to the client.
  - `daemon_auth.rs::immediate_reuses_containing_auto_export`: after the push above, a push to `<tmp>/dest/sub` succeeds, and the snapshot still has exactly one export.
  - `daemon_auth.rs::immediate_off_rejects_unexported_path`: `Grants { immediate: false, all: None, exports: {} }` plus one unrelated export the key is granted. A push to `<tmp>/elsewhere` gets `Code::Unauthorized`, and no export is added.
  - `daemon_auth.rs::immediate_missing_path_is_bad_path`: a pull of `<tmp>/nope` gets `Code::BadPath`.
  - `sync_e2e.rs::immediate_push_then_resume_uses_index`: through `FileAuthorizer` on a temp config with only `[immediate]`, push a tree to an absolute path, then push again unchanged. The second run hashes 0 files on the daemon (`daemon_files_hashed`), which proves the auto export's index is used.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test autoexport --test daemon_auth --test sync_e2e immediate`
- [ ] **Step 3: Implement `auto_root`, `Authorizer::add_auto_export` for both authorizers, and the daemon path.** In `handle`, before `validate` returns `no_access`: if `grants.immediate`, then `resolve_path`, then `auto_root(abs, direction == ClientSends)`, then `add_auto_export(root, id)`, then a new `snapshot()`, then `validate` again. The fresh snapshot must live in `handle` for the rest of the session, because `Resolved` borrows from it. A root that already has an export is never added twice: before adding, check `containing` against the fresh snapshot.
- [ ] **Step 4: Run the tests and see them pass.** Same command, then `cargo test -p irsync`.
- [ ] **Step 5: Commit.** `git commit -m "Serve immediate-mode paths through automatic exports"`

---

### Task 3: Daemon lock file in the state dir

**Files:**
- Create: `crates/irsync/src/daemon_lock.rs`
- Modify: `crates/irsync/src/lib.rs`, `crates/irsync/src/daemon.rs` (`run_from_settings` takes the lock before binding), `crates/irsync/src/main.rs` (`run_daemon` also cancels on SIGTERM, not only SIGINT)
- Test: `crates/irsync/tests/daemon_lock.rs` (new)

**Interfaces:**
- Produces:
  - `pub struct DaemonLock` (holds the locked `File`, released on drop).
  - `pub struct LockInfo { pub pid: u32, pub exe: PathBuf, pub version: String }`.
  - `pub fn acquire(state_dir: &Path) -> Result<DaemonLock, LockError>`. It opens or creates `state_dir/daemon.lock` and calls `File::try_lock()`. When it gets the lock, it truncates the file and writes `pid=…\nexe=…\nversion=…\n` (`std::env::current_exe()`, `LONG_VERSION`). `LockError::Held(LockInfo)` when another process holds it.
  - `pub fn probe(state_dir: &Path) -> Option<LockInfo>`. `Some` when a live daemon holds the lock: try to lock, release at once if that works, otherwise read the info.
  - The daemon fails to start with `another irsync daemon (pid N, <exe>) is already running for <state_dir>` when the lock is held.

- [ ] **Step 1: Write the failing tests**
  - `daemon_lock.rs::acquire_writes_info_and_probe_sees_it`: run a `spawn_daemon_binary` daemon (see `tests/common`). `probe(server_home/xdg/irsync)` returns its pid, `exe == CARGO_BIN_EXE_irsync` and the version.
  - `daemon_lock.rs::second_daemon_refuses`: a second `irsync daemon` with the same HOME exits non-zero, and its stderr contains `already running`.
  - `daemon_lock.rs::probe_none_after_sigterm`: after `kill -TERM <pid>` the daemon exits within 10 s and `probe` returns `None`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test daemon_lock`
- [ ] **Step 3: Implement `daemon_lock.rs`, the SIGTERM handler and the acquire call** (held in `run_from_settings` until it returns).
- [ ] **Step 4: Run the tests and see them pass.** Same command, then `cargo test -p irsync`. Check that no existing test runs two daemons on one state dir.
- [ ] **Step 5: Commit.** `git commit -m "Hold a lock file while the daemon runs"`

---

### Task 4: `irsync ssh-setup`, the server side of onboarding

**Files:**
- Create: `crates/irsync/src/ssh/setup.rs` (server side, pure parts plus `run`)
- Create: `crates/irsync/src/ssh/mod.rs` (`pub mod setup;`; Task 5 adds more)
- Modify: `crates/irsync/src/lib.rs`, `crates/irsync/src/cli/mod.rs` (hidden `Command::SshSetup { #[arg(long)] client: String, files: FilesArgs }`), `crates/irsync/src/main.rs` (dispatch)
- Test: `crates/irsync/tests/ssh_setup.rs` (new)

**Interfaces:**
- Consumes: `grant_immediate` (Task 1), `daemon_lock::probe` (Task 3), `identity::load_or_generate`, `lock_config`.
- Produces:
  - `pub struct SetupReport { pub version: String, pub endpoint_id: EndpointId, pub identity: Created, pub grant: GrantOutcome, pub daemon: DaemonOutcome, pub log: PathBuf }` with
    `enum Created { Existing, Generated }`,
    `enum GrantOutcome { Added, Already, ImmediateOff }`,
    `enum DaemonOutcome { Running, Started, Restarted { old_version: String }, Foreign { exe: PathBuf, version: String } }`.
  - `impl SetupReport { pub fn to_lines(&self) -> String; pub fn parse(s: &str) -> Result<SetupReport, String> }`. These are `key=value` lines, one per field, with keys `version`, `endpoint_id`, `identity`, `grant`, `daemon`, `daemon_old_version`, `daemon_exe`, `daemon_version`, `log`. Values are lower-case words (`added|already|immediate_off`, `running|started|restarted|foreign`, `existing|generated`). The client parses only the lines between `irsync-setup-begin` and `irsync-setup-end`, so text a login shell prints is ignored.
  - `pub fn run(paths: &Paths, client: EndpointId) -> anyhow::Result<SetupReport>`, in this order:
    1. Load or generate the daemon identity at `settings.daemon.identity` (passphrase-less only; an encrypted daemon key is an error saying to start the daemon by hand).
    2. If `settings.daemon.immediate`, call `grant_immediate` under `lock_config`. If not, `ImmediateOff`.
    3. `probe(state_dir)`:
       - `None`: start the daemon.
       - `Some` with `exe == current_exe()` and `version == LONG_VERSION`: `Running`.
       - `Some` with `exe == current_exe()` and a different version: SIGTERM that pid, wait up to 10 s for `probe` to return `None`, then start (`Restarted`).
       - `Some` with any other exe: `Foreign`, and nothing is touched.
    4. Starting means spawning `current_exe() daemon` (plus `--config` if one was given) in its own session (`setsid` in `pre_exec`), with stdin `/dev/null` and stdout and stderr appended to `state_dir/daemon.log`. Then wait up to 10 s for `probe` to return `Some` with that child's pid. If the child exits first, fail with the last 20 lines of the log.

- [ ] **Step 1: Write the failing tests** (each one uses its own temp HOME via `irsync_bin`, with `[daemon] relay_mode = "disabled"`, `portmap = false`, discovery off and `listen = ["127.0.0.1:0"]`; kill the daemon at the end using the pid from `daemon.lock`)
  - `ssh_setup.rs::report_roundtrips`: `parse(to_lines(r)) == r` for each enum variant. Noise before `irsync-setup-begin` is ignored.
  - `ssh_setup.rs::fresh_home_generates_grants_starts`: running `irsync ssh-setup --client <ID>` reports `identity=generated`, `grant=added`, `daemon=started`. The config has `[immediate] rw = [ID]`, and `probe` returns `Some`.
  - `ssh_setup.rs::second_run_changes_nothing`: run again and you get `identity=existing`, `grant=already`, `daemon=running`, with the config bytes unchanged.
  - `ssh_setup.rs::immediate_off_reports_and_does_not_grant`: with `daemon.immediate = false` you get `grant=immediate_off` and no `[immediate]` table.
  - `ssh_setup.rs::setup_restarts_own_stale_daemon`: write a `daemon.lock` held by a helper process (a test-only `flock`-holding child: run `sh -c 'exec 9>>LOCK; flock 9; sleep 60'` and write the info with `exe=<CARGO_BIN_EXE_irsync>` and `version=old`). The report says `daemon=restarted`, `daemon_old_version=old`, and the helper is gone. Skip this test if `flock(1)` isn't installed.
  - `ssh_setup.rs::setup_leaves_foreign_daemon`: the same helper with `exe=/usr/bin/other`. The report says `daemon=foreign`, and the helper is still alive.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test ssh_setup`
- [ ] **Step 3: Implement `ssh/setup.rs`, the hidden subcommand (`#[command(hide = true)]`) and its dispatch.** Print the report between the markers on stdout. Exit 0 on success, 1 with `irsync: …` on stderr otherwise.
- [ ] **Step 4: Run the tests and see them pass.** Same command.
- [ ] **Step 5: Commit.** `git commit -m "Add ssh-setup: grant, identity and daemon start in one server-side step"`

---

### Task 5: Client SSH driver: probe, install, run setup

**Files:**
- Create: `crates/irsync/src/ssh/client.rs`
- Create: `crates/irsync/tests/fixtures/fake-ssh` (executable sh script)
- Modify: `crates/irsync/src/ssh/mod.rs`, `crates/irsync/src/cli/mod.rs` (`SyncOptions.rsh: Option<String>`, flag `--rsh CMD`, env `IRSYNC_RSH`; long-only)
- Test: `crates/irsync/tests/ssh_client.rs` (new)

**Interfaces:**
- Consumes: `SetupReport::parse` (Task 4), `LONG_VERSION`.
- Produces:
  - `pub const REMOTE_BIN: &str = ".local/share/irsync/bin/irsync";` (relative to the remote home).
  - `pub struct Rsh { argv: Vec<String> }` with `pub fn from_opts(flag: Option<&str>) -> Rsh` (flag, else `IRSYNC_RSH`, else `ssh`; split on ASCII whitespace) and `fn command(&self, dest: &str, remote_cmd: &str) -> std::process::Command`, which builds `argv… dest remote_cmd`. The terminal is inherited so password and host-key prompts work. `remote_cmd` is fixed text built by irsync, plus an EndpointId; it never contains user input.
  - `pub struct Probe { pub os: Option<&'static str>, pub arch: Option<&'static str>, pub installed: Option<String> }`. `installed` is the first line of `~/REMOTE_BIN --version`, if any.
  - `pub fn probe(rsh: &Rsh, dest: &str) -> anyhow::Result<Probe>`. One SSH call: `uname -sm; "$HOME/REMOTE_BIN" --version 2>/dev/null || true`.
  - `pub fn parse_uname(line: &str) -> (Option<&'static str>, Option<&'static str>)`, using the mapping in Global Constraints.
  - `pub enum Install { Current, Copied { replaced: Option<String> } }` and `pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe) -> anyhow::Result<Install>`. It is `Current` when `installed == LONG_VERSION`. If OS and arch equal `std::env::consts::{OS, ARCH}`, it streams `current_exe()` into `mkdir -p "$HOME/.local/share/irsync/bin" && cat > "$HOME/REMOTE_BIN.tmp" && chmod 755 "$HOME/REMOTE_BIN.tmp" && mv -f "$HOME/REMOTE_BIN.tmp" "$HOME/REMOTE_BIN"`. Otherwise it fails with `myserver runs <os> <arch>, this irsync is <os> <arch>: install irsync on myserver by hand (downloading builds for other platforms is not supported yet)`.
  - `pub fn run_setup(rsh: &Rsh, dest: &str, client: EndpointId) -> anyhow::Result<SetupReport>`. Runs `"$HOME/REMOTE_BIN" ssh-setup --client <id>` and parses stdout. A non-zero exit becomes an error carrying the remote stderr.

`tests/fixtures/fake-ssh` takes the destination as `$1` and the remote command as the rest. It appends one line `$1` to `$FAKE_SSH_LOG`, then runs `HOME="$FAKE_SSH_HOME" XDG_STATE_HOME="$FAKE_SSH_HOME/xdg" sh -c "$*"` (after `shift`), passing stdin through. With `FAKE_SSH_UNAME` set, it puts a shell function `uname() { echo "$FAKE_SSH_UNAME"; }` in front of the command.

- [ ] **Step 1: Write the failing tests**
  - `ssh_client.rs::parse_uname_maps`: `"Linux x86_64"` → `(Some("linux"), Some("x86_64"))`; `"Darwin arm64"` → `(Some("macos"), Some("aarch64"))`; `"Linux amd64"` → x86_64; `"FreeBSD x86_64"` → `(None, Some("x86_64"))`.
  - `ssh_client.rs::rsh_splits_like_rsync`: `from_opts(Some("ssh -p 2222"))` gives argv `["ssh","-p","2222"]`. With no flag and `IRSYNC_RSH` unset, you get `["ssh"]`.
  - `ssh_client.rs::install_copies_then_is_current` (through `fake-ssh`): the first `ensure_installed` returns `Copied { replaced: None }`, and `FAKE_SSH_HOME/REMOTE_BIN` is executable and byte-equal to the test binary. Calling `probe` then `ensure_installed` again returns `Current`.
  - `ssh_client.rs::install_refuses_other_platform`: with `FAKE_SSH_UNAME="Linux riscv64"`, the error contains `install irsync on` and nothing is written under `FAKE_SSH_HOME`.
  - `ssh_client.rs::install_replaces_other_version`: pre-place a script at `REMOTE_BIN` that prints `irsync 0.0.1 (old)`. You get `Copied { replaced: Some("irsync 0.0.1 (old)") }`.

  These call the library functions with `Rsh::from_opts(Some(<path to fake-ssh>))`. The binary they copy must be the real `irsync`, not the test executable, so add `#[cfg(feature = "test-hooks")] pub fn set_exe_override(path: PathBuf)` in `ssh/client.rs` (used in place of `current_exe()` when set), and have the tests point it at `env!("CARGO_BIN_EXE_irsync")`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test ssh_client`
- [ ] **Step 3: Implement `ssh/client.rs`, the fixture script (`chmod +x`, committed executable) and the `--rsh` option.**
- [ ] **Step 4: Run the tests and see them pass.** Same command.
- [ ] **Step 5: Commit.** `git commit -m "Drive the server over the system ssh: probe, copy the binary, run ssh-setup"`

---

### Task 6: Client first run: onboard unknown hosts, save the alias, report

**Files:**
- Create: `crates/irsync/src/ssh/onboard.rs`
- Modify: `crates/irsync/src/main.rs` (`connect_for`: when `host` is neither an alias nor an EndpointId, load the client identity, then `onboard`, then reload the config and continue with the new alias)
- Test: `crates/irsync/tests/onboard_e2e.rs` (new)

**Interfaces:**
- Consumes: `probe`, `ensure_installed`, `run_setup`, `Rsh` (Task 5); `host_add` + `HostFlags.ssh` (Task 1); `lock_config`.
- Produces:
  - `pub struct Onboarded { pub alias: String, pub endpoint_id: EndpointId, pub previous_id: Option<EndpointId>, pub report: SetupReport, pub install: Install }`.
  - `pub fn onboard(paths: &Paths, rsh: &Rsh, dest: &str, client: EndpointId) -> anyhow::Result<Onboarded>`. It runs probe → `ensure_installed` → `run_setup` → `host_add(alias = dest, id, HostFlags { ssh: Some(dest), .. })` under `lock_config`. If a split `hosts.toml` exists (`ensure_editable(Table::Hosts)` fails), it returns that error.
  - `pub fn summary(o: &Onboarded) -> Vec<String>`. These are the lines printed on stderr, prefixed `irsync: `. The exact copy, one line each, only the ones that apply:
    - `set up <dest> over ssh:`
    - `  installed irsync <LONG_VERSION> at ~/.local/share/irsync/bin/irsync` / `  replaced irsync <old> with <LONG_VERSION> at …`
    - `  generated the daemon identity`
    - `  granted this client (<id short>) access to any path` / `  immediate mode is off on <dest>: only exports granted to this client are reachable`
    - `  started the daemon (log: <log>)` / `  restarted the daemon (was <old>)` / `  another irsync daemon is running there (<exe>, <version>); left it alone`
    - `  saved host alias <dest> -> <id short>` / `  updated host alias <dest>: <old short> -> <new short>`
    - If the daemon was started or restarted, the how-to follows. `To keep the daemon running after a reboot, make it a user service:` then, for Linux, a 6-line `~/.config/systemd/user/irsync.service` with `ExecStart=%h/.local/share/irsync/bin/irsync daemon` plus `systemctl --user enable --now irsync` and `loginctl enable-linger $USER`. For macOS, a `~/Library/LaunchAgents/org.irsync.daemon.plist` with `ProgramArguments` = that path + `daemon`, `RunAtLoad` and `KeepAlive`, plus `launchctl load -w …`. Pick the OS from `Probe.os`.
  - A `Foreign` daemon whose version differs from `LONG_VERSION` isn't an error during setup. The sync that follows reports the daemon's version mismatch as it does today.

- [ ] **Step 1: Write the failing tests** (client HOME and fake server HOME are separate tempdirs. `IRSYNC_RSH=<fake-ssh>`. The server config pins `daemon.listen = ["127.0.0.1:<free port>"]`, and the client passes `--address 127.0.0.1:<port>`, so the dial works offline. The test's `Drop` kills the daemon using the pid from `daemon.lock`.)
  - `onboard_e2e.rs::first_push_sets_up_and_syncs`: `irsync -a <src>/ myserver:<serverhome>/dest` exits 0. `<serverhome>/dest` equals `<src>`. Stderr contains `set up myserver over ssh:`, `installed irsync`, `started the daemon`, `saved host alias myserver`, `user service`. The client config has `[hosts.myserver]` with `ssh = "myserver"`. `FAKE_SSH_LOG` has 3 lines.
  - `onboard_e2e.rs::second_push_uses_no_ssh`: after the run above, change a file and run again. Exit 0, the change arrives, and `FAKE_SSH_LOG` still has 3 lines.
  - `onboard_e2e.rs::user_at_host_alias`: `u@myserver:` → alias `u@myserver` is saved and works on the second run.
  - `onboard_e2e.rs::immediate_off_still_saves_alias`: the server config has `daemon.immediate = false` and an export `dest` granted to the client's id (pre-generate the client identity with `irsync id generate`). The push to `myserver:dest` succeeds. Stderr has `immediate mode is off on myserver`, and the alias is saved.
  - `onboard_e2e.rs::other_platform_changes_nothing`: with `FAKE_SSH_UNAME="Linux riscv64"`, it exits 1, stderr says `install irsync on myserver by hand`, the client config has no `[hosts]`, and the server HOME has no `.config/irsync`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test onboard_e2e`
- [ ] **Step 3: Implement `ssh/onboard.rs` and the `connect_for` hook.** Reorder `connect_for` so the client identity is loaded before target resolution when onboarding is needed. Print `summary` lines through the `LogWriter`, so they land above the progress block.
- [ ] **Step 4: Run the tests and see them pass.** Same command, then `cargo test -p irsync`.
- [ ] **Step 5: Commit.** `git commit -m "Set up an unknown host over ssh on the first sync and save its alias"`

---

### Task 7: Fall back to SSH when a saved host never answers; docs

**Files:**
- Modify: `crates/irsync/src/session/reconnect.rs` (a typed `NeverReached` error is attached when `!reached && attempt >= dial_retries`)
- Modify: `crates/irsync/src/main.rs` `sync`: if a session error's chain contains `NeverReached`, the host entry has `ssh`, and no fallback has happened in this invocation, run `onboard(…, entry.ssh, …)`, rebuild `Connected` with `connect_for`, and retry that source once. Print `irsync: <alias> did not answer; checking it over ssh` first.
- Modify: `README.md` (the Example leads with the SSH flow; the manual five steps stay as "without SSH"), `book/src/tutorial/first-sync.md`, `book/src/reference/command-line.md` (`--rsh`, `IRSYNC_RSH`, `grant add immediate`), `book/src/reference/configuration.md` (`[immediate]`, `daemon.immediate`, `exports.*.auto`, `hosts.*.ssh`), `book/src/reference/files-and-state.md` (`daemon.lock`, `daemon.log`, `~/.local/share/irsync/bin/irsync`), `book/src/how-to/run-the-daemon.md` (the user-service how-to text from Task 6)
- Test: `crates/irsync/tests/onboard_e2e.rs`

**Interfaces:**
- Consumes: `onboard`, `summary` (Task 6).
- Produces: `pub struct NeverReached;` (implements `Error`, `Display` = `the daemon never answered`) in `session/reconnect.rs`.

- [ ] **Step 1: Write the failing tests**
  - `onboard_e2e.rs::dead_daemon_is_restarted_over_ssh`: after a first run, kill the daemon (pid from `daemon.lock`), change a file, and push with `--dial-retries 0`. Exit 0, the change arrives, stderr has `did not answer; checking it over ssh` and `started the daemon`, and `FAKE_SSH_LOG` has grown.
  - `onboard_e2e.rs::rebuilt_server_updates_alias`: after a first run, kill the daemon, delete the server's `daemon_ed25519*`, and push with `--dial-retries 0`. Exit 0, stderr has `updated host alias myserver:`, and the config's `endpoint_id` has changed.
  - `onboard_e2e.rs::refusal_never_uses_ssh`: after a first run, `irsync grant remove immediate <client id>` on the server HOME, then push to a new path. Exit 1 with the existing no-access hint. `FAKE_SSH_LOG` is unchanged, and the server config still has no immediate grant for the client.
  - `onboard_e2e.rs::alias_without_ssh_does_not_fall_back`: a hand-made alias (`irsync host add`) pointing at a dead id, pushed with `--dial-retries 0`, exits 1 with `gave up after 1 attempt`. `FAKE_SSH_LOG` stays empty.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync --test onboard_e2e`
- [ ] **Step 3: Implement `NeverReached`, the fallback in `sync`, and the docs.**
- [ ] **Step 4: Run the tests and see them pass.** `cargo test -p irsync`, then `./site/build.sh` (or `mdbook build book`) without errors.
- [ ] **Step 5: Commit.** `git commit -m "Restart or re-set up a silent host over ssh, and document SSH onboarding"`

---

## Later (not in this plan)

Downloading signed release binaries for a server whose OS or CPU differs from the client's. It waits for the release-pipeline workstream (see the spec's "Later item").
