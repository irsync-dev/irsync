# Check data for bitrot

Bitrot is data that changes on disk without anyone writing it: a flipped
bit, a bad sector, a faulty controller. The file's size and modification time
stay the same, so most tools do not notice. `fileset` reads every byte again
and compares it with the checksum it recorded.

The examples use `~/Photos`. Every option is in the [reference](reference.md).

## Record, then check later

Record a manifest while you know the data is good:

```sh
fileset init ~/Photos
```

Months later, run `check`. No copy is involved:

```sh
fileset check ~/Photos
```

A file whose bytes changed while its size and modification time stayed the
same shows up as a `CHG` line with `blake3` alone:

```text
CHG 2026/01/IMG_0001.jpg blake3=81c4b7f7e0549f1514e9cae97cf40cf133920418d3dc71bedbf60ec9bd6148cb→7d271ea55b6b5af8d6a863e4530228f65ffc4a33086a485d73e84ee84243463f
```

`fileset` reports; it repairs nothing. Restore the file from another copy,
and check that copy first so that you do not spread the damage.

## Rot or an edit?

`check` reports every difference, including edits you made on purpose. Tell
them apart by the fields:

```text
CHG 2026/01/IMG_0001.jpg blake3=81c4…→7d27…
CHG IMG_0003.jpg blake3=d1cd…→67f6… mtime=2026-11-10T19:44:21.607204393Z→2026-11-10T19:44:45.211616300Z size=2→15
```

The first file changed content and nothing else. That is the sign of rot, or
of a tool that wrote the file and set the old time back. The second changed
content, time and size together. That is an edit. (The hashes here are
shortened.)

Always run `check` before [`update`](keep-current.md). An `update` rehashes
files you edited and keeps the stored hash of files whose size, time and inode
did not change. Read the `check` output first, so that an edit does not hide
a rotted file next to it. `update --rehash` records the current content as
correct, damage included.

## Check on a schedule

`check` exits 0 when clean, 1 when it found differences and 2 when the run
itself failed. A cron job can use that. This line checks at 03:00 on the first of each
month. It writes everything to a log, and prints a message, which cron mails
to its owner, only when the check did not exit 0:

```text
0 3 1 * * fileset check -q ~/Photos > ~/fileset-check.log 2>&1 || echo "fileset check exited with $?; see ~/fileset-check.log"
```

`-q` hides progress. The summary line still goes to stderr, which is why the
log takes both streams. Add `--fail-on-warnings` when an unreadable file
should count as a failure too. Warnings alone give exit 0 otherwise.

Exit 1 and exit 2 are different problems. Exit 1 means your data differs from
the manifest. Exit 2 means `fileset` could not run, for example because the
disk is not mounted or the manifest is missing.

## Read-only media and snapshots

`check` never changes the tree. But a manifest inside a read-only tree may
fail to open, with exit 2 and a message suggesting `--manifest`. Copy the
manifest somewhere writable and point `--manifest` at the copy:

```sh
cp /mnt/snapshot/photos/.fileset.sqlite /tmp/photos.fileset.sqlite
fileset check --manifest /tmp/photos.fileset.sqlite /mnt/snapshot/photos
```
