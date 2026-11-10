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

- **A change, after hashing, in a chunk irsync sends.** Before a chunk leaves
  the sender, irsync reads it again and checks it against the hash. A chunk
  that changed, or that the file got too short to hold, fails this check, and
  the file is sent again once. If it keeps changing, the file fails and the
  run ends with exit 23.
- **Other changes after hashing.** A change only in chunks the destination
  takes from its own copy is not seen, and neither is data appended after the
  hash. The destination gets the file as it was when hashed, with the
  modification time irsync listed. The next run sees the newer time and usually
  sends the file again.
- **Deleted after it was listed.** If irsync still had to read it, the file
  fails, the run ends with exit 23, and the destination keeps its old copy.
  Otherwise it is not noticed in this run; the next run with `--delete`
  removes it.
- **Created after the listing.** It is not sent in this run.

**Known limit: a file written while irsync is still hashing it can arrive as
a mix of old and new content, with exit 0 and no warning.** The hash is taken
in one pass. If a program writes to the start of the file after the pass has
read it, and to the end before the pass gets there, the hash describes a mix
that never existed on disk. The destination can then build exactly that mix:
it reuses the old start from its own copy and receives the new end. Every
check passes, because every check compares against that hash.

The next run usually repairs it. The destination keeps the modification time
irsync listed, and the source's time has moved on since, so the next run
sees a difference and sends the file again. It does not when the last write
falls in the same timestamp tick as the listing, or when the writing program
resets the modification time. rsync has the same weakness.

What to do:

- Do not sync files that are being written. Stop the program, or sync from
  a snapshot.
- If you must, run the sync again once the writes have stopped, or run it
  with `-c`, which compares contents instead of size and time.
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
