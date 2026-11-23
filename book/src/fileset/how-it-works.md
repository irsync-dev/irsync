# How fileset works

This page explains what a manifest holds, what a check compares, and what
`fileset` cannot see. Commands and options are in the [reference](reference.md).
`fileset` lives in `crates/fileset` and shares no code with `irsync`.

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

The manifest is a SQLite file, `.fileset.sqlite`, at the top of the tree. Two
other backends exist: `fjall` stores it as a directory, and `redb` as a
single file that can be read from media that cannot be written to (see
[The redb backend](reference.md#the-redb-backend)). The default is the one to
use unless you have a reason.

`update` keeps a stored checksum while a file's size, modification time and
inode (on Windows, file index) are unchanged. That is what makes it fast, and
why you [check before you update](keep-current.md#check-before-you-update).
The change time is not part of that decision, on any system.

## On Windows

`fileset` runs on Windows, on NTFS, and records what the two worlds share:
files, directories, content, sizes, modification times, hard links and
symlinks. A Windows manifest holds these values:

| Field | Value on Windows |
| --- | --- |
| `kind` | `dir`, `file` or `symlink`; any other reparse point, a junction included, is `other` |
| `size` | the size in bytes; 0 for a directory and for a symlink |
| `mtime` | the last write time, in steps of 100 ns |
| `blake3`, `target` | as on Unix |
| `ino`, `dev`, `nlink` | the file index, the volume serial number and the link count |
| `ctime` | the NTFS change time |
| `mode`, `uid`, `gid`, `rdev`, extended attributes | zero or empty, so never a difference |

- **Names are bytes.** On Unix a name is the raw bytes of the file system. On
  Windows it is WTF-8: plain UTF-8 for a well-formed name, and a lossless
  three-byte form for an unpaired surrogate, which NTFS allows. A well-formed
  name has the same bytes in a Windows manifest as in a Unix one. Bytes that
  are not valid WTF-8 are an error, never repaired or replaced. Stored paths
  use `/` on both systems, and so do the patterns of `list`.
- **Links are never followed.** The walk does not enter a symlink, a junction
  or a mounted volume, and hashing opens a file without following a reparse
  point, so a link swapped in after the walk is an error and not another
  file's hash. A manifest that came with a tree is held to the same rule: a
  symlink or a junction in its place is refused.
- **A manifest belongs to one family.** It records whether Unix or Windows
  made it, in a `platform` key. `check`, `update` and `list` refuse a manifest
  from the other family. The format has no new schema version for this: a
  manifest without the key is a Unix manifest, which every older one is. The
  cost is that a `fileset` from before the key does not refuse a Windows
  manifest and reports false differences in `mode`, `uid` and `gid`, so use a
  current `fileset` on both sides. The message and the details are in the
  [reference](reference.md#windows).
- **The path may be long.** The tree's root is resolved to the extended form
  (`\\?\`), so the 260-character limit does not apply.
- **The volume's own folders are skipped.** At the root of the tree,
  `System Volume Information`, which cannot be read, and `$RECYCLE.BIN`,
  which holds every user's deleted files, are neither recorded nor entered,
  whatever their case. See [Built-in excludes](reference.md#built-in-excludes).
- **Some reparse points hold data.** A cloud placeholder (OneDrive Files
  On-Demand) or a deduplicated file on Windows Server is a reparse point.
  `fileset` records it as `other` and does not hash it, so a check compares
  little for it and never its content. Neither has been tested, OneDrive
  included.

## Comparison tiers

`check` compares a tree with the manifest. How much it compares depends on
the tier.

There are three tiers: relaxed (content only, plus type, symlink target and
device numbers), the default (adds permissions, modification time and size)
and paranoid (adds owner, group, link count and extended attributes). The
table of exactly which fields each tier compares is in the
[reference](reference.md#comparison-tiers).

Pick the tier by what the copy tool can keep. A plain `cp` or an `rclone`
copy to a backend without permissions fits `--relaxed`. An `rsync -a` copy
between machines that share user ids fits the default plus
`--require uid --require gid`. `--ignore` and `--require` adjust any tier one
field at a time.

The tier decides what counts as a difference. Content is compared in every
tier, so changed bytes always show.

## What fileset cannot catch

`fileset` reports differences in what it records. Some things it does not
record, or does not compare, so a copy can differ from the original and still
pass.

- **Holes in sparse files.** `fileset` hashes the bytes a program reads. A
  hole reads as zeros, so a sparse file and a fully written copy of it have the
  same checksum. A check cannot tell whether a copy kept the holes.
- **Hard links.** Only the link count is recorded, and it is compared only
  with `--require nlink` or `--paranoid`. `fileset` does not record which
  paths share a file, so a copy that turned hard links into separate files passes the default check.
- **ACLs on macOS.** Access control lists are not recorded.
- **On Windows, the read-only attribute and the security descriptor.** Neither
  is recorded, so a change to either is not reported. The same holds for the
  other attributes, the creation time and alternate data streams. A copy that
  loses them passes the check.
- **Names that differ only in case, on a file system that ignores case.**
  NTFS, and a case-insensitive macOS volume, find a name in any case. A
  manifest made where `a` and `A` are two names, checked where they are one,
  does not report a missing one of the two, and a rename that changes only
  the case shows as an added entry, with no deleted one.
- **Anything it did not look at.** Mount points are not crossed unless you
  pass `--cross-mount-points`. A tree with btrfs subvolumes is cut short by
  default. A warning names the directory. `check` reports what lies below as
  `unverified`, which counts as a difference. `update` drops it from the
  manifest.
- **Timestamp precision.** Modification times are compared to the nanosecond.
  A file system with coarser timestamps shows `mtime=` changes on every file.
  Use `--ignore mtime` or `--relaxed`.
- **A manifest that is wrong from the start.** `fileset` compares against what
  it recorded. If the tree was already damaged at `init`, the manifest records
  the damage.

The [reference](reference.md#known-limitations) lists two more: symlink
permission bits between Linux and macOS, and the sparse journal of the `fjall`
backend. The [reference](reference.md#windows) also says which Windows
volumes are not supported: ReFS, FAT, exFAT, network shares and the folders a
virtual machine shares with its host.

`fileset` reports and repairs nothing, and it never changes the tree.
