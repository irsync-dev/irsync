# Verify a transfer with fileset

Use this when you want proof, from a tool that shares no code with `irsync`,
that a tree arrived intact. You record a manifest of the tree on the client,
let `irsync` carry the tree and the manifest to the daemon, and check the copy
against the manifest there. The same manifest later tells you whether either
copy has rotted on disk.

The examples use the setup from [Your first sync](../tutorial/first-sync.md):
the laptop is the client with `~/Photos`, and the server is the daemon, known
on the laptop as `home`, with the export `photos` at `/srv/photos`. Every
option is listed in the [fileset reference](../reference/fileset.md).

## Install fileset

Install `fileset` on both machines:

```sh
cargo install --git https://github.com/irsync-dev/irsync fileset
```

## 1. Record a manifest on the client

On the laptop, create the manifest:

```sh
fileset init ~/Photos
```

This hashes every regular file and writes `~/Photos/.fileset.sqlite`. At the
end it prints a summary on stderr:

```text
entries=1843 hashed=1790 reused=0 warnings=0
```

If you have recorded a manifest before, bring it up to date instead. Only
files whose size, modification time or inode changed are hashed again:

```sh
fileset update ~/Photos
```

## 2. Sync

Push the tree, manifest included:

```sh
irsync -av ~/Photos/ home:photos/
```

The manifest is an ordinary file in the tree, so `irsync` carries it like any
other. If you removed files on the laptop since the last sync, add
`--delete`, or the daemon's copy keeps them and the check reports them as
`ADD`.

Wait until the sync has finished. An interrupted sync leaves
`.irsync.<name>.part` files behind (see
[Files and state](../reference/files-and-state.md)), and the check reports
those too.

## 3. Check on the daemon's machine

On the server, check the copy against the manifest that arrived with it. Run
this as an account that can read every file, such as the account the daemon
runs as:

```sh
fileset check /srv/photos
```

A clean copy prints nothing on stdout, a summary on stderr, and exits with
code 0:

```text
added=0 deleted=0 changed=0 unchanged=1843 warnings=0
```

Otherwise each difference is one line on stdout, and the exit code is 1:

```text
CHG 2026/01/IMG_0042.jpg blake3=9f2c41e0b7a35d68c1f04e2b9a7d3c5e8f1b6a0d4c2e7f93b5a8d1c6e0f4b297→51d07a3e9c2b4f8d6e1a0c5b7f3d9e2a4c8b6f0d1e5a3c7b9f2d4e6a8c0b1d35
DEL 2026/01/IMG_0043.jpg
```

`ADD` is on disk but not in the manifest, `DEL` is in the manifest but not on
disk, and `CHG` shows each differing field as `stored→observed`.

The default comparison covers type, content, symlink targets, device numbers,
permissions, modification times and sizes. Owners are not compared by
default: `-a` keeps the owner only when the daemon runs as root, and maps user
and group names to the daemon's ids unless you pass `--numeric-ids`. When both
machines use the same ids, compare owners as well:

```sh
fileset check --require uid --require gid /srv/photos
```

If the laptop runs macOS and the server Linux, or the other way round, symlink
permissions differ between the two systems. Leave them out:

```sh
fileset check --ignore mode /srv/photos
```

## Check a disk for bitrot later

Months later, run `check` again on either machine, with no sync in between:

```sh
fileset check /srv/photos
```

A file whose bytes changed while its size and modification time stayed the
same shows up as `CHG <path> blake3=…→…`. `fileset` reports; it repairs
nothing. A normal sync does not repair such a file either, because it compares
size and modification time. Copy the good version from the other machine with
`-c`, which compares content. If the server's copy is damaged, push again:

```sh
irsync -avc ~/Photos/ home:photos/
```

If the laptop's copy is damaged, pull instead:

```sh
irsync -avc home:photos/ ~/Photos/
```

Check the other copy first, so that you do not spread the damage.

Run `check` before `update` on a tree you suspect. `update` keeps the stored
hash of a file whose size, modification time and inode are unchanged, so the
damage stays visible to the next `check`. `update --rehash` records the
current content as correct.

To check a read-only backup or a snapshot, copy its manifest somewhere
writable and point `--manifest` at the copy:

```sh
cp /mnt/snapshot/photos/.fileset.sqlite /tmp/photos.fileset.sqlite
fileset check --manifest /tmp/photos.fileset.sqlite /mnt/snapshot/photos
```
