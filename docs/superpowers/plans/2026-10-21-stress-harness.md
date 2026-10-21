# irsync Stress Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new workspace crate `stress` building the `irsync-stress` binary, which runs the filegen → fileset → irsync → fileset-check loop against a loopback daemon for N iterations and stops at the first discrepancy with the artefacts the spec lists left on disk.

**Architecture:** One binary that drives the three existing binaries (`irsync`, `filegen`, `fileset`) as subprocesses. Small modules, one per concern: CLI, layout on disk, locating the tools, the daemon (spawn, startup parsing, log copying with markers, shutdown), the client sync command, verification (BLAKE3 comparison and `fileset check`), the per-iteration report, and the orchestrating loop. Everything that builds a command line or formats a line is a pure function with a unit test; the processes are exercised once, end to end, in an integration test with tiny parameters.

**Tech Stack:** Rust 2021, clap 4 (derive), serde + toml for `run.toml`, blake3, nix (signals), rand (default seed), jiff (timestamps), anyhow. All already workspace dependencies.

**Spec:** `docs/superpowers/specs/2026-10-21-stress-harness-design.md`

## Global Constraints

- Workspace CI (`.github/workflows/ci.yml`): `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, on Linux and macOS. Every task ends with all three clean.
- The tools are driven through their command lines only. The crate depends on none of `irsync`, `filegen`, `fileset` (not even as dev-dependencies). The endpoint id is an opaque string.
- The harness never deletes the root directory, on any exit path.
- Daemon config: `relay_mode = "disabled"`, `portmap = false`, `[discovery]` with `dht = false`, `dns = false`, `mdns = false`, `listen` via `--listen 127.0.0.1:0` on the command line. Module name `data`, authorized_keys line `modules="data:rw" <pub line>`.
- Client sync flags, in this order before anything else: `-a -H --delete --reconnect 0 --address 127.0.0.1:<port> --identity <key> -F <empty config>`, then the passthrough args, then source and destination with trailing slashes on local paths.
- Client environment: `HOME` and `XDG_STATE_HOME` set below the client's config directory; `IRSYNC_CONFIG` and `IRSYNC_PASSPHRASE` removed; `RUST_LOG` set to `--log-level` (default `debug`) for both client and daemon.
- Exit codes: 0 every iteration passed; 1 at least one iteration failed verification (client exit 23, manifest hash mismatch, or `fileset check` exit 1); 2 the harness or a tool could not run (any other non-zero exit from any child, daemon startup timeout, daemon death, `fileset check` exit 2).
- Layout and file names exactly as the spec's `Layout under ROOT` section: `run.toml`, `SRC/{config,data}`, `DST/{config,data}`, `filegen/`, `logs/daemon.log`, `logs/ping.client.log`, `logs/iter-NNNN.client.log`, `logs/iter-NNNN.check.txt`, `logs/iter-NNNN.manifest.blake3` (four-digit zero-padded iteration numbers).
- Sentinel file: `SRC/data/.irsync-stress-ping`.
- Daemon shutdown: SIGINT, then SIGKILL after a grace period (builder's choice, 10 s is fine).
- Binaries for the integration test: `cargo test --workspace` (or `cargo build --workspace` first) is required; `cargo test -p stress` alone does not build the three other binaries. The test fails with a message naming the missing binary rather than skipping.

## Review Focus

1. A relative or space-containing `<ROOT>` path: every path handed to a child must be absolute, since filegen and fileset resolve relative paths against their own cwd and `run.toml` must be usable from anywhere. Pinned in Task 2 (`layout_canonicalises_root`).
2. The daemon exits during startup (bad config, port in use): the harness must report its exit and stderr at once, not sit through the 15 s startup timeout. Pinned in Task 4 (`daemon_that_exits_early_is_reported_without_waiting`).
3. Ctrl-C during a long sync: the daemon must not be left running. Pinned in Task 9 (`sigint_terminates_the_daemon`).
4. A `fileset check` that cannot run at all (manifest missing on the destination because the sync never carried it): exit 2 must become a harness failure, never a pass. Pinned in Task 6 (`check_exit_2_is_a_harness_error`).
5. A client that exits 23: the client log must be complete and the manifest comparison and check must still run so the report shows which files were affected. Pinned in Task 8 (`exit_23_still_runs_verification`) through the result model, and the log completeness by giving the child a `File` as stderr rather than a pipe (Task 5).

---

### Task 1: Crate scaffold and command line

**Files:**
- Create: `crates/stress/Cargo.toml`
- Create: `crates/stress/src/main.rs`
- Create: `crates/stress/src/lib.rs`
- Create: `crates/stress/src/cli.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum Direction { Push, Pull }            // clap ValueEnum, lowercase
  pub struct Cli {
      pub root: PathBuf,                       // positional
      pub direction: Direction,                // --direction, required
      pub iterations: u64,                     // --iterations, default 10
      pub seed: Option<u64>,                   // --seed
      pub steps: u64,                          // --steps, default 100
      pub max_total_size: Option<String>,      // --max-total-size, passed through verbatim
      pub max_file_size: Option<String>,       // --max-file-size, passed through verbatim
      pub filegen_set: Vec<String>,            // --filegen-set KEY=VALUE, repeatable
      pub log_level: String,                   // --log-level, default "debug"
      pub keep_going: bool,                    // --keep-going
      pub irsync: Option<PathBuf>,             // --irsync
      pub filegen: Option<PathBuf>,            // --filegen
      pub fileset: Option<PathBuf>,            // --fileset
      pub client_args: Vec<OsString>,          // everything after `--`  (clap: last = true, allow_hyphen_values = true)
  }
  ```
  `Cli` derives `clap::Parser` with `#[command(name = "irsync-stress", version)]`. `main` uses `Cli::try_parse()` as `crates/filegen/src/main.rs` does: help/version exit 0, parse errors exit 2.

- [ ] **Step 1: Create the crate.** `crates/stress/Cargo.toml` with package name `stress`, `version.workspace`, `edition.workspace`, `[[bin]] name = "irsync-stress", path = "src/main.rs"`, dependencies `clap`, `serde`, `toml`, `blake3`, `nix` (features `["signal", "process"]`), `libc`, `rand`, `jiff`, `anyhow`; dev-dependency `tempfile`. All `{ workspace = true }`. `lib.rs` declares `pub mod cli;` (other modules are added by later tasks). `main.rs` parses and, for now, prints the parsed `Cli` with `{:?}` and exits 0.

- [ ] **Step 2: Write the failing tests** in `cli.rs` under `#[cfg(test)]`:

```rust
#[test]
fn defaults_and_passthrough() {
    let c = Cli::try_parse_from(["irsync-stress", "--direction", "push", "/r", "--", "--parallel", "4", "--stats"]).unwrap();
    assert_eq!(c.iterations, 10);
    assert_eq!(c.steps, 100);
    assert_eq!(c.log_level, "debug");
    assert!(c.seed.is_none() && c.max_total_size.is_none() && !c.keep_going);
    assert_eq!(c.client_args, ["--parallel", "4", "--stats"]);
}

#[test]
fn direction_is_required() {
    assert!(Cli::try_parse_from(["irsync-stress", "/r"]).is_err());
}

#[test]
fn filegen_set_repeats_and_sizes_are_verbatim() {
    let c = Cli::try_parse_from(["irsync-stress", "--direction", "pull", "--filegen-set", "a=1", "--filegen-set", "b=2", "--max-total-size", "8M", "/r"]).unwrap();
    assert_eq!(c.filegen_set, ["a=1", "b=2"]);
    assert_eq!(c.max_total_size.as_deref(), Some("8M"));
    assert!(matches!(c.direction, Direction::Pull));
}
```

- [ ] **Step 3: Run** `cargo test -p stress` — expected: compile failure (no `Cli`).
- [ ] **Step 4: Implement `cli.rs`** as specified in Interfaces.
- [ ] **Step 5: Run** `cargo test -p stress && cargo clippy -p stress --all-targets -- -D warnings && cargo fmt --all --check` — expected: 3 passed, clean.
- [ ] **Step 6: Commit** `feat(stress): crate scaffold and command line`.

---

### Task 2: Layout and `run.toml`

**Files:**
- Create: `crates/stress/src/layout.rs`
- Create: `crates/stress/src/run_file.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  // layout.rs
  pub struct Layout {
      pub root: PathBuf,        // canonical, absolute
      pub run_file: PathBuf,    // root/run.toml
      pub src_config: PathBuf, pub src_data: PathBuf,
      pub dst_config: PathBuf, pub dst_data: PathBuf,
      pub filegen_dir: PathBuf, // root/filegen
      pub logs: PathBuf,        // root/logs
  }
  impl Layout {
      /// Creates `root` if missing; errors if it exists and is not an empty directory. Creates every subdirectory.
      pub fn create(root: &Path) -> anyhow::Result<Layout>;
      pub fn daemon_log(&self) -> PathBuf;                       // logs/daemon.log
      pub fn ping_client_log(&self) -> PathBuf;                  // logs/ping.client.log
      pub fn client_log(&self, iter: u64) -> PathBuf;            // logs/iter-0001.client.log
      pub fn check_report(&self, iter: u64) -> PathBuf;          // logs/iter-0001.check.txt
      pub fn manifest_hashes(&self, iter: u64) -> PathBuf;       // logs/iter-0001.manifest.blake3
      pub fn ping_file(&self) -> PathBuf;                        // SRC/data/.irsync-stress-ping
      pub fn filegen_state(&self, seed: u64) -> PathBuf;         // filegen/filegen-<seed>.state.yaml
      /// (config dir, data dir) of the side the daemon serves, by direction.
      pub fn daemon_side(&self, d: Direction) -> (&Path, &Path);
      pub fn client_side(&self, d: Direction) -> (&Path, &Path);
  }

  // run_file.rs
  #[derive(Serialize, Deserialize, Debug, PartialEq)]
  pub struct RunFile {
      pub started: String,              // RFC 3339, from jiff::Timestamp::now()
      pub seed: u64,
      pub direction: String,            // "push" | "pull"
      pub iterations: u64,
      pub steps: u64,
      pub max_total_size: Option<String>,
      pub max_file_size: Option<String>,
      pub filegen_set: Vec<String>,
      pub log_level: String,
      pub keep_going: bool,
      pub client_args: Vec<String>,     // lossy UTF-8 of the OsStrings
      pub tools: ToolVersions,          // { irsync: {path, version}, filegen: ..., fileset: ... }
      pub daemon: Option<DaemonInfo>,   // { endpoint_id: String, address: String, pid: u32 }, filled in once known
  }
  impl RunFile {
      pub fn write(&self, path: &Path) -> anyhow::Result<()>;   // toml::to_string_pretty, atomic (write tmp, rename)
      pub fn read(path: &Path) -> anyhow::Result<RunFile>;
  }
  ```

- [ ] **Step 1: Write the failing tests** in `layout.rs`:

```rust
#[test]
fn layout_canonicalises_root() {
    let t = tempfile::tempdir().unwrap();
    // `relative_to_cwd(abs)`: one `..` per component of `current_dir()`, then `abs` without its leading `/`.
    let rel = relative_to_cwd(&t.path().join("run dir"));
    assert!(rel.is_relative());
    let l = Layout::create(&rel).unwrap();
    assert!(l.root.is_absolute());
    assert!(l.src_data.ends_with("SRC/data") && l.dst_config.ends_with("DST/config"));
    for d in [&l.src_config, &l.src_data, &l.dst_config, &l.dst_data, &l.filegen_dir, &l.logs] { assert!(d.is_dir()); }
}

#[test]
fn non_empty_root_is_refused_and_missing_root_is_created() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("x"), b"").unwrap();
    assert!(Layout::create(t.path()).is_err());
    let fresh = t.path().join("new/root");
    assert!(Layout::create(&fresh).is_ok());
}

#[test]
fn names_follow_the_spec() {
    let t = tempfile::tempdir().unwrap();
    let l = Layout::create(&t.path().join("r")).unwrap();
    assert_eq!(l.client_log(1).file_name().unwrap(), "iter-0001.client.log");
    assert_eq!(l.check_report(12).file_name().unwrap(), "iter-0012.check.txt");
    assert_eq!(l.manifest_hashes(3).file_name().unwrap(), "iter-0003.manifest.blake3");
    assert_eq!(l.filegen_state(42).file_name().unwrap(), "filegen-42.state.yaml");
    assert_eq!(l.daemon_side(Direction::Push).1, l.dst_data);
    assert_eq!(l.daemon_side(Direction::Pull).1, l.src_data);
}
```

and in `run_file.rs`:

```rust
#[test]
fn run_file_round_trips() {
    let t = tempfile::tempdir().unwrap();
    let r = RunFile { /* every field set, daemon: Some(...) */ };
    r.write(&t.path().join("run.toml")).unwrap();
    assert_eq!(RunFile::read(&t.path().join("run.toml")).unwrap(), r);
}
```

- [ ] **Step 2: Run** `cargo test -p stress` — expected: compile failure.
- [ ] **Step 3: Implement** `layout.rs` and `run_file.rs`; add `pub mod layout; pub mod run_file;` to `lib.rs`. `Layout::create` uses `create_dir_all` then `canonicalize`.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): run layout and run.toml`.

---

### Task 3: Locating the tools

**Files:**
- Create: `crates/stress/src/tools.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Tool { pub path: PathBuf, pub version: String }   // version: first line of `<path> --version`
  pub struct Tools { pub irsync: Tool, pub filegen: Tool, pub fileset: Tool }
  impl Tools {
      /// Each override wins; else `<dir of current_exe()>/<name>`; else `PATH` (via `which`-style search over `PATH`). Runs `--version` on each; a tool that is missing or fails `--version` is an error naming the tool and the places searched.
      pub fn locate(irsync: Option<&Path>, filegen: Option<&Path>, fileset: Option<&Path>) -> anyhow::Result<Tools>;
      pub fn versions(&self) -> ToolVersions;   // for RunFile
  }
  /// The search without `--version`, for tests: returns the first existing candidate.
  pub fn find(name: &str, override_: Option<&Path>, exe_dir: &Path, path_env: &OsStr) -> Option<PathBuf>;
  ```

- [ ] **Step 1: Write the failing tests** in `tools.rs`:

```rust
#[test]
fn override_wins_then_exe_dir_then_path() {
    let t = tempfile::tempdir().unwrap();
    let (exe_dir, path_dir) = (t.path().join("exe"), t.path().join("path"));
    for d in [&exe_dir, &path_dir] { std::fs::create_dir(d).unwrap(); }
    std::fs::write(path_dir.join("filegen"), b"").unwrap();
    assert_eq!(find("filegen", None, &exe_dir, path_dir.as_os_str()), Some(path_dir.join("filegen")));
    std::fs::write(exe_dir.join("filegen"), b"").unwrap();
    assert_eq!(find("filegen", None, &exe_dir, path_dir.as_os_str()), Some(exe_dir.join("filegen")));
    assert_eq!(find("filegen", Some(Path::new("/o/filegen")), &exe_dir, path_dir.as_os_str()), Some(PathBuf::from("/o/filegen")));
    assert_eq!(find("nothing", None, &exe_dir, path_dir.as_os_str()), None);
}

#[test]
fn missing_tool_names_itself() {
    let err = Tools::locate(Some(Path::new("/nonexistent/irsync")), None, None).unwrap_err().to_string();
    assert!(err.contains("irsync"), "{err}");
}
```

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement** `tools.rs`; add to `lib.rs`.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): locate the irsync, filegen and fileset binaries`.

---

### Task 4: The daemon

**Files:**
- Create: `crates/stress/src/daemon.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Consumes: `Layout`, `Tool`.
- Produces:
  ```rust
  pub struct DaemonConfig<'a> { pub config_dir: &'a Path, pub module_path: &'a Path, pub authorized_pub_line: &'a str }
  /// Writes `<config_dir>/irsyncd.toml` (identity `<config_dir>/daemon_ed25519`, authorized_keys `<config_dir>/authorized_keys`, state_dir `<config_dir>/state`, the isolated settings, `[modules.data] path = <module_path>`) and `<config_dir>/authorized_keys` with `modules="data:rw" <pub line>\n`. Returns the toml path.
  pub fn write_config(c: &DaemonConfig) -> anyhow::Result<PathBuf>;

  /// Runs `irsync keygen --out <config_dir>/id_ed25519`; returns (private key path, first stdout line = the public key line).
  pub fn client_keygen(irsync: &Path, config_dir: &Path) -> anyhow::Result<(PathBuf, String)>;

  pub struct Startup { pub endpoint_id: String, pub address: SocketAddr }
  /// Reads lines until a `listening on:` line follows an `endpoint id:` line; an unexpected line, EOF, or `timeout` is an error. `lines` is anything that yields the daemon's stdout lines (an `mpsc::Receiver<String>` fed by a reader thread).
  pub fn parse_startup(lines: &mpsc::Receiver<String>, timeout: Duration) -> anyhow::Result<Startup>;

  /// The daemon log file with a shared writer so markers and the copied stderr interleave in one file.
  pub struct DaemonLog(Arc<Mutex<File>>);
  impl DaemonLog {
      pub fn create(path: &Path) -> anyhow::Result<DaemonLog>;
      pub fn marker(&self, text: &str) -> anyhow::Result<()>;   // writes "=== <text> ===\n" and flushes
  }

  pub struct Daemon { child: Child, pub startup: Startup, log: DaemonLog, copier: Option<JoinHandle<()>> }
  impl Daemon {
      /// Spawns `<irsync> daemon --config <toml> --listen 127.0.0.1:0` with stdin null, stdout piped, stderr piped to a copier thread writing into `log`, `RUST_LOG=<log_level>`, `IRSYNC_PASSPHRASE` removed. Waits for startup with `timeout`. If the child exits before the lines arrive, returns an error that includes its exit status and the last lines of its stderr, immediately.
      pub fn start(irsync: &Path, toml: &Path, log: DaemonLog, log_level: &str, timeout: Duration) -> anyhow::Result<Daemon>;
      pub fn pid(&self) -> u32;
      pub fn marker(&self, text: &str) -> anyhow::Result<()>;
      /// Error if the child has already exited (so the loop can turn "daemon died" into exit 2).
      pub fn check_alive(&mut self) -> anyhow::Result<()>;
      /// SIGINT, wait up to `grace`, then SIGKILL; joins the copier. Idempotent.
      pub fn stop(&mut self, grace: Duration);
  }
  impl Drop for Daemon { /* stop(Duration::from_secs(10)) */ }
  ```

- [ ] **Step 1: Write the failing tests** in `daemon.rs`:

```rust
fn channel_of(lines: &[&str]) -> mpsc::Receiver<String> { /* send each, drop sender */ }

#[test]
fn parse_startup_reads_id_then_address() {
    let rx = channel_of(&["endpoint id: abc123", "listening on: 127.0.0.1:43210"]);
    let s = parse_startup(&rx, Duration::from_secs(1)).unwrap();
    assert_eq!(s.endpoint_id, "abc123");
    assert_eq!(s.address, "127.0.0.1:43210".parse::<SocketAddr>().unwrap());
}

#[test]
fn parse_startup_rejects_address_before_id_unknown_lines_and_eof() {
    assert!(parse_startup(&channel_of(&["listening on: 127.0.0.1:1"]), Duration::from_secs(1)).is_err());
    assert!(parse_startup(&channel_of(&["endpoint id: a", "hello"]), Duration::from_secs(1)).is_err());
    assert!(parse_startup(&channel_of(&["endpoint id: a"]), Duration::from_secs(1)).is_err());   // EOF
}

#[test]
fn parse_startup_times_out() {
    let (_tx, rx) = mpsc::channel::<String>();   // keep the sender alive so it is a timeout, not EOF
    let t = Instant::now();
    assert!(parse_startup(&rx, Duration::from_millis(200)).is_err());
    assert!(t.elapsed() >= Duration::from_millis(200));
}

#[test]
fn markers_and_copied_stderr_share_one_file() {
    let t = tempfile::tempdir().unwrap();
    let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
    log.marker("ping").unwrap();
    // copy a fake stderr through the same writer the copier uses
    copy_into(&log, std::io::Cursor::new(b"line one\n".to_vec()));
    log.marker("iteration 1").unwrap();
    assert_eq!(std::fs::read_to_string(t.path().join("d.log")).unwrap(), "=== ping ===\nline one\n=== iteration 1 ===\n");
}

#[test]
fn daemon_that_exits_early_is_reported_without_waiting() {
    // A fake "irsync" that ignores its arguments, prints to stderr and exits 3.
    let t = tempfile::tempdir().unwrap();
    let fake = t.path().join("irsync");
    std::fs::write(&fake, "#!/bin/sh\necho 'config error' >&2\nexit 3\n").unwrap();
    chmod_755(&fake);
    let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
    let started = Instant::now();
    let err = Daemon::start(&fake, Path::new("/dev/null"), log, "debug", Duration::from_secs(15)).unwrap_err().to_string();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(err.contains("exit") && err.contains("config error"), "{err}");
}

#[test]
fn stop_kills_a_child_that_ignores_sigint() {
    let t = tempfile::tempdir().unwrap();
    let fake = t.path().join("irsync");
    std::fs::write(&fake, "#!/bin/sh\ntrap '' INT\necho 'endpoint id: x'\necho 'listening on: 127.0.0.1:1'\nwhile :; do sleep 1; done\n").unwrap();
    chmod_755(&fake);
    let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
    let mut d = Daemon::start(&fake, Path::new("/dev/null"), log, "debug", Duration::from_secs(15)).unwrap();
    let pid = d.pid();
    d.stop(Duration::from_millis(500));
    assert!(nix::sys::signal::kill(Pid::from_raw(pid as i32), None).is_err());   // gone
}
```

Note for the fake-script tests: `trap '' INT` in `sh` makes SIGINT ignored, so only the SIGKILL path ends the child; `echo` output must be flushed (it is, line-buffered to a pipe is fine for `sh`'s builtin echo).

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement** `daemon.rs`; `pub mod daemon;` in `lib.rs`. Startup line reading: a thread reads stdout lines into an `mpsc::Sender<String>`; `parse_startup` uses `recv_timeout` with the remaining budget. Early-exit detection: while waiting, poll `child.try_wait()` every 100 ms alongside `recv_timeout(100ms)`; on exit, join the copier and read the tail of the log file for the message.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): spawn, watch and stop the daemon`.

---

### Task 5: The client sync

**Files:**
- Create: `crates/stress/src/client.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Consumes: `Direction`, `Layout`, `Startup`.
- Produces:
  ```rust
  pub struct ClientSetup { pub identity: PathBuf, pub config: PathBuf /* empty file */, pub home: PathBuf, pub xdg_state: PathBuf }
  /// Creates the empty config file and the `home/` and `xdg/` directories under `config_dir`. `identity` comes from `client_keygen`.
  pub fn setup(config_dir: &Path, identity: PathBuf) -> anyhow::Result<ClientSetup>;

  /// The complete argument vector (without the program): the global constraints' flag list, then `extra`, then source and destination.
  pub fn sync_args(d: Direction, layout: &Layout, endpoint_id: &str, port: u16, setup: &ClientSetup, extra: &[OsString]) -> Vec<OsString>;

  pub enum SyncExit { Ok, Partial /* 23 */, Other(i32) /* or signal: -1 */ }
  pub struct SyncResult { pub exit: SyncExit, pub elapsed: Duration }
  /// Runs the client with stdin null, stdout and stderr both appended to `log` (a File handle each, via `Stdio::from`), env per the global constraints.
  pub fn run_sync(irsync: &Path, args: &[OsString], setup: &ClientSetup, log_level: &str, log: &Path) -> anyhow::Result<SyncResult>;
  ```

- [ ] **Step 1: Write the failing tests** in `client.rs`:

```rust
#[test]
fn push_args_in_spec_order() {
    let t = tempfile::tempdir().unwrap();
    let l = Layout::create(&t.path().join("r")).unwrap();
    let s = setup(&l.src_config, l.src_config.join("id_ed25519")).unwrap();
    let a = sync_args(Direction::Push, &l, "ID", 4321, &s, &["--parallel".into(), "4".into()]);
    let a: Vec<String> = a.iter().map(|o| o.to_string_lossy().into_owned()).collect();
    let expect_prefix = ["-a", "-H", "--delete", "--reconnect", "0", "--address", "127.0.0.1:4321", "--identity", s.identity.to_str().unwrap(), "-F", s.config.to_str().unwrap(), "--parallel", "4"];
    assert_eq!(&a[..expect_prefix.len()], &expect_prefix);
    assert_eq!(&a[expect_prefix.len()..], [format!("{}/", l.src_data.display()), "ID:data/".to_string()]);
}

#[test]
fn pull_swaps_source_and_destination() {
    /* as above with Direction::Pull: last two are ["ID:data/", "<dst_data>/"] and identity/config live under dst_config */
}

#[test]
fn exit_codes_classify() {
    // three fake irsync scripts, `echo err-<n> >&2; exit <n>` for n in 0, 23, 1
    /* run_sync each → SyncExit::Ok, Partial, Other(1); the log file contains "err-<n>" */
}
```

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement** `client.rs`; `pub mod client;`. For the fake-exit test, `run_sync` passes the parent's environment through except the removed variables.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): the client sync invocation`.

---

### Task 6: Verification: manifest hashes and `fileset check`

**Files:**
- Create: `crates/stress/src/verify.rs`
- Create: `crates/stress/src/fileset.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  // verify.rs
  pub fn blake3_file(path: &Path) -> anyhow::Result<String>;   // lowercase hex
  pub struct ManifestHashes { pub src: String, pub dst: String }
  impl ManifestHashes { pub fn matches(&self) -> bool }
  /// Hashes `<src_data>/.fileset.sqlite` and `<dst_data>/.fileset.sqlite` (a missing file is an error: the sync should have carried it) and writes `report` as two lines `src <hex>\ndst <hex>\n`.
  pub fn compare_manifests(src_data: &Path, dst_data: &Path, report: &Path) -> anyhow::Result<ManifestHashes>;

  // fileset.rs
  pub fn init(fileset: &Path, tree: &Path) -> anyhow::Result<()>;     // `fileset init -q <tree>`; non-zero exit is an error with stderr
  pub fn update(fileset: &Path, tree: &Path) -> anyhow::Result<()>;   // `fileset update -q <tree>`
  #[derive(Debug, PartialEq)] pub struct CheckCounts { pub added: u64, pub deleted: u64, pub changed: u64, pub unchanged: u64, pub warnings: u64 }
  pub enum CheckOutcome { Clean(CheckCounts), Differences(CheckCounts) }
  /// `fileset check -q <tree>`; stdout then stderr written to `report`. Exit 0 → Clean, 1 → Differences, anything else → Err (harness failure). Counts parsed from the summary line `added=N deleted=N changed=N unchanged=N warnings=N` on stderr; if that line is missing the counts are all zero and the report file still holds everything.
  pub fn check(fileset: &Path, tree: &Path, report: &Path) -> anyhow::Result<CheckOutcome>;
  pub fn parse_summary(stderr: &str) -> Option<CheckCounts>;
  ```

- [ ] **Step 1: Write the failing tests**:

```rust
// verify.rs
#[test]
fn identical_and_different_manifests() {
    let t = tempfile::tempdir().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    for p in [&s, &d] { std::fs::create_dir(p).unwrap(); std::fs::write(p.join(".fileset.sqlite"), b"same").unwrap(); }
    let h = compare_manifests(&s, &d, &t.path().join("r.blake3")).unwrap();
    assert!(h.matches());
    assert_eq!(std::fs::read_to_string(t.path().join("r.blake3")).unwrap(), format!("src {}\ndst {}\n", h.src, h.dst));
    std::fs::write(d.join(".fileset.sqlite"), b"other").unwrap();
    assert!(!compare_manifests(&s, &d, &t.path().join("r2.blake3")).unwrap().matches());
    std::fs::remove_file(d.join(".fileset.sqlite")).unwrap();
    assert!(compare_manifests(&s, &d, &t.path().join("r3.blake3")).is_err());
}

// fileset.rs
#[test]
fn summary_line_parses() {
    assert_eq!(parse_summary("WARN x\nadded=1 deleted=2 changed=3 unchanged=4 warnings=5\n"), Some(CheckCounts { added: 1, deleted: 2, changed: 3, unchanged: 4, warnings: 5 }));
    assert_eq!(parse_summary("nothing here"), None);
}

#[test]
fn check_exit_codes_map() {
    // fake fileset scripts: exit 0 with a summary, exit 1 with a summary and an "ADD x" line on stdout, exit 2
    /* Clean(counts) / Differences(counts) and report file == stdout + stderr / Err */
}

#[test]
fn check_exit_2_is_a_harness_error() {
    /* the exit-2 fake above: `check(...)` returns Err whose message contains "fileset check" */
}
```

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement** both modules; add to `lib.rs`. `blake3_file`: `std::io::copy` from the file into a `blake3::Hasher`.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): manifest hash comparison and fileset wrappers`.

---

### Task 7: filegen wrappers

**Files:**
- Create: `crates/stress/src/filegen.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct FilegenOpts<'a> { pub steps: u64, pub max_total_size: Option<&'a str>, pub max_file_size: Option<&'a str>, pub sets: &'a [String] }
  /// `run --seed <seed> --steps <n> --out-dir <out_dir> -q [--max-total-size S] [--max-file-size S] [--set KV]... <tree>`
  pub fn fill_args(seed: u64, out_dir: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString>;
  /// `run --resume <state> --steps <n> -q [same size flags and sets] <tree>`
  pub fn mutate_args(state: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString>;
  /// Runs `<filegen> <args>` with stdin null, stderr inherited (filegen's own progress goes to the harness's stderr, `-q` keeps it quiet), non-zero exit is an error with the exit code.
  pub fn run(filegen: &Path, args: &[OsString]) -> anyhow::Result<()>;
  ```

- [ ] **Step 1: Write the failing tests**:

```rust
#[test]
fn fill_and_mutate_args() {
    let o = FilegenOpts { steps: 100, max_total_size: Some("8M"), max_file_size: Some("1M"), sets: &["names.max_len=200".into()] };
    let a = strs(fill_args(42, Path::new("/r/filegen"), Path::new("/r/SRC/data"), &o));
    assert_eq!(a, ["run", "--seed", "42", "--steps", "100", "--out-dir", "/r/filegen", "-q", "--max-total-size", "8M", "--max-file-size", "1M", "--set", "names.max_len=200", "/r/SRC/data"]);
    let o2 = FilegenOpts { steps: 5, max_total_size: None, max_file_size: None, sets: &[] };
    let m = strs(mutate_args(Path::new("/r/filegen/filegen-42.state.yaml"), Path::new("/r/SRC/data"), &o2));
    assert_eq!(m, ["run", "--resume", "/r/filegen/filegen-42.state.yaml", "--steps", "5", "-q", "/r/SRC/data"]);
}

#[test]
fn non_zero_exit_is_an_error_naming_the_code() {
    /* fake filegen `exit 2` → Err containing "2" */
}
```

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement**; add to `lib.rs`.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): filegen fill and mutate invocations`.

---

### Task 8: The iteration report

**Files:**
- Create: `crates/stress/src/report.rs`
- Modify: `crates/stress/src/lib.rs`

**Interfaces:**
- Consumes: `SyncExit`, `CheckOutcome`, `CheckCounts`.
- Produces:
  ```rust
  pub struct Iteration {
      pub number: u64, pub total: u64,
      pub sync: SyncResult,
      pub manifest_ok: bool,
      pub check: CheckOutcome,
  }
  impl Iteration {
      pub fn passed(&self) -> bool;   // sync Ok && manifest_ok && check Clean
      /// `iter 0002/0100  sync 11.8s  exit 23   manifest ok  check FAILED (added=0 deleted=1 changed=2)`; `manifest MISMATCH` when the hashes differ; `check ok` when clean; seconds with one decimal; the exit code left-aligned in a column of width 4 (`exit 0   `, `exit 23  `, `exit sig ` for a signal).
      pub fn line(&self) -> String;
  }
  /// 0 if every iteration passed, else 1. (Exit 2 never comes from here: it is the `Err` path of the loop.)
  pub fn exit_code(iterations: &[Iteration]) -> i32;
  pub fn header(root: &Path, seed: u64, direction: Direction, endpoint_id: &str, address: &SocketAddr) -> String;   // multi-line
  pub fn footer(iterations: &[Iteration], total: Duration) -> String;   // "passed=N failed=N elapsed=..."
  ```

- [ ] **Step 1: Write the failing tests**:

```rust
#[test]
fn line_format_matches_spec() {
    let ok = Iteration { number: 1, total: 100, sync: SyncResult { exit: SyncExit::Ok, elapsed: Duration::from_millis(12_340) }, manifest_ok: true, check: CheckOutcome::Clean(zero()) };
    assert_eq!(ok.line(), "iter 0001/0100  sync 12.3s  exit 0    manifest ok  check ok");
    let bad = Iteration { number: 2, total: 100, sync: SyncResult { exit: SyncExit::Partial, elapsed: Duration::from_millis(11_800) }, manifest_ok: true, check: CheckOutcome::Differences(CheckCounts { added: 0, deleted: 1, changed: 2, unchanged: 9, warnings: 0 }) };
    assert_eq!(bad.line(), "iter 0002/0100  sync 11.8s  exit 23   manifest ok  check FAILED (added=0 deleted=1 changed=2)");
}

#[test]
fn exit_23_still_runs_verification() {
    // An iteration with exit 23 carries a real check outcome, and fails even if the check was clean.
    let it = Iteration { number: 1, total: 1, sync: SyncResult { exit: SyncExit::Partial, elapsed: Duration::ZERO }, manifest_ok: true, check: CheckOutcome::Clean(zero()) };
    assert!(!it.passed());
    assert_eq!(exit_code(&[it]), 1);
}

#[test]
fn manifest_mismatch_alone_fails() { /* sync Ok, manifest_ok false, check Clean → !passed(), line contains "manifest MISMATCH" */ }

#[test]
fn all_passed_is_zero() { /* two passing iterations → 0 */ }
```

- [ ] **Step 2: Run** — expected: compile failure.
- [ ] **Step 3: Implement**; add to `lib.rs`.
- [ ] **Step 4: Run** tests, clippy, fmt — expected: clean.
- [ ] **Step 5: Commit** `feat(stress): iteration lines and exit codes`.

---

### Task 9: The loop, signals, main, and the end-to-end test

**Files:**
- Create: `crates/stress/src/run.rs`
- Create: `crates/stress/src/signals.rs`
- Modify: `crates/stress/src/main.rs`
- Modify: `crates/stress/src/lib.rs`
- Create: `crates/stress/tests/e2e.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  ```rust
  // signals.rs
  /// Installs SIGINT and SIGTERM handlers. The handler sends SIGINT to the pid stored in the shared atomic (if non-zero), then `_exit(130)`. Async-signal-safe only: an atomic load, `libc::kill`, `libc::_exit`. Pattern: `crates/filegen/src/signals.rs`.
  pub fn install() -> Arc<AtomicI32>;   // the daemon pid slot; `run` stores the pid once the daemon is up and clears it after `stop`

  // run.rs
  /// The whole sequence of the spec's "Sequence" section. Returns the process exit code (0 or 1); `Err` means exit 2 and the message is printed by main as `irsync-stress: <error>`.
  pub fn run(cli: &Cli) -> anyhow::Result<i32>;
  ```
  `main`: `Cli::try_parse` as in Task 1, then `match run(&cli) { Ok(c) => exit(c), Err(e) => { eprintln!("irsync-stress: {e:#}"); exit(2) } }`.

- [ ] **Step 1: Write the end-to-end test** `crates/stress/tests/e2e.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_irsync-stress")) }

/// The three tools must sit next to the harness binary: `cargo test --workspace` builds them; `cargo test -p stress` does not.
fn require_tools() {
    let dir = bin().parent().unwrap().to_path_buf();
    for t in ["irsync", "filegen", "fileset"] {
        assert!(dir.join(t).exists(), "{} missing: run `cargo build --workspace` first", dir.join(t).display());
    }
}

fn run_harness(direction: &str, root: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(["--direction", direction, "--iterations", "2", "--steps", "20", "--seed", "7",
               "--max-total-size", "8M", "--max-file-size", "1M",
               "--filegen-set", "names.max_len=200"])   // avoids the known NAME_MAX bug (spec: "Known irsync bug")
        .args(extra)
        .arg(root)
        .env_remove("IRSYNC_PASSPHRASE").env_remove("IRSYNC_CONFIG")
        .output().unwrap()
}

#[test]
fn push_two_iterations_pass_and_leave_the_artefacts() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let o = run_harness("push", &root, &[]);
    assert!(o.status.success(), "stdout:\n{}\nstderr:\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("iter 0001/0002") && out.contains("iter 0002/0002") && out.contains("check ok"));
    for f in ["run.toml", "logs/daemon.log", "logs/ping.client.log", "logs/iter-0001.client.log", "logs/iter-0002.client.log",
              "logs/iter-0001.check.txt", "logs/iter-0002.manifest.blake3", "DST/data/.fileset.sqlite", "SRC/data/.fileset.sqlite",
              "filegen/filegen-7.state.yaml", "filegen/filegen-7.log"] {
        assert!(root.join(f).exists(), "{f} missing");
    }
    let daemon_log = std::fs::read_to_string(root.join("logs/daemon.log")).unwrap();
    assert!(daemon_log.contains("=== ping ===") && daemon_log.contains("=== iteration 2 ==="));
    let run = std::fs::read_to_string(root.join("run.toml")).unwrap();
    assert!(run.contains("seed = 7") && run.contains("direction = \"push\"") && run.contains("endpoint_id"));
}

#[test]
fn pull_two_iterations_pass() { /* same with "pull"; also assert DST/data/.irsync-stress-ping exists */ }

#[test]
fn non_empty_root_exits_2() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("x"), b"").unwrap();
    let o = run_harness("push", t.path(), &[]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn sigint_terminates_the_daemon() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let mut child = Command::new(bin())
        .args(["--direction", "push", "--iterations", "1000", "--steps", "200", "--seed", "7",
               "--max-total-size", "64M", "--max-file-size", "4M", "--filegen-set", "names.max_len=200"])
        .arg(&root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // wait until run.toml names the daemon pid (written once startup is parsed)
    let pid = wait_for_daemon_pid(&root.join("run.toml"), Duration::from_secs(30));
    nix::sys::signal::kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    // the daemon is gone (poll up to 5 s: the handler signals it, the daemon exits on its own SIGINT path)
    assert!(wait_until_gone(pid, Duration::from_secs(5)));
    assert!(root.exists());   // never deleted
}
```

`wait_for_daemon_pid` polls `RunFile::read` (or a string search for `pid = `) every 100 ms; `wait_until_gone` polls `kill(pid, None)` until it errors. The test reads `pid` from `run.toml`, so Task 2's `DaemonInfo.pid` is what makes this test possible.

- [ ] **Step 2: Build the workspace and run the e2e test** — `cargo build --workspace && cargo test -p stress --test e2e` — expected: compile failure (no `run`).
- [ ] **Step 3: Implement `signals.rs` and `run.rs`**, wire `main.rs`. The loop in `run`:
  1. `Layout::create`, `Tools::locate`, seed = `cli.seed.unwrap_or_else(rand::random)`, write `RunFile` with `daemon: None`.
  2. `signals::install()`.
  3. `client_keygen` into the client side's config dir; `write_config` for the daemon side; `DaemonLog::create`; `Daemon::start` (timeout 15 s); store pid in the signal slot; update and rewrite `RunFile` with `DaemonInfo`; print `header`.
  4. Ping: write the sentinel (`seed=<seed> started=<timestamp>\n`), `daemon.marker("ping")`, `run_sync` to `ping_client_log`; exit must be `Ok` and `DST/data/.irsync-stress-ping` must equal the source bytes, else `Err`.
  5. `filegen::run(fill_args(...))`, `fileset::init(src_data)`.
  6. For `i in 1..=iterations`: `daemon.check_alive()?`, `marker("iteration i")`, `run_sync` to `client_log(i)`; `SyncExit::Other(c)` → `Err(anyhow!("irsync client exited {c}"))`; `compare_manifests`; `fileset::check` to `check_report(i)`; build `Iteration`, `println!(line)`, push; if `!passed && !cli.keep_going` → break; if `i < iterations` → `filegen::run(mutate_args(...))`, `fileset::update(src_data)`.
  7. Clear the signal slot, `daemon.stop(10 s)`, print `footer`, return `exit_code(&iterations)`.
  Every `?` leaves the daemon to `Drop`, which stops it. On a failed iteration without `--keep-going`, print the check report path after the line.
- [ ] **Step 4: Run** `cargo build --workspace && cargo test -p stress && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check` — expected: all tests pass, including the four e2e tests, clean.
- [ ] **Step 5: Run the whole workspace** `cargo test --workspace` — expected: pass.
- [ ] **Step 6: Commit** `feat(stress): the stress loop, signal handling and end-to-end test`.

---

### Task 10: Documentation, decisions file, acceptance run

**Files:**
- Create: `crates/stress/README.md`
- Create: `docs/IRSYNC_HARNESS_DECISIONS.md`
- Modify: `README.md` (workspace, after the `## filegen` section)

- [ ] **Step 1: Write `crates/stress/README.md`** in the style of `crates/filegen/README.md`: one-paragraph purpose linking the spec; the command line with every flag and the `--` passthrough; the layout tree from the spec; "After a failure" listing the four things to look at (`run.toml`, both manifests, `logs/iter-NNNN.check.txt`, `logs/iter-NNNN.client.log` and the marker in `logs/daemon.log`); exit codes; a note that `cargo test -p stress` needs `cargo build --workspace` first; the known NAME_MAX bug and the `--filegen-set names.max_len=200` workaround.
- [ ] **Step 2: Add a `## irsync-stress` section to the workspace `README.md`**, three to five lines, pointing at the crate README.
- [ ] **Step 3: Create `docs/IRSYNC_HARNESS_DECISIONS.md`** with the header paragraph in the style of `docs/FILEGEN_DECISIONS.md` (naming the spec and this plan), a "left to the builder" section listing the calls made in Tasks 1–9 (timeouts, sentinel name, pid in `run.toml`, the CI test's `names.max_len=200` which the spec's CI invocation did not have, SIGINT handler behaviour and its limit: a client or filegen child in flight when the harness is signalled from outside the terminal is not killed by the harness), and a "Known irsync bugs found by the harness" section seeded with the NAME_MAX entry from the spec.
- [ ] **Step 4: Acceptance run.** Build release binaries (`cargo build --workspace --release`) and run, from the `target/release` directory so the tools are found next to the binary:

```
./irsync-stress --direction push --iterations 300 --keep-going --seed 1 /tmp/stress-push 2>&1 | tee /tmp/stress-push.out
./irsync-stress --direction pull --iterations 300 --keep-going --seed 2 /tmp/stress-pull 2>&1 | tee /tmp/stress-pull.out
```

Expected: both runs reach the footer. Some iterations report `exit 23` from the NAME_MAX bug; every failed iteration must be explained by its `check.txt` and `client.log` (either the NAME_MAX bug, or a new finding). Record in the decisions file: the commands, the pass/fail counts, the explanation of each failure class, and any new irsync bug as its own entry.

- [ ] **Step 5: Run** `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace` — expected: clean.
- [ ] **Step 6: Commit** `docs(stress): README, decisions file and acceptance run`.
