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

## Listing

A single remote operand lists instead of copying:

- `irsync myserver:` prints the exports this client may reach, one
  `NAME PATH ACCESS` line each, `ACCESS` being `rw` or `ro` with `read_only`
  applied.
- `irsync myserver:photos/` prints the files a pull would fetch, and writes
  nothing. It recurses unless `--no-recursive` is given.
