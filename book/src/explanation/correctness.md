# How we know irsync is correct

For a sync tool, correctness comes first and speed second. This page states
what irsync claims, what backs each part of the claim, how you can check it
yourself, and where the claim stops.

## The claim

> A completed `-a --delete` sync of a source that is not changing leaves the
> destination exactly matching the source.

Each condition matters:

- **Completed** means the run ended with exit code 0. A run that was
  interrupted, or that ended with exit 23 (some files failed), is not
  complete. See [Outside the claim](#outside-the-claim).
- **`-a`** keeps permissions, times, symlinks, devices and special files.
  **`--delete`** removes destination files that are gone from the source.
- **Not changing** means nothing writes to the source while the sync runs.
  [Files that change during a sync](#files-that-change-during-a-sync) says
  what happens otherwise.

"Exactly" covers the items below. The last column says what checks each one.
[fileset](../fileset/introduction.md) is a separate checking tool, described
in the next section.

| Item | Kept by | Checked by |
| --- | --- | --- |
| Names, and which entries exist | any sync; `--delete` removes extras | fileset's default check (`ADD`, `DEL`) |
| Type (file, directory, symlink, device, FIFO) | `-a` | fileset's default check |
| File contents | any sync | fileset's default check; irsync also checks every chunk and the whole file with BLAKE3 |
| Permissions and modification times | `-a` | fileset's default check |
| Symlink targets | `-a` | fileset's default check |
| Device numbers | `-a` | fileset's default check |
| Hard links | `-H`, which `-a` does not include (as in rsync) | fileset with `--require nlink` or `--paranoid` only |
| Holes in sparse files | files sparse in the source are written sparse (see `-S` in the [command line](../reference/command-line.md)) | the engine test `sparse_file_stays_sparse` and the `sparse` campaign row (which runs with `-S`) only; fileset does not record holes |
| Deletions | `--delete` | fileset's default check (`ADD` for anything left behind) |

So for hard links, the full command is `-aH --delete`. That is what the test
loops run.

Some things irsync does not carry at all, such as extended attributes and
ACLs. Owners are kept only when the receiving side runs as root (see `-o` in
the [command line](../reference/command-line.md)), and sockets are never
copied. These edges are listed in [Known limitations](../reference/limitations.md).

## Check it yourself with fileset

You do not have to take the claim on trust. fileset is a second line of
defence: it records a manifest of a tree (each file's checksum and metadata),
and later checks a copy against it. It is a separate tool that shares none
of irsync's code, so a mistake in how irsync syncs does not repeat in the
check. It is also the same
check the test loops below run after each sync.

Record a manifest on the client, sync, then check the copy on the server:

```sh
fileset init ~/Photos
irsync -a --delete ~/Photos/ myserver:photos/
fileset check /srv/photos          # run this on myserver
```

The default check compares type, content, symlink target, device numbers,
permissions, modification time and size. Widen it when you need to:

- `--require nlink` adds the link count, so a hard link that arrived as a
  separate copy shows up. `--paranoid` adds it too, along with owner, group
  and extended attributes.
- fileset does not record holes in sparse files. A sparse file and a fully
  written copy of it pass every tier.

[Verify an irsync transfer](../fileset/verify-irsync.md) walks through the
same check step by step (with `-av` and without `--delete`). The [fileset reference](../fileset/reference.md#comparison-tiers)
lists every field each tier compares.

## How we check it, layer by layer

The tests run from fast and narrow to slow and broad.

**Engine tests.** The tests in `crates/irsync/tests/` run two irsync engines
in one process and sync real trees between them. A test build adds hooks
that record the order of internal steps, so a test can pin, for example,
that a file changed while it was sent is sent again, or that a stale local
chunk is fetched over the network instead. See
[Testing](../contributing/testing.md).

**The stress loop.** `irsync-stress` runs a loop on one machine: a
generator of random file changes (filegen, a tool for contributors) changes
the source, fileset records it, irsync syncs with `-aH --delete`, and fileset
checks the destination. It repeats for many iterations, so each sync starts
from the last one's result. See [Stress testing](../contributing/stress.md).

**The same loop over a bad network.** `irsync-netstress` runs that loop with
every packet passing through a simulated link: limited speed, delay, bursts
of loss, and dead zones that cut the link and force a reconnect and resume.
See [Network stress with netstress](../contributing/netstress.md).

**Writing to the source during the sync.** Both stress tools have a
`--concurrent` mode in which the generator keeps changing the source while
irsync syncs it. Each destination file is then judged against every state
the source held, content and metadata, and a second sync with nothing
writing must leave an exact copy. The aim: no destination file is ever a
state the source never held. A self-test with the change checks switched off
proves the judge catches torn files. See
[Writing during the sync](../contributing/bug-hunting.md#writing-during-the-sync---concurrent).

**Campaigns.** Long runs over many random seeds, each aimed at one area:
hard links, renames, sparse files, metadata, timestamps, symlinks,
deletion, long names, and more. After a run the destination is also
checked with `--paranoid`, and with `du -s` for sparse files. See
[the campaign matrix](../contributing/bug-hunting.md#the-campaign-matrix).

## The evidence

The loops find real bugs, not just pass. Among them: file names near the
length limit that could not be transferred, and metadata applied to a hard
link reaching names the source had unlinked. Each is fixed and has a
regression test. The list is in
[Bugs the harness has found](../contributing/stress.md#bugs-the-harness-has-found).

## Outside the claim

The claim does not cover these cases.

- **A sync without `-a`** does not keep permissions and times.
- **A sync without `--delete`** leaves files that are gone from the source in
  place.
- **An interrupted sync, or one that ends with exit 23,** is incomplete. It
  may leave `.irsync.<name>.part` files behind. Running the same command
  again resumes it.
- **A power cut soon after a sync.** By default irsync does not flush files to
  disk before it renames them into place (`--fsync` is off, as in rsync).
  After a power cut or a kernel crash in the seconds after a run, a file can
  keep its name, size and time but lose its content. The next run's quick
  check does not notice; `-c` does. See `--fsync` in the
  [command line](../reference/command-line.md).
- **Files that change during the sync.** See the next section.

### Files that change during a sync

irsync first lists the source, then reads each file that needs sending to
compute its hash (a checksum of each chunk and of the whole file), then sends
the chunks. Here is what happens when a file changes along the way.

- **A change while irsync hashes the file.** When the pass is done, the
  sender checks the open file again: its size, modification time, change
  time and inode must be what they were when the pass began. If not, the hash
  may describe a mix of old and new content, so the file is hashed again.
  The error text is "changed while it was hashed".
- **A change after hashing.** Before the receiver puts a verified file in
  place, it asks the sender. The sender looks at the source again and
  compares it with the stat its hash was taken under. A difference gives
  "changed after it was hashed". This also catches a change only in chunks
  the destination takes from its own copy, which the sender never reads
  again.
- **A change in a chunk irsync sends.** Before a chunk leaves the sender,
  irsync reads it again and checks it against the hash.
- **One retry, then exit 23.** Every check above shares one retry per file.
  If the file changes again, it is given up with an error that ends in
  "(after retry)", the other files carry on, and the run exits 23. The
  destination keeps its old copy. With `--inplace` and `--append` there is
  no old copy to keep: they write into the destination file itself, so it
  already holds the written data when the change is found. The run still
  exits 23.
- **Deleted after it was listed.** If irsync still had to read it, the file
  fails, the run ends with exit 23, and the destination keeps its old copy.
  Otherwise it is not noticed in this run; the next run with `--delete`
  removes it.
- **Created after the listing.** It is not sent in this run.

- **A hard-link name that changes, with `-H`.** irsync groups the names of
  a file when it lists the source, and sends the file once, under its first
  name. Just before it makes the other names links to the copy, the
  receiver asks the sender to look at each name again. A name that is no
  longer exactly the file that was hashed is not linked. A name replaced by
  a file of its own is sent as that file, in the same run, with its retry
  already used. A name that is gone fails, and the run exits 23. A name
  whose first name was not sent, because the destination was up to date, is
  linked as listed: both names held that content when irsync listed them.

These checks compare size, modification time, change time, inode,
permissions, owner and group. Programs can reset the modification time, but
nothing can set the change time back.

A sent file is placed with the size, modification time, permissions, owner
and group it had when its hash was taken, not the ones the listing saw. So a
`chmod`, or another file saved over it, between the listing and the hash
cannot leave new content with old permissions: content and metadata always
come from the same moment.
Because they compare the change time, a change to a file's metadata alone
during the sync counts as a change, even though its content is the same:
`chmod` or `chown`, extended attributes set by an indexer or a virus
scanner, or a new hard link made by a snapshot tool. The file is retried,
and the run exits 23 if it happens again.

#### With `--paranoid`

`--paranoid` closes the gaps the checks above leave.

- **Clones.** Before hashing a file, irsync makes a reflink clone of it: a
  copy that shares the file's disk blocks and never changes afterwards. It
  hashes and sends the clone. The destination then gets a state the source
  really held, whatever the source does meanwhile. A clone is accepted even
  if the source changes after it was made; irsync logs a note. Cloning works
  on btrfs, xfs, ZFS 2.2 and newer with block cloning, and bcachefs on Linux,
  and on APFS on macOS (not yet tested). Clones live in
  `.irsync-clones-<uid>/<session>/`, mode 0700, in the highest directory on
  the file's filesystem that you can write to. Directories that group or
  others can write to, without the sticky bit, are skipped. irsync removes
  the clones when the run ends. Clones a crash left are removed by a later
  `--paranoid` run that clones on that filesystem. The scan skips these
  folders. Each file in flight keeps its clone open; if irsync runs out of
  file descriptors, that file is hashed a second time instead of cloned.
- **Without clones.** Where the filesystem cannot clone, irsync hashes the
  file a second time before the receiver places it and compares the whole
  digests. A difference gives "changed after it was sent".
- **A final re-check.** At the end of the run, irsync looks at every file it
  listed once more. A file that changed is sent again, from a clone where
  possible. A file that is gone, or no longer a regular file, does not fail
  the run, but it ends with exit 24, "some source files vanished". A file
  irsync cannot read gives exit 23. When several codes apply, 1 wins over
  25, 25 over 23, and 23 over 24.
- **On a pull**, `--paranoid` travels to the daemon, which does the sending
  and so runs all of this.

Memory-mapped writers: a program can write into a file through a memory
mapping, and the file's times do not change until the system writes the pages
out. In a test on ZFS 2.4 (`clone_includes_unflushed_mmap_writes`), a clone
made right after such a write held the new bytes. So on that filesystem a
clone includes memory-mapped writes that have not reached the disk. Other
filesystems have not been tested. Where irsync cannot clone, the second hash
reads the file as the system holds it. Whether that includes them has not
been tested.

**Costs.** On a filesystem that clones, `--paranoid` makes one clone and
deletes one per sent file. Elsewhere it reads each sent file one more time.
At the end it makes one `stat` per listed file. It also creates and removes
the clone folder, which changes the modification time of the directory that
holds it, on each run. Under `--paranoid`, the final re-check treats a
changed mode or modification time as a change, because the stat differs. The
file is then sent again in full as a delta, which reads both sides, even
though its content is unchanged.

#### Limits

- **Memory-mapped writers, without `--paranoid`.** The checks cannot see a
  write through a mapping before its times change. `--paranoid` may help;
  only clones on ZFS 2.4 are shown to include such writes (see the
  memory-mapped note above).
- **Coarse timestamps.** Fine-grained change times exist on ext4, xfs, btrfs
  and tmpfs on Linux 6.13 and newer. Elsewhere (older kernels, other local
  filesystems such as ZFS, network filesystems, FAT) assume coarse
  timestamps: a write in the same tick as the first stat can slip through the
  stat checks.
- **Files that change after the listing but are not sent.** Without
  `--paranoid`, irsync does not look at them again. The destination keeps
  the version from the time of the listing, a state the source really held.
  The next run sends the new one.
- **Resumed `--paranoid` runs.** A resumed run re-checks only the files it
  sent itself. If the connection drops after the re-check and before the
  session ends, the list of vanished files can be lost, and the resumed run
  can exit 0 where the first would have exited 24.
- **A re-sent file has no retry left.** A file sent again by the final
  re-check starts with its retry used, so a transient mismatch fails it at
  once. Its hard-link names at the destination are linked to the new copy.
- **Hard-link names, with `-H`.** The final re-check stats only the main
  name of each group of hard-linked files. The link check that follows it
  looks at the other names again, but only those of files sent in this run.
  If such a name is deleted at the source, it fails rather than counting as
  vanished: the run exits 23 instead of 24.
- **Bind mounts** share one device number, and cloning across them fails
  with `EXDEV`, so that filesystem falls back to the second hash.
- **A umask of 002** makes new directories group-writable, which may leave
  no safe place for the clone folder. irsync then uses the second hash.

What to do:

- Where the source is written while you sync, `--paranoid` helps, within
  the tested limits above.
- Better still, sync from a snapshot.
- Run the sync again once the writes have stopped, or run it with `-c`,
  which compares contents instead of size and time.
- Check the result with fileset.

### What the tests cannot see

Every bug the test loops find has to pass three filters:

1. **The generator must provoke it.** A change the generator never makes is
   never tested.
2. **fileset must observe it.** The loops run fileset's default check, which
   does not compare hard links, owners or extended attributes, and does not
   record holes. Campaigns widen this after a run.
3. **A known bug must not hide it.** A bug that fails every iteration hides
   anything else underneath it.

[Hunting bugs](../contributing/bug-hunting.md#what-the-loop-can-and-cannot-see)
has the detail.

## For contributors

[Testing](../contributing/testing.md) says which test to add or run for each
kind of change.
