# Global `-v`, `-q`, `--config` and `--passphrase-file` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `-v`, `-q` and `--config PATH` are accepted before or after any irsync command. `-v`/`-q` follow one rule: `-v` is the command's own extra output, `-vv` and up raise the log level. `--config` names the same file everywhere. `--passphrase-file` is global too, and `id generate` encrypts the new key with it.

**Architecture:** The four flags become clap global arguments on the top-level `Args` (each defined once), and `args_conflicts_with_subcommands` is removed so clap finds a command after them. A check after parsing rejects every other sync flag given with a command. `main` hands the global values to every command, and every command passes the count minus one to `log_default`. A new `generate_openssh_with` takes an optional passphrase, and only `id generate` passes one.

**Tech Stack:** Rust, clap 4.6.7 (derive), tracing. Tests with `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-16-global-verbose-quiet-design.md` (issue #31)

## Global Constraints

- Work on the branch `global-verbose-quiet`.
- Commit messages: a short summary line, a blank line, a brief plain-English why. No `Co-Authored-By`, no "Generated with" line, no attribution of any kind.
- A sync prints exactly what it prints today.
- `irsync -a gc` must fail (exit 2); a flag before a command is never dropped silently.
- `irsync -a myserver:photos/ config` and `irsync src gc` stay syncs.
- `-v` with `-q` is accepted on every command, and `-q` wins.
- `RUST_LOG` still overrides the level.
- `id generate --passphrase-file` never writes an unencrypted key: an unreadable file or an empty first line is an error (exit 1) and nothing is written or removed.
- Keys generated on the side (first sync, `id show`, `id list`, daemon start, ssh setup) stay unencrypted, with or without the flag.
- `IRSYNC_PASSPHRASE` and the prompt never encrypt a new key.
- Levels, for every command: no flag and `-v` warnings; `-vv` info; `-vvv` debug; `-vvvv` trace; `-vvvvv` trace plus protocol messages; `-q` errors only. That is `log_default(verbose.saturating_sub(1), quiet)`.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the last commit: `tools/ci.sh check`.

## Review Focus

Each line has a test in the task named.

1. A flag the command also has, given before it (`irsync -n gc`): must fail with the flag named, not run with the flag dropped. Task 1.
2. A sync option with a value before a command (`irsync --bwlimit 5 gc`): must fail the same way. Task 1.
3. A management command run with no flag, on a config that works today: same stdout, same exit code, nothing new on stderr. Task 2.
4. `irsync gc -v` in a script that expected info logs: stderr is now empty; `-vv` brings them back. Task 2.
5. `--config` used to be three separate flags. Every command must read the one global value wherever it was typed; a command that silently falls back to the default file would edit the wrong config. Task 1 (parser) and Task 2 (`--config` before a management command, on the real binary).
6. `ssh-setup` now sets up logging. The client reads its report over ssh. Its stdout must not change with `-vv`. Task 2.
7. A passphrase file with a trailing newline, with `\r\n`, or with more lines: the key must unlock with the same file through `id show --passphrase-file`. Task 3.
8. `id generate --force --passphrase-file` with an empty or missing passphrase file: the existing key must still be there afterwards. Task 3.
9. `IRSYNC_PASSPHRASE` set while generating without the flag: the key is unencrypted. Task 3.

## Files

- `crates/irsync/src/cli/mod.rs`: the flag definitions, the check, help text.
- `crates/irsync/src/identity.rs`: reading the passphrase file, encrypting a generated key.
- `crates/irsync/src/main.rs`: hands the global pair to the commands, sets up logging.
- `crates/irsync/tests/cli.rs`: parser tests.
- `crates/irsync/tests/commands_e2e.rs`, `crates/irsync/tests/ssh_setup.rs`: tests of the real binary.
- `book/src/reference/command-line.md`, `book/src/reference/management.md`, `book/src/reference/configuration.md`: reference docs.

---

### Task 1: The four flags are global and the parser accepts them anywhere

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs`
- Modify: `crates/irsync/src/main.rs` (only so it compiles; behaviour stays)
- Test: `crates/irsync/tests/cli.rs`

**Interfaces:**
- Produces: `Args { cmd: Option<Command>, log: LogArgs, files: FilesArgs, sync: SyncArgs }`, with `LogArgs { verbose: u8, quiet: bool }` (`Copy`) and `FilesArgs { config: Option<PathBuf>, passphrase_file: Option<PathBuf> }`. `SyncArgs` has no `verbose`, `quiet`, `config` or `passphrase_file`. No command struct or variant has a `log`, `files`, `config` or `passphrase_file` field: `Command::Gc { dry_run }`, `Command::Show` (a unit variant), `Command::SshSetup { client, recheck, no_dht }`, `ExportCmd::Update { dry_run, names }`, `IdCmd::List` (unit), `IdCmd::Show { daemon, all }`. `SyncOptions.verbose`, `.quiet`, `.config` and `.passphrase_file` are unchanged for their users.

- [ ] **Step 1: Replace the old test and write the failing tests**

In `tests/cli.rs`, delete `subcommands_take_verbose_and_quiet`, and in the `export update` test near line 470 drop `log` from the pattern and assert `a.log.verbose == 1 && !a.log.quiet` instead. The existing assertions on `d.config`, `e.files.config`, `ix.files.config`, `h.files.config`, `g.files.config`, `i.files.config` and `c.files.config` (near lines 207, 223, 235, 357, 618, 624, 657, 771) become assertions on `a.files.config` of the parsed `Args`. Existing assertions on a command's `passphrase_file` (the `show --passphrase-file /p` one near line 715 and any on `id show`, `id list` or `daemon`) become assertions on `a.files.passphrase_file`. Add:

```rust
fn err(a: &[&str]) -> String {
    let mut v = vec!["irsync"];
    v.extend_from_slice(a);
    Args::try_parse_from(v).unwrap_err().to_string()
}

#[test]
fn verbose_and_quiet_are_accepted_before_and_after_any_command() {
    let a = parse(&["-v", "gc"]);
    assert!(matches!(a.cmd, Some(Command::Gc { .. })));
    assert_eq!((a.log.verbose, a.log.quiet), (1, false));
    let a = parse(&["gc", "-n", "-q"]);
    assert!(matches!(a.cmd, Some(Command::Gc { dry_run: true, .. })));
    assert_eq!((a.log.verbose, a.log.quiet), (0, true));
    for args in [
        &["-vv", "id", "show"][..],
        &["id", "-vv", "show"],
        &["id", "show", "-vv"],
        &["id", "show", "--verbose", "--verbose"],
    ] {
        let a = parse(args);
        assert!(matches!(a.cmd, Some(Command::Id(_))), "{args:?}");
        assert_eq!(a.log.verbose, 2, "{args:?}");
    }
    assert!(parse(&["--quiet", "host", "list"]).log.quiet);
    assert_eq!(parse(&["daemon", "-vv"]).log.verbose, 2);
    assert_eq!(parse(&["index", "-v", "/srv/a"]).log.verbose, 1);
    assert_eq!(parse(&["-v", "show"]).log.verbose, 1);
    assert_eq!(parse(&["-v", "config", "get", "daemon.listen"]).log.verbose, 1);
}

#[test]
fn verbose_with_quiet_is_accepted_everywhere() {
    for args in [&["daemon", "-v", "-q"][..], &["-q", "gc", "-v"], &["-vq", "src", "h:m/"]] {
        let a = parse(args);
        assert_eq!((a.log.verbose, a.log.quiet), (1, true), "{args:?}");
    }
}

#[test]
fn a_sync_still_reads_verbose_and_quiet() {
    let o = opts(&["-avv", "src", "h:m/"]);
    assert_eq!((o.verbose, o.quiet), (2, false));
    assert!(opts(&["-q", "src", "h:m/"]).quiet);
    let a = parse(&["-v", "home:photos/"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["home:photos/"] && a.log.verbose == 1);
}

#[test]
fn a_path_named_like_a_command_is_still_a_path() {
    let a = parse(&["-a", "myserver:photos/", "config"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["myserver:photos/", "config"]);
    let a = parse(&["src", "gc"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["src", "gc"]);
    let a = parse(&["-v", "./gc", "h:m/"]);
    assert!(a.cmd.is_none() && a.sync.paths == ["./gc", "h:m/"]);
}

#[test]
fn config_is_accepted_before_and_after_any_command() {
    let f = Some(PathBuf::from("f"));
    for args in [
        &["--config", "f", "host", "list"][..],
        &["host", "--config", "f", "list"],
        &["host", "list", "--config", "f"],
        &["--config", "f", "daemon"],
        &["daemon", "--config", "f"],
        &["--config", "f", "gc"],
        &["index", "--config", "f", "/srv/a"],
        &["-v", "--config", "f", "id", "show"],
        &["show", "--config=f"],
    ] {
        let a = parse(args);
        assert!(a.cmd.is_some(), "{args:?}");
        assert_eq!(a.files.config, f, "{args:?}");
    }
    assert_eq!(opts(&["--config", "f", "src", "h:m/"]).config, f);
    assert_eq!(opts(&["src", "h:m/", "--config", "f"]).config, f);
    assert_eq!(parse(&["host", "list"]).files.config, None);
}

#[test]
fn passphrase_file_is_accepted_before_and_after_any_command() {
    let f = Some(PathBuf::from("p"));
    for args in [
        &["--passphrase-file", "p", "id", "show"][..],
        &["id", "--passphrase-file", "p", "show"],
        &["id", "show", "--passphrase-file", "p"],
        &["id", "list", "--passphrase-file", "p"],
        &["id", "generate", "--passphrase-file", "p"],
        &["id", "generate", "--daemon", "--force", "--passphrase-file=p"],
        &["show", "--passphrase-file", "p"],
        &["--passphrase-file", "p", "daemon"],
        &["daemon", "--passphrase-file", "p"],
        // Accepted and ignored where no key is loaded.
        &["gc", "--passphrase-file", "p"],
        &["--passphrase-file", "p", "host", "list"],
    ] {
        let a = parse(args);
        assert!(a.cmd.is_some(), "{args:?}");
        assert_eq!(a.files.passphrase_file, f, "{args:?}");
    }
    assert_eq!(opts(&["--passphrase-file", "p", "src", "h:m/"]).passphrase_file, f);
    assert_eq!(parse(&["id", "show"]).files.passphrase_file, None);
}

#[test]
fn a_sync_option_with_a_command_is_rejected_by_name() {
    assert!(err(&["-a", "gc"]).contains("-a is a sync option and cannot be used with 'gc'"));
    assert!(err(&["-n", "gc"]).contains("-n is a sync option and cannot be used with 'gc'"));
    assert!(err(&["-av", "id", "show"]).contains("-a is a sync option and cannot be used with 'id'"));
    assert!(err(&["--bwlimit", "5", "gc"])
        .contains("--bwlimit is a sync option and cannot be used with 'gc'"));
    let e = Args::try_parse_from(["irsync", "-a", "gc"]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    // After the command, the command's own flags are untouched.
    assert!(matches!(parse(&["gc", "-n"]).cmd, Some(Command::Gc { dry_run: true, .. })));
    // `-v gc host:path` is `gc` with an argument it does not take.
    assert!(Args::try_parse_from(["irsync", "-v", "gc", "host:path"]).is_err());
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p irsync --test cli 2>&1 | tail -20`
Expected: does not compile (`no field log on Args`).

- [ ] **Step 3: Change the definitions in `src/cli/mod.rs`**

- Remove `args_conflicts_with_subcommands = true` from `Args`; keep `disable_help_subcommand = true`.
- Add `#[command(flatten)] pub log: LogArgs` to `Args`, between `cmd` and `sync`.
- `LogArgs`: both fields get `global = true`; remove `conflicts_with = "quiet"`. Doc comments (they are the help text on every command):
  - `verbose`: `More output. -v: the command's own extra output (a sync lists each file transferred and ends with a transfer summary). -vv adds info logging (and a sync's summary per session), -vvv debug, -vvvv trace, -vvvvv trace plus every protocol message.`
  - `quiet`: `Print errors only, and no summary or progress. Wins over -v.`
  - The struct's own comment: `How much any command says. Global: accepted before or after the command.`
- Remove `verbose` and `quiet` from `SyncArgs`, and the `log` field from `Command::Gc`, `ExportCmd::Update`, `IndexArgs` and `DaemonArgs`.
- Add `#[command(flatten)] pub files: FilesArgs` to `Args`, after `log`. `FilesArgs` keeps `global = true`; its comment becomes `The config file every command reads. Global: accepted before or after the command.` and the flag's help stays `The config file (default: irsync.toml in the config directory).` It gains a second field, `passphrase_file`, `#[arg(long, global = true, value_name = "PATH")]`, with the help `Read an identity's passphrase from the first line of this file (else IRSYNC_PASSPHRASE, else a prompt on a terminal). id generate encrypts the new key with it.`
- Remove `passphrase_file` from `SyncArgs`, `DaemonArgs`, `Command::Show`, `IdCmd::List` and `IdCmd::Show`.
- Remove `config` from `SyncArgs` and `DaemonArgs`, and the `files` field from `Command::Gc`, `Command::Show`, `Command::SshSetup`, `IndexArgs`, `ConfigArgs`, `IdArgs`, `HostArgs`, `ExportArgs` and `GrantArgs`.
- `SyncOptions::from_args`: `verbose: args.log.verbose, quiet: args.log.quiet, config: args.files.config.clone(), passphrase_file: args.files.passphrase_file.clone()`.
- The `Daemon` doc comment: replace `Logs go to stderr at info level (-v: debug, -vv: trace, -q: errors only; RUST_LOG overrides).` with `Logs go to stderr at warning level (-vv: info, -vvv: debug, -vvvv: trace, -q: errors only; RUST_LOG overrides).`

- [ ] **Step 4: Add the check**

In `src/cli/mod.rs`, called from `Args::try_parse_from` right after `try_get_matches_from`:

```rust
/// With a command, every sync argument set on the command line is an
/// error: clap no longer rejects it once `-v`/`-q` may come first.
fn reject_sync_args_with_command(m: &clap::ArgMatches) -> Result<(), clap::Error>
```

For each argument of `SyncArgs` (take the ids from `SyncArgs::augment_args(clap::Command::new("sync")).get_arguments()`), when `m.subcommand_name()` is `Some(cmd)` and `m.value_source(id) == Some(ValueSource::CommandLine)`, return `Args::command().error(ErrorKind::ArgumentConflict, format!("{flag} is a sync option and cannot be used with '{cmd}'"))`. `flag` is `-x` when the argument has a short, else `--long`. Report the first one in `SyncArgs` declaration order.

- [ ] **Step 5: Make `main.rs` compile without changing behaviour**

`main` copies `args.log` and passes it on: `run_daemon(d, log, files)`, `run_gc(*dry_run, log, files)`, `run_index(ix, log, files)`, and `run_export(e, log, files)` which passes `log` to `export_update`. Inside, `d.log`/`a.log` become `log`. `files` is `&args.files`: every `run_*` function that read its own `files` field (`run_host`, `run_export`, `run_grant`, `run_id`, `run_config`, `run_show`, `run_ssh_setup`, `run_gc`, `run_index`) takes it as a `&FilesArgs` parameter instead, and `run_daemon` reads `files.config` where it read `d.config`. The same for the passphrase file: `run_daemon`, `run_show`, `id_list` and `id_show` read `files.passphrase_file` where they read their own field or parameter. `id_generate` does not use it yet (Task 3). Nothing else changes in this task.

- [ ] **Step 6: Run the tests**

Run: `cargo test -q -p irsync --test cli` then `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass. If a test of `--help` output fails, read the new usage line of `irsync --help`: it must still show `[OPTIONS] [SRC... DEST]...` and the commands; set `override_usage` on `Args` only if it reads wrong.

- [ ] **Step 7: Commit**

```bash
git add crates/irsync/src/cli/mod.rs crates/irsync/src/main.rs crates/irsync/tests/cli.rs
git commit -m "Accept -v, -q, --config and --passphrase-file on any command

irsync -v id show was read as a sync of two paths (#31). The four
flags are now global and defined once, and a sync option given with a
command is rejected by name."
```

---

### Task 2: One meaning for the flags on every command

**Files:**
- Modify: `crates/irsync/src/main.rs`
- Test: `crates/irsync/tests/commands_e2e.rs`, `crates/irsync/tests/ssh_setup.rs`

**Interfaces:**
- Consumes: `args.log: LogArgs` from Task 1.
- Produces: `fn init_command_logging(log: LogArgs)` in `main.rs`.

- [ ] **Step 1: Write the failing tests**

In `tests/commands_e2e.rs`. Add `.env_remove("RUST_LOG")` to the `irsync` helper at the top of the file. `home` is a fresh temp directory as in the other tests of the file.

```rust
#[test]
fn verbose_before_a_management_command_runs_the_command() {
    // plain = irsync(&home, &["id", "show"]), which generates the client identity
    for args in [&["-v", "id", "show"][..], &["id", "show", "-v"], &["-q", "id", "show"]] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert_eq!(o.stdout, plain.stdout, "{args:?}");
        assert!(o.stderr.is_empty(), "{args:?}");
    }
    assert!(plain.stderr.is_empty());
    let o = irsync(&home, &["host", "list", "-v"]);
    assert!(o.status.success() && o.stderr.is_empty());
}

#[test]
fn config_before_a_command_names_the_file_the_command_uses() {
    // other = home.join("other.toml"); it does not exist yet.
    let other_s = other.to_str().unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(&home, &["--config", other_s, "host", "add", "laptop", &id]).status.success());
    assert!(std::fs::read_to_string(&other).unwrap().contains("[hosts.laptop]"));
    assert!(!home.join(".config/irsync/irsync.toml").exists());
    let before = irsync(&home, &["--config", other_s, "host", "list"]);
    let after = irsync(&home, &["host", "list", "--config", other_s]);
    assert!(before.status.success() && before.stdout == after.stdout);
    assert!(String::from_utf8_lossy(&before.stdout).contains("laptop"));
    assert!(!String::from_utf8_lossy(&irsync(&home, &["host", "list"]).stdout).contains("laptop"));
}

#[test]
fn vv_turns_logging_on_for_every_command() {
    for args in [&["gc", "-vv"][..], &["-vv", "id", "show"], &["host", "list", "-vv"], &["show", "-vv"]] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains("starting irsync"), "{args:?}");
    }
    // stdout is the same with and without logging.
    assert_eq!(irsync(&home, &["-vv", "id", "show"]).stdout, irsync(&home, &["id", "show"]).stdout);
}

#[test]
fn one_v_on_gc_logs_nothing_and_quiet_drops_the_summary() {
    let o = irsync(&home, &["gc", "-v"]);
    assert!(o.status.success() && o.stderr.is_empty());
    assert!(String::from_utf8_lossy(&o.stdout).contains("0 part file(s)"));
    let o = irsync(&home, &["-q", "gc"]);
    assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty());
    let o = irsync(&home, &["gc", "-vv", "-q"]);
    assert!(o.stdout.is_empty() && o.stderr.is_empty());
}

#[test]
fn a_sync_option_before_a_command_exits_2() {
    let o = irsync(&home, &["-a", "gc"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("-a is a sync option and cannot be used with 'gc'"));
}

```

And in `tests/ssh_setup.rs`, which has the `Home` helper (`setup`, `setup_with`) that runs `ssh-setup` and parses its report:

```rust
#[test]
fn the_report_still_parses_with_logging_on() {
    // home: a `Home` built as in the neighbouring tests; client: any EndpointId.
    let plain = home.setup(&client);
    let logged = home.setup_with(&client, &["-vv"]);
    // `setup_with` already asserts success and a report that parses.
    assert_eq!(format!("{logged:?}"), format!("{plain:?}"));
}
```

If the two reports differ for a reason other than logging (the daemon was started by the first run), compare `logged` with the report of a second plain run instead. If `SetupReport` has no `Debug`, compare the fields the neighbouring tests compare.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p irsync --test commands_e2e 2>&1 | tail -20` and `cargo test -p irsync --test ssh_setup the_report_still 2>&1 | tail`
Expected: `config_before_a_command_names_the_file_the_command_uses` already passes (Task 1 did the work; this pins it on the real binary). `vv_turns_logging_on_for_every_command` fails for `id show` (no log line), `one_v_on_gc_logs_nothing_and_quiet_drops_the_summary` fails (`gc -v` logs `starting irsync`).

- [ ] **Step 3: Implement**

In `main.rs`:

```rust
/// Logging for every command but a sync and the daemon, which set up
/// their own: `-v` alone is still warnings, as for a sync.
fn init_command_logging(log: LogArgs) {
    init_logging(log.verbose.saturating_sub(1), log.quiet, LogWriter::new());
}
```

Call it in `main` in the arms of `Gc`, `Index`, `Host`, `Export`, `Grant`, `Id`, `Config`, `Show` and `SshSetup`, before the `run_*` call. Remove the `init_logging` calls from `run_gc`, `run_index` and `export_update` (a second call would install a second subscriber). Those three keep using `log.quiet` to drop their summary lines. The daemon and the sync are untouched: they already subtract one.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass. A management test that now sees an unexpected warning on stderr is a real finding: report it, do not silence it.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/main.rs crates/irsync/tests/commands_e2e.rs crates/irsync/tests/ssh_setup.rs
git commit -m "Count -v the same way on every command

-v is the command's own extra output and -vv and up raise the log
level, as a sync already does. gc, index and export update used to log
at info with one -v; the management commands had no logging at all."
```

---

### Task 3: `id generate` encrypts the key with `--passphrase-file`

**Files:**
- Modify: `crates/irsync/src/identity.rs`
- Modify: `crates/irsync/src/main.rs` (`id_generate`)
- Test: `crates/irsync/tests/identity.rs`, `crates/irsync/tests/commands_e2e.rs`

**Interfaces:**
- Consumes: `FilesArgs.passphrase_file: Option<PathBuf>` from Task 1.
- Produces, in `identity.rs`:
  - `pub fn read_passphrase_file(path: &Path) -> Result<String, IdentityError>`: the first line, without its line ending (`str::lines`, so `\r\n` too). An empty first line or an empty file is `Ok("")`.
  - `IdentityError::EmptyPassphraseFile(PathBuf)`, message `passphrase file {0} is empty`.
  - `pub fn generate_openssh_with(path: &Path, comment: &str, passphrase: Option<&str>) -> Result<SecretKey, IdentityError>`. The existing `generate_openssh(path, comment)` keeps its signature and calls it with `None`, so its many callers do not change.

- [ ] **Step 1: Write the failing library tests**

In `tests/identity.rs` (it already uses `ssh_key::PrivateKey` and `tempfile`; add the new names to its `use irsync::identity::{...}`):

```rust
#[test]
fn generate_with_a_passphrase_writes_an_encrypted_key_that_unlocks() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("id_ed25519");
    let secret = generate_openssh_with(&path, "irsync", Some("hunter2")).unwrap();
    assert!(PrivateKey::read_openssh_file(&path).unwrap().is_encrypted());
    assert!(matches!(load_secret_key(&path, None), Err(IdentityError::PassphraseRequired)));
    assert!(load_secret_key(&path, Some("wrong")).is_err());
    assert_eq!(load_secret_key(&path, Some("hunter2")).unwrap().public(), secret.public());
    // The .pub file is the public half of the same key.
    let line = std::fs::read_to_string(d.path().join("id_ed25519.pub")).unwrap();
    assert_eq!(line.trim(), public_key_line(&secret, "irsync"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn generate_without_a_passphrase_is_unencrypted_as_before() {
    let d = tempfile::tempdir().unwrap();
    for (name, secret) in [
        ("a", generate_openssh_with(&d.path().join("a"), "irsync", None).unwrap()),
        ("b", generate_openssh(&d.path().join("b"), "irsync").unwrap()),
    ] {
        let path = d.path().join(name);
        assert!(!PrivateKey::read_openssh_file(&path).unwrap().is_encrypted());
        assert_eq!(load_secret_key(&path, None).unwrap().public(), secret.public());
    }
}

#[test]
fn the_passphrase_file_is_read_up_to_its_first_line_ending() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("p");
    for (text, want) in [
        ("hunter2", "hunter2"),
        ("hunter2\n", "hunter2"),
        ("hunter2\r\n", "hunter2"),
        ("hunter2\nsecond line\n", "hunter2"),
        (" two words ", " two words "),
        ("", ""),
        ("\nhunter2\n", ""),
    ] {
        std::fs::write(&f, text).unwrap();
        assert_eq!(read_passphrase_file(&f).unwrap(), want, "{text:?}");
    }
    assert!(read_passphrase_file(&d.path().join("missing")).is_err());
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p irsync --test identity 2>&1 | tail`
Expected: does not compile (no `generate_openssh_with`, no `read_passphrase_file`).

- [ ] **Step 3: Implement in `identity.rs`**

- `read_passphrase_file` as above; `PassphraseSource::passphrase` calls it instead of reading the file itself, so unlocking and generating read the file the same way.
- `generate_openssh_with` is today's `generate_openssh` body plus the passphrase. With `Some(p)`, the text written to `path` is `key.encrypt(&mut OsRng, p)?.to_openssh(LineEnding::LF)?`; the `.pub` line and the returned secret still come from the unencrypted `key`. The exists-checks, the atomic write and the modes do not change.
- `generate_openssh(path, comment)` becomes `generate_openssh_with(path, comment, None)`.

- [ ] **Step 4: Run the library tests**

Run: `cargo test -q -p irsync --test identity`
Expected: pass.

- [ ] **Step 5: Write the failing end-to-end tests**

In `tests/commands_e2e.rs`. The `irsync` helper already removes `IRSYNC_PASSPHRASE` and gives no terminal. `home` is a fresh temp home; `key` is `home.join(".config/irsync/id_ed25519")`; `pub_key` is `home.join(".config/irsync/id_ed25519.pub")`; `pf` is a passphrase file under `home` and `pf_s` its path as `&str`.

```rust
#[test]
fn id_generate_encrypts_the_key_with_the_passphrase_file() {
    std::fs::write(&pf, "hunter2\nnot part of it\n").unwrap();
    let gen = irsync(&home, &["id", "generate", "--passphrase-file", pf_s]);
    assert!(gen.status.success(), "{}", String::from_utf8_lossy(&gen.stderr));
    assert!(ssh_key::PrivateKey::read_openssh_file(&key).unwrap().is_encrypted());
    assert!(String::from_utf8_lossy(&gen.stderr).contains("wrote "));
    // stdout: the public key line, then the id, as without the flag.
    let out = String::from_utf8_lossy(&gen.stdout).to_string();
    let id = out.lines().last().unwrap().to_string();
    assert_eq!(out.lines().count(), 2);
    assert!(out.starts_with("ssh-ed25519 "));

    // The same flag and file unlock it, before or after the command.
    for args in [&["id", "show", "--passphrase-file", pf_s][..], &["--passphrase-file", pf_s, "id", "show"]] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stdout).contains(&id), "{args:?}");
    }
    // Without a passphrase it does not load.
    let o = irsync(&home, &["id", "show"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("is encrypted"));
    // A wrong passphrase is refused.
    let wrong = home.join("wrong");
    std::fs::write(&wrong, "hunter3\n").unwrap();
    assert!(!irsync(&home, &["id", "show", "--passphrase-file", wrong.to_str().unwrap()]).status.success());
}

#[test]
fn id_generate_encrypts_with_daemon_out_and_force() {
    std::fs::write(&pf, "s3cret\r\n").unwrap();
    let o = irsync(&home, &["id", "generate", "--daemon", "--passphrase-file", pf_s]);
    assert!(o.status.success());
    let daemon_key = home.join(".config/irsync/daemon_ed25519");
    assert!(ssh_key::PrivateKey::read_openssh_file(&daemon_key).unwrap().is_encrypted());
    assert!(irsync(&home, &["id", "show", "--daemon", "--passphrase-file", pf_s]).status.success());

    let out = home.join("elsewhere/key");
    let out_s = out.to_str().unwrap();
    assert!(irsync(&home, &["--passphrase-file", pf_s, "id", "generate", "--out", out_s]).status.success());
    let first = std::fs::read(&out).unwrap();
    assert!(ssh_key::PrivateKey::read_openssh_file(&out).unwrap().is_encrypted());
    // --force replaces it, encrypted again.
    assert!(irsync(&home, &["id", "generate", "--out", out_s, "--force", "--passphrase-file", pf_s]).status.success());
    assert_ne!(std::fs::read(&out).unwrap(), first);
    assert!(ssh_key::PrivateKey::read_openssh_file(&out).unwrap().is_encrypted());
}

#[test]
fn id_generate_with_a_bad_passphrase_file_writes_and_removes_nothing() {
    let empty = home.join("empty");
    std::fs::write(&empty, "").unwrap();
    let blank_first = home.join("blank");
    std::fs::write(&blank_first, "\nhunter2\n").unwrap();
    let missing = home.join("missing");
    for (f, msg) in [(&empty, "is empty"), (&blank_first, "is empty"), (&missing, "missing")] {
        let o = irsync(&home, &["id", "generate", "--passphrase-file", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{f:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains(msg), "{f:?}");
        assert!(!key.exists() && !pub_key.exists(), "{f:?}");
        assert!(o.stdout.is_empty());
    }
    // With --force the existing key survives a bad passphrase file.
    assert!(irsync(&home, &["id", "generate"]).status.success());
    let before = std::fs::read(&key).unwrap();
    let o = irsync(&home, &["id", "generate", "--force", "--passphrase-file", empty.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(std::fs::read(&key).unwrap(), before);
}

#[test]
fn only_id_generate_with_the_file_encrypts() {
    // 1. `id generate` with no flag but IRSYNC_PASSPHRASE=hunter2 in the
    //    environment (build this one Command by hand, as the helper removes
    //    the variable): succeeds, and the key is not encrypted.
    assert!(!ssh_key::PrivateKey::read_openssh_file(&key).unwrap().is_encrypted());
    // 2. In a second fresh home: `id show --passphrase-file pf` generates
    //    the client key on the side (stderr has "generated identity"), and
    //    that key is not encrypted.
    assert!(!ssh_key::PrivateKey::read_openssh_file(&key2).unwrap().is_encrypted());
}
```

- [ ] **Step 6: Run them and see them fail**

Run: `cargo test -p irsync --test commands_e2e id_generate only_id_generate 2>&1 | tail -20`
Expected: the first three fail (the key is written unencrypted; the bad files are ignored). `only_id_generate_with_the_file_encrypts` passes already and stays as a guard.

- [ ] **Step 7: Implement in `main.rs`**

`id_generate(f, daemon, out, force)` reads the passphrase first, before it creates a directory or removes a file for `--force`:

- `f.passphrase_file` is `None`: call `generate_openssh_with(&path, comment, None)`. `IRSYNC_PASSPHRASE` is not looked at.
- `Some(p)`: `read_passphrase_file(p)`; a read error is reported with the path in the message (`reading passphrase file {path}`); an empty result is `IdentityError::EmptyPassphraseFile(p)`; otherwise pass `Some(&pass)`.

Output is unchanged.

- [ ] **Step 8: Run the tests**

Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/irsync/src/identity.rs crates/irsync/src/main.rs crates/irsync/tests/identity.rs crates/irsync/tests/commands_e2e.rs
git commit -m "Encrypt the key id generate writes with --passphrase-file

irsync could unlock an encrypted identity but not make one. The
passphrase is the first line of the file, read the same way as for
unlocking."
```

---

### Task 4: Reference docs

**Files:**
- Modify: `book/src/reference/command-line.md`
- Modify: `book/src/reference/management.md`
- Modify: `book/src/reference/configuration.md`

No tests: a book-only change.

- [ ] **Step 1: `command-line.md`**

- Add a short section before `### irsync daemon`, titled `### Flags every command takes`, that says: `-v`, `-q`, `--config PATH` and `--passphrase-file PATH` are accepted before or after any command (`irsync -v gc`, `irsync id show -vv`); `-v` asks for the command's own extra output and shows nothing more where a command has none; `-vv` gives info logs, `-vvv` debug, `-vvvv` trace, `-vvvvv` trace plus every protocol message; `-q` logs errors only and drops summaries, and wins over `-v`; `RUST_LOG` overrides the level; `--config PATH` names the config file for every command (`irsync --config f host list`); `--passphrase-file PATH` gives the passphrase of an encrypted key to the commands that load one, is ignored by the others, and makes `irsync id generate` write the new key encrypted; any other sync option before a command is an error.
- `irsync daemon`: in the usage block `[-v|-q]` becomes `[-v] [-q]`; the two table rows point at the new section (`-v`: "Nothing extra; `-vv` and up log more.").
- `irsync index` and `irsync gc`: same usage change; `-v` row becomes "Nothing extra; `-vv` and up log more."; `-q` row stays.
- The `--config PATH` rows of the sync, `daemon`, `index` and `gc` tables stay; no wording change is needed.
- The row for `host`, `export`, `grant`, `id`, `config`, `show` in the commands table: add that they take `-v`, `-q`, `--config` and `--passphrase-file` too.

- [ ] **Step 2: `management.md`**

`irsync export update [-n] [-v\|-q] NAME...` becomes `irsync export update [-n] [-v] [-q] NAME...`. Add one sentence near the top of the page: every command here accepts `-v` and `-q`, before or after it; see the command-line reference. The paragraph on `--config` (lines 15 to 17) says the flag may stand before or after the verb; add that it may stand before the noun too: `irsync --config C export add ...`.
- The `id` table: `irsync id generate` gains `[--passphrase-file PATH]` in its command column, and its effect says: with it, the private key is written encrypted with the passphrase on the first line of that file; a file that is missing or empty is an error and nothing is written; `IRSYNC_PASSPHRASE` does not encrypt. Keys generated on the side (by a sync, `id show`, the daemon) are never encrypted.

- [ ] **Step 3: `configuration.md`**

The paragraph on encrypted keys near line 123 gains one sentence: `irsync id generate --passphrase-file PATH` makes one.

- [ ] **Step 4: Full check and commit**

Run: `tools/ci.sh check`
Expected: passes.

```bash
git add book/src/reference/command-line.md book/src/reference/management.md book/src/reference/configuration.md
git commit -m "Document the flags every command takes"
```
