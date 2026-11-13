# Remote addresses

A remote operand is `HOST:PATH`.

- `HOST` is an alias from a `[hosts.ALIAS]` table (see
  [Configuration](configuration.md#hosts)) or an `EndpointId`, in hex or
  z-base-32 as iroh prints it. Aliases match exactly; there are no patterns.
  The client pins the `EndpointId` it dials: a daemon with another key fails
  the connection.
- `PATH` is sent to the daemon as typed and resolved on the daemon's machine.
  The client does not interpret it, apart from the trailing slash.

An operand is remote when it has a `:` before its first `/`, as in rsync.
`HOST` may not be empty.

## Resolution order

The daemon reads `PATH` in this order:

1. **An export name**, then a path inside it. When the first component of a
   relative `PATH` names an export, the rest is resolved under that export's
   directory: `myserver:photos/2024` is `2024` inside the `photos` export.
2. **A path relative to the home directory** of the account the daemon runs
   as. `myserver:Pictures` is `~/Pictures` on the daemon's machine;
   `myserver:` and `myserver:~` are the home directory itself, and
   `myserver:~/Pictures` is the same as `myserver:Pictures`.
3. **An absolute path**, as is: `myserver:/backup/myclient`.

`.` and empty components are dropped. A `..` component or a NUL byte is
refused.

## Exports decide access

Whatever the spelling, the resolved path must lie inside an exported directory
that the client has a grant for, and that export's grants apply. A path
outside every export the client may use is refused as "no access", whether or
not it exists, so the answer reveals nothing about the daemon's file system.

Exports may be nested. A grant reaches everything below its export's
directory, exports nested inside it included. When the path lies inside
several exports the client may use:

- the export with the longest path decides the session's root and access;
- between exports with the same path, the one the operand named wins, then the
  one with the better access (read-write over read-only, `read_only` applied).

Exports the client has no grant for take no part. A grant on an outer export
is therefore not stopped by an inner export, not even by a `read_only` inner
one, and the answer for a path never depends on where ungranted exports sit.

To open everything the daemon's account can reach, export the root, and grant
it:

```sh
irsync export add root:/
```

`irsync export add home:~` exports the home directory.

## Trailing slashes

rsync's trailing-slash rule holds on both sides. A source that ends in `/`
copies the directory's contents; one without copies the directory itself.

| Command | Result on the daemon |
| --- | --- |
| `irsync -a ~/Photos/ myserver:photos/` | The contents of `~/Photos` in the `photos` export. |
| `irsync -a ~/Photos myserver:photos/` | `~/Photos` as `Photos` inside the `photos` export. |
| `irsync -a myserver:photos/ ~/copy/` | The contents of the export in `~/copy`. |
| `irsync -a myserver:photos ~/copy/` | The export as `~/copy/photos`. |

## Copying one file

A source may be a single file, in either direction. It lands where rsync
would put it. `T` is the destination as typed, and `L` the local destination
on a pull.

| Command | `T` is a directory | `T` does not exist | `T` is a file |
| --- | --- | --- | --- |
| `irsync f.iso myserver:T` | `T/f.iso` | a file named `T` | `T` is updated |
| `irsync f.iso myserver:T/` | `T/f.iso` | directory `T` is created, then `T/f.iso` | error |
| `irsync myserver:dir/f.iso L` | `L/f.iso` | a file named `L` | `L` is updated |
| `irsync myserver:dir/f.iso L/` | `L/f.iso` | directory `L` is created, then `L/f.iso` | error |

- A trailing slash on the file source (`f.iso/`) is an error. A lone source
  that fails this way exits 1; with other sources, the run exits 23.
- If `T/f.iso` already exists and is a directory, the copy fails and nothing
  is replaced. rsync would replace an empty directory; `irsync` never
  replaces a directory with a file.
- An existing file is updated as it is inside a directory copy: delta
  transfer, `.part` files, `--inplace`, `--append`, `-c`, `-u`, `-n`, `-i`
  and the metadata flags all apply, and a dropped connection resumes.
- Nothing else in the destination directory is touched. `--delete`,
  `--delete-excluded` and `--max-delete` are accepted and do nothing.
- `--include` and `--exclude` are matched against the file's name. An
  excluded file is not copied, and the run exits 0.
- A symlink or special file as the source is treated as it would be inside a
  directory copy: a symlink is copied as a link with `-l` or `-a` and
  skipped otherwise; a device or special file needs `-D` or `-a`. A source
  symlink is not followed.
- Several sources may be files and directories mixed (push only). The
  destination is then always a directory, and each file lands under its own
  name.
- `irsync myserver:dir/f.iso` with no destination lists that one file.
- The missing parent directories of `T` are handled as for a directory
  destination.
- On a push the daemon does not follow a symlink at `T`. A symlink to a
  file, or one that points nowhere, is replaced by the file. A symlink to a
  directory is an error ("is a symlink to a directory"); name the real
  directory instead.

A pull without a trailing slash first asks the daemon whether the path is a
file, a directory or missing. That costs one extra short connection. A
remote symlink as the last component counts as a file and is pulled as a
link, even if it points to a directory. A remote file that does not exist is
an error.

With an automatic ("immediate") grant, a push of a file to a missing `T`
exports `T`'s parent directory, which must already exist, and creates
nothing. The client then has lasting read-write access to that whole
directory. When `T` is a directory, or the destination ends in `/`, the
grant is as for a directory push.

## Listing

A single remote operand lists instead of copying:

- `irsync myserver:` prints the exports this client may reach, one
  `NAME PATH ACCESS` line each, `ACCESS` being `rw` or `ro` with `read_only`
  applied.
- `irsync myserver:photos/` prints the files a pull would fetch, and writes
  nothing. It recurses unless `--no-recursive` is given.
