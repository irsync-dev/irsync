# fileset -v Per-Entry Lines Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** With `-v`, `fileset check`, `update` and `init` print one prefixed line per entry on stdout and draw no spinner.

**Architecture:** Two new line formatters in `report.rs`. A small `Report` writer in `engine/mod.rs` owns stdout for both engines: it always writes the lines `check` prints today, writes the new lines only under `-v`, and flushes after each line under `-v`. The `INFO` path through `Warnings` is removed. `main.rs` picks no spinner when `-v` is given.

**Tech Stack:** Rust, the `fileset` crate (`crates/fileset`), clap, indicatif.

**Spec:** `docs/superpowers/specs/2026-11-16-fileset-verbose-entries-design.md`

## Global Constraints

- Without `-v`, stdout of every command is byte for byte what it is today. The existing `ADD`, `CHG` and `DEL` lines of `check` do not change.
- Prefixes are exactly `EQL`, `CHG`, `ADD`, `DEL`, `SKP`, each followed by one space.
- Entry form: `<path> kind=<kind> size=<n> mtime=<mtime> mode=<mode>`, then ` blake3=<64 hex>` only when the entry has a checksum. Use `escape`, `Kind::name`, `fmt_mtime`, `fmt_mode`, `fmt_blake3` as `compare::diff` does.
- Skip form: `SKP <path> reason=builtin-exclude`.
- `WARN` lines and the summary stay on stderr. Nothing prints `INFO` any more.
- `-v` stays a plain switch that conflicts with `-q`. No new flags.
- Commit messages: a short summary line, a blank line, then a plain sentence on why. No attribution trailers of any kind.
- Per task, run `cargo test -q -p fileset --all-features --no-fail-fast` from the repo root. Before the last commit, run `tools/ci.sh check`.

## Review Focus

Each line names the task whose tests pin it.

1. Stdout closed early (`fileset check -v DIR | head -1`): the run ends with exit code 2 and an `error:` line, with no panic. Task 4.
2. A path with a newline or bytes that are not UTF-8: still exactly one line per entry, with the path escaped (`a\x0ab`). Task 1.
3. `update -v` on a file that cannot be read: the entry line is printed without `blake3=`, next to the existing `WARN cannot hash`. Task 3.
4. `update -v` when an older manifest holds a built-in exclude and what is below it: `SKP` for the exclude, and a `DEL` line for every stored row dropped. Task 3.
5. `update -v --rehash` on an unchanged file: it prints `EQL`, not `CHG`, although it was hashed again. Task 3.

## File Structure

- `crates/fileset/src/report.rs`: gains `line_entry` and `line_skp`.
- `crates/fileset/src/engine/mod.rs`: gains `Report`; `Warnings` loses `info` and `verbose`.
- `crates/fileset/src/engine/check.rs`, `engine/update.rs`: print through `Report`.
- `crates/fileset/src/walk.rs`: loses `excluded_info`.
- `crates/fileset/src/main.rs`, `src/cli.rs`: no spinner under `-v`, help text.
- `crates/fileset/tests/common/mod.rs`, `tests/engine.rs`, `tests/cli.rs`: tests.
- `book/src/fileset/reference.md`: the `-v` row and the Output section.

---

### Task 1: Line formatters

**Files:**
- Modify: `crates/fileset/src/report.rs`

**Interfaces:**
- Produces:
  - `pub fn line_entry(prefix: &str, path: &[u8], e: &Entry) -> String`
  - `pub fn line_skp(path: &[u8]) -> String`

- [ ] **Step 1: Write the failing tests** in `report.rs`'s `tests` module. Build the `Entry` with the fields set below and every other field zero, empty or `None` (see how `compare.rs` tests build one).

```rust
#[test]
fn entry_line_with_checksum() {
    let mut h = [0u8; 32];
    h[0] = 0xab;
    let e = entry(Kind::File, 13, Timespec { sec: 0, nsec: 5 }, 0o644, Some(h));
    assert_eq!(
        line_entry("EQL", b"a/b", &e),
        format!(
            "EQL a/b kind=file size=13 mtime=1970-01-01T00:00:00.000000005Z mode=0644 blake3=ab{}",
            "00".repeat(31)
        )
    );
}
#[test]
fn entry_line_without_checksum_has_no_blake3_field() {
    let e = entry(Kind::Dir, 96, Timespec { sec: 0, nsec: 0 }, 0o755, None);
    assert_eq!(
        line_entry("ADD", b"d", &e),
        "ADD d kind=dir size=96 mtime=1970-01-01T00:00:00.000000000Z mode=0755"
    );
}
#[test]
fn entry_and_skip_lines_escape_the_path() {
    let e = entry(Kind::File, 0, Timespec { sec: 0, nsec: 0 }, 0o600, None);
    let l = line_entry("CHG", b"a\nb\xff", &e);
    assert!(l.starts_with("CHG a\\x0ab\\xff kind=file "), "{l}");
    assert!(!l.contains('\n'));
    assert_eq!(line_skp(b".Trashes"), "SKP .Trashes reason=builtin-exclude");
    assert_eq!(line_skp(b"a\nb"), "SKP a\\x0ab reason=builtin-exclude");
}
```

`entry(kind, size, mtime, mode, blake3) -> Entry` is a test-only helper in the same module.

- [ ] **Step 2: Run** `cargo test -q -p fileset --lib report` and see the three tests fail to compile (`line_entry` not found).
- [ ] **Step 3: Implement `line_entry` and `line_skp`** in `report.rs`. Format `mode` the way `compare::diff` does (`fmt_mode(e.mode)`).
- [ ] **Step 4: Run** `cargo test -q -p fileset --lib report`. Expected: all pass.
- [ ] **Step 5: Commit**

```bash
git add crates/fileset/src/report.rs
git commit -m "Add the entry and skip report lines" -m "fileset -v will print one line per entry (issue #30); these are the two new line formats."
```

---

### Task 2: `check -v`

**Files:**
- Modify: `crates/fileset/src/engine/mod.rs`, `crates/fileset/src/engine/check.rs`
- Test: `crates/fileset/tests/engine.rs`

**Interfaces:**
- Consumes: `line_entry`, `line_skp` from Task 1.
- Produces, in `engine/mod.rs`:

```rust
/// Report lines on stdout. With `verbose`, every line is flushed as it is written.
pub struct Report<'a> { /* out: &'a mut dyn Write, verbose: bool */ }
impl<'a> Report<'a> {
    pub fn new(out: &'a mut dyn Write, verbose: bool) -> Self;
    /// A line printed with or without `-v`.
    pub fn line(&mut self, text: &str) -> io::Result<()>;
    /// A line printed only with `-v`; `text` is not called otherwise.
    pub fn verbose_line(&mut self, text: impl FnOnce() -> String) -> io::Result<()>;
}
```

- [ ] **Step 1: Write the failing tests** in `tests/engine.rs`, each registered in a `for_both_backends!` block as `name => name_sqlite, name_fjall`. Set `-v` with `opts.common.verbose = true` on `check_opts(...)` and run with `run_check_opts`.

  - `check_verbose_prints_eql_per_entry`: tree `[("f", "x"), ("d/g", "y")]`; `chmod` `f` to `0o644`; `set_mtime(f, 1_000_000_000, 5)`; update; check with `-v`. Assert:
    - outcome `Clean`;
    - stdout contains the exact line `format!("EQL f kind=file size=1 mtime=2001-09-09T01:46:40.000000005Z mode=0644 blake3={}\n", blake3::hash(b"x").to_hex())`;
    - a line starts with `EQL . kind=dir size=` and one with `EQL d kind=dir size=`, and neither contains `blake3=`;
    - every stdout line starts with `EQL `, and the number of lines equals the `unchanged=N` in the stderr summary;
    - stderr does not contain `INFO`.
  - `check_verbose_keeps_the_difference_lines`: set up as `check_reports_add_del_chg` does. Run check without `-v` and with it. Assert the lines of the `-v` run that do not start with `EQL ` or `SKP ` equal the lines of the plain run, in the same order, and that the `-v` run has at least one `EQL ` line.
  - `check_without_verbose_prints_no_eql_or_skp`: tree `[("f", "x"), (".Trashes/t", "y")]`; update; plain check. Assert stdout is `""`.
  - `check_verbose_relaxed_has_no_checksum`: tree `[("f", "x")]`; update; check with `Tier::Relaxed` and `-v`. Assert the `EQL f ` line does not contain `blake3=`.
  - `check_verbose_prints_skp_on_stdout`: tree `[("f", "x"), (".Trashes/t", "y")]`; update; check with `-v`. Assert stdout contains `"SKP .Trashes reason=builtin-exclude\n"`, no stdout line mentions `.Trashes/t`, and stderr does not contain `INFO`.
  - `check_verbose_flushes_every_line` (one backend is enough, plain `#[test]`): call `check` directly with a test writer that counts `flush` calls. With `-v` on tree `[("f", "x")]` after an update, flushes are at least the number of lines written. Without `-v`, the count is 0.

- [ ] **Step 2: Run** `cargo test -q -p fileset --test engine check_verbose` and see them fail (no `EQL` lines).
- [ ] **Step 3: Add `Report`** to `engine/mod.rs` as given under Interfaces.
- [ ] **Step 4: Use it in `check.rs`.** Wrap `out` in one `Report::new(out, common.verbose)` at the top of `check` and send every existing `writeln!(out, ...)` through `Report::line`. Where `diffs.is_empty()`, add `verbose_line(|| line_entry("EQL", &obs.path, &obs.entry))`. For `WalkEvent::Excluded`, replace the `warnings.info(...)` call with `verbose_line(|| line_skp(&path))`. Leave `Warnings::info` in place; Task 3 removes it.
- [ ] **Step 5: Run** `cargo test -q -p fileset --all-features --no-fail-fast`. Expected: all pass.
- [ ] **Step 6: Commit**

```bash
git add crates/fileset/src/engine crates/fileset/tests/engine.rs
git commit -m "Print every entry in fileset check -v" -m "Matching entries print an EQL line and built-in excludes an SKP line, both on stdout and flushed as they are written (issue #30)."
```

---

### Task 3: `update -v` and `init -v`, and the end of `INFO`

**Files:**
- Modify: `crates/fileset/src/engine/update.rs`, `crates/fileset/src/engine/mod.rs`, `crates/fileset/src/engine/check.rs`, `crates/fileset/src/walk.rs`
- Test: `crates/fileset/tests/common/mod.rs`, `crates/fileset/tests/engine.rs`, `crates/fileset/tests/cli.rs`

**Interfaces:**
- Consumes: `Report` from Task 2; `line_entry`, `line_skp`, `line_del` from `report.rs`.
- Produces:
  - `Warnings::new(err: &'a mut dyn Write) -> Self` (the `verbose` parameter and `Warnings::info` are gone).
  - In `tests/common/mod.rs`: `pub fn run_update_out(opts: &UpdateOpts) -> anyhow::Result<(Outcome, String, String)>` returning outcome, stdout and stderr. `run_update_opts` keeps asserting that stdout is empty, which is the "nothing without `-v`" check for every existing update test.

- [ ] **Step 1: Write the failing tests** in `tests/engine.rs` (each for both backends) using `run_update_out` with `opts.common.verbose = true`.

  - `init_verbose_prints_add_per_entry`: tree `[("f", "x"), ("d/g", "y")]`, `f` at mode `0o644` and mtime `(1_000_000_000, 5)`. Assert stdout contains the exact line `ADD f kind=file size=1 mtime=2001-09-09T01:46:40.000000005Z mode=0644 blake3=<hex of blake3("x")>`, every line starts with `ADD `, and the number of lines equals `entries=N` in the summary.
  - `update_verbose_prints_eql_chg_add_del`: init tree `[("same", "x"), ("edit", "x"), ("gone", "x")]`; then write `"xy"` to `edit`, remove `gone`, create `new` with `"z"`; update with `-v`. Assert stdout has a line starting `EQL same kind=file size=1 `, one starting `CHG edit kind=file size=2 ` that ends with `blake3=<hex of blake3("xy")>`, one starting `ADD new kind=file size=1 `, and the exact line `DEL gone`. Assert `DEL gone` comes after every `EQL`, `CHG` and `ADD` line.
  - `update_verbose_rehash_prints_eql` (Review Focus 5): init tree `[("f", "x")]`; update with `rehash: true` and `-v`. Assert the line for `f` starts with `EQL f ` and contains `blake3=`. Do not assert on `.` or the manifest's own entry: writing the manifest changes both.
  - `update_verbose_unreadable_file_has_no_checksum` (Review Focus 3): tree `[("f", "x")]`; `chmod` `f` to `0o000`; return early, as `unreadable_spotlight_tree` does, if the file can still be read. Init with `-v`. Assert stderr contains `WARN cannot hash f`, and the stdout line starting `ADD f ` has no `blake3=`. Restore the mode before the temp dir drops.
  - `update_verbose_drops_old_builtin_exclude_rows` (Review Focus 4): tree `[("f", "x"), (".Trashes/501/old", "y")]`; init with `builtin_excludes = false`; update with `-v` and built-in excludes on. Assert stdout contains `SKP .Trashes reason=builtin-exclude` and the exact lines `DEL .Trashes`, `DEL .Trashes/501` and `DEL .Trashes/501/old`.
  - Change `update_skips_unreadable_builtin_exclude`: its `-v` half now uses `run_update_out` and asserts stdout contains `"SKP .Spotlight-V100 reason=builtin-exclude\n"` and stderr does not contain `INFO`.
  - Change `cli_builtin_excludes_flags` in `tests/cli.rs`: `init -v` now has `SKP .Trashes reason=builtin-exclude\n` on stdout and no `INFO` on stderr.

- [ ] **Step 2: Add `run_update_out`** to `tests/common/mod.rs`, then run `cargo test -q -p fileset --test engine verbose` and see the new tests fail (stdout empty).
- [ ] **Step 3: Print entry lines in `update.rs`.** Wrap `_out` in a `Report`. In the sink, for each `WalkEvent::Entry` print through `verbose_line`, after the hash result is known and before the entry is batched: `ADD` when the manifest held nothing for the path, `EQL` when the row is unchanged (the existing `unchanged` test), `CHG` otherwise. All three use `line_entry` with the entry as recorded. `WalkEvent::Excluded` prints `line_skp`.
- [ ] **Step 4: Print `DEL` from the sweep.** `Store::sweep` calls `keep` exactly once per stored path in both backends, in key order. Wrap the existing `seen.contains(path)` closure so that, when it returns `false`, it prints `line_del(path)` through `verbose_line`. `keep` is a `&dyn Fn` that returns `bool`, so hold the `Report` in a `RefCell`, keep the first write error in a `RefCell<Option<io::Error>>`, and return that error after `sweep` returns.
- [ ] **Step 5: Remove `INFO`.** Delete `Warnings::info` and the `verbose` field and parameter in `engine/mod.rs`, update both `Warnings::new` call sites, and delete `walk::excluded_info`.
- [ ] **Step 6: Run** `cargo test -q -p fileset --all-features --no-fail-fast`. Expected: all pass.
- [ ] **Step 7: Commit**

```bash
git add crates/fileset
git commit -m "Print every entry in fileset update -v and init -v" -m "ADD, CHG, EQL, DEL and SKP lines go to stdout under -v. The SKP line replaces the INFO line on stderr, so INFO is gone (issue #30)."
```

---

### Task 4: No spinner under `-v`, help text and the book

**Files:**
- Modify: `crates/fileset/src/main.rs`, `crates/fileset/src/cli.rs`, `book/src/fileset/reference.md`
- Test: `crates/fileset/src/main.rs` (unit test), `crates/fileset/tests/cli.rs`

**Interfaces:**
- Produces, in `main.rs`: `fn show_spinner(quiet: bool, verbose: bool, stderr_is_terminal: bool) -> bool`. `progress_for` takes `verbose` as well and uses it.

- [ ] **Step 1: Write the failing tests.**

In `main.rs`:

```rust
#[test]
fn spinner_only_on_a_terminal_without_quiet_or_verbose() {
    assert!(show_spinner(false, false, true));
    assert!(!show_spinner(false, true, true));
    assert!(!show_spinner(true, false, true));
    assert!(!show_spinner(false, false, false));
}
```

In `tests/cli.rs`:

  - `cli_check_verbose_lists_entries`: `init` the `tree()` fixture, then `check -v`. Exit code 0; every stdout line starts with `EQL `; stdout is not empty. Then plain `check`: stdout is empty.
  - `cli_verbose_help_names_the_entry_lines`: `check --help` output contains `one line per entry`.
  - `cli_check_verbose_survives_closed_stdout` (Review Focus 1): build a tree of 2000 small files, `init -q`, then spawn `check -v` with stdout piped, drop the read end at once, and wait. Assert the exit code is 2 or 0, the process was not killed by a signal, and stderr does not contain `panicked`. (0 is allowed: a fast run can finish before the pipe closes.)

- [ ] **Step 2: Run** `cargo test -q -p fileset show_spinner cli_` and see the new tests fail.
- [ ] **Step 3: Implement `show_spinner`** and thread `verbose` from `common_opts` to `progress_for` in `main.rs`. `common_opts` returns the quiet flag today; return what `progress_for` needs.
- [ ] **Step 4: Change the `-v` help text** in `cli.rs` to: `Print one line per entry on stdout, without progress output`.
- [ ] **Step 5: Update `book/src/fileset/reference.md`.**
  - The `-v, --verbose` row: prints one line per entry on stdout, turns progress off, conflicts with `--quiet`; link to the Output section.
  - Output section: keep what is there for `check` without `-v`. Add the `-v` lines: the two prefix tables from the spec (`check`, and `init`/`update`), the five-line example from the spec, the entry form with the rule for when `blake3=` is left off, and `SKP <path> reason=builtin-exclude`. Say that `DEL` lines come after the walk.
  - The stderr paragraph: progress shows only when stderr is a terminal and neither `-q` nor `-v` is given.
  - Search `book/src` and `crates/fileset/README.md` for `INFO` and fix any mention of the old line.
- [ ] **Step 6: Run** `tools/ci.sh check` from the repo root. Expected: it ends without a failure.
- [ ] **Step 7: Commit**

```bash
git add crates/fileset book/src/fileset/reference.md
git commit -m "Turn the fileset spinner off under -v and document the lines" -m "The per-entry lines and the spinner would share the terminal otherwise. The reference page lists the five prefixes (issue #30)."
```
