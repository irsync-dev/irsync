# `fileset list` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `fileset list PATH [PATTERN]` (alias `ls`) prints one line per manifest entry, with the fields `check -v` prints plus `target=` for symlinks, and everything recorded with `-v`.

**Architecture:** Four small pieces inside `crates/fileset`. The SQLite walk (`for_each`) reads attributes with one left join. A new `glob` module matches a pattern against path bytes. `report.rs` gains the listing line, built from the same field text as `line_entry`. A new `engine/list.rs` opens the manifest read-only, walks it and prints the entries the pattern accepts; `cli.rs` and `main.rs` wire it up.

**Tech Stack:** Rust, clap derive, rusqlite, fjall. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-16-fileset-list-design.md`

## Global Constraints

- `list` never writes to the manifest or the tree. The manifest is opened with `OpenMode::ReadOnly`.
- The default line of an entry is exactly what `line_entry` prints after its prefix, plus ` target=<target>` for a symlink with a recorded target. The output of `line_entry` for `check`, `init` and `update` must not change.
- No prefix on listing lines. Order is the store's order (`Store::for_each`); nothing is sorted.
- Pattern rules, on bytes, case-sensitive, against the whole stored path: `*` any run except `/`; `**` any run including `/`; `?` one byte except `/`; `[abc]`, `[a-z]`, `[!a-z]`, `[^a-z]` one byte except `/`; `\` takes the next byte literally; a `[` with no closing `]` is a plain `[`. No pattern is an error.
- Exit codes: 0 listed, also when nothing matched; 2 when the manifest cannot be found or read.
- Nothing on stderr on success.
- No new dependencies.
- Commit messages: a short summary line, a blank line, a plain-English reason. No attribution trailers of any kind.
- Per-task test command: `cargo test -q -p fileset <filter>`. Before the last commit: `tools/ci.sh check`.

## Review Focus

1. A pattern typed with a trailing slash or a leading `./` (`2026/`, `./2026`, as tab completion and habit produce): lists what `2026` lists, not nothing. Test in Task 2.
2. An unquoted pattern the shell expanded (`fileset list ~/Photos *.jpg` arrives as several arguments): exit 2 with a message that says to quote the pattern, not clap's bare "unexpected argument". Test in Task 4.
3. An empty pattern (`''`): lists nothing, exit 0, no error. Test in Task 2.
4. `PATH` that does not exist or is a file, without `--manifest`: exit 2 and the error `check` gives for it. Test in Task 4.
5. `*` at the top level also matches the root entry `.`: pinned as it is, so a later change is deliberate. Test in Task 2.

---

### Task 1: The SQLite walk reads attributes with one join

**Files:**
- Modify: `crates/fileset/src/store/sqlite.rs` (`for_each`, about line 336; `load_xattrs`, about line 177)
- Test: `crates/fileset/tests/store_suite.rs` (inside `suite`, so both backends run it)

**Interfaces:**
- Produces: nothing new. `Store::for_each` keeps its signature and its results.

This is a change of how, not what, so the test is written first and must pass both before and after.

- [ ] **Step 1: Pin the behaviour.** In `suite`, after the "byte order" block, add entries with no attribute, one, and three (inserted out of name order), then walk:

```rust
// for_each hands every entry its attributes, in name order
let mut none = entry(3);
none.xattrs = vec![];
let mut one = entry(3);
one.xattrs = vec![(b"user.a".to_vec(), b"1".to_vec())];
let mut three = entry(3);
three.xattrs = vec![
    (b"user.c".to_vec(), b"3".to_vec()),
    (b"user.a".to_vec(), vec![0, 255]),
    (b"user.b".to_vec(), b"".to_vec()),
];
s.put_batch(&[
    (b"w/none".to_vec(), none),
    (b"w/one".to_vec(), one),
    (b"w/three".to_vec(), three),
])
.unwrap();
let mut seen = Vec::new();
s.for_each(&mut |k, e| {
    if k.starts_with(b"w/") {
        seen.push((k.to_vec(), e.xattrs.clone()));
    }
    Ok(())
})
.unwrap();
assert_eq!(
    seen,
    vec![
        (b"w/none".to_vec(), vec![]),
        (b"w/one".to_vec(), vec![(b"user.a".to_vec(), b"1".to_vec())]),
        (
            b"w/three".to_vec(),
            vec![
                (b"user.a".to_vec(), vec![0, 255]),
                (b"user.b".to_vec(), b"".to_vec()),
                (b"user.c".to_vec(), b"3".to_vec()),
            ]
        ),
    ]
);
```

Adjust any later assertion in `suite` that counts entries.

- [ ] **Step 2: Run it and see it pass**

Run: `cargo test -q -p fileset --test store_suite`
Expected: PASS for both backends (the current walk already returns this).

- [ ] **Step 3: Rewrite `SqliteStore::for_each`** as one statement, `SELECT e.path, {ENTRY_COLS}, x.name, x.value FROM entries e LEFT JOIN xattrs x ON x.path = e.path ORDER BY e.path, x.name`, grouping consecutive rows with the same path into one `Entry` the way `get_entry` does for a single path. The callback runs once per entry, after its last row. Remove `load_xattrs` if nothing else uses it.

- [ ] **Step 4: Run the suite and the engine tests**

Run: `cargo test -q -p fileset`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/fileset/src/store/sqlite.rs crates/fileset/tests/store_suite.rs
git commit -m "Read attributes with one join when walking a SQLite manifest" -m "The walk did one extra lookup per entry. A left join does the same in one statement: 0.27 s for a million entries, against 0.15 s without attributes. fileset list (#29) walks every entry."
```

---

### Task 2: The pattern matcher

**Files:**
- Create: `crates/fileset/src/glob.rs`
- Modify: `crates/fileset/src/lib.rs` (add `pub mod glob;`)
- Test: unit tests in `crates/fileset/src/glob.rs`

**Interfaces:**
- Produces:

```rust
/// A glob over path bytes; see the module docs for the rules.
pub struct Pattern { /* tokens */ }
impl Pattern {
    /// Never fails. Trailing `/`s and one leading `./` of `text` are ignored.
    pub fn new(text: &[u8]) -> Pattern;
    /// Whether the whole of `path` (a stored relative path) matches.
    pub fn matches(&self, path: &[u8]) -> bool;
}
```

- [ ] **Step 1: Write the failing tests** as one table test:

```rust
#[test]
fn rules() {
    // (pattern, path, matches)
    let cases: &[(&[u8], &[u8], bool)] = &[
        (b"a.jpg", b"a.jpg", true),
        (b"a.jpg", b"A.jpg", false),
        (b"*.jpg", b"a.jpg", true),
        (b"*.jpg", b"d/a.jpg", false),
        (b"*", b"d/a", false),
        (b"**", b"d/a", true),
        (b"**/*.jpg", b"d/e/a.jpg", true),
        (b"**/*.jpg", b"a.jpg", false),
        (b"d/**", b"d/e/a.jpg", true),
        (b"d/**", b"d", false),
        (b"d", b"d/a", false),
        (b"a?c", b"abc", true),
        (b"a?c", b"a/c", false),
        (b"[abc].txt", b"b.txt", true),
        (b"[a-c].txt", b"d.txt", false),
        (b"[!a-c].txt", b"d.txt", true),
        (b"[^a-c].txt", b"a.txt", false),
        (b"[a-c]", b"/", false),
        (b"[!a]", b"/", false),
        (b"\\*", b"*", true),
        (b"\\*", b"x", false),
        (b"[ab", b"[ab", true),
        (b"a\xffb", b"a\xffb", true),
        (b"a?b", b"a\xffb", true),
        // Review focus: what people type.
        (b"2026/", b"2026", true),
        (b"2026///", b"2026", true),
        (b"./2026", b"2026", true),
        (b"", b"a", false),
        (b"", b".", false),
        // The root is stored as `.`: one segment, so `*` matches it.
        (b"*", b".", true),
        (b".", b".", true),
        (b"**", b".", true),
        (b"*.jpg", b".", false),
    ];
    for (pat, path, want) in cases {
        assert_eq!(
            Pattern::new(pat).matches(path),
            *want,
            "{:?} on {:?}",
            String::from_utf8_lossy(pat),
            String::from_utf8_lossy(path)
        );
    }
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -q -p fileset glob`
Expected: does not compile (`Pattern` missing).

- [ ] **Step 3: Implement `Pattern`.** Tokens `Lit(u8)`, `One`, `Star`, `StarStar`, `Class { neg, items: Vec<(u8, u8)> }`; a recursive match over the token slice. `crates/irsync/src/filter.rs` (`tokens`, `class`, `glob`, about lines 90-200) has the same rules and is the model; copy the approach, not a dependency. A pattern that is only `./` or only slashes becomes the empty pattern. The module doc comment lists the rules from Global Constraints.

- [ ] **Step 4: Run it and see it pass**

Run: `cargo test -q -p fileset glob`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fileset/src/glob.rs crates/fileset/src/lib.rs
git commit -m "Add a glob matcher over path bytes to fileset" -m "fileset list (#29) takes a pattern. The rules are those of irsync's exclude patterns; the matcher is a second, small implementation so fileset gains no dependency. TODO.md has the item to revisit both."
```

---

### Task 3: The listing line

**Files:**
- Modify: `crates/fileset/src/report.rs` (`line_entry`, about line 66)
- Test: unit tests in `crates/fileset/src/report.rs`

**Interfaces:**
- Produces: `pub fn line_listed(path: &[u8], e: &Entry, verbose: bool) -> String`.

- [ ] **Step 1: Write the failing tests.** Build entries by hand (the module's existing tests show how). Use `mtime` 1769860800 s, 5 ns, which prints as `2026-01-31T12:00:00.000000005Z`.

```rust
#[test]
fn listed_line_is_the_entry_line_without_a_prefix() {
    // file: kind File, mode 0o644, size 12, blake3 [0x11; 32]
    assert_eq!(
        line_listed(b"d/a.txt", &file, false),
        format!("d/a.txt kind=file size=12 mtime=2026-01-31T12:00:00.000000005Z mode=0644 blake3={}", "11".repeat(32))
    );
    assert_eq!(format!("EQL {}", line_listed(b"d/a.txt", &file, false)), line_entry("EQL", b"d/a.txt", &file));
    // dir: no blake3, no target
    assert_eq!(line_listed(b"d", &dir, false), "d kind=dir size=4096 mtime=2026-01-31T12:00:00.000000005Z mode=0755");
    // symlink with target b"a b\n"
    assert!(line_listed(b"l", &link, false).ends_with(" mode=0777 target=a b\\x0a"));
    // symlink whose target was not recorded: no target field
    assert!(!line_listed(b"l", &link_without_target, false).contains("target="));
}

#[test]
fn listed_line_verbose_adds_everything_recorded() {
    // file with uid 1000, gid 100, nlink 2, xattrs [(b"user.a", b"1"), (b"user.b", <70 bytes of b'x'>)]
    let line = line_listed(b"a", &file, true);
    assert!(line.ends_with(&format!(
        " uid=1000 gid=100 nlink=2 xattr:user.a=1 xattr:user.b={}…",
        "x".repeat(64)
    )), "{line}");
    assert!(!line.contains("rdev="));
    // chardev with rdev 259
    assert!(line_listed(b"dev", &chardev, true).contains(" nlink=1 rdev=259"));
    // the verbose line starts with the default line
    assert!(line.starts_with(&line_listed(b"a", &file, false)));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p fileset listed_line`
Expected: does not compile (`line_listed` missing).

- [ ] **Step 3: Implement.** Split `line_entry` so the text after the prefix comes from one private function that both use; `line_entry`'s output stays byte for byte the same. `line_listed` appends, in this order: ` target=<escape(target)>` when `e.target` is `Some`; then with `verbose`: ` uid=`, ` gid=`, ` nlink=`, ` rdev=` only for `Kind::CharDev` and `Kind::BlockDev`, and ` xattr:<escape(name)>=<fmt_bytes_value(value)>` per attribute in the order stored.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test -q -p fileset report`
Expected: PASS, including the existing `line_entry` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/fileset/src/report.rs
git commit -m "Add the fileset listing line" -m "For fileset list (#29): the fields check -v prints, with no prefix, the symlink target, and everything recorded when verbose."
```

---

### Task 4: The `list` command

**Files:**
- Create: `crates/fileset/src/engine/list.rs`
- Modify: `crates/fileset/src/engine/mod.rs` (add `pub mod list;`), `crates/fileset/src/cli.rs` (`Command`, new `ListArgs`), `crates/fileset/src/main.rs` (dispatch)
- Modify: `book/src/fileset/reference.md`, `book/src/fileset/introduction.md`
- Test: create `crates/fileset/tests/list.rs`; add to `crates/fileset/tests/cli.rs`

**Interfaces:**
- Consumes: `glob::Pattern::{new, matches}` (Task 2), `report::line_listed(path, e, verbose)` (Task 3), `Store::for_each` (Task 1).
- Produces:

```rust
pub struct ListOpts {
    pub root: PathBuf,
    pub manifest: Option<PathBuf>,
    pub pattern: Option<Vec<u8>>,
    pub verbose: bool,
}
/// Prints the manifest's entries that the pattern accepts. `Ok(Outcome::Clean)` also
/// when nothing matched, and when `out` was closed early (a broken pipe).
pub fn list(opts: &ListOpts, out: &mut dyn Write) -> anyhow::Result<Outcome>;
```

- [ ] **Step 1: Write the failing tests.**

`crates/fileset/tests/list.rs`, with `mod common;`. A helper `listing(root, backend, pattern, verbose) -> String` creates nothing: it calls `list` into a `Vec<u8>`. A helper `fixture(backend) -> TempDir` builds, with the `common` helpers, the tree `2024/a.jpg` ("aaa"), `2024/b.jpg`, `2024-old/x`, `2024.txt`, `top.jpg`, a symlink `latest -> 2024/a.jpg`, a fifo `pipe`, fixed mtimes via `set_mtime`, and runs `init` for `backend` (`common::run_update`).

```rust
#[test]
fn lists_every_entry_in_stored_order_on_both_backends() {
    // paths (first word of each line) for Backend::Sqlite:
    // [".", "2024", "2024-old", "2024-old/x", "2024.txt", "2024/a.jpg", "2024/b.jpg",
    //  "latest", "pipe", "top.jpg"]   (the SQLite manifest file itself is listed where it sorts, if check -v lists it)
    // The whole listing is equal for Backend::Sqlite and Backend::Fjall once the
    // manifest's own line is removed.
}

#[test]
fn a_file_line_equals_the_check_verbose_line_without_its_prefix() {
    // run check with verbose on the unchanged fixture; take the line starting
    // "EQL 2024/a.jpg "; strip "EQL "; assert it is a line of the listing.
}

#[test]
fn a_symlink_shows_its_target_and_a_directory_no_checksum() {
    // line for "latest" ends with " target=2024/a.jpg"; line for "2024" has no "blake3=".
}

#[test]
fn a_pattern_selects_by_whole_path() {
    // "*.jpg"      -> ["top.jpg"]
    // "**/*.jpg"   -> ["2024/a.jpg", "2024/b.jpg"]
    // "2024/**"    -> ["2024/a.jpg", "2024/b.jpg"]
    // "2024"       -> ["2024"]
    // "2024/"      -> ["2024"]
    // "nope*"      -> []  and Outcome::Clean
}

#[test]
fn verbose_adds_owner_links_and_attributes() {
    // every line contains " uid=" and " nlink="; skip the attribute part when
    // xattr::set on a fixture file fails (filesystem without user attributes).
}

#[test]
fn a_copied_manifest_lists_without_its_tree() {
    // copy <root>/.fileset.sqlite elsewhere; root = a path that does not exist,
    // manifest = the copy; same listing as from the tree.
}

#[test]
fn a_path_that_is_not_utf8_lists_escaped_and_matches_by_bytes() {
    // file named b"x\xffy" (skip where the filesystem refuses the name);
    // listing has a line starting "x\\xffy "; pattern b"x\xffy" selects it.
}

#[test]
fn list_leaves_the_manifest_file_untouched() {
    // bytes and mtime of .fileset.sqlite equal before and after list.
}

#[test]
fn no_manifest_is_the_error_check_gives() {
    // tree without init: list Err's text == check Err's text.
}
```

In `crates/fileset/tests/cli.rs`:

```rust
#[test]
fn cli_ls_is_list() { /* same stdout for `list` and `ls`; exit 0; stderr empty */ }

#[test]
fn cli_list_exit_codes() {
    // no manifest -> 2; PATH missing -> 2 and stderr equals check's for the same PATH;
    // pattern that matches nothing -> 0 with empty stdout.
}

#[test]
fn cli_list_takes_no_walk_options() {
    // each of ["--jobs","2"], ["--walk-threads","2"], ["--cross-mount-points"],
    // ["--no-dir-sort"], ["--no-builtin-excludes"], ["--fail-on-warnings"], ["-q"] -> exit 2
}

#[test]
fn cli_list_says_to_quote_an_expanded_pattern() {
    // `list <root> a.jpg b.jpg` -> exit 2, stderr contains "quote the pattern"
}

#[test]
fn cli_list_stops_quietly_when_stdout_closes() {
    // tree of 3000 files; spawn `list` with stdout piped; read one line; drop the
    // pipe; wait: exit code 0, stderr empty.
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -q -p fileset --test list --test cli`
Expected: `list.rs` does not compile (`engine::list` missing); the new `cli.rs` tests fail with "unrecognized subcommand".

- [ ] **Step 3: Implement `engine/list.rs`.** Find the manifest as `check` does (`canonical_root`, `store::locate(&root, manifest, None, false)`, `store::open(.., OpenMode::ReadOnly)` with `check`'s error context), except: when `manifest` is given and `root` does not exist, skip `canonical_root` and pass `root` as written. Build `Pattern::new` when a pattern is given. `for_each`: skip entries the pattern rejects, `writeln!` `line_listed`. A write error of kind `BrokenPipe` ends the walk and returns `Ok(Outcome::Clean)`; any other write error is returned.

- [ ] **Step 4: Wire the command.**

`cli.rs`:

```rust
/// List the entries recorded in a manifest
#[command(visible_alias = "ls")]
List(ListArgs),

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Root of the tree
    pub path: PathBuf,
    /// List only entries whose path matches this glob (quote it): `*` and `?` stop at `/`, `**` crosses it
    pub pattern: Option<OsString>,
    /// Location of the manifest instead of <PATH>/.fileset.sqlite or <PATH>/.fileset.fjall; with it, <PATH> need not exist
    #[arg(long, value_name = "MPATH")]
    pub manifest: Option<PathBuf>,
    /// Also print owner, link count, device numbers and extended attributes
    #[arg(short = 'v', long)]
    pub verbose: bool,
    #[arg(hide = true, trailing_var_arg = true)]
    pub extra: Vec<OsString>,
}
```

`main.rs`: for `Command::List`, a non-empty `extra` is `fail("more than one pattern; quote the pattern so the shell does not expand it")`. Otherwise call `list`. For this command only, a `BrokenPipe` from the final `out.flush()` is exit 0, not "cannot write to stdout".

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo test -q -p fileset`
Expected: all pass. `help_lists_all_verbs` in `cli.rs` gains `list`.

- [ ] **Step 6: Document it** in `book/src/fileset/reference.md`:

- Commands: add `fileset list   [-v] [--manifest <MPATH>] <PATH> [PATTERN]` to the block, and a `### list` section: what it prints, that it reads only the manifest, that `ls` is the same command, that with `--manifest` the path need not exist.
- Options: a `### list only` table with `-v` and `--manifest`, and a sentence that the options under "All commands" other than `--manifest` do not apply to `list`.
- Output: a `### Listing` section with the three-line example from the spec, the default fields, the `-v` fields, the order (with the `2024` example), and the pattern rules with the four examples (`*.jpg`, `**/*.jpg`, `2026/**`, `2026`), including that trailing slashes and a leading `./` are ignored and that the pattern must be quoted.
- Exit codes: `0` also covers "`list` printed its entries, or none matched".

In `book/src/fileset/introduction.md`, where the commands are named: one sentence, `fileset list` prints what a manifest holds.

- [ ] **Step 7: Full check**

Run: `tools/ci.sh check`
Expected: `ci.sh: passed: check`.

- [ ] **Step 8: Commit**

```bash
git add crates/fileset/src crates/fileset/tests/list.rs crates/fileset/tests/cli.rs book/src/fileset/reference.md book/src/fileset/introduction.md
git commit -m "Add fileset list" -m "There was no way to see what a manifest holds short of checking a tree against it. list prints every entry, or those matching a glob, in the line check -v uses. Closes #29."
```
