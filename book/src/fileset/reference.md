# fileset reference

`fileset` records the state of a directory tree in a manifest and later
verifies the tree against it. It lives in `crates/fileset` and shares no code
with `irsync`. For an introduction, see [Introduction to fileset](introduction.md).
The how-to pages are [Verify an rsync or rclone transfer](verify-other-tools.md),
[Verify an irsync transfer](verify-irsync.md), [Check data for bitrot](bitrot.md)
and [Keep a manifest current](keep-current.md).

Install it with:

```sh
cargo install --git https://github.com/irsync-dev/irsync fileset
```

## Commands

```text
fileset init   [OPTIONS] <PATH>
fileset update [OPTIONS] <PATH>
fileset check  [OPTIONS] <PATH>
fileset list   [-v] [--manifest <MPATH>] <PATH> [PATTERN]
```

`<PATH>` is the root of the tree and must be a directory.

### init

`init` scans the tree and creates its manifest, at `<PATH>/.fileset.sqlite`
by default. It refuses to run when:

- a manifest already exists. The exception is a manifest left by an `init`
  that was interrupted (killed, or the machine went down): it holds nothing,
  so `init` replaces it with a warning.
- the tree lies inside another fileset: a `.fileset.sqlite` or
  `.fileset.fjall` exists in a parent directory of `<PATH>`. This is checked
  before anything is written.
- the tree holds a manifest in a subdirectory. This is found when the scan
  reaches it; `init` then removes the manifest it had begun.

Nested filesets are not supported. If the scan fails, `init` removes the
manifest it began.

### update

`update` scans the tree and brings an existing manifest up to date. It
refuses to run if there is no manifest, or if the manifest was left by an
interrupted `init` (run `init` again). A manifest in a subdirectory is a
warning, not an error.

A regular file is hashed again only when its size, `mtime` or inode differ
from the manifest, or when the manifest holds no hash for it. Otherwise the
stored hash is kept. A file that cannot be read produces the warning
`cannot hash <path>` and is recorded without a hash. Rows that have not changed are not rewritten. Whatever
the scan did not see is removed from the manifest: deleted entries, and also
everything below a directory that could not be read or a mount point that was
not crossed. Each such directory and mount point produces a warning.

`init` and `update` share one engine and differ only in these preconditions.

### check

`check` scans the tree and reports every difference between it and the
manifest. It never changes the tree or the manifest's contents. Opening a fjall
manifest does touch its journal and lock files.

`check` hashes a regular file only when the manifest has an entry for it and
content is compared. A file the manifest holds no hash for (it could not be
read when it was recorded) produces the warning
`no stored checksum for <path>`. A manifest in a subdirectory produces the
warning `nested manifest at <path>`.

### list

`list` prints what the manifest holds: one line per entry, with its checksum
and metadata (see [Listing](#listing)). `fileset ls` is the same command.

`list` reads only the manifest. It does not look at the tree, so it says what
was recorded, not whether the tree still matches; that is `check`. It never
changes the manifest's contents or the tree. As with `check`, opening a fjall
manifest does touch its journal and lock files; a SQLite manifest is only
read. With `--manifest`, `<PATH>` does not have to exist, so a copy of a
manifest can be listed without its tree.

With a `PATTERN`, only the entries whose path matches it are listed. Quote the
pattern, or the shell expands it first.

## Options

### All commands

| Option | Effect |
| --- | --- |
| `--manifest <MPATH>` | Use the manifest at `MPATH` instead of `<PATH>/.fileset.sqlite` or `<PATH>/.fileset.fjall`. It may lie outside the tree. A directory is read as fjall, a file as SQLite. |
| `--fail-on-warnings` | Exit 1 instead of 0 when the run produced warnings. |
| `--cross-mount-points` | Descend into directories on other filesystems. |
| `--no-dir-sort` | Do not sort directory entries before iterating. |
| `-j, --jobs <N>` | Threads that hash file contents. Default: the number of CPUs. `N` is at least 1. |
| `--walk-threads <N>` | Threads that list directories and read metadata. Default: 4. `N` is at least 1. |
| `--no-builtin-excludes` | Walk the [built-in excludes](#built-in-excludes) like any other directory. |
| `-q, --quiet` | Suppress progress. |
| `-v, --verbose` | Print one line per entry on stdout as it is handled (see [Every entry, with `-v`](#every-entry-with--v)), and turn progress off. Conflicts with `--quiet`. |

More walk threads are often slower. On a 16-core machine with an APFS SSD, a
no-change `update` of a 1.7 million entry tree took 24 s with 4 threads and
30 s with 8: the kernel serialises much of the metadata work.

Without `--manifest`, `fileset` looks for `.fileset.sqlite` and
`.fileset.fjall` at the root. If both exist, it stops and asks for
`--manifest`.

The options above are for the commands that walk a tree. Of them, `list`
takes only `--manifest`.

### init only

| Option | Effect |
| --- | --- |
| `--store <sqlite\|fjall>` | Backend to create. Default: `sqlite`. |

### update only

| Option | Effect |
| --- | --- |
| `--rehash` | Hash every regular file again, even if size, `mtime` and inode are unchanged. |
| `--force-xattr-reads` | Read every entry's extended attributes. By default an entry whose type, inode, size, `mtime` and `ctime` match the manifest keeps the attributes recorded there (changing an attribute updates the `ctime`); only an entry whose attributes could not be read in full is read again. |

### check only

| Option | Effect |
| --- | --- |
| `--relaxed` | Compare the relaxed tier (below). Conflicts with `--paranoid`. |
| `--paranoid` | Compare the paranoid tier (below). Conflicts with `--relaxed`. |
| `--ignore <FIELD>` | Drop one field from the comparison. Repeatable. |
| `--require <FIELD>` | Add one field to the comparison. Repeatable. |

### list only

| Option | Effect |
| --- | --- |
| `-v, --verbose` | Also print the owner, the link count, device numbers and extended attributes of each entry. |

## Comparison tiers

| Tier | Compared |
| --- | --- |
| `--relaxed` | type, content (`blake3`), symlink `target`, device numbers (`rdev`) |
| default | relaxed, plus `mode`, `mtime`, `size` |
| `--paranoid` | default, plus `uid`, `gid`, `nlink`, `xattr` |

The type is always compared. `--ignore` and `--require` adjust whichever tier
is in effect, so "default plus owner" is `--require uid --require gid`. Field
names:

```text
blake3 target rdev mode mtime size uid gid nlink xattr
```

An unknown field name is an error (exit 2).

Some fields apply only to some types:

- `blake3` applies to regular files, `target` to symlinks, `rdev` to
  character and block devices.
- `size` and `nlink` are not compared for directories.
- `mtime` is not compared for the root, nor for the directory that holds the
  manifest when the manifest lies inside the tree: writing the manifest
  changes both.
- A manifest inside the tree is recorded, but only its type is compared.

## Output

`check` writes one line per differing entry to stdout:

```text
ADD docs/new.txt kind=file
DEL docs/old.txt
CHG docs/a.txt mode=0644→0600 size=12→13
```

- `ADD`: on disk, not in the manifest. `kind` is one of `dir`, `file`,
  `symlink`, `fifo`, `socket`, `chardev`, `blockdev`, `other`.
- `DEL`: in the manifest, not on disk. This includes everything recorded
  below a directory that is now a file or a symlink.
- `CHG`: in both, with at least one compared field differing. Each field is
  shown as `field=<stored>→<observed>`. If the type differs, only `kind=` is
  shown.
- `CHG <path> kind=<stored kind>→unverified`: recorded in the manifest, but
  `check` could not observe it. It lies below a directory that could not be
  read or a mount point that was not crossed, or it could not be stat'ed.
  Content that cannot be verified counts as a difference (exit 1), not just a
  warning.

Field values:

| Field | Shown as |
| --- | --- |
| `blake3` | 64 hexadecimal digits; `unreadable` when the file could not be read |
| `mode` | four octal digits, such as `0644` or `4755` |
| `mtime` | UTC with nanoseconds, such as `2026-01-31T12:00:00.000000005Z` |
| `size`, `uid`, `gid`, `nlink`, `rdev` | decimal |
| `target` | the link target; `-` if none |
| `xattr` | one `xattr:<name>=<stored>→<observed>` per attribute that differs; `-` where the attribute is absent; values longer than 64 bytes are cut and end in `…` |

Paths, link targets and attribute names and values are escaped. Printable
UTF-8 and the space pass through, `\` becomes `\\`, control bytes, DEL and
bytes that are not valid UTF-8 become `\xNN`, and C1 controls and the bidi
formatting characters become `\u{...}`.

### Every entry, with `-v`

With `-v`, every entry gets a line on stdout, in the order it is walked and
as soon as it is handled. `check` adds two kinds of line to the ones above:

| Prefix | Meaning | Printed |
| --- | --- | --- |
| `EQL` | in both, every compared field matches | `-v` only |
| `CHG` | in both, something differs | always |
| `ADD` | on disk, not in the manifest | always |
| `DEL` | in the manifest, not on disk | always |
| `SKP` | skipped on purpose | `-v` only |

```text
EQL 2026/01/IMG_0001.jpg kind=file size=48213 mtime=2026-01-15T12:00:00.000000000Z mode=0644 blake3=81c4b7f7e0549f1514e9cae97cf40cf133920418d3dc71bedbf60ec9bd6148cb
CHG 2026/01/IMG_0002.jpg mode=0644→0600
ADD new.jpg kind=file
SKP .Spotlight-V100 reason=builtin-exclude
DEL old.jpg
```

`init` and `update` print nothing on stdout without `-v`. With it, they use
the same prefixes for what happened to the manifest:

| Prefix | Meaning |
| --- | --- |
| `ADD` | new entry recorded |
| `CHG` | entry changed in a way `check` could report |
| `EQL` | nothing `check` could report changed |
| `DEL` | entry dropped: the path is gone or is no longer walked |
| `SKP` | skipped on purpose |

- `EQL` in `check`, and `ADD`, `CHG` and `EQL` in `init` and `update`, show
  the entry in full: `<path> kind= size= mtime= mode=`, then `blake3=` when
  the entry has a checksum. `check` shows what is on disk; `init` and
  `update` show the entry as now recorded.
- `blake3=` is left off for anything that is not a regular file, for the
  manifest itself, for a file that could not be read, and in a `check` that
  does not compare content (`--ignore blake3`).
- `SKP <path> reason=builtin-exclude`: one of the
  [built-in excludes](#built-in-excludes) was skipped.
- `DEL` lines come after the walk, in `check` and in `update`.
- In `update`, `CHG` covers every field `check --paranoid` compares, and a
  checksum gained or lost. The root's mtime and a manifest inside the tree
  change whenever the manifest is written; `check` does not report that, so
  both show as `EQL`.
- In `update`, `DEL` is also printed for entries that are still on disk but
  no longer walked: those below a built-in exclude, and those below a
  directory that could not be read.

`grep -v '^EQL'` leaves what differs.

### Listing

`list` writes one line per manifest entry to stdout, and nothing to stderr:

```text
$ fileset list ~/Photos '2026/**'
2026/01 kind=dir size=4096 mtime=2026-01-31T12:00:00.000000005Z mode=0755
2026/01/IMG_0001.jpg kind=file size=48213 mtime=2026-01-15T12:00:00.000000000Z mode=0644 blake3=81c4b7f7e0549f1514e9cae97cf40cf133920418d3dc71bedbf60ec9bd6148cb
2026/latest kind=symlink size=15 mtime=2026-01-31T12:00:00.000000005Z mode=0777 target=01/IMG_0001.jpg
```

The line is the one `check -v` prints for an entry, without a prefix:
`<path> kind= size= mtime= mode=`, then `blake3=` when the entry has a
checksum. A symlink also shows `target=`. The root of the tree is listed as
`.`.

With `-v`, each line continues with `uid=`, `gid=` and `nlink=`, then `rdev=`
for a character or block device, then one `xattr:<name>=<value>` per extended
attribute, in name order. A value longer than 64 bytes is cut and ends in
`…`.

Entries come in the order the manifest stores them: by path, compared byte by
byte. Uppercase sorts before lowercase and `img10` before `img9`. A directory
is followed by its contents, except that a neighbour whose name goes on with
a character that sorts before `/` (a space, `-`, `.`, `+` or `,`) comes in
between:

```text
2024
2024-old
2024.txt
2024/a.jpg
```

The pattern is matched against the whole path below the root:

| In the pattern | Matches |
| --- | --- |
| `*` | any run of characters except `/` |
| `**` | any run of characters, `/` included |
| `?` | one character except `/` |
| `[abc]`, `[a-z]` | one of these characters; `[!a-z]` or `[^a-z]`: any other, except `/` |
| `\` and a character | that character itself |
| anything else | itself |

So `'*.jpg'` lists the top level only, `'**/*.jpg'` every level below it,
`'2026/**'` everything under `2026`, and `2026` that one entry: a pattern
that matches a directory does not list what is inside it. Upper and lower
case differ. Trailing slashes and a leading `./` are ignored, so `2026/`
means `2026`. No pattern is an error; one that matches nothing prints
nothing. The wildcards are those of irsync's `--exclude` patterns, but here
a pattern always covers the whole path: there, `*.jpg` matches at every
level and a leading `/` ties a pattern to the top.

When its reader stops early, as in `fileset list ~/Photos | head`, `list`
ends without an error.

### Warnings and summary

Warnings go to stderr, prefixed `WARN `. Progress goes to stderr only when
stderr is a terminal and neither `-q` nor `-v` is given. A summary line goes to stderr at
the end:

```text
added=1 deleted=1 changed=1 unchanged=40 warnings=0     (check)
entries=43 hashed=3 reused=40 warnings=0                (init, update)
```

## Built-in excludes

macOS keeps `.DocumentRevisions-V100`, `.Spotlight-V100`, `.TemporaryItems`,
`.Trashes` and `.fseventsd` at the top of every volume it touches, and
ordinary users cannot read them. At the root of the tree, and at every mount
point crossed with `--cross-mount-points`, entries with these names are
neither recorded nor descended, on every OS: on a disk read elsewhere they
are readable but still junk. Skipping one is not a warning, so it does not
trip `--fail-on-warnings`. The same names deeper in the tree are ordinary
entries.

A manifest written with `--no-builtin-excludes` may hold them. `check`
without that option does not report them, and the next `update` without it
drops them.

## Files

| Path | Contents |
| --- | --- |
| `<PATH>/.fileset.sqlite` | The manifest, SQLite backend (default). Its `-wal`, `-shm` and `-journal` sidecars are skipped by the scan. |
| `<PATH>/.fileset.fjall` | The manifest, fjall backend: a directory. |

An `update` that finishes leaves the SQLite manifest as a single file. If
another process holds it open, the `-wal` sidecar stays, with a warning; the
next `update` folds it in.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Clean. For `list`: the entries were printed, also when none matched. |
| `1` | `check` found differences; or, with `--fail-on-warnings`, the run produced warnings. The second is the only way `init` and `update` return 1. |
| `2` | The run itself failed. |

Exit code 2 covers, among others: a manifest that cannot be opened or read;
a root that is not a directory; a manifest schema newer than this `fileset`;
`check` on a manifest schema older than this `fileset` (`update` migrates
it); both manifests at the root without `--manifest`; `init` on a tree that
already has a manifest, lies inside a fileset or contains a nested one;
`update` on a tree that has none.

## Read-only media

See [Check data for bitrot](bitrot.md#read-only-media-and-snapshots).

## Known limitations

- Symlink permission bits differ between Linux (always `0777`) and macOS
  (set from the umask, often `0755`), so a tree carried between them shows
  `mode=` changes on symlinks. Use `--ignore mode` or `--relaxed`.
- fjall preallocates a sparse 64 MiB journal in the manifest directory. Copy
  a fjall manifest with a sparse-aware tool, such as `cp --sparse=always` or
  `rsync --sparse`, to avoid writing 64 MiB of zeros.

Holes in sparse files, hard links, ACLs, mount points, timestamp precision
and the other things a check cannot see are in
[What fileset cannot catch](how-it-works.md#what-fileset-cannot-catch).
