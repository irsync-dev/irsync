# Clean up state

An interrupted transfer leaves state behind on purpose, so that the next run
can resume: partial files beside their destinations (or under
`--partial-dir`), and a dormant session in the index under `state_dir`.
Garbage collection removes what nobody is coming back for. It also forgets the
index entries of files that are gone or were replaced.

Most of the time you need not do anything: the client and the daemon collect
on their own. Run `irsync gc` when you want the space back now, or after you
stopped syncing a directory for good.

## What runs on its own

- **The client**, at the start of each sync that is not a dry run, collects
  under that sync's local root. It walks the root at most once a day, or
  sooner once a partial-file record has gone an hour without being touched.
  It removes partial files and sessions older than 7 days.
- **The daemon** checks each export once an hour, starting a minute after the
  export's index is opened. It walks an export at most once a day, unless
  partial-file records have become collectable since the last walk. It removes
  partial files and sessions older than `dormant_session_timeout`, 7 days by
  default (see [Configuration](../reference/configuration.md#daemon)).

A partial file is kept as long as a stored session still plans to write it.

## Collect by hand

1. See what would go:

   ```sh
   irsync gc -n
   ```

   It lists every partial file it would remove, then a summary line:
   `N part file(s), N session(s), N index entries would be removed`.

2. Remove it:

   ```sh
   irsync gc
   ```

`irsync gc` visits every root that has an index under `state_dir`: each local
directory this account has synced or indexed, and the exports of a daemon that
runs in this account with the same config. For each root it removes partial
files and dormant sessions older than 7 days, and the index entries of regular
files that are gone or were replaced by another file. Use `--config PATH` when
the `state_dir` you mean is set in another config file.

An entry under a directory that could not be read is kept, since the file may
only be unreadable. A walk that finds no file at all removes no entries, since
the root may be a mount point whose file system is not mounted.

## See also

- [Seed the index](seed-the-index.md)
- [Command line](../reference/command-line.md#irsync-gc): the flags of
  `irsync gc`.
