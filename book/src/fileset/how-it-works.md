# How fileset works

This page explains what a manifest holds, what a check compares, and what
`fileset` cannot see. Commands and options are in the [reference](reference.md).

## What a manifest records

`fileset init` walks the tree and records one entry for every file,
directory, symlink and other item it finds. Each entry holds:

- the path and the type: directory, regular file, symlink, fifo, socket,
  character device or block device;
- for a regular file, a BLAKE3 checksum of its content;
- for a symlink, its target; for a device, its device numbers;
- permissions, owner and group, size, modification time and link count;
- extended attributes, which are named values some file systems attach to a
  file.

The manifest is a SQLite file, `.fileset.sqlite`, at the top of the tree. A
second backend, `fjall`, stores it as a directory. The default is the one to
use unless you have a reason.

`update` keeps a stored checksum while a file's size, modification time and
inode are unchanged. That is what makes it fast, and why you
[check before you update](keep-current.md#check-before-you-update).

## Comparison tiers

`check` compares a tree with the manifest. How much it compares depends on
the tier:

| Tier | Compared |
| --- | --- |
| `--relaxed` | type, content, symlink target, device numbers |
| default | relaxed, plus permissions, modification time, size |
| `--paranoid` | default, plus owner, group, link count, extended attributes |

Pick the tier by what the copy tool can keep. A plain `cp` or an `rclone`
copy to a backend without permissions fits `--relaxed`. An `rsync -a` copy
between machines that share user ids fits the default plus
`--require uid --require gid`. `--ignore` and `--require` adjust any tier one
field at a time. The exact field names and the fields that apply only to some
types are in the [reference](reference.md#comparison-tiers).

The tier decides what counts as a difference. Content is compared in every
tier, so changed bytes always show.

## What fileset cannot catch

`fileset` reports differences in what it records. Some things it does not
record, or does not compare, so a copy can differ from the original and still
pass.

- **Holes in sparse files.** `fileset` hashes the bytes a program reads. A
  hole reads as zeros, so a sparse file and a fully written copy of it have the
  same checksum. A check cannot tell whether a copy kept the holes.
- **Hard links.** Only the link count is recorded, and only the paranoid tier
  compares it. `fileset` does not record which paths share a file, so a copy
  that turned hard links into separate files passes the default check.
- **ACLs on macOS.** Access control lists are not recorded.
- **Anything it did not look at.** Mount points are not crossed unless you
  pass `--cross-mount-points`. A tree with btrfs subvolumes is cut short by
  default. `check` reports what lies below as `unverified`, which counts as a
  difference.
- **Timestamp precision.** Modification times are compared to the nanosecond.
  A file system with coarser timestamps shows `mtime=` changes on every file.
  Use `--ignore mtime` or `--relaxed`.
- **A manifest that is wrong from the start.** `fileset` compares against what
  it recorded. If the tree was already damaged at `init`, the manifest records
  the damage.

The limitations the reference lists, such as symlink permission bits between
Linux and macOS and the sparse journal of the `fjall` backend, are in
[Known limitations](reference.md#known-limitations).

`fileset` reports and repairs nothing, and it never changes the tree.
