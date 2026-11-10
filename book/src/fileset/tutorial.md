# Tutorial: check a copy made with rsync

You will make a small tree of three files, record a manifest of it, copy the
tree with `rsync -a`, check the copy, and then see what a damaged copy looks
like. It takes a few minutes. You need `fileset`
([install](introduction.md#install)) and `rsync`.

Everything happens in two throwaway directories, `~/fileset-demo` and
`~/fileset-demo-copy`. Nothing here touches your real files or backups.

## 1. Make a tree and record a manifest

```sh
mkdir -p ~/fileset-demo/2026/01
printf 'first photo\n'  > ~/fileset-demo/2026/01/IMG_0001.jpg
printf 'second photo\n' > ~/fileset-demo/2026/01/IMG_0002.jpg
printf 'third photo\n'  > ~/fileset-demo/IMG_0003.jpg
find ~/fileset-demo -exec touch -d '2026-01-15 12:00' {} +
fileset init ~/fileset-demo
```

The `touch` gives every item an old modification time, as a real tree would
have. (Without it, `rsync` can set the times of the new directories a moment
differently and the check in step 3 shows noise.) `fileset init` hashes every
regular file and writes `~/fileset-demo/.fileset.sqlite`. It prints a summary
on stderr:

```text
entries=7 hashed=3 reused=0 warnings=0
```

The seven entries are the root, two directories, three files and the
manifest. The manifest lives inside the tree. That matters in the next step.

## 2. Copy the tree

```sh
rsync -a ~/fileset-demo/ ~/fileset-demo-copy/
```

The manifest is an ordinary file, so `rsync` copies it too. If you leave it
out with `--exclude .fileset.sqlite`, the copy has no manifest. Then point
`--manifest` at the original when you check (see step 3).

## 3. Check the copy

```sh
fileset check ~/fileset-demo-copy
```

A clean copy prints nothing on stdout, a summary on stderr, and exits with
code 0:

```text
added=0 deleted=0 changed=0 unchanged=7 warnings=0
```

If the copy has no manifest, use the original's:

```sh
fileset check --manifest ~/fileset-demo/.fileset.sqlite ~/fileset-demo-copy
```

The manifest then shows up as `DEL .fileset.sqlite`, because the copy lacks
the file. That one line is expected:

```text
added=0 deleted=1 changed=0 unchanged=6 warnings=0
DEL .fileset.sqlite
```

## 4. See a difference

Damage the copy on purpose. This flips one byte of a file, so the size stays
the same, and puts the old modification time back, as bitrot would. It also
deletes a file and adds one:

```sh
printf 'X' | dd of="$HOME/fileset-demo-copy/IMG_0003.jpg" bs=1 seek=2 conv=notrunc
touch -r ~/fileset-demo/IMG_0003.jpg ~/fileset-demo-copy/IMG_0003.jpg
rm ~/fileset-demo-copy/2026/01/IMG_0002.jpg
echo n > ~/fileset-demo-copy/new.jpg
fileset check ~/fileset-demo-copy
```

Each difference is one line on stdout, and the exit code is 1:

```text
CHG 2026/01 mtime=2026-01-15T12:00:00.000000000Z→2026-11-10T21:10:56.879697180Z
CHG IMG_0003.jpg blake3=d7e964217db4d891f80f8add25b9a5b1f82d11d135a7188db527f3af6cdaa802→266b8cdd39a8f500d03472f37ce533e3a4efbc9be56a4435ed771f892324bc80
ADD new.jpg kind=file
DEL 2026/01/IMG_0002.jpg
```

The hashes are the real ones for these file contents, so you should see the
same. The time on the first line is when you ran the commands.

`CHG IMG_0003.jpg` shows `blake3` alone: the content changed and nothing else
did. That is how bitrot looks. `CHG 2026/01` is a side effect of the `rm`:
deleting a file changes its directory's modification time. `ADD` is on disk
but not in the manifest. `DEL` is in the manifest but not on disk. `CHG`
shows each differing field as `stored→observed`. `fileset` reports and
repairs nothing. To repair, copy the good files again.

When you are done, remove the two directories: `rm -r ~/fileset-demo
~/fileset-demo-copy`.

Next: [verify an rsync or rclone transfer](verify-other-tools.md) in daily
use, or [check for bitrot](bitrot.md).
