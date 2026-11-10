# Verify an rsync or rclone transfer

Use this to prove that a copy made by `rsync`, `rclone`, `cp` or another tool
matches the original. The examples copy `~/Photos` to `/mnt/backup/Photos`.
For a first run, see the [tutorial](tutorial.md). Every option is in the
[reference](reference.md).

## The steps

Record the manifest before you copy:

```sh
fileset init ~/Photos
```

Copy, and wait for the copy tool to finish:

```sh
rsync -a ~/Photos/ /mnt/backup/Photos/
```

Check the copy, on the machine that holds it:

```sh
fileset check /mnt/backup/Photos
```

Exit code 0 means the copy matches. Exit code 1 means it differs, and each
difference is a line on stdout. Exit code 2 means the check itself failed.

If you changed the tree since you recorded the manifest, run
`fileset update ~/Photos` first, then copy again. See
[Keep a manifest current](keep-current.md).

## The manifest is part of the tree

`init` writes `.fileset.sqlite` into the tree, so the copy tool copies it
along. If you filter the copy, for example with `rsync --exclude`, the manifest
may stay behind. Then `check` stops with `no manifest found`. Point
`--manifest` at the original's manifest:

```sh
fileset check --manifest ~/Photos/.fileset.sqlite /mnt/backup/Photos
```

The copy lacks the manifest file, so the output includes `DEL .fileset.sqlite`.
Ignore that one line. If the original is on another machine, copy the
manifest file over first.

To keep the manifest out of the tree altogether, record it elsewhere with
`fileset init --manifest <MPATH> ~/Photos` and pass the same `--manifest` to
every `check`.

## Tools that do not keep all metadata

The default check compares content, type, symlink targets, device numbers,
permissions, modification times and sizes. A tool that does not keep
permissions or times makes every file look changed. That is not damage. It
is a copy that carries less metadata.

`cp -r` without `-p` keeps neither permissions nor times. The check shows a
`CHG` line for every file and directory:

```text
CHG 2026 mtime=2026-11-10T19:44:21.606059787Z→2026-11-10T19:44:39.596886187Z
CHG 2026/01 mtime=2026-11-10T19:44:21.607204393Z→2026-11-10T19:44:39.596918066Z
CHG 2026/01/IMG_0001.jpg mtime=2026-11-10T19:44:21.607204393Z→2026-11-10T19:44:39.596918066Z
CHG 2026/01/IMG_0002.jpg mtime=2026-11-10T19:44:21.607204393Z→2026-11-10T19:44:39.596944775Z
CHG IMG_0003.jpg mtime=2026-11-10T19:44:21.607204393Z→2026-11-10T19:44:39.596966804Z
```

Here only `mtime` differs, because the umask on that machine left the
permissions alone. On another machine `mode=` shows up too. `rclone` keeps
permissions on few backends, and keeps modification times to a precision that
depends on the backend, so expect the same kind of lines after an rclone copy.

Compare only what the tool can keep. Drop one field at a time:

```sh
fileset check --ignore mtime /mnt/backup/Photos
fileset check --ignore mode --ignore mtime /mnt/backup/Photos
```

Or use the relaxed tier, which compares type, content, symlink targets and
device numbers only:

```sh
fileset check --relaxed /mnt/backup/Photos
```

```text
added=0 deleted=0 changed=0 unchanged=7 warnings=0
```

Content is still compared in every tier, so a changed byte still shows. Add
back what you can trust, such as `--relaxed --require size`. The tiers are
explained in [How fileset works](how-it-works.md#comparison-tiers).

## When the copy is not on a local disk

`fileset` reads a local path. It cannot check an `rclone` copy that lives in
S3 or another object store.

- Mount the remote and check the mount point: `rclone mount myremote:photos
  /mnt/remote`, then `fileset check --relaxed --manifest
  ~/Photos/.fileset.sqlite /mnt/remote`. Every file is read over the network,
  so this is slow on a big tree.
- Or run `fileset check` on a machine that holds the copy as ordinary files.

Object stores do not keep permissions, owners or symlinks as a file system
does, so use `--relaxed` there.

## Other things that make a check differ

- Run the check after the copy tool has finished. An interrupted `rsync`
  leaves temporary files behind, and `check` reports them as `ADD`.
- Copy deletions too. Without `rsync --delete`, files you removed from the
  original stay in the copy and show as `ADD`.
- Between macOS and Linux, symlink permissions differ. Use `--ignore mode`.
