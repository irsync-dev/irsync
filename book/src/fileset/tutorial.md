# Tutorial: check a copy made with rsync

You will record a manifest of a photo tree, copy the tree with `rsync -a`,
check the copy, and then see what a damaged copy looks like. It takes a few
minutes. You need `fileset` ([install](introduction.md#install)) and `rsync`.

The tree is `~/Photos`. The backup goes to `/mnt/backup/Photos`.

## 1. Record a manifest

```sh
fileset init ~/Photos
```

`fileset` hashes every regular file and writes `~/Photos/.fileset.sqlite`.
It prints a summary on stderr:

```text
entries=1843 hashed=1790 reused=0 warnings=0
```

The manifest lives inside the tree. That matters in the next step.

## 2. Copy the tree

```sh
rsync -a ~/Photos/ /mnt/backup/Photos/
```

The manifest is an ordinary file, so `rsync` copies it too. If you leave it
out with `--exclude`, the copy has no manifest. Then point `--manifest` at
the original when you check (see step 3).

## 3. Check the copy

```sh
fileset check /mnt/backup/Photos
```

A clean copy prints nothing on stdout, a summary on stderr, and exits with
code 0:

```text
added=0 deleted=0 changed=0 unchanged=1843 warnings=0
```

If the copy has no manifest, use the original's:

```sh
fileset check --manifest ~/Photos/.fileset.sqlite /mnt/backup/Photos
```

The manifest then shows up as `DEL .fileset.sqlite`, because the copy lacks
the file. That one line is expected.

## 4. See a difference

Damage the backup on purpose. This changes one file's content and keeps its
modification time, as bitrot would. It also deletes a file and adds one:

```sh
echo X > /mnt/backup/Photos/IMG_0003.jpg
touch -r ~/Photos/IMG_0003.jpg /mnt/backup/Photos/IMG_0003.jpg
rm /mnt/backup/Photos/2026/01/IMG_0002.jpg
echo n > /mnt/backup/Photos/new.jpg
fileset check /mnt/backup/Photos
```

Each difference is one line on stdout, and the exit code is 1:

```text
CHG IMG_0003.jpg blake3=d1cd1ec45291d06cdde016568971990c7e4da895f2e5a8a705d4feeb79578a69→13486a0503b83f75cfbc58d0d378ecb29c352c16374f97a6810845f956493b24
ADD new.jpg kind=file
DEL 2026/01/IMG_0002.jpg
```

The hashes above come from a small test tree. Yours will differ.

`ADD` is on disk but not in the manifest. `DEL` is in the manifest but not on
disk. `CHG` shows each differing field as `stored→observed`. `fileset`
reports and repairs nothing. To repair, copy the good files again.

Next: [verify an rsync or rclone transfer](verify-other-tools.md) in daily
use, or [check for bitrot](bitrot.md).
