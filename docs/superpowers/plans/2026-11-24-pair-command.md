# irsync pair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move every use of ssh out of sync and listing into a new command, `irsync pair HOST`, which also makes the server's daemon a systemd or launchd user service.

**Architecture:** The ssh onboarding code (`crates/irsync/src/ssh/`) stays; only its caller changes, from `connect_for` and `recheck_over_ssh` in `main.rs` to a new `run_pair`. A sync that meets an unknown host or a silent daemon stops with a message that names `irsync pair`. On the server, `ssh-setup` gains a service step (new `ssh/service.rs`) that writes and starts a user service, falling back to today's background start.

**Tech Stack:** Rust, clap, the existing fake-ssh e2e harness (`crates/irsync/tests/onboard_e2e.rs`), mdbook.

**Spec:** `docs/superpowers/specs/2026-11-24-pair-command-design.md` (issue #277)

## Global Constraints

- No backward compatibility, no deprecation period: `--ssh` is removed; `--rsh`, `--no-install`, `--no-remote-dht` leave sync.
- A sync, push, pull or listing never runs ssh. Every e2e test that syncs checks `w.ssh_calls()` did not grow.
- `--no-remote-dht` becomes `--no-dht` on `pair`.
- A test-hooks build finds no service manager unless `IRSYNC_TEST_SERVICE_BIN_DIR` names a directory of stand-ins: tests must never touch the developer's real systemd or launchd.
- Windows service: not in this change. Leave a comment on `Manager` that a Windows variant comes once the daemon runs on Windows.
- Messages and docs: plain words, short sentences (the book's style); examples use `myclient` / `myserver`, home paths as `~`.
- Every `unsafe` block needs a `// SAFETY:` comment (none expected).
- Full check before pushing: `tools/ci.sh check`.

## Review Focus

1. A server home path with a space or a quote: the systemd `ExecStart` and the plist must still start the right binary (test in Task 3).
2. ssh lands in a session with no reachable systemd user manager (no `XDG_RUNTIME_DIR`): `pair` reports "no user session", starts the daemon in the background, and does not fail (test in Task 4).
3. A typo in a host name on a push: no ssh is run, and the message says `irsync pair NAME` (test in Task 2).
4. A daemon already held by someone else's service (a foreign lock): `pair` installs no user service that would fight it (test in Task 4).
5. `pair` run twice with the same version: the service is not restarted and only one daemon runs (test in Task 4).

---

### Task 1: The `irsync pair` command

Adds `pair` beside the sync's ssh path; the sync is unchanged until Task 2.

**Files:**
- Create: `crates/irsync/src/ssh/pair.rs` (and `pub mod pair;` in `ssh/mod.rs`)
- Modify: `crates/irsync/src/cli/mod.rs` (`Command`, new `PairArgs`)
- Modify: `crates/irsync/src/main.rs` (dispatch, new `run_pair`)
- Test: `crates/irsync/tests/cli.rs`, `crates/irsync/tests/onboard_e2e.rs`, unit tests in `ssh/pair.rs`

**Interfaces:**
- Produces, in `cli/mod.rs`:
  - `Command::Pair(PairArgs)`, doc line: "Set a server up over ssh: install irsync there, let this client in, start the daemon and save the host. Syncs then go over iroh; ssh is used only here."
  - `pub struct PairArgs { pub host: String, pub rsh: Option<String>, pub no_install: bool, pub no_dht: bool, pub identity: Option<PathBuf> }` — `host` positional `HOST`; `--rsh CMD`, `--no-install`, `--no-dht`, `--identity FILE` with the help text the sync flags have now.
- Produces, in `ssh/pair.rs`:
  - `pub enum PairKind { First, ClientOnly, Saved }`
  - `pub struct PairTarget { pub host: String, pub dest: String, pub alias: Option<String>, pub kind: PairKind, pub found_as: Option<String> }` — `host` the name used from here on; `found_as` set when an address found a saved host by its `ssh` value (for the "is saved as" line).
  - `pub fn target(hosts: &Hosts, host: &str) -> anyhow::Result<PairTarget>` — the decision now at the top of `connect_for` (`by_ssh` lookup, `first` / `client_only`, dest = saved `ssh` else the name). Strips one trailing `:`. An EndpointId is an error: `pair needs a host alias or an ssh destination, not an EndpointId`. Several saved hosts with that `ssh` value: today's error text.
  - `pub fn paired_line(alias: &str) -> String` = `paired with {alias}; syncs go over iroh, not ssh`
- Produces, in `main.rs`: `fn run_pair(a: &PairArgs, log: LogArgs, files: &FilesArgs) -> i32`: `target` → `onboard::check` / `check_dest` → client key (as `load_client_key` does) → `irsync: pairing with {host} over ssh` → `onboard::onboard` with `SetupFlags { recheck: kind == Saved, no_dht, no_install: no_install || ssh_install == Some(false) }` → `report_onboarded` → `irsync: {paired_line}`. Errors keep today's context strings with "setting it up over ssh failed". Exit 0 / 1.

- [ ] **Step 1: Write the failing tests**
  - `cli.rs`: `pair_parses_host_and_flags` (`irsync pair --no-dht --no-install --rsh 'ssh -p 2222' mike@myserver` gives `Command::Pair` with those values); `pair_refuses_a_sync_flag` (`irsync pair -a myserver` is an error naming `-a`).
  - `ssh/pair.rs` unit tests over a `Hosts` built in the test: `unknown_name_is_a_first_pairing` (dest `myserver`, alias `None`, `First`); `trailing_colon_is_dropped`; `saved_host_uses_its_ssh_value` (entry `ssh = "mike@nas"` → dest `mike@nas`, `Saved`); `address_finds_the_saved_host` (`found_as` set); `client_only_entry_is_client_only`; `endpoint_id_is_refused` (error text above).
  - `onboard_e2e.rs`: `pair_sets_up_a_new_host` (`w.irsync_bare(&["pair", "myserver"])` succeeds, stderr has `irsync: paired with myserver; syncs go over iroh, not ssh` and `saved host alias myserver`, then a push with `--address` adds no ssh call); `pair_user_at_host_saves_both_names`; `pair_again_does_not_regrant` (port of `fallback_after_grant_removed_does_not_regrant`: grant removed on the server, second `pair` reports `has no immediate grant` and the grant stays gone).

- [ ] **Step 2: Run them and see them fail**
  Run: `cargo test -q -p irsync --all-features --test cli pair_ && cargo test -q -p irsync --all-features --lib ssh::pair && cargo test -q -p irsync --all-features --test onboard_e2e pair_`
  Expected: compile errors (no `Command::Pair`, no `ssh::pair`).

- [ ] **Step 3: Implement `PairArgs`, `ssh::pair::target`, `paired_line`, `run_pair`** as in Interfaces. Add `Pair` to the commands that call `init_command_logging`.

- [ ] **Step 4: Run the tests again** — same command, expected PASS.

- [ ] **Step 5: Commit** — `git commit -m 'Add irsync pair, the ssh setup as a command of its own'`

---

### Task 2: Sync and listing never use ssh

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (drop `rsh`, `ssh`, `no_remote_dht`, `no_install` from `SyncArgs`; `rsh`, `force_ssh`, `no_remote_dht`, `no_install` from `SyncOptions`; rewrite `AFTER_HELP`'s quick start around `irsync pair myserver`)
- Modify: `crates/irsync/src/main.rs` (`connect_for`, the session loop in `sync`, the listing)
- Modify: `crates/irsync/src/session/reconnect.rs` (two message functions)
- Test: `crates/irsync/tests/onboard_e2e.rs` (migration below), `crates/irsync/tests/cli.rs`

**Interfaces:**
- Consumes: `run_pair` and `ssh::pair::target` (Task 1).
- Produces, in `session/reconnect.rs`:
  - `pub fn not_paired(host: &str) -> String` = `{host} is not a saved host or an EndpointId. To set it up over ssh: irsync pair {host}. To add it by hand: irsync host add {host} <daemon-id>`
  - `pub fn client_only(host: &str) -> String` = `{host} is saved as a client only: this machine has never dialled its daemon. To set it up over ssh: irsync pair {host}`
  - `pub fn not_answering(alias: &str) -> String` = `{alias} did not answer. On {alias}, check that the daemon is running: systemctl --user status irsync (Linux) or launchctl print gui/$(id -u)/org.irsync.daemon (macOS). Its log: journalctl --user -u irsync, or ~/.local/state/irsync/daemon.log for a daemon started in the background. To start it again from here: irsync pair {alias}`
- `connect_for(host, opts, log)` loses its `hints` parameter and `Connected` loses `onboarded`. `recheck_over_ssh`, `Recheck`, `SSH_HOST_FIRST_DIAL` and the `rechecked` / `ssh_failed` state go; `report_onboarded` stays for `run_pair`.
- A host that never answered (`NeverReached`), on a sync or a listing: with an `ssh` value in its entry → the error plus `not_answering(alias)`; without → `with_wrong_id_hint` as today. The first dial gets what a host without `ssh` gets today.

- [ ] **Step 1: Migrate the e2e tests first (they fail until Step 3).** Every test that set a host up by its first push now runs `w.irsync_bare(&["pair", NAME])` first. Then:

  | Today | Becomes |
  |---|---|
  | `first_push_sets_up_and_syncs` | `first_push_to_an_unknown_host_stops_without_ssh`: exit 1, stderr has `irsync pair myserver`, `ssh_calls() == 0`, no client config written. Plus `pair_then_push_syncs`. |
  | `dead_daemon_is_restarted_over_ssh` | `dead_daemon_stops_the_sync_with_the_pair_hint`: exit non-zero, stderr has `myserver did not answer`, `systemctl --user status irsync`, `irsync pair myserver`; ssh calls unchanged; then `pair myserver` and the push succeed. |
  | `a_dead_daemon_found_by_address_is_restarted` | `a_dead_daemon_found_by_address_names_its_alias` (hint says `irsync pair myserver`, not the address) |
  | `fallback_updates_the_alias_not_its_ssh_name`, `rebuilt_server_*` | the same checks, the second run being `pair` |
  | `force_ssh_checks_a_saved_host`, `force_ssh_sets_up_a_client_only_host`, `force_ssh_refuses_an_endpoint_id` | `pair_checks_a_saved_host`, `pair_sets_up_a_client_only_host`, `pair_refuses_an_endpoint_id` |
  | `an_address_takes_ssh_and_no_remote_dht`, `no_remote_dht_turns_server_publishing_off` | `pair` with an address, and with `--no-dht` |
  | `no_install_flag_stops_a_first_push`, `ssh_install_false_in_config_does_the_same` | `pair --no-install` / `ssh_install = false` with `pair` |
  | `ssh_failure_tries_the_daemon_again`, `no_second_ssh_round_right_after_setup` | deleted (nothing left to test) |
  | `a_dead_daemon_is_not_restarted_through_a_root_account` | `pair_through_a_root_account_is_refused` for a saved host |
  | `alias_without_ssh_does_not_fall_back`, `hand_added_host_*`, `refusal_never_uses_ssh` | kept; assert no `irsync pair` hint for a host without `ssh` |

  Add `a_typo_in_the_host_runs_no_ssh` (Review Focus 3): with `myserver` paired, a push to `myservr:/x` exits 1, stderr has `irsync pair myservr`, ssh calls unchanged.
  In `cli.rs`: `sync_has_no_ssh_flags` — each of `--ssh`, `--rsh x`, `--no-install`, `--no-remote-dht` with `a/ h:b` fails to parse.

- [ ] **Step 2: Run and see the new ones fail**
  Run: `cargo test -q -p irsync --all-features --test onboard_e2e --test cli`
  Expected: `first_push_to_an_unknown_host_stops_without_ssh`, `dead_daemon_stops_the_sync_with_the_pair_hint`, `sync_has_no_ssh_flags` and the typo test FAIL.

- [ ] **Step 3: Implement** the flag removal, the `connect_for` / session-loop / listing changes and the three message functions as in Interfaces; rewrite `AFTER_HELP` (quick start: `irsync pair myserver`, then push and pull; one line that ssh is used only by `pair`).

- [ ] **Step 4: Run** `cargo test -q --workspace --all-features --no-fail-fast` — expected PASS; then `rg -n 'force_ssh|no_remote_dht|recheck_over_ssh|SSH_HOST_FIRST_DIAL' crates` prints nothing.

- [ ] **Step 5: Commit** — `git commit -m 'Stop a sync from running ssh'` with a body saying a sync now stops and names `irsync pair` instead.

---

### Task 3: The user-service definitions (server side, no side effects)

**Files:**
- Create: `crates/irsync/src/ssh/service.rs` (and `pub mod service;`)
- Modify: `crates/irsync/src/ssh/setup.rs` (`SetupReport` field, `to_lines`, `parse`)
- Modify: `crates/irsync/src/ssh/onboard.rs` (test helper `onboarded` fills the new field)
- Test: unit tests in `service.rs` and `setup.rs`

**Interfaces:**
- Produces, in `ssh/service.rs`:
  - `pub enum Manager { Systemd, Launchd }` — comment: a Windows service is the third kind, once the daemon runs on Windows (#277's design).
  - `pub enum ServiceOutcome { Systemd, Launchd, Skipped, NotTouched, Unavailable, Failed(String), Foreign(PathBuf) }` — Skipped: `--no-service`; NotTouched: another daemon holds the lock; Unavailable: no manager or no user session; Failed: the manager refused (linger, enable, start); Foreign: a service file irsync did not write and that does not start this binary.
  - `pub const MARKER: &str = "Written by irsync pair"` — a `# ` comment line in the unit, an XML comment in the plist.
  - `pub fn unit_path(home: &Path, m: Manager) -> PathBuf` — `~/.config/systemd/user/irsync.service`, `~/Library/LaunchAgents/org.irsync.daemon.plist`.
  - `pub fn unit_text(m: Manager, exe: &Path, config: Option<&Path>) -> String` — the unit and plist of "Run the daemon" with `exe` absolute (not `%h` / `$HOME`) and `--config PATH` appended when `config` is given (as `daemon_command` does for a non-default config). systemd: each argument in double quotes with `\` and `"` escaped and `%` as `%%`; plist: XML-escaped.
  - `pub enum Existing { Absent, Ours, UsesThisBinary, Foreign }` and `pub fn classify(text: Option<&str>, m: Manager, exe: &Path) -> Existing` — Ours when the text has `MARKER`; UsesThisBinary when its `ExecStart` / first `ProgramArguments` string is `exe` followed by `daemon`.
- `SetupReport` gains `pub service: ServiceOutcome`; lines `service=systemd|launchd|skipped|not_touched|unavailable|failed|foreign`, plus `service_error=` (newlines flattened, as `old_bin_error`) or `service_file=`. No `service` line parses as `Skipped`. `setup::run` sets `Skipped` for now.

- [ ] **Step 1: Write the failing tests**
  - `systemd_unit_runs_this_binary`: `unit_text(Systemd, "/h/.local/bin/irsync", None)` contains `MARKER`, `ExecStart="/h/.local/bin/irsync" "daemon" "-vv"`, `Restart=on-failure`, `WantedBy=default.target`.
  - `systemd_unit_quotes_odd_paths` (Review Focus 1): exe `/home/a b/it's%/irsync`, config `/c d/irsync.toml` → `ExecStart="/home/a b/it's%%/irsync" "daemon" "-vv" "--config" "/c d/irsync.toml"`.
  - `launchd_plist_runs_this_binary`: Label `org.irsync.daemon`, `<string>/h/.local/bin/irsync</string><string>daemon</string><string>-vv</string>`, `RunAtLoad` and `KeepAlive` true, `MARKER`; an exe with `&` comes out as `&amp;`.
  - `classify_*`: `None` → Absent; our own text → Ours; the book's hand-written unit with `ExecStart=/h/.local/bin/irsync daemon -vv` and exe `/h/.local/bin/irsync` → UsesThisBinary; `ExecStart=/usr/bin/irsync daemon` → Foreign.
  - `setup.rs`: `report_round_trips_each_service_outcome` (all seven); `report_without_a_service_line_is_skipped`.

- [ ] **Step 2: Run** `cargo test -q -p irsync --all-features --lib ssh::` — expected FAIL (missing items).
- [ ] **Step 3: Implement** as in Interfaces.
- [ ] **Step 4: Run** the same — expected PASS.
- [ ] **Step 5: Commit** — `git commit -m 'Add the user-service files irsync pair will write'`

---

### Task 4: `pair` makes the daemon a user service

**Files:**
- Modify: `crates/irsync/src/ssh/service.rs` (detection and the actions)
- Modify: `crates/irsync/src/ssh/setup.rs` (step 4 of `run`), `ssh/client.rs` (`setup_command`), `ssh/onboard.rs` (`SetupFlags`, `summary`, `howto`)
- Modify: `crates/irsync/src/cli/mod.rs` (`PairArgs::no_service`, `Command::SshSetup { no_service }`), `main.rs` (pass it through)
- Create: `crates/irsync/tests/fixtures/fake-service-bin/{systemctl,loginctl}` (shellcheck-clean, `#!/bin/sh`)
- Test: `crates/irsync/tests/onboard_e2e.rs`, unit tests in `onboard.rs`

**Interfaces:**
- Consumes: Task 3's `Manager`, `ServiceOutcome`, `unit_path`, `unit_text`, `classify`.
- Produces:
  - `pub fn detect() -> Option<Manager>` — Linux: `systemctl --user show-environment` exits 0; macOS: `launchctl` is on PATH. In a test-hooks build the tools are looked up only in `IRSYNC_TEST_SERVICE_BIN_DIR`; unset → `None`.
  - `pub fn start(m: Manager, home: &Path, exe: &Path, config: Option<&Path>, restart: bool) -> Result<(), String>` — systemd: `loginctl show-user` `-p Linger`, if not `yes` `loginctl enable-linger` (failure → Err before anything is written); write the unit (private write, only when Absent or Ours); `systemctl --user daemon-reload`; `enable --now irsync`, or `restart irsync` when `restart`. launchd: write the plist; `launchctl bootstrap gui/UID PLIST`, else `user/UID` (an already-loaded job is not an error); `launchctl kickstart -k DOMAIN/org.irsync.daemon` when `restart`.
  - `pub fn is_active(m: Manager) -> bool` — `systemctl --user is-active --quiet irsync` / `launchctl print DOMAIN/org.irsync.daemon` exits 0.
  - `SetupFlags { .., no_service: bool }` → `--no-service` on `ssh-setup`; `setup::run(paths, client, client_name, recheck, no_dht, keep_old, no_service)`.
- Step 4 of `setup::run`, in this order:
  1. Lock held by a foreign daemon → today's `Foreign`, service `NotTouched`.
  2. `no_service` → today's code, `Skipped`. `detect()` is `None` → today's code, `Unavailable`. The service file is `Foreign` → today's code, `Foreign(path)`.
  3. Otherwise: a daemon of ours running outside the service (lock held, `!is_active`) is stopped with `stop_daemon`; then `start(.., restart = is_active && lock version != LONG_VERSION)`; when active with the current version, nothing is started (`DaemonOutcome::Running`, or `Reloaded` after `--no-dht` as today). Wait for the lock as `start_daemon` does. `DaemonOutcome` keeps its meaning (Started / Restarted / Running / Reloaded).
  4. `start` failed, or the lock never appeared → `Failed(msg)` and today's background start.
- `onboard::summary` lines (after the daemon line):
  - Systemd: `  the daemon runs as a systemd user service (irsync.service), also after a reboot`
  - Launchd: `  the daemon runs as a launchd agent (org.irsync.daemon); after a reboot it starts when you log in on the Mac`
  - Skipped: `  the daemon runs in the background until the server reboots (--no-service)`
  - Unavailable: `  no systemd or launchd user session there: the daemon runs in the background until the server reboots`
  - Failed(e): `  warning: could not make the daemon a user service: {e}; it runs in the background until the server reboots`
  - Foreign(p): `  {p} was not written by irsync; left it alone. The daemon runs in the background until the server reboots` (path with `~`)
  - NotTouched: nothing.
  `howto` prints its manual steps only for Unavailable, Failed and Foreign.
- Fake tools (env from the test, passed on by fake-ssh): log each call to `$FAKE_SERVICE_LOG`. `systemctl --user show-environment` fails when `FAKE_SYSTEMD_DOWN=1`. `loginctl show-user` prints `Linger=yes` unless `FAKE_LINGER=no`; `enable-linger` fails when `FAKE_LINGER_REFUSED=1`. `enable --now` / `restart` read `ExecStart` from `$HOME/.config/systemd/user/irsync.service`, kill the pid in `$HOME/.fake-service.pid` if any, start the command in the background (output to `$HOME/fake-service.log`) and store its pid; `is-active` exits 0 when that pid lives. `World` gets `service: bool` that sets `IRSYNC_TEST_SERVICE_BIN_DIR`; its `Drop` already kills the lock holder.

- [ ] **Step 1: Write the failing tests** (`onboard_e2e.rs`, `w.service = true` unless said)
  - `pair_makes_a_systemd_user_service`: unit file exists with `MARKER`; service log has `enable --now irsync`; stderr has the Systemd line and no "To keep the daemon running" how-to; a push then works.
  - `pair_twice_keeps_one_daemon` (Review Focus 5): second `pair` → no `restart` in the log, lock pid unchanged.
  - `pair_moves_a_background_daemon_into_the_service`: `pair --no-service`, then `pair` → the old pid is gone, the lock pid is the service's.
  - `pair_with_no_service_writes_no_unit`: Skipped line, no unit file, daemon running.
  - `pair_without_a_user_session_falls_back` (Review Focus 2): `FAKE_SYSTEMD_DOWN=1` → Unavailable line, how-to printed, exit 0.
  - `pair_falls_back_when_linger_is_refused`: `FAKE_LINGER=no FAKE_LINGER_REFUSED=1` → `warning: could not make the daemon a user service`, no unit file, daemon running, exit 0.
  - `pair_leaves_a_foreign_unit_alone`: a unit with `ExecStart=/usr/bin/irsync daemon` → file unchanged, Foreign line.
  - `pair_leaves_a_foreign_daemon_without_a_service` (Review Focus 4): reuse the setup of `service_file_keeps_the_old_binary_and_daemon` → no unit written, no `enable` in the log.
  - `onboard.rs` unit tests: `summary_names_the_service` (each outcome → its line above); `howto_only_without_a_service`.
  - existing tests run with `service = false` and now also see the Unavailable line; adjust any exact-output assertions.

- [ ] **Step 2: Run** `cargo test -q -p irsync --all-features --test onboard_e2e pair_ && cargo test -q -p irsync --all-features --lib ssh::onboard` — expected FAIL.
- [ ] **Step 3: Implement** as in Interfaces.
- [ ] **Step 4: Run** `cargo test -q --workspace --all-features --no-fail-fast` and `shellcheck crates/irsync/tests/fixtures/fake-service-bin/*` — expected PASS, no findings.
- [ ] **Step 5: Commit** — `git commit -m 'Let irsync pair run the daemon as a user service'`

---

### Task 5: The book and the man pages

**Files:**
- Rename: `book/src/how-to/ssh-setup.md` → `book/src/how-to/pair.md`, title "Pair with a server over ssh"; update `book/src/SUMMARY.md` and every link to it.
- Modify: `book/src/tutorial/first-sync.md` ("The quick way": `irsync pair myserver`, its output, then push; say once that ssh is used only by `pair`), `book/src/reference/command-line.md` (a `pair` section with `--rsh`, `--no-install`, `--no-dht`, `--no-service`, `--identity`; the four flags gone from sync), `book/src/reference/remote-addresses.md` (an unknown `HOST` is an error that names `pair`), `book/src/how-to/run-the-daemon.md` ("Keep a daemon started over ssh running" → what `pair` sets up, and the manual steps for `--no-service`), `book/src/how-to/troubleshooting.md` (the "did not answer" message and what to check), `book/src/reference/configuration.md` (`ssh_install` applies to `pair`), and `introduction.md`, `explanation/threat-model.md`, `reference/files-and-state.md`, `reference/management.md`, `how-to/direct-and-relay.md` wherever they describe ssh in a sync.
- Check the man page sources in `packaging/man/`.

- [ ] **Step 1: Edit** the pages above. On macOS, say plainly that the agent starts after a reboot only once someone logs in on the Mac.
- [ ] **Step 2: Check nothing old is left** — `rg -n -- '--ssh\b|--no-remote-dht|checking it over ssh|not a saved alias|ssh-setup\.md' book packaging crates/irsync/src` prints nothing (the hidden `ssh-setup` command name in code is fine).
- [ ] **Step 3: Run** `python3 -m unittest discover -s tools/tests` — expected OK; `tools/build-man.sh $(mktemp -d)` builds the three pages.
- [ ] **Step 4: Run** `tools/ci.sh check` — expected to pass.
- [ ] **Step 5: Commit** — `git commit -m 'Describe irsync pair in the book'`
