# fileset

`fileset` records the state of a directory tree in a manifest stored at the
root of that tree, and later verifies the tree against it. It serves two jobs:
archival integrity (run `init` once, run `check` months or years later, and
learn whether any file was lost, any byte flipped or any metadata drifted), and
independent validation of irsync (`init` on the source, let irsync carry the
tree and its manifest to the destination, `check` there), using code that
shares nothing with irsync. Full design:
[docs/superpowers/specs/2026-10-19-fileset-design.md](../../docs/superpowers/specs/2026-10-19-fileset-design.md).

## Commands

```
fileset init   [OPTIONS] <PATH>
fileset update [OPTIONS] <PATH>
fileset check  [OPTIONS] <PATH>
```

`init` scans the tree and creates its manifest; it refuses to run if one
already exists, unless that one was left by an `init` that was interrupted
(killed, or the machine went down): such a manifest holds nothing worth
keeping, `init` replaces it with a warning, and `update` refuses it and says
to run `init` again. Because nested filesets are not supported, it also refuses a
tree that lies inside an existing fileset (a `.fileset.sqlite` or
`.fileset.fjall` in any parent directory of `<PATH>`, checked before anything
is written) and a tree that holds one in any subdirectory (found when the
scan reaches it, after which the manifest begun at the root is removed
again). `update` scans the tree and
brings an existing manifest up to date; it refuses to run if none exists, and
only warns about nested manifests, as `check` does. `init` and `update` share
one engine and differ only in those preconditions. `check` scans the tree and reports every
difference between it and the manifest; it never modifies the tree's entries
or the manifest's contents. (Opening a fjall manifest does touch its journal
and lock files.)

All verbs:

- `--manifest <MPATH>`: location of the manifest instead of
  `<PATH>/.fileset.sqlite` or `<PATH>/.fileset.fjall`; may be outside the tree
- `--fail-on-warnings`: exit 1 instead of 0 when the run produced warnings
- `--cross-mount-points`: descend into directories on other filesystems
- `--no-dir-sort`: do not sort directory entries before iterating
- `-j, --jobs <N>`: hashing threads (default: CPU count); `N` must be at least 1
- `--no-builtin-excludes`: walk the built-in excludes (below) like any other
  directory
- `-q, --quiet`: suppress progress
- `-v, --verbose`: also print an `INFO` line for each built-in exclude
  skipped; conflicts with `--quiet`

`init` only:

- `--store <sqlite|fjall>`: backend to create (default: sqlite)

`update` only:

- `--rehash`: re-hash every regular file even if size, mtime and inode are
  unchanged

`check` only:

- `--relaxed`: compare only type, content and symlink target
- `--paranoid`: compare everything that is recorded and comparable
- `--ignore <FIELD>`: drop one field from the comparison (repeatable)
- `--require <FIELD>`: add one field to the comparison (repeatable)

## Comparison tiers

| Tier | Compared |
| --- | --- |
| `--relaxed` | type, content (`blake3`), symlink `target`, device numbers (`rdev`) |
| default | relaxed, plus `mode`, `mtime`, `size` |
| `--paranoid` | default, plus `uid`, `gid`, `nlink`, `xattr` |

`--ignore` and `--require` adjust whichever tier is in effect, so "default plus
owner" is `--require uid --require gid`. Field names:
`blake3 target rdev mode mtime size uid gid nlink xattr`.

## Output

One line per differing object, to stdout:

```
ADD docs/new.txt kind=file
DEL docs/old.txt
CHG docs/a.txt mode=0644→0600 size=12→13
```

- `ADD`: on disk, not in the manifest.
- `DEL`: in the manifest, not on disk.
- `CHG`: in both, with at least one compared field differing; each field is
  shown as `field=<stored>→<observed>`. If the type differs, only `kind=` is
  shown.
- `CHG <path> kind=<stored kind>→unverified`: recorded in the manifest, but
  `check` could not observe it: it lies below a directory that could not be
  read or a mount point that was not crossed, or it could not be stat'ed.
  Content that cannot be verified counts as a difference (exit 1), not just a
  warning.

Warnings go to stderr prefixed `WARN `. Progress goes to stderr only when
stderr is a terminal and `-q` is not given. A summary line goes to stderr at
the end:

```
added=1 deleted=1 changed=1 unchanged=40 warnings=0     (check)
entries=43 hashed=3 reused=40 warnings=0                (init, update)
```

## Built-in excludes

macOS keeps `.DocumentRevisions-V100`, `.Spotlight-V100`, `.TemporaryItems`,
`.Trashes` and `.fseventsd` at the top of every volume it touches, and
ordinary users cannot read them. At the root of the tree, and at every mount
point crossed with `--cross-mount-points`, entries with these names are
neither recorded nor descended, on every OS: on a disk read elsewhere they are
readable but still junk. Skipping one is not a warning, so it does not trip
`--fail-on-warnings`. The same names deeper in the tree are ordinary entries.

A manifest written before this (or with `--no-builtin-excludes`) may hold
them: `check` does not report them, and the next `update` drops them.

## Exit codes

- `0`: clean
- `1`: `check` found differences; or, with `--fail-on-warnings`, the run
  produced warnings (this is the only way `init` and `update` return 1)
- `2`: the run itself failed (manifest unreadable, root not a directory,
  schema too new, `init` on a tree that already has a manifest, lies inside
  a fileset or contains a nested one, `update` on a tree that has none, ...)

## Read-only media

To check a read-only mounted backup or a snapshot, copy the manifest off it and
point `--manifest` at the copy. `check` with the manifest inside a read-only
tree may fail with exit 2 and a message suggesting `--manifest`.

## Known limitations

- Symlink permission bits differ between Linux (always `0777`) and macOS
  (set from the umask, often `0755`), so a tree carried between them shows
  `mode=` changes on symlinks; use `--ignore mode` or `--relaxed`.
- `mtime` is compared to the nanosecond, so copying to a filesystem with
  coarser timestamps shows `mtime=` changes; use `--ignore mtime` or
  `--relaxed`.
- macOS ACLs are not recorded.
- btrfs subvolumes: mount points are not crossed unless
  `--cross-mount-points` is given, and on btrfs every subvolume reports its
  own device number, so a tree containing subvolumes or snapshots is cut short
  by default (with a warning naming the directory, and `check` reporting what
  lies below it as `unverified`) and needs `--cross-mount-points`.
- fjall preallocates a sparse 64 MiB journal in the manifest directory; copy a
  fjall manifest with a sparse-aware tool (for example `cp --sparse=always` or
  `rsync --sparse`) to avoid writing 64 MiB of zeros.
