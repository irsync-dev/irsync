# fileset

`fileset` records the state of a directory tree in a manifest stored at the
root of that tree, and later verifies the tree against it. It serves two jobs:
archival integrity (run `update` once, run `check` months or years later, and
learn whether any file was lost, any byte flipped or any metadata drifted), and
independent validation of irsync (`update` on the source, let irsync carry the
tree and its manifest to the destination, `check` there), using code that
shares nothing with irsync. Full design:
[docs/superpowers/specs/2026-10-19-fileset-design.md](../../docs/superpowers/specs/2026-10-19-fileset-design.md).

## Commands

```
fileset update [OPTIONS] <PATH>
fileset check  [OPTIONS] <PATH>
```

`update` scans the tree and brings the manifest up to date. `check` scans the
tree and reports every difference between it and the manifest; it never writes.

Both verbs:

- `--manifest <MPATH>`: location of the manifest instead of
  `<PATH>/.fileset.sqlite` or `<PATH>/.fileset.fjall`; may be outside the tree
- `--fail-on-warnings`: exit 1 instead of 0 when the run produced warnings
- `--cross-mount-points`: descend into directories on other filesystems
- `--no-dir-sort`: do not sort directory entries before iterating
- `-j, --jobs <N>`: hashing threads (default: CPU count); `N` must be at least 1
- `-q, --quiet`: suppress progress

`update` only:

- `--store <sqlite|fjall>`: backend to create if no manifest exists yet
  (default: sqlite)
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

Warnings go to stderr prefixed `WARN `. Progress goes to stderr only when
stderr is a terminal and `-q` is not given. A summary line goes to stderr at
the end:

```
added=1 deleted=1 changed=1 unchanged=40 warnings=0     (check)
entries=43 hashed=3 reused=40 warnings=0                (update)
```

## Exit codes

- `0`: clean
- `1`: differences found (or, with `--fail-on-warnings`, warnings in a run that
  would otherwise exit 0)
- `2`: the run itself failed (manifest unreadable, root not a directory,
  schema too new, ...)

## Read-only media

To check a read-only mounted backup or a snapshot, copy the manifest off it and
point `--manifest` at the copy. `check` with the manifest inside a read-only
tree may fail with exit 2 and a message suggesting `--manifest`.

## Limitation: btrfs subvolumes

Mount points are not crossed unless `--cross-mount-points` is given. On btrfs
every subvolume reports its own device number, so a tree containing subvolumes
or snapshots is cut short by default (with a warning naming the directory) and
needs `--cross-mount-points`.
