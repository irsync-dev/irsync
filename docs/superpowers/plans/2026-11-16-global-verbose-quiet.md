# Global `-v` and `-q` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `-v` and `-q` are accepted before or after any irsync command and follow one rule: `-v` is the command's own extra output, `-vv` and up raise the log level.

**Architecture:** The two flags become clap global arguments on the top-level `Args`, and `args_conflicts_with_subcommands` is removed so clap finds a command after them. A check after parsing rejects every other sync flag given with a command. `main` hands the one global pair to every command, and every command passes the count minus one to `log_default`.

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
- Levels, for every command: no flag and `-v` warnings; `-vv` info; `-vvv` debug; `-vvvv` trace; `-vvvvv` trace plus protocol messages; `-q` errors only. That is `log_default(verbose.saturating_sub(1), quiet)`.
- Everyday test run: `cargo test -q --workspace --all-features --no-fail-fast`. Before the last commit: `tools/ci.sh check`.

## Review Focus

Each line has a test in the task named.

1. A flag the command also has, given before it (`irsync -n gc`, `irsync --config f host list`): must fail with the flag named, not run with the flag dropped. Task 1.
2. A sync option with a value before a command (`irsync --bwlimit 5 gc`): must fail the same way. Task 1.
3. A management command run with no flag, on a config that works today: same stdout, same exit code, nothing new on stderr. Task 2.
4. `irsync gc -v` in a script that expected info logs: stderr is now empty; `-vv` brings them back. Task 2.
5. `ssh-setup` now sets up logging. The client reads its report over ssh. Its stdout must not change with `-vv`. Task 2.

## Files

- `crates/irsync/src/cli/mod.rs`: the flag definitions, the check, help text.
- `crates/irsync/src/main.rs`: hands the global pair to the commands, sets up logging.
- `crates/irsync/tests/cli.rs`: parser tests.
- `crates/irsync/tests/commands_e2e.rs`, `crates/irsync/tests/ssh_setup.rs`: tests of the real binary.
- `book/src/reference/command-line.md`, `book/src/reference/management.md`: reference docs.

---

### Task 1: The flags are global and the parser accepts them anywhere

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs`
- Modify: `crates/irsync/src/main.rs` (only so it compiles; behaviour stays)
- Test: `crates/irsync/tests/cli.rs`

**Interfaces:**
- Produces: `Args { cmd: Option<Command>, log: LogArgs, sync: SyncArgs }`, with `LogArgs { verbose: u8, quiet: bool }` (`Copy`). `SyncArgs` has no `verbose`/`quiet`. `Command::Gc { dry_run, files }`, `ExportCmd::Update { dry_run, names }`, `IndexArgs` and `DaemonArgs` have no `log` field. `SyncOptions.verbose`/`.quiet` are unchanged for their users.

- [ ] **Step 1: Replace the old test and write the failing tests**

In `tests/cli.rs`, delete `subcommands_take_verbose_and_quiet`, and in the `export update` test near line 470 drop `log` from the pattern and assert `a.log.verbose == 1 && !a.log.quiet` instead. Add:

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
fn a_sync_option_with_a_command_is_rejected_by_name() {
    assert!(err(&["-a", "gc"]).contains("-a is a sync option and cannot be used with 'gc'"));
    assert!(err(&["-n", "gc"]).contains("-n is a sync option and cannot be used with 'gc'"));
    assert!(err(&["-av", "id", "show"]).contains("-a is a sync option and cannot be used with 'id'"));
    assert!(err(&["--bwlimit", "5", "gc"])
        .contains("--bwlimit is a sync option and cannot be used with 'gc'"));
    assert!(err(&["--config", "f", "host", "list"])
        .contains("--config is a sync option and cannot be used with 'host'"));
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
- `SyncOptions::from_args`: `verbose: args.log.verbose, quiet: args.log.quiet`.
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

`main` copies `args.log` and passes it on: `run_daemon(d, log)`, `run_gc(*dry_run, log, files)`, `run_index(ix, log)`, and `run_export(e, log)` which passes it to `export_update`. Inside, `d.log`/`a.log` become `log`. Nothing else changes in this task.

- [ ] **Step 6: Run the tests**

Run: `cargo test -q -p irsync --test cli` then `cargo test -q --workspace --all-features --no-fail-fast`
Expected: all pass. If a test of `--help` output fails, read the new usage line of `irsync --help`: it must still show `[OPTIONS] [SRC... DEST]...` and the commands; set `override_usage` on `Args` only if it reads wrong.

- [ ] **Step 7: Commit**

```bash
git add crates/irsync/src/cli/mod.rs crates/irsync/src/main.rs crates/irsync/tests/cli.rs
git commit -m "Accept -v and -q before or after any command

irsync -v id show was read as a sync of two paths (#31). The flags are
now global, and a sync option given with a command is rejected by name."
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
Expected: `vv_turns_logging_on_for_every_command` fails for `id show` (no log line), `one_v_on_gc_logs_nothing_and_quiet_drops_the_summary` fails (`gc -v` logs `starting irsync`).

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

### Task 3: Reference docs

**Files:**
- Modify: `book/src/reference/command-line.md`
- Modify: `book/src/reference/management.md`

No tests: a book-only change.

- [ ] **Step 1: `command-line.md`**

- Add a short section before `### irsync daemon`, titled `### -v and -q on every command`, that says: both flags are accepted before or after any command (`irsync -v gc`, `irsync id show -vv`); `-v` asks for the command's own extra output and shows nothing more where a command has none; `-vv` gives info logs, `-vvv` debug, `-vvvv` trace, `-vvvvv` trace plus every protocol message; `-q` logs errors only and drops summaries, and wins over `-v`; `RUST_LOG` overrides the level; any other sync option before a command is an error.
- `irsync daemon`: in the usage block `[-v|-q]` becomes `[-v] [-q]`; the two table rows point at the new section (`-v`: "Nothing extra; `-vv` and up log more.").
- `irsync index` and `irsync gc`: same usage change; `-v` row becomes "Nothing extra; `-vv` and up log more."; `-q` row stays.
- The row for `host`, `export`, `grant`, `id`, `config`, `show` in the commands table: add that they take `-v` and `-q` too.

- [ ] **Step 2: `management.md`**

`irsync export update [-n] [-v\|-q] NAME...` becomes `irsync export update [-n] [-v] [-q] NAME...`. Add one sentence near the top of the page: every command here accepts `-v` and `-q`, before or after it; see the command-line reference.

- [ ] **Step 3: Full check and commit**

Run: `tools/ci.sh check`
Expected: passes.

```bash
git add book/src/reference/command-line.md book/src/reference/management.md
git commit -m "Document -v and -q on every command"
```
