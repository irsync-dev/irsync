# `fileset list`

Date: 2026-11-16

Issue: #29

## Intent

A study user reached for `fileset list` and found no such command. Today
the only way to learn what a manifest holds is to run `check` against a
tree and read the differences. There is no way to ask "what is recorded
here, and with which checksum?".

`fileset list` prints the content of a manifest: every entry with its
checksum and metadata, in the line format `check -v` already prints.

## Goals

- `fileset list PATH [PATTERN]` prints one line per manifest entry.
- With no pattern, every entry. With a pattern, the entries whose path
  matches it.
- The main fields by default, everything recorded with `-v`.
- `ls` is another name for `list`.

## Non-goals

- Reading the tree. `list` only reads the manifest; it does not say whether
  the tree still matches. That is `check`.
- Sorting, columns, totals, or output formats other than the line below.
- Showing the manifest's own details (created, updated, tool version).
- Using the pattern to read less of the manifest. Every entry is read and
  tested.
- Patterns on `check`, `init` or `update`.

## Anti-goals

- `list` must never write to the manifest or to the tree.
- The line must not drift from what `check -v` prints. A script that reads
  one should read the other.

## Approaches

For the pattern matcher:

1. **A small matcher inside fileset.** Chosen. About 80 lines plus tests,
   on raw bytes, no new dependency. It follows the rules of irsync's
   `--exclude` patterns. Cost: two implementations to keep alike.
2. **Move irsync's matcher (`crates/irsync/src/filter.rs`) into a shared
   crate.** Rejected for now: a new crate and a change on the sync path,
   for a listing feature. Can be done later if a third user appears.
3. **The `globset` library.** Rejected: new third-party crates to review,
   and rules that differ slightly from irsync's.

For reading extended attributes: the walk over a SQLite manifest does one
extra lookup per entry today. It changes to a single left join on the
attribute table, as the single-entry lookup already does. Measured on one
million entries: 0.15 s without attributes, 0.27 s with the join. So `list`
has one code path for both modes, and `check` gets the faster walk too.

## Design

### Command

```
fileset list [-v] [--manifest MPATH] PATH [PATTERN]
fileset ls   ...
```

Decided:

- `PATH` is the root of the tree, as on the other commands. `--manifest`
  works as it does there.
- `PATTERN` is optional. Without it, every entry is listed.
- `-v` prints everything recorded for each entry.

My calls:

- None of the other shared options (`--jobs`, `--walk-threads`,
  `--cross-mount-points`, `--no-dir-sort`, `--no-builtin-excludes`,
  `--fail-on-warnings`, `-q`) exist on `list`. They are about walking and
  hashing.
- With `--manifest`, `PATH` does not have to exist: a copy of a manifest
  can be listed without its tree.
- A manifest that cannot be opened is treated as `check` treats it: the
  same errors, exit code 2. This includes a manifest left unfinished by an
  interrupted `init`, if `check` refuses it.

### Output

Decided: one line per entry on stdout, in the order the manifest stores
them. No prefix: the line starts with the path. The fields are those of
`check -v` (`line_entry` in `crates/fileset/src/report.rs`), plus `target=`
for a symlink.

```
$ fileset list ~/Photos '2026/**'
2026 kind=dir size=4096 mtime=2026-01-31T12:00:00.000000005Z mode=0755
2026/01/IMG_0001.jpg kind=file size=48213 mtime=2026-01-15T12:00:00.000000000Z mode=0644 blake3=81c4b7f7e0549f1514e9cae97cf40cf133920418d3dc71bedbf60ec9bd6148cb
2026/latest kind=symlink size=12 mtime=2026-01-31T12:00:00.000000005Z mode=0777 target=01/IMG_0001.jpg
```

- `<path> kind= size= mtime= mode=`, then `blake3=` when the entry has a
  checksum, then `target=` when it is a symlink with a recorded target.
- Value formats and escaping are the ones the reference documents under
  "Output".

With `-v`, the line continues with:

- `uid=` and `gid=`, `nlink=`
- `rdev=` for a character or block device
- one `xattr:<name>=<value>` per extended attribute, in name order, the
  value cut at 64 bytes as in `CHG` lines

My calls:

- The root of the tree is listed as `.`, which is how it is stored.
- Nothing is printed on stderr on success: no progress, no summary.
- When stdout is closed early (`fileset list ~/Photos | head`), `list`
  stops quietly and exits 0. Any other write error is exit 2.

### Order

Decided: byte order of the whole path, which is how both backends store
entries. No sorting is done. A directory is followed by its contents,
except that a sibling whose name continues with a byte below `/` (space,
`-`, `.`, `+`, `,`) comes in between:

```
2024
2024-old
2024.txt
2024/a.jpg
```

### Pattern

Decided: a glob pattern, matched against the whole path below the root.

My calls, following irsync's `--exclude` patterns:

- `*` matches any run of bytes except `/`.
- `**` matches any run of bytes, `/` included.
- `?` matches one byte except `/`.
- `[abc]`, `[a-z]`, `[!a-z]` and `[^a-z]` match one byte except `/`.
- `\` takes the next byte literally.
- Anything else matches itself. A `[` with no closing `]` is a plain `[`.
  No pattern is an error.
- The match is on bytes and is case-sensitive.

So `*.jpg` lists the top level only, `**/*.jpg` every level below the top,
`2026/**` everything under `2026`, and `2026` only that one entry. A
pattern that matches a directory does not list what is inside it.

The pattern is matched against the stored path, not the escaped form that
is printed.

### Exit codes

- 0: listed, also when the pattern matched nothing.
- 2: the manifest could not be found or read.

### Docs

`book/src/fileset/reference.md`: a `list` section under Commands, its
options, the line under Output (with the `-v` fields and the pattern
rules), and exit code 0 for "nothing matched". One sentence in
`book/src/fileset/introduction.md` or the tutorial where the commands are
first named.

## Tests

- Every entry kind lists with the right fields; a symlink shows `target=`;
  a directory has no `blake3=`.
- `-v` adds `uid`, `gid`, `nlink`, `rdev` for devices, and attributes in
  name order.
- The default line of a regular file equals the `EQL` line `check -v`
  prints for it, minus the prefix.
- Both backends print the same listing for the same tree.
- Order: the `2024` example above.
- Pattern: each rule above, a pattern that matches nothing (exit 0, no
  output), and a path with bytes that are not UTF-8.
- `--manifest` on a copy of a manifest with no tree.
- No manifest: exit 2 and the error `check` gives.
- `ls` behaves as `list`.
- The manifest file is unchanged after `list` (same bytes, same mtime).
- The SQLite walk returns the same entries and attributes as before the
  join, including entries with none.

## Left to the builder

- Where the matcher and the list code live inside the crate.
- How `-v` fields are added to the line without changing `line_entry`'s
  output for the other commands.
- Whether the fjall backend needs any change (its walk already reads
  attributes with the entry).
