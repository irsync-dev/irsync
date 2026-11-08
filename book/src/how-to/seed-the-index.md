# Seed the index

Use this when a destination already holds a copy of the data that `irsync`
did not make: an earlier rsync, a restore from backup, a disk you copied by
hand. Seeding hashes those files into the destination's index first, so that
the first session reuses their content for renamed and copied files instead
of fetching it over the network.

`irsync` keeps one index per root under `state_dir` (see
[Configuration](../reference/configuration.md#identity-and-state)). The index
only knows files that `irsync` has hashed: files it placed, files it updated,
and files read by a `-c` run. Without seeding, a session still finds some
reuse on its own: before it fetches a whole file of 64 KiB or more, it hashes
up to four destination files of exactly that size that the index does not
know yet. Seeding covers the rest.

Seeding never makes a wrong decision. Content taken from a local file is
verified like any other, and an index row that has gone stale falls back to a
fetch.

## On the client, before a pull

Index the directory that the pull writes into. For
`irsync -a myserver:photos/ ~/Photos/` that is `~/Photos`; for
`irsync -a myserver:photos ~/` it is `~/photos`.

1. See how much would be read:

   ```sh
   irsync index -n ~/Photos
   ```

   It prints one line per root: `ROOT: N file(s), M to hash (B bytes)`.

2. Hash the files:

   ```sh
   irsync index ~/Photos
   ```

   Files whose index row is still fresh (same size, modification time, change
   time, device and inode) are skipped, so running it again costs only the walk.

You can name several roots in one command. If a file cannot be read or a
directory cannot be listed, `irsync index` reports it, carries on, and exits
with code 23.

## On the daemon, before a push

Run these on the daemon's machine, as the account the daemon runs as and with
the same `--config`, so that they write to the daemon's `state_dir`.

- Hash one export, or every export:

  ```sh
  irsync export update photos
  irsync export update all
  ```

  `-n` counts what would be hashed, as for `irsync index`. The daemon uses
  the new rows from its next session on.

- Or let the daemon hash an export every time it starts, which suits an export
  whose files change outside `irsync`:

  ```sh
  irsync config set exports.photos.warm_index true
  ```

  For a new export, `irsync export add photos:/srv/photos --warm-index` does
  the same. The daemon hashes the export in the background when it starts, or
  at the first connection for an export added while it runs, and logs
  `index warmed` when done.

## See also

- [Clean up state](clean-up.md): how index entries of deleted files are
  removed.
- [Command line](../reference/command-line.md#irsync-index): the flags of
  `irsync index`.
