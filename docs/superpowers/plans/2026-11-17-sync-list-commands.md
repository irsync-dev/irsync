# `sync` and `list` commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `irsync sync …` and `irsync list HOST[:PATH]` as named forms of the rsync-style sync and the one-operand listing, with the short forms rewritten into the long ones at parse time.

**Architecture:** `sync` and `list` are ordinary clap subcommands that reuse `SyncArgs`. `Args::try_parse_from` normalises every sync or listing into `Command::Sync` / `Command::List`, so `main.rs` dispatches only on named commands and the top-level `args.sync` is left empty. Code that read `args.sync` reads `args.sync_args()` instead.

**Tech Stack:** Rust, clap 4 (derive + builder), the existing integration test harness in `crates/irsync/tests/common`.

**Spec:** `docs/superpowers/specs/2026-11-16-sync-list-commands-design.md`

## Global Constraints

- The rsync-style forms (`irsync [OPTIONS] SRC... DEST`, `irsync HOST:`, `irsync HOST:PATH`) keep their exact behaviour: output, exit codes, resume, errors.
- `irsync --help` keeps listing every transfer option at the top level; it only gains `sync` and `list` in its command list.
- A command name is recognised only as the first argument. A local path named like a command is written `./sync`. No guessing whether a word is a path.
- `list` takes exactly one operand: `HOST`, `HOST:` or `HOST:PATH`; a bare `HOST` becomes `HOST:`.
- Commits go on `main`, message = short summary line, blank line, short why. No attribution trailers of any kind.

## Review Focus

- `irsync -av sync myserver:` — `sync` after a flag is a source path, as `host list` already is after a flag. Test in Task 1.
- `irsync list ./photos` or `irsync list photos/x` (no colon, looks like a path) — a usage error, never a dial to a host named `./photos`. Test in Task 1.
- `irsync sync --exclude a --include b x h:m` — filter order kept when the options sit under the subcommand. Test in Task 1.
- `irsync list a: b:` and `irsync list` — usage errors naming the expected operand. Test in Task 1.
- `irsync sync -a ./x` (one local operand) — the same missing-`<SRC>... <DEST>` usage error as `irsync -a ./x`. Test in Task 1.

---

### Task 1: Parse `sync` and `list`, and normalise short forms into them

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`Args`, `Args::command_tree`, `Args::try_parse_from`, `ordered_filters` caller, `Command`, `SyncArgs` derive, `SyncOptions::from_args`)
- Modify: `crates/irsync/src/main.rs` (`main` dispatch, `run_sync`, `run_list`, `locations`, `direction_of`, and the readers at the current lines ~430, 823-824, 907-908)
- Modify: tests that read `args.sync` / `a.cmd.is_none()`: `crates/irsync/tests/cli.rs` (lines ~196-198, 604, 629, 742-744) and any other hit of `grep -rn '\.sync\.\|cmd.is_none' crates/irsync/tests`
- Test: `crates/irsync/tests/cli.rs`

**Interfaces:**
- Produces:
  - `Command::Sync(SyncArgs)` — about: `Copy files to or from a host (the rsync-style form, named).`
  - `Command::List(SyncArgs)` — about: `List a host's exports, or the files a pull of HOST:PATH would fetch.`
  - `SyncArgs` derives `Default`.
  - `impl Args { pub fn sync_args(&self) -> &SyncArgs }` — the `SyncArgs` inside `Command::Sync` or `Command::List`; panics with `"sync_args: not a sync or list command"` otherwise.
  - After `Args::try_parse_from` returns `Ok`, `args.cmd` is always `Some(..)` and `args.sync == SyncArgs::default()` (compare field-wise in tests if `PartialEq` isn't derived; don't add `PartialEq` just for this — check `args.sync.paths.is_empty()`).
  - `SyncOptions::from_args(args: &Args)` keeps its signature and reads `args.sync_args()`.

- [ ] **Step 1: Write the failing tests in `crates/irsync/tests/cli.rs`**

Add a helper and these tests (adapt `Command` matching to `let … else` as the file does):

```rust
fn sync_paths(a: &Args) -> Vec<String> {
    match &a.cmd {
        Some(Command::Sync(s)) => s.paths.clone(),
        c => panic!("not Sync: {c:?}"),
    }
}
fn list_paths(a: &Args) -> Vec<String> {
    match &a.cmd {
        Some(Command::List(s)) => s.paths.clone(),
        c => panic!("not List: {c:?}"),
    }
}

#[test]
fn short_sync_is_rewritten_into_sync_command() {
    for (short, long) in [
        (vec!["-av", "a", "b", "h:m/"], vec!["sync", "-av", "a", "b", "h:m/"]),
        (vec!["h:m/", "local/"], vec!["sync", "h:m/", "local/"]),
    ] {
        let (s, l) = (parse(&short), parse(&long));
        assert_eq!(sync_paths(&s), sync_paths(&l));
        assert!(s.sync.paths.is_empty() && l.sync.paths.is_empty());
        assert_eq!(format!("{:?}", SyncOptions::from_args(&s)),
                   format!("{:?}", SyncOptions::from_args(&l)));
    }
}

#[test]
fn listings_are_rewritten_into_list_command() {
    assert_eq!(list_paths(&parse(&["home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["sync", "home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["list", "home:"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["list", "home"])), ["home:"]);
    assert_eq!(list_paths(&parse(&["-v", "home:photos/"])), ["home:photos/"]);
    assert_eq!(list_paths(&parse(&["list", "-v", "home:photos/"])), ["home:photos/"]);
}

#[test]
fn filters_keep_order_under_the_sync_command() {
    let a = parse(&["sync", "--exclude=a", "--include-from=inc", "--include", "b",
                    "--exclude-from", "exc", "x", "h:m"]);
    assert_eq!(SyncOptions::from_args(&a).filters, vec![
        FilterRule::Exclude("a".into()),
        FilterRule::IncludeFrom(PathBuf::from("inc")),
        FilterRule::Include("b".into()),
        FilterRule::ExcludeFrom(PathBuf::from("exc")),
    ]);
}

#[test]
fn command_words_after_a_flag_are_paths() {
    assert_eq!(sync_paths(&parse(&["-av", "sync", "h:m/"])), ["sync", "h:m/"]);
    assert_eq!(sync_paths(&parse(&["-av", "list", "h:m/"])), ["list", "h:m/"]);
    assert_eq!(sync_paths(&parse(&["./sync", "h:m/"])), ["./sync", "h:m/"]);
}

#[test]
fn sync_and_list_usage_errors() {
    for bad in [
        vec!["sync"], vec!["sync", "-a", "./x"], vec!["-a", "./x"],
        vec!["list"], vec!["list", "a:", "b:"],
        vec!["list", "./photos"], vec!["list", "photos/x"],
    ] {
        let mut full = vec!["irsync"];
        full.extend(bad.iter());
        assert!(Args::try_parse_from(full).is_err(), "{bad:?}");
    }
}
```

If `SyncOptions` has no `Debug`, compare a handful of fields instead (`preserve.recursive`, `delete`, `filters`, `dry_run`, `verbose`) — don't add derives only for the test.

Also update the existing tests that assert `a.cmd.is_none()` / read `a.sync.paths` (`subcommands_parse`, `old_bare_verbs_are_gone`, `file_flags_are_accepted_after_the_noun_in_either_position`, `one_remote_operand_is_a_listing`) to the new shape via `sync_paths` / `list_paths`. Their meaning doesn't change.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --test cli`
Expected: compile errors (`Command::Sync`, `Command::List` don't exist).

- [ ] **Step 3: Add the commands and the normalisation in `crates/irsync/src/cli/mod.rs`**

- Add `Sync(SyncArgs)` and `List(SyncArgs)` as the **first** two `Command` variants (so they head the command list in `--help`), with the abouts from Interfaces.
- In `Args::command_tree`, give `list` its own operand name: `.mut_subcommand("list", |c| c.mut_arg("paths", |a| a.value_name("HOST[:PATH]")))`.
- In `try_parse_from`, replace the current missing-operand check and `args.sync.filters = …` with a normalisation step. Rules, in order:
  - `cmd == None`: move `args.sync` (via `std::mem::take`) into `Command::Sync`, filters from the top-level matches.
  - `Command::Sync(s)` / `Command::List(s)` from the user: filters from `m.subcommand_matches("sync" | "list")`.
  - Then for `Sync(s)`: one operand that parses as remote → becomes `List(s)`; fewer than two operands otherwise → the existing `MissingRequiredArgument` error with the existing text.
  - For `List(s)`: not exactly one operand → `MissingRequiredArgument` / `TooManyValues` error with text `list takes one operand: HOST or HOST:PATH`. An operand with no `:` gets `:` appended, but only if it contains no `/`; then it must parse with `Location::parse` as remote, else `ValueValidation` error with the same text.
- `SyncArgs`: add `Default` to the derive.
- `Args::sync_args` as in Interfaces; `SyncOptions::from_args` reads it.

- [ ] **Step 4: Switch `crates/irsync/src/main.rs` to the named commands**

- Remove the `args.cmd.is_none() && paths.len() == 1` block and the `None` arm. Add arms `Some(Command::Sync(_))` → `run_sync`, `Some(Command::List(_))` → `run_list`. `None` is now unreachable: `unreachable!("try_parse_from always sets a command")`.
- Every read of `args.sync` becomes `args.sync_args()`.
- `run_list`'s file-listing branch currently mutates `args.sync.paths`. Make it build the dry-run pull as a `Command::Sync` with `paths = vec![operand, dest]` (take the `SyncArgs` out of `Command::List`, edit, put it back as `Sync`) and call `run_sync` with that.

- [ ] **Step 5: Run the CLI tests and the whole workspace**

Run: `cargo test -q -p irsync --test cli`
Expected: PASS.
Run: `cargo test -q --workspace --all-features --no-fail-fast`
Expected: PASS. Fix any remaining `args.sync` reader the compiler or a test points at the same way as above (`args.sync_args()`).

- [ ] **Step 6: Commit**

```bash
git add crates/irsync/src/cli/mod.rs crates/irsync/src/main.rs crates/irsync/tests
git commit -m "Add the sync and list commands (#62)

The rsync-style sync and the one-operand listing are now rewritten into
them at parse time, so both spellings run the same code."
```

### Task 2: Help and end-to-end checks through the binary

**Files:**
- Test: `crates/irsync/tests/commands_e2e.rs` (help), `crates/irsync/tests/listing_e2e.rs` (list and sync against a daemon)

**Interfaces:**
- Consumes: the `sync` / `list` commands from Task 1; `irsync_bin`, `spawn_daemon_binary` and `setup()` already in those files.

- [ ] **Step 1: Write the tests**

In `commands_e2e.rs`, following `help_points_at_the_last_line_of_id_show`:

```rust
#[test]
fn help_names_sync_and_list_and_each_has_its_own() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let top = irsync(&home, &["--help"]);
    assert!(top.status.success());
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(top.contains("  sync ") && top.contains("  list "), "{top}");
    assert!(top.contains("--archive"), "transfer options stay at the top: {top}");
    for (cmd, want) in [("sync", "SRC... DEST"), ("list", "HOST[:PATH]")] {
        let o = irsync(&home, &[cmd, "--help"]);
        assert!(o.status.success(), "{cmd}");
        let h = String::from_utf8_lossy(&o.stdout);
        assert!(h.contains(want) && h.contains("--archive"), "{cmd}: {h}");
    }
}
```

In `listing_e2e.rs`:

```rust
#[test]
fn list_command_matches_host_colon() {
    let s = setup();
    let colon = format!("{}:", s.daemon_id);
    let want = ok(&s.client, &["--address", &s.address, &colon]);
    for args in [
        vec!["list", "--address", &s.address, &s.daemon_id],
        vec!["list", "--address", &s.address, &colon],
        vec!["sync", "--address", &s.address, &colon],
    ] {
        assert_eq!(ok(&s.client, &args), want, "{args:?}");
    }
}

#[test]
fn list_command_lists_files() {
    let s = setup();
    let operand = format!("{}:photos/", s.daemon_id);
    let out = ok(&s.client, &["list", "--address", &s.address, &operand]);
    assert!(out.contains("a/x.jpg") && out.contains("b.jpg"), "{out:?}");
    assert!(!s.client.join("photos").exists());
}

#[test]
fn sync_command_pushes() {
    let s = setup();
    let src = s.client.join("up");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("new.jpg"), b"n").unwrap();
    let dest = format!("{}:photos/", s.daemon_id);
    let src_arg = format!("{}/", src.display());
    ok(&s.client, &["sync", "-a", "--address", &s.address, &src_arg, &dest]);
    assert_eq!(std::fs::read(s.photos.join("new.jpg")).unwrap(), b"n");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -q -p irsync --test commands_e2e help_names_sync_and_list && cargo test -q -p irsync --test listing_e2e`
Expected: PASS (Task 1 already built the behaviour; these pin it at the binary level). If the `--help` command list uses different spacing than `"  sync "`, match what clap prints and keep the check specific to the command list.

- [ ] **Step 3: Commit**

```bash
git add crates/irsync/tests/commands_e2e.rs crates/irsync/tests/listing_e2e.rs
git commit -m "Test sync and list through the binary (#62)"
```

### Task 3: Docs

**Files:**
- Modify: `book/src/reference/command-line.md`, `book/src/reference/remote-addresses.md` (listing lines ~104, ~131), `README.md`, the page(s) under `site/` that show `irsync -av …` (find with `grep -rn "irsync -av\|irsync myserver:" README.md site book/src`)
- Modify: `AFTER_HELP` in `crates/irsync/src/cli/mod.rs` only if a line there would read wrong; don't rewrite it.

- [ ] **Step 1: Write the docs**

- `command-line.md`: a synopsis showing both `irsync [OPTIONS] SRC... DEST` and `irsync sync [OPTIONS] SRC... DEST`, and `irsync list HOST[:PATH]` next to `irsync HOST:` / `irsync HOST:PATH`. Call the rsync-style form "the rsync-compatible shorthand", never "legacy". Add one short paragraph stating the rule: a command name counts only as the first argument; write a local path of that name as `./sync` (applies to every command name), and after any option every word is an operand.
- `remote-addresses.md`: where it describes one-operand listings, mention `irsync list` as the same thing.
- README and `site/`: next to the first sync example, show the `sync` spelling once and say scripts should prefer it; next to a listing example, show `irsync list myserver`.

- [ ] **Step 2: Check the book builds**

Run: `mdbook build book`
Expected: builds with no warnings about broken links.

- [ ] **Step 3: Full regression, then commit**

Run: `tools/ci.sh check`
Expected: passes (format, clippy both ways, all tests including the slow ones, doc tests).

```bash
git add book README.md site crates/irsync/src/cli/mod.rs
git commit -m "Document the sync and list commands (#62)"
```
