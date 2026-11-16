# Management commands default to `list` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `irsync id list` lists the identities this machine has, and `irsync id`, `host`, `export`, `grant` and `config` with no subcommand run their `list`.

**Architecture:** `IdCmd` gets a `List` variant handled by a new `id_list` in `main.rs`, which shares its printing with `id_show`. Each noun's `cmd` field becomes `Option<...Cmd>`; the `run_*` function for the noun treats `None` as that noun's `List`.

**Tech Stack:** Rust, clap derive. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-16-default-to-list-design.md`

## Global Constraints

- `irsync id show`, `show --daemon` and `show -a` keep their exact behaviour and output. Do not edit their existing tests.
- A bare noun only lists. It must never create or change `irsync.toml`.
- No command line that works today may stop working or print something else.
- A bare noun takes only `--config`. `irsync grant photos` and `irsync id --passphrase-file F` stay usage errors.
- The daemon identity is never created by `id list`.
- Commit messages: a short summary line, a blank line, a plain-English reason. No attribution trailers of any kind.
- Each task's test command: `cargo test -q -p irsync --all-features --test <file> <name>`. Before the last commit: `tools/ci.sh check`.

## Review Focus

1. `irsync grant photos` (a list target without `list`): still a usage error, exit 2, not a silent `grant list photos`. Test in Task 2.
2. `irsync id list` when the daemon key exists but cannot be read: fails and names the key file, as `show --daemon` does; it does not skip the daemon silently. Test in Task 1.
3. A bare noun on a fresh home: exit 0 and no `irsync.toml` afterwards. Test in Task 2.
4. `irsync host --config PATH` (flag after a bare noun): reads that file, same output as `host list --config PATH`. Test in Task 2.
5. `irsync id --help`: still prints help, exit 0, and does not create an identity. Test in Task 2.

---

### Task 1: `irsync id list`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`IdCmd`, about line 266)
- Modify: `crates/irsync/src/main.rs` (`run_id` about line 1768, `id_show` about line 1851)
- Modify: `book/src/reference/management.md` (the `## id` table)
- Test: `crates/irsync/tests/commands_e2e.rs`, `crates/irsync/tests/cli.rs`

**Interfaces:**
- Produces: `IdCmd::List { passphrase_file: Option<PathBuf> }`, and `fn id_list(f: &FilesArgs, passphrase_file: Option<PathBuf>) -> i32` in `main.rs`. Task 2 calls both.

- [ ] **Step 1: Write the failing tests** in `commands_e2e.rs`, using the file's `irsync` and `ok` helpers.

```rust
#[test]
fn id_list_prints_the_client_and_the_daemon_when_there_is_one() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    // Fresh machine: the client identity is created, the daemon's is not.
    let o = irsync(&home, &["id", "list"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0], "client:");
    assert!(home.join(".config/irsync/id_ed25519").exists());
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    // With a daemon identity: both blocks, exactly what `show -a` prints,
    // and no pointer on stderr.
    ok(&home, &["id", "generate", "--daemon"]);
    let o = irsync(&home, &["id", "list"]);
    assert!(o.status.success());
    assert!(o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert_eq!(out, ok(&home, &["id", "show", "--all"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[3]), (6, "client:", "daemon:"));
}

#[test]
fn id_list_names_a_daemon_key_it_cannot_read() {
    // Same setup and root-skip as an_unreadable_daemon_key_is_named_in_the_error.
    // After chmod 0o000 on daemon_ed25519:
    let o = irsync(&home, &["id", "list"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains(key.to_str().unwrap()), "{err}");
}
```

In `cli.rs`, next to the other `id` parse assertions (about line 663):

```rust
assert!(matches!(
    parse(&["id", "list", "--passphrase-file", "/p"]).cmd,
    Some(Command::Id(ref i))
        if matches!(i.cmd, IdCmd::List { passphrase_file: Some(ref p) } if p == &PathBuf::from("/p"))
));
assert!(Args::try_parse_from(["irsync", "id", "list", "--daemon"]).is_err());
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test commands_e2e id_list`
Expected: both fail with "unrecognized subcommand 'list'" in the output. The `cli.rs` assertion fails to compile (`IdCmd::List` does not exist).

- [ ] **Step 3: Add `IdCmd::List`** in `cli/mod.rs`, placed before `Show`:

```rust
/// List the identities this machine has: the client's, then the
/// daemon's when there is one, each under a `client:` or `daemon:`
/// label. A missing client identity is generated as for a sync; the
/// daemon's is never generated.
List {
    /// Read an encrypted identity's passphrase from the first line of
    /// this file (else IRSYNC_PASSPHRASE, else a prompt on a terminal).
    #[arg(long, value_name = "PATH")]
    passphrase_file: Option<PathBuf>,
},
```

- [ ] **Step 4: Implement `id_list`** in `main.rs` and dispatch it from `run_id`.

Move the three closures inside `id_show` (`print`, `client`, `show_daemon`) out into private functions so both commands use them:

```rust
fn print_identity(secret: &iroh::SecretKey, role: IdRole);
fn print_client_identity(settings: &Settings, pass: &PassphraseSource) -> anyhow::Result<()>;
fn print_daemon_identity(settings: &Settings, pass: &PassphraseSource) -> anyhow::Result<()>;
```

(`Settings` is whatever type `Config::load(..)?.settings` has.) Their bodies are the closures' bodies, unchanged. `id_show` calls them and is otherwise as before.

`id_list` runs under `finish("id list", ..)`: load settings, print the client identity, then print the daemon identity only if `settings.daemon.identity.symlink_metadata().is_ok()`. No stderr note.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test -q -p irsync --all-features --test commands_e2e id_` and `cargo test -q -p irsync --all-features --test cli`
Expected: all pass, including the untouched `id_generates_and_prints_client_then_daemon_then_both` and `id_show_on_a_server_points_at_the_daemon_id`.

- [ ] **Step 6: Document it.** In `book/src/reference/management.md`, add this as the first row of the `## id` table:

```markdown
| `irsync id list [--passphrase-file PATH]` | Print every identity this machine has: `client:` with the client's public key line and `EndpointId`, then `daemon:` with the daemon's when a daemon key exists. Generates the client's key if no key serves; never the daemon's. |
```

- [ ] **Step 7: Commit**

```bash
git add crates/irsync/src/cli/mod.rs crates/irsync/src/main.rs crates/irsync/tests/commands_e2e.rs crates/irsync/tests/cli.rs book/src/reference/management.md
git commit -m "Add irsync id list" -m "Every other noun has list; id only had show, which hides the daemon identity. list prints what the machine has. See #32."
```

---

### Task 2: A bare noun runs `list`

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs` (`IdArgs`, `HostArgs`, `ExportArgs`, `GrantArgs`, `ConfigArgs`; the doc comments of `Command::Host`, `Export`, `Grant`, `Id`, `Config`)
- Modify: `crates/irsync/src/main.rs` (`run_id`, `run_host`, `run_export`, `run_grant`, `run_config`)
- Modify: `crates/irsync/tests/cli.rs` (about line 670)
- Modify: `book/src/reference/management.md` (intro), `book/src/reference/command-line.md` (line 186)
- Test: `crates/irsync/tests/commands_e2e.rs`

**Interfaces:**
- Consumes: `IdCmd::List { passphrase_file }` and `id_list` from Task 1.
- Produces: `pub cmd: Option<IdCmd>` (and the same for `HostCmd`, `ExportCmd`, `GrantCmd`, `ConfigCmd`).

- [ ] **Step 1: Write the failing tests** in `commands_e2e.rs`.

```rust
#[test]
fn a_bare_noun_runs_its_list() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    // One host, one export, one grant, as
    // host_add_then_grant_add_by_alias_then_list sets up.
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["host", "add", "laptop", &id]);
    ok(&home, &["export", "add", &format!("p:{}", home.join("p").display())]);
    ok(&home, &["grant", "add", "p", "laptop"]);
    // The client identity exists before the comparison, so neither run
    // prints "generated identity".
    ok(&home, &["id", "generate"]);
    for noun in ["id", "host", "export", "grant", "config"] {
        let bare = irsync(&home, &[noun]);
        let list = irsync(&home, &[noun, "list"]);
        assert!(bare.status.success(), "{noun}");
        assert!(!list.stdout.is_empty(), "{noun}");
        assert_eq!(bare.stdout, list.stdout, "{noun}");
        assert_eq!(bare.stderr, list.stderr, "{noun}");
    }
    // The flag after a bare noun names the file.
    let other = d.path().join("other.toml");
    std::fs::write(&other, format!("[hosts.nas]\nendpoint_id = \"{id}\"\n")).unwrap();
    let c = other.to_str().unwrap();
    assert_eq!(
        ok(&home, &["host", "--config", c]),
        ok(&home, &["host", "list", "--config", c])
    );
    assert!(ok(&home, &["host", "--config", c]).contains("nas"));
}

#[test]
fn a_bare_noun_changes_nothing_and_takes_no_list_arguments() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    for noun in ["host", "export", "grant", "config"] {
        assert!(irsync(&home, &[noun]).status.success(), "{noun}");
    }
    assert!(!home.join(".config/irsync/irsync.toml").exists());
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    // `--help` is still help, and makes no identity.
    let o = irsync(&home, &["id", "--help"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("Usage: irsync id"));
    assert!(!home.join(".config/irsync/id_ed25519").exists());
    // Arguments of `list` need `list` typed out.
    for args in [&["grant", "photos"][..], &["id", "--passphrase-file", "/p"], &["id", "--daemon"]] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
}
```

In `cli.rs`, replace the comment and first assertion at about line 670 (`// id is a noun: it needs a verb.` and `assert!(Args::try_parse_from(["irsync", "id"]).is_err());`) with:

```rust
// A noun alone parses: it runs `list`.
for noun in ["id", "host", "export", "grant", "config"] {
    assert!(Args::try_parse_from(["irsync", noun]).is_ok(), "{noun}");
}
assert!(matches!(parse(&["id"]).cmd, Some(Command::Id(ref i)) if i.cmd.is_none()));
```

Keep the `["irsync", "id", "--daemon"]` assertion that follows it.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p irsync --all-features --test commands_e2e a_bare_noun`
Expected: both fail; the bare nouns exit 2 with a usage page.

- [ ] **Step 3: Make the subcommand optional.** In `cli/mod.rs`, change `pub cmd: XCmd` to `pub cmd: Option<XCmd>` in `IdArgs`, `HostArgs`, `ExportArgs`, `GrantArgs` and `ConfigArgs`. Append this sentence to the doc comment of each of `Command::Host`, `Export`, `Grant`, `Id` and `Config`: `With no subcommand, runs `list`.`

- [ ] **Step 4: Fall back to `list`** in each `run_*` in `main.rs`: where it matches on `x.cmd`, match on the given command or, for `None`, that noun's list: `IdCmd::List { passphrase_file: None }`, `HostCmd::List`, `ExportCmd::List`, `GrantCmd::List { target: None }`, `ConfigCmd::List`. Fix the other uses of `.cmd` on these five types that the compiler reports (tests in `cli.rs` match on `i.cmd` directly; wrap their patterns in `Some(..)`).

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test -q -p irsync --all-features --test commands_e2e` and `cargo test -q -p irsync --all-features --test cli`
Expected: all pass.

- [ ] **Step 6: Document it.**

In `book/src/reference/management.md`, add after the first paragraph:

```markdown
A noun on its own runs its `list`: `irsync host`, `irsync export`,
`irsync grant`, `irsync id` and `irsync config` print what is there and
change nothing. Arguments of `list`, such as a grant target, need `list`
typed out.
```

In `book/src/reference/command-line.md` line 186, after "See [Management commands](management.md).", add: `Each of the first five alone runs its `list`.`

- [ ] **Step 7: Full check**

Run: `tools/ci.sh check`
Expected: passes (formatting, clippy with and without OpenTelemetry, all tests, doc tests).

- [ ] **Step 8: Commit**

```bash
git add crates/irsync/src/cli/mod.rs crates/irsync/src/main.rs crates/irsync/tests/commands_e2e.rs crates/irsync/tests/cli.rs book/src/reference/management.md book/src/reference/command-line.md
git commit -m "Run list when a management noun is given alone" -m "irsync id, host, export, grant and config printed a usage page when typed alone. Listing is what a user most likely wants there, and it changes nothing. Closes #32."
```
