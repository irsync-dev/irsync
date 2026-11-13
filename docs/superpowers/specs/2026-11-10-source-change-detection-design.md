# Detecting source files that change during a sync: design

Date: 2026-11-10

## Why

The correctness work found a silent failure. If a program writes to a
source file while irsync's sender is still hashing it, the destination can
end up as a mix of old and new content that the source never held. The run
ends with exit 0 and no warning. Details, file:line references and a
reproduction are in `TODO.md` as of commit 5497ee9, "Torn file when the
source changes during hashing, 2026-11-10" (since moved to
`docs/IRSYNC_DECISIONS.md`, R45).

Correctness comes first for irsync. A sync that finishes quietly with a
wrong copy is the worst outcome it can have. The user's goal: **if irsync
detects that a file changed during the sync, the run ends with an error.**
rsync has the same weakness, so this goes beyond parity.

## Goals

1. Close the torn-file hole for every run, at almost no cost.
2. Catch any change to a sent file between hashing and the receiver placing
   it, for every run, at almost no cost.
3. Offer `--paranoid` for users who want more: a frozen copy (clone) of
   each sent file where the filesystem allows it, a second hash where it
   does not, and a final re-check of every listed file.
4. When a change is detected: try the file once more; if it changes again,
   give up on that file, carry on with the rest, and end the run with
   exit 23.

## Non-goals

- A consistent snapshot of the whole tree. Each file is handled on its own.
- Copying live databases or other programs that write through memory-mapped
  pages. Documented as a limit, not solved (see "Known limits").
- Immutable flags (`chattr +i`, `chflags uchg`). The user dropped them: on
  Linux they need root, and they change the file's change time.
- Backward compatibility. There are no users yet, so the protocol and
  session state may change freely.

## Anti-goals

- A run that ends with exit 0 while the destination holds content the
  source never held, in any case these checks can see.
- Making normal runs noticeably slower. The always-on checks must cost no
  more than one `fstat`/`stat` per hashed or sent file.
- `--paranoid` leaving clones behind, or making clones visible as files in
  the synced tree.
- Breaking or blocking the program that writes to the source.

## Behaviour

### Always on (no flag)

The sender keeps, for each file it hashes, the stat it took when it opened
the file for hashing (`hasher.rs`, `open_regular_nofollow_stat`): size,
modification time, change time and inode. Call this the *hash stat*.

1. **After hashing.** `fstat` the still-open file and compare it with the
   hash stat. If any of the four values differs, the digest may be torn.
   Treat the file as changed.
2. **Before the receiver places the file** (see "Ask before placing"
   below), stat the source file again and compare with the hash stat. If
   it differs, the file changed after hashing, perhaps in chunks the
   destination copied from its own old copy, which the sender never
   re-reads. Treat the file as changed.

The change time is the key value. Programs can reset the modification time,
but nothing can set the change time back. On Linux 6.13 and newer, ext4,
xfs, btrfs and tmpfs give a fine-grained change time to a file that has
been stat'ed since its last change, so a write right after the hash stat
still shows. (The development machine runs Linux 7.0.) On filesystems with
coarse timestamps a write in the same tick can still slip through. That is
a documented limit.

**Ask before placing** (the user's decision). Today the receiver verifies a
file, renames it into place and then reports `Done`; after that neither
side can retry it. Change the order: once the receiver has verified a file's
`.part`, it asks the sender whether the source is unchanged, and waits
before renaming it into place. The sender runs check 2 and answers. Only
on "unchanged" does the receiver place the file and report `Done`. A
changed file is never placed, so its retry is the ordinary one below. The
cost is one extra round trip per file; the files in flight (up to 4,096)
hide most of it. With `--inplace` and `--append` the receiver writes into
the destination file itself, so there is nothing to hold back; for those,
a "changed" answer still means retry, then exit 23, but the destination
already holds the written data. Say so in the docs.

### When a file is found changed

This is the same path as today's chunk mismatch (`InvalidData` →
`retry`, `MAX_RETRIES` = 1 in `sender_role.rs`), and it shares that one
retry. A file is tried at most twice in total, whichever check fires.

- **First time:** re-offer the file. Hash it again from scratch, plan and
  send.
- **Second time:** give up on that file. Record an error that names the
  file and says it changed during the sync. Continue with the other files.
  The run ends with exit 23. The destination keeps whatever it held before
  (its old copy, or nothing), as on any failed file.

### Metadata sent with a file (Claude's call)

Every send, first try or retry, carries the size and times the file's
digest was taken under (the hash stat, or for a clone the source's stat at
the moment of cloning), not the walk's. Content and metadata then always
belong together. This needs a field on the message that carries the chunk
list (for example `ChunkListPart`), because today the receiver applies the
walk's entry (`recv_io.rs` `finalize`). It replaces the old safety net in
which a retry kept the walk's mtime so that the next run noticed the file;
the new checks do that job now.

### `--paranoid`

A new client flag. The **sending side** runs the checks. On a push that is
the client. On a pull it is the daemon, so the flag travels in the session
request (`proto/irsync/v1/common.proto`, next to `checksum`).

1. **Clone before hashing, where possible.** For each file the sender will
   hash and send, first make a reflink clone on the same filesystem. Hash
   and send the clone, not the original.
   - Supported, with no root needed: btrfs; xfs with reflink; ZFS 2.2+ with
     block cloning (checked on this machine as a normal user, ZFS 2.4.1);
     bcachefs; APFS on macOS (`clonefile`). Not supported: ext4, tmpfs, NFS,
     FAT.
   - The destination gets the file as it was at the moment of cloning. If
     the source changes after the clone, the run still finishes cleanly:
     the copy is a state the source really held. (The user's decision.)
     Log at info level that the source changed after cloning. This is not
     an error and does not affect the exit code.
   - The metadata sent for the file is the source's stat taken at the
     moment of cloning (Claude's call; see "Metadata sent with a file"). A
     Linux clone is a new file with its own inode and times, so its own stat
     is not used.
   - **Where clones live** (the user's decision): in a hidden folder as far
     up the file's filesystem as possible. Start at the filesystem's mount
     point and take the highest directory on the path down to the file that
     irsync can write to. It must be on the same filesystem as the file
     (same device). The folder is excluded from the sync: the walk never
     lists it, and the delete pass never touches it. Name it so it is
     clearly irsync's and per user (Claude's call, e.g.
     `.irsync-clones-<uid>`). If the folder lies inside the synced tree, the
     walk skips it like any other irsync scratch file. If no writable
     directory exists on that filesystem, the file falls back to point 2.
   - A cloned file needs no second hash, and the "after hashing" and
     "before placing" checks do not apply to it: the clone cannot change.
   - Clones are deleted when the receiver reports the file placed or failed,
     or the sender gives up on it,
     and when the session ends. A clone left behind by a crash is removed by
     the next run, the same way `.part` files are cleaned up.
   - If cloning fails for a file (unsupported filesystem, the source folder
     is not writable, or a different filesystem below a mount point), fall
     back to point 2 for that file. That is not an error.
2. **Hash again before placement, where cloning was not possible.** When the
   receiver asks whether the file may be placed, hash the source file again
   and compare with the digest it was sent under. This is the only check that sees
   writers that do not update the file's times. A mismatch is a change:
   one more try, then exit 23.
3. **Re-check every listed file at the end.** Before the session finishes,
   stat every regular file the walk listed, including files nothing was
   sent for. Compare each with the stat of its last send, or with the
   walk's stat if it was not sent (size, mtime, ctime, inode).
   - **Changed:** send it again (from a clone where possible). This needs
     a new message by which the sender asks the receiver to plan a file
     again with a fresh entry, because the receiver never planned files it
     judged unchanged (the user's decision). The session stays open until
     those re-plans finish, and the resume state must be able to take a
     file back out of "done". If the file changes again without a clone,
     give up on it, and the run ends with exit 23. With a clone, the
     clone's copy is accepted as above.
   - **Gone, or no longer a regular file** (deleted, replaced by a folder
     or symlink): it cannot be sent again. The destination keeps the
     version it got, a state the source really held. Report the file, and
     end the run with **exit 24**, rsync's "some source files vanished",
     unless a worse code (23) applies (the user's decision). Add exit 24
     to the command-line reference.
   - **Unreadable:** treat it as a failed file, exit 23.

`--paranoid` costs, roughly: one clone and one delete per sent file on
cloning filesystems; one extra full read per sent file elsewhere; and one
stat per listed file at the end.

## Known limits (to document)

- **Memory-mapped writers** (databases and similar) can change a file
  through pages they had already dirtied, without updating its times until
  the system writes them out. The always-on checks cannot see this.
  `--paranoid` sees it through the second hash where it can't clone.
  Whether a clone includes such unwritten pages must be tested before the
  docs claim anything (see "Open questions").
- **Coarse timestamps** (older kernels, network filesystems, FAT): a write
  in the same tick as the hash stat is invisible to the always-on checks.
- **Files that change after the walk but are not sent** are not checked
  without `--paranoid`. The destination then holds the version from the
  walk's time, a state the source really held. The next run sends the new
  one.

## Tests

- **The regression test for the original bug.** Turn the reproduction in
  `TODO.md` into a deterministic test. Add a `test-hooks` hook inside the
  hash pass, in `SideHooks`, that runs a callback after N bytes have been
  read, and have it write to the start and the end of the file. Without the
  fix it must fail: exit 0 with a torn destination. With the fix it must
  pass: one retry; then either a correct copy (if the second hash sees a
  stable file) or exit 23 (if the hook writes again).
- **One test per check:** after hashing, at placement (a change only in
  chunks the destination copies locally), and each `--paranoid` check
  (clone; second hash; final re-check of an unchanged file). Each shows one
  retry, then exit 23 when the change repeats.
- **Clone tests** run where `target/` supports reflinks and skip, saying
  so, where it does not, as the existing clone tests in `inplace.rs` do.
  Also test: a clone is accepted even when the source changes after it;
  clones are removed after success, failure and interruption; the next run
  removes a leftover clone.
- **Pull direction:** `--paranoid` reaches the daemon and its checks run
  there.
- **A long-run check** (not in CI): a stress-campaign row, or a filegen
  workload, that writes to files while irsync syncs them. The builder
  decides how.

## Docs to update

- `book/src/explanation/correctness.md`, "Files that change during a
  sync": the torn case is now caught, and the result is retry, then
  exit 23. Describe `--paranoid` and the limits above.
- `book/src/explanation/transfer-algorithm.md`: the "Change during
  transfer" row and the Integrity bullet.
- `book/src/reference/command-line.md`: a `--paranoid` row with its cost.
- `TODO.md`: close the torn-file entry and point at this design.

## Left to the builder

- How cleanup finds clone folders after a crash (for example, the session
  state records each folder it created), and whether fileset should skip
  them. They exist only while a run is in progress.
- How macOS cloning is done (`clonefile`). Today's `recv.rs` `reflink` is
  Linux-only, and its macOS version returns `Unsupported`.
- The exact shape of the "ask before placing" and "plan again" messages.
- How the final re-check fits the session's resume state, so an
  interrupted `--paranoid` run resumes correctly.
- The wording of the error and info messages.

## Open questions to settle while building

- Does a reflink clone include writes that sit in memory-mapped pages not
  yet written to disk, on btrfs, xfs, ZFS and APFS? Test it. The docs say
  only what the test shows.
