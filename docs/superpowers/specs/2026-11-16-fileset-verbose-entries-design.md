# fileset -v: one line per entry

Issue: #30. Size: a small change.

## Intent

User feedback: during `fileset check` the only sign of life is a spinner, and
entries that match print nothing. With `-v`, fileset prints one line on stdout
for every entry as it is handled, so a run can be watched and its output kept
as a full record.

## Goals

- `fileset check -v`, `update -v` and `init -v` print one line per entry on
  stdout, in walk order, as each entry is handled.
- Every line starts with a three-letter prefix, so the lines line up.
- Without `-v`, stdout is byte for byte what it is today.

## Non-goals

- No `-vv` or other levels. `-v` stays a plain switch.
- No new `-P` flag. The spinner stays automatic when `-v` is not given.
- No change to `WARN` lines or the final summary. Both stay on stderr.
- No "hashed or reused" marker on `update` lines.

## Anti-goals

- Changing the `ADD`, `CHG` or `DEL` lines `check` prints today. Scripts and
  five pages of the book rely on them.
- Per-entry lines sharing the terminal with the spinner.

## Design

### `-v` turns the spinner off

With `-v`, no spinner is drawn, as with `filegen -v`. `-v` and `-q` still
conflict. With `-v`, stdout is flushed after every line, so lines appear as
entries are handled and not in bursts.

### Prefixes

Each prefix says how the disk and the manifest relate for one path, and means
the same in every command. One space follows the prefix.

`check`:

| Prefix | Meaning | Printed |
| --- | --- | --- |
| `EQL` | in both, every compared field matches | `-v` only |
| `CHG` | in both, something differs | always |
| `ADD` | on disk, not in the manifest | always |
| `DEL` | in the manifest, not on disk | always |
| `SKP` | skipped on purpose | `-v` only |

`init` and `update` (all `-v` only; they print nothing on stdout today):

| Prefix | Meaning |
| --- | --- |
| `ADD` | new entry recorded |
| `CHG` | entry changed in a way `check` could report |
| `EQL` | nothing `check` could report changed |
| `DEL` | entry dropped: the path is gone or is no longer walked |
| `SKP` | skipped on purpose |

`init` only ever prints `ADD` and `SKP`.

In `update`, `CHG` covers every field `check --paranoid` compares, and a
checksum gained or lost. A row rewritten for anything else is `EQL`: the
root's mtime and an in-tree manifest change on every manifest write, and
`check` does not report either. This keeps `CHG` meaning the same in both
commands and the same for both backends.

### Line formats

```text
EQL 2026/01/IMG_0001.jpg kind=file size=48213 mtime=2026-01-15T12:00:00.000000000Z mode=0644 blake3=81c4b7f7e0549f1514e9cae97cf40cf133920418d3dc71bedbf60ec9bd6148cb
CHG 2026/01/IMG_0002.jpg mode=0644→0600
ADD new.jpg kind=file
SKP .Spotlight-V100 reason=builtin-exclude
DEL old.jpg
```

- The entry form is `<path> kind= size= mtime= mode=`, then `blake3=` when the
  entry has a checksum. Directories, symlinks, and files not hashed in this
  run (`check --ignore blake3`, a hash that failed) have none,
  and the field is left off. Values use the existing formatters in
  `report.rs`; the checksum is printed in full.
- `check`: `EQL` uses the entry form with the values seen on disk. `CHG`,
  `ADD` and `DEL` are unchanged.
- `init` and `update`: `ADD`, `CHG` and `EQL` all use the entry form, with the
  entry as now recorded. `DEL` is `DEL <path>`.
- `SKP <path> reason=<reason>`. The only reason today is `builtin-exclude`.
  This replaces the stderr line `INFO skipping built-in exclude <path>`;
  nothing prints `INFO` any more.

### Order

Lines come in walk order. `DEL` lines come after the walk, as in `check`
today.

## Approaches

1. **Stdout, as lines next to `ADD`/`CHG`/`DEL`. Picked.** One ordered
   stream holding every entry; `fileset check -v > log` is a full record.
   Matches `irsync -v`, which lists files on stdout. Stdout changes only
   under `-v`.
2. **Stderr, own prefix.** Stdout never changes, but the two streams mix out
   of order on a terminal and a full record needs both captured.
3. **Stderr, as `INFO checked …`.** As 2, and harder to tell from the other
   `INFO` lines.

Prefix for a match: `OK` does not line up. Symbols (`===`, `+++`, `---`,
`!!!`, `###`) line up but clash with the shell (`!!!`), regex (`+++`), diff
headers and Markdown, and would change today's lines. `EQL` was picked over
`UNC`, `SAM`, `MAT` and `VFY`: it claims only that the compared fields are
equal.

## Docs

- `book/src/fileset/reference.md`: the `-v` row of the options table and the
  Output section (prefixes, entry form, `init`/`update` output).
- `-v` help text in `crates/fileset/src/cli.rs`.

## Tests

In `crates/fileset/tests/`: for each of `check`, `update` and `init`, stdout
with `-v` holds one line per entry with the right prefix and fields; stdout
without `-v` is unchanged; `SKP` replaces `INFO`; `blake3=` is left off where
there is no checksum.

## Left to the builder

- How `update` learns which paths the cleanup drops. `Store::sweep` already
  calls a `keep` closure once per stored path, so `DEL` can likely be printed
  from there with no backend change. Confirm that both backends call it
  exactly once per path.
- How the "no spinner under `-v`" rule and the per-line flush are wired
  through `main.rs` and the engines.
- Whether `Warnings::info` and its `verbose` field are removed or kept.
