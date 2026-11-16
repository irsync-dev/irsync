# The Daemon Refuses to Run as Root Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `irsync daemon` refuses to start as root unless the server's owner opts in by flag or config key, and ssh setup refuses a root account before it changes anything.

**Architecture:** One helper in `main.rs`, `running_as_root()`, answers "is the effective user id 0". `run_daemon` and `run_ssh_setup` ask it first thing. On the client, `onboard` stops right after the ssh probe when the probe reports user id 0. Tests make a process believe it is root through an environment variable that only a `test-hooks` build reads, the pattern `IRSYNC_TEST_RELEASE_URL` already uses.

**Tech Stack:** Rust, clap derive, nix (`geteuid`, already a dependency). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-16-refuse-root-daemon-design.md`

## Global Constraints

- The opt-in lives on the server only: the flag `--run-as-root-i-know-what-i-am-doing` on `irsync daemon`, or `allow_root = true` under `[daemon]`. Nothing a client sends may switch it on.
- "Root" is the effective user id 0.
- The daemon's check comes before any side effect: no identity created, no socket, no lock file.
- ssh setup refuses a root account always; the server's opt-in does not change that.
- The check lives in the commands (`main.rs`), not in the daemon library. In-process tests that start a daemon are not touched.
- Exit code 1 for every refusal.
- Refusal text of the daemon, exactly:

  ```
  irsync daemon: refusing to run as root: the daemon has not had a security audit,
    and as root any client with write access can create setuid files and device nodes.
    Run it as an unprivileged account. To run as root anyway, pass
    --run-as-root-i-know-what-i-am-doing or set `allow_root = true` under [daemon].
  ```

- Commit messages: a short summary line, a blank line, a plain-English reason. No attribution trailers of any kind.
- Per-task test command: `cargo test -q -p irsync --all-features --test <file>`. Before the last commit: `tools/ci.sh check`.

## Review Focus

1. As root, with `allow_root = true` in a config file that has a syntax error elsewhere: the user sees the config error naming the file, not "refusing to run as root". Test in Task 1.
2. As root, `allow_root = true` in a file given with `--config PATH` (not the default file): the daemon starts. Test in Task 1.
3. The ssh probe fails to see root (a login banner, an odd `id`): `irsync ssh-setup` on the server still refuses, before it writes a grant or an identity. Test in Task 2.
4. A saved host whose daemon died, with a root ssh destination: the automatic recheck is refused and says why; the daemon is not restarted. Test in Task 2.
5. `allow_root = "yes"` (a string): a config error that names the key, on every command, not a silent `false`. Test in Task 1.

---

### Task 1: `irsync daemon` refuses root

**Files:**
- Modify: `crates/irsync/src/config/settings.rs` (`DaemonSettings`, about line 177, and its default about line 227)
- Modify: `crates/irsync/src/manage/config.rs:49` (the `Bool` keys)
- Modify: `crates/irsync/src/cli/mod.rs` (`DaemonArgs`, about line 519)
- Modify: `crates/irsync/src/main.rs` (`run_daemon`, about line 1302)
- Modify: `book/src/reference/configuration.md:197`, `book/src/reference/command-line.md:191-205`
- Test: create `crates/irsync/tests/daemon_root.rs`; add to `crates/irsync/tests/config_files.rs`, `crates/irsync/tests/cli.rs`

**Interfaces:**
- Produces, all in `main.rs`: `fn running_as_root() -> bool`; the environment variable `IRSYNC_TEST_EUID_ROOT` (value `1` makes `running_as_root` true in a `test-hooks` build; ignored otherwise). Also `DaemonSettings::allow_root: bool` and `DaemonArgs::run_as_root: bool`. Task 2 uses `running_as_root` and the variable.

- [ ] **Step 1: Write the failing tests.**

In `config_files.rs`, beside `daemon_immediate_defaults_on`:

```rust
#[test]
fn daemon_allow_root_defaults_off() {
    let c = Config::parse("", Path::new("c")).unwrap();
    assert!(!c.settings.daemon.allow_root);
    let c = Config::parse("[daemon]\nallow_root = true\n", Path::new("c")).unwrap();
    assert!(c.settings.daemon.allow_root);
    let e = Config::parse("[daemon]\nallow_root = \"yes\"\n", Path::new("c"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("allow_root"), "{e}");
}
```

In `cli.rs`, in the test that parses `daemon` arguments (find it with `Command::Daemon`):

```rust
assert!(matches!(parse(&["daemon"]).cmd, Some(Command::Daemon(ref d)) if !d.run_as_root));
assert!(matches!(
    parse(&["daemon", "--run-as-root-i-know-what-i-am-doing"]).cmd,
    Some(Command::Daemon(ref d)) if d.run_as_root
));
```

New file `crates/irsync/tests/daemon_root.rs`. Helpers: a `Home` holding a tempdir with the offline daemon config that `World::server_config` in `onboard_e2e.rs` writes (relay disabled, portmap off, a loopback `listen` port, discovery off), plus an `extra` string appended under `[daemon]`; and `fn daemon(home, as_root: bool, args: &[&str]) -> Child` that spawns `env!("CARGO_BIN_EXE_irsync") daemon` with `HOME`, `XDG_STATE_HOME = home/xdg`, stderr piped, and `IRSYNC_TEST_EUID_ROOT=1` when `as_root` (removed otherwise). A started daemon is detected with `irsync::daemon_lock::probe`, as `onboard_e2e.rs` does, then sent SIGTERM and its stderr collected. Tests that pass `as_root = false` return early when `nix::unistd::geteuid().is_root()`.

```rust
#[test]
fn as_root_the_daemon_refuses_and_creates_nothing() {
    // daemon(home, true, &[]) → wait for exit
    assert_eq!(status.code(), Some(1));
    for part in [
        "irsync daemon: refusing to run as root: the daemon has not had a security audit,",
        "--run-as-root-i-know-what-i-am-doing or set `allow_root = true` under [daemon].",
    ] {
        assert!(err.contains(part), "{err}");
    }
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    assert!(!home.join("xdg").exists());
}

#[test]
fn as_root_the_flag_lets_the_daemon_start_with_a_warning() {
    // daemon(home, true, &["--run-as-root-i-know-what-i-am-doing"]) → probe sees it → SIGTERM
    assert!(err.contains("running as root"), "{err}");
    assert!(!err.contains("refusing to run as root"), "{err}");
}

#[test]
fn as_root_the_config_key_lets_the_daemon_start_from_another_file() {
    // The default config has no opt-in; other.toml (same offline config plus
    // `allow_root = true`) is given with --config.
    // daemon(home, true, &["--config", other]) → probe sees it → SIGTERM
    assert!(err.contains("running as root"), "{err}");
}

#[test]
fn as_root_a_broken_config_is_reported_as_such() {
    // Config: "[daemon]\nallow_root = true\n[hosts\n"
    assert_eq!(status.code(), Some(1));
    assert!(err.contains("irsync.toml"), "{err}");
    assert!(!err.contains("refusing to run as root"), "{err}");
}

#[test]
fn not_as_root_nothing_changes() {
    // daemon(home, false, &[]) → probe sees it → SIGTERM
    assert!(!err.contains("root"), "{err}");
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test daemon_root --test config_files --test cli`
Expected: compile errors for `allow_root` and `run_as_root`. After stubbing nothing else: the `as_root_*` refusal test fails because the daemon starts.

- [ ] **Step 3: Add the key and the flag.**

- `DaemonSettings`: `pub allow_root: bool`, default `false`, wired exactly as `immediate` is. Doc comment: `/// Let the daemon run as root (see `irsync daemon`'s refusal).`
- `manage/config.rs`: add `["daemon", "allow_root"]` to the `Bool` arm beside `["daemon", "immediate"]`, so `irsync config set daemon.allow_root true` works.
- `DaemonArgs`:

```rust
/// Run even as root. The daemon has not had a security audit, and as
/// root any client with write access can create setuid files and
/// device nodes. The config key `daemon.allow_root` does the same.
#[arg(long = "run-as-root-i-know-what-i-am-doing")]
pub run_as_root: bool,
```

- [ ] **Step 4: Add the check** in `main.rs`.

```rust
/// Whether this process runs as root (effective user id 0). A test-hooks
/// build also says yes when IRSYNC_TEST_EUID_ROOT is "1".
fn running_as_root() -> bool;
```

In `run_daemon`, as the first thing after the runtime is built and before `early_settings`/telemetry: when `running_as_root()` and the flag is not given, load the config with `Config::load(&Paths::resolve(d.config.as_deref()))`. A load error is printed as `irsync daemon: {e:#}` and returns 1. A loaded config without `allow_root` prints the refusal text from Global Constraints and returns 1. Otherwise carry on.

After telemetry is initialised, when `running_as_root()`: `tracing::warn!("running as root: any client with write access can create setuid files and device nodes")`.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test -q -p irsync --all-features --test daemon_root --test config_files --test cli`
Expected: all pass.

- [ ] **Step 6: Document it.**

`configuration.md`, a row after `immediate`:

```markdown
| `allow_root` | `false` | Let the daemon run as root. Without it, or the flag `--run-as-root-i-know-what-i-am-doing`, a daemon started as root exits at once. See [Do not run it as root](../how-to/run-the-daemon.md#do-not-run-it-as-root). |
```

Also add `allow_root = false           # true: let the daemon run as root` to the example block near line 57, beside `immediate`.

`command-line.md`: add `[--run-as-root-i-know-what-i-am-doing]` to the usage line at 191, and a row after `--passphrase-file`:

```markdown
| `--run-as-root-i-know-what-i-am-doing` | Start even as root. Without it, or `allow_root = true` under `[daemon]`, the daemon refuses to run as root: it has not had a security audit, and as root any client with write access can create setuid files and device nodes. |
```

- [ ] **Step 7: Commit**

```bash
git add crates/irsync/src crates/irsync/tests/daemon_root.rs crates/irsync/tests/config_files.rs crates/irsync/tests/cli.rs book/src/reference/configuration.md book/src/reference/command-line.md
git commit -m "Refuse to run the daemon as root without an opt-in" -m "A root daemon applies the owner, setuid bits and device nodes a client sends, and daemon mode has not had a security audit. The server's owner can opt in with a flag or a config key. See #34."
```

---

### Task 2: ssh setup refuses a root account

**Files:**
- Modify: `crates/irsync/src/ssh/onboard.rs` (`onboard` about line 54; the warning about line 153; `Onboarded::root` about line 30; the test `root_daemon_gets_a_warning` about line 301)
- Modify: `crates/irsync/src/main.rs` (`run_ssh_setup`, about line 1517)
- Modify: `crates/irsync/tests/fixtures/fake-ssh`
- Modify: `book/src/how-to/run-the-daemon.md:79-90`, `book/src/tutorial/first-sync.md` (the section "The quick way over ssh")
- Test: `crates/irsync/tests/onboard_e2e.rs`, unit tests in `crates/irsync/src/ssh/onboard.rs`

**Interfaces:**
- Consumes: `running_as_root()` and `IRSYNC_TEST_EUID_ROOT` from Task 1.
- Produces: `pub const ROOT_ACCOUNT: &str` in `ssh/onboard.rs`, the sentence both refusals share: `"irsync does not set up a daemon as root"`.

- [ ] **Step 1: Teach the fake ssh to be root.** In `fake-ssh`, add an optional `FAKE_SSH_ID_U`: when set, an `id` script that prints its value goes first on `PATH`, the same way `FAKE_SSH_UNAME` supplies `uname` (both may be set together). In `onboard_e2e.rs`, give `World` two fields, both `false` in `World::new`: `probe_root: bool` (sets `FAKE_SSH_ID_U=0`, else the variable is removed) and `server_root: bool` (sets `IRSYNC_TEST_EUID_ROOT=1` on the client process, which the fake ssh hands on to the server commands; else removed).

- [ ] **Step 2: Write the failing tests** in `onboard_e2e.rs`.

```rust
#[test]
fn a_root_account_is_refused_before_anything_is_installed() {
    let mut w = World::new();
    w.probe_root = true;
    std::fs::remove_dir_all(w.server.path().join(".config")).unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("the ssh account on myserver is root, and irsync does not set up a daemon as root"), "{err}");
    assert!(err.contains("https://irsync.org/book/how-to/run-the-daemon.html"), "{err}");
    assert_eq!(w.ssh_calls(), 1, "only the probe");
    assert!(!w.client_config().contains("[hosts"), "{}", w.client_config());
    assert!(!w.server.path().join(".config/irsync").exists());
    assert!(!w.server.path().join(".local").exists());
    assert!(!w.server.path().join("xdg").exists());
    assert!(!dest.exists());
}

/// The probe did not see root, but the server command does.
#[test]
fn ssh_setup_itself_refuses_root_and_grants_nothing() {
    let mut w = World::new();
    w.server_root = true;
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("irsync does not set up a daemon as root"), "{err}");
    assert!(!w.server_toml().contains("[immediate]"), "{}", w.server_toml());
    assert!(!w.server.path().join(".config/irsync/daemon_ed25519").exists());
    assert!(!w.client_config().contains("[hosts"), "{}", w.client_config());
}

/// Modelled on dead_daemon_is_restarted_over_ssh.
#[test]
fn a_dead_daemon_is_not_restarted_through_a_root_account() {
    let mut w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    w.kill_daemon();
    w.probe_root = true;
    let o = w.irsync(&["-a", &w.src_arg(), &target]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("irsync does not set up a daemon as root"), "{err}");
    // Not restarted.
    assert!(probe(&w.server.path().join("xdg/irsync")).is_none());
}
```

For the last assertion, call `probe` with the state directory the file's other tests use (see `World::kill_daemon`).

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test onboard_e2e root`
Expected: all three fail; the syncs succeed and a daemon runs.

- [ ] **Step 4: Refuse on the client.** In `onboard`, directly after `let p = probe(rsh, dest)?;`:

```rust
if p.root {
    anyhow::bail!(
        "the ssh account on {dest} is root, and {ROOT_ACCOUNT}.\n  \
         Use another account, or set the daemon up by hand:\n  \
         https://irsync.org/book/how-to/run-the-daemon.html"
    );
}
```

Then remove what can no longer happen: the `if o.root && ...` warning in `summary`, the field `Onboarded::root`, and the unit test `root_daemon_gets_a_warning`. `Probe::root` stays.

- [ ] **Step 5: Refuse on the server.** In `run_ssh_setup`, before anything else: when `running_as_root()`, print `irsync: this account is root, and {ROOT_ACCOUNT}` to stderr and return 1.

- [ ] **Step 6: Run the tests and see them pass**

Run: `cargo test -q -p irsync --all-features --test onboard_e2e` and `cargo test -q -p irsync --all-features --lib ssh::onboard`
Expected: all pass.

- [ ] **Step 7: Document it.**

`run-the-daemon.md`, section "Do not run it as root": keep the first paragraph. Replace the paragraph that starts "The [ssh setup]... warns when you sign in as root" with:

```markdown
The daemon enforces this: started as root, it prints why and exits. It has
not had a security audit, so running it as root must be your own choice. To
make that choice, start it with `--run-as-root-i-know-what-i-am-doing`, or
set `allow_root = true` under `[daemon]` in its config file. It then logs a
warning at every start.

The [ssh setup](../tutorial/first-sync.md#the-quick-way-over-ssh) never
starts a daemon as root. When the ssh account is root it stops before it
installs or changes anything, whatever the server's config says. Use another
account, or set the daemon up by hand as described above.
```

`first-sync.md`, in "The quick way over ssh", after the sentence that introduces the command: `The account you log in to must not be root: irsync refuses to set up a daemon as root.`

- [ ] **Step 8: Full check**

Run: `tools/ci.sh check`
Expected: `ci.sh: passed: check`.

- [ ] **Step 9: Commit**

```bash
git add crates/irsync/src crates/irsync/tests/onboard_e2e.rs crates/irsync/tests/fixtures/fake-ssh book/src/how-to/run-the-daemon.md book/src/tutorial/first-sync.md
git commit -m "Refuse to set up a daemon over ssh for a root account" -m "A first sync to root@host started a root daemon with an immediate grant, which lets that client read and write every file on the server. The client now stops after the probe, and ssh-setup on the server refuses too. Closes #34."
```
