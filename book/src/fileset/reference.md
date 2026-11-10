# fileset

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
| `-v, --verbose` | Also print an `INFO` line for each built-in exclude skipped. Conflicts with `--quiet`. |

More walk threads are often slower. On a 16-core machine with an APFS SSD, a
no-change `update` of a 1.7 million entry tree took 24 s with 4 threads and
30 s with 8: the kernel serialises much of the metadata work.

Without `--manifest`, `fileset` looks for `.fileset.sqlite` and
`.fileset.fjall` at the root. If both exist, it stops and asks for
`--manifest`.

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

Warnings go to stderr, prefixed `WARN `. Progress goes to stderr only when
stderr is a terminal and `-q` is not given. A summary line goes to stderr at
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
| `0` | Clean. |
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
- `mtime` is compared to the nanosecond, so copying to a filesystem with
  coarser timestamps shows `mtime=` changes. Use `--ignore mtime` or
  `--relaxed`.
- macOS ACLs are not recorded.
- Mount points are not crossed unless `--cross-mount-points` is given. On
  btrfs every subvolume reports its own device number, so a tree that contains
  subvolumes or snapshots is cut short by default: a warning names the
  directory, `check` reports what lies below it as `unverified`, and `update`
  drops it from the manifest. Such a tree needs `--cross-mount-points`.
- fjall preallocates a sparse 64 MiB journal in the manifest directory. Copy
  a fjall manifest with a sparse-aware tool, such as `cp --sparse=always` or
  `rsync --sparse`, to avoid writing 64 MiB of zeros.
