# Remote paths: one syntax for exports and real paths

Date: 2026-10-28. Status: approved design, not yet planned.

Builds on `2026-10-23-zero-config-cli-design.md` (hosts, exports, grants)
and `2026-10-28-config-cli-design.md` (the `host`, `export`, `grant` and
`id` nouns). This document changes only the remote operand of a copy, what
the daemon does with it, and one new listing. It does not change flags,
the local operand, the config files or the management commands.

## Intent

Today a remote operand is `HOST:EXPORT/PATH`: the first path component is
the name of an export and nothing else is accepted, so `theoden:/srv/photos`
is a usage error that tells you to export the directory under a name. That
is rsync's module syntax, which the user has not used in twenty years and
finds clunky. What they have used every day is `host:/path` over ssh.

The goal is one spelling that covers both: the real path on the daemon's
machine works the way it does over ssh, and a short export name still
works as a convenience. Exports stop being the namespace and become the
access boundary: they decide which trees a peer may reach and with what
rights, and the name of an export matters for grants and for the short
form, nothing else.

Two journeys drove the design. A backup is a one-liner pushed by hand from
shell history, `irsync -av ~/Documents/ theoden:/backup/laptop/Documents/`,
and it should read like the rsync-over-ssh line it replaces. A photo sync
is a short shell script around one destination, `irsync -av theoden:photos/
~/Photos`, where the short name keeps the script stable if the server moves
its photos. Anything irsync might "remember" for those lines (a named
destination, a saved flag set) belongs in the shell, not in irsync.

## Goals

- `HOST:PATH` where `PATH` is an absolute path on the daemon's machine, a
  path relative to the daemon's home directory, or an export name followed
  by a path inside it, with one resolution order.
- Exports as the access boundary: any path inside an exported directory is
  reachable with that export's grants; nothing outside any export is.
- A client-side way to learn what it may reach: `irsync HOST:` lists the
  exports this client has a grant for, with their real paths.
- Nothing about the server's layout is revealed to a peer that may not use
  it. Fail closed: a daemon with no exports serves nothing.

## Non-goals

- Both operands remote, `rsync://` URLs, or `~user/path` forms.
- Named destinations or saved flag sets in irsync. The shell owns those.
- Any change to the flags, the trailing-slash rule on either side, the
  local operand, or the config files.
- A detailed directory listing (sizes, times). A single remote operand
  naming a directory lists it in the format the dry run already uses.
- A built-in export over `/` or the home directory. Opening the whole
  filesystem is an explicit `irsync export add root:/`.

## The remote operand

An operand is remote when a `:` appears before any `/`, as today (rsync's
rule); the text before the colon is an alias from hosts.toml or a raw
EndpointId, resolved on the client as today, and a local path that
contains a colon is written `./a:b` or absolutely. Everything after the
colon is `PATH`, passed to the daemon as typed except that a trailing `/`
is stripped and sets `contents_only`, as today. A lone `/` is kept:
`theoden:/` is the root of the daemon's filesystem, not the home (my
call; the reviewer asked). The client no longer splits off an export name
and no longer rejects a leading `/`.

The rsync trailing-slash rule on the *local* side (a push of `~/photos`
without a slash lands in `DEST/photos`) is applied by the daemon, not the
client. Today the client appends the source's basename to the remote path
before sending; under the rules below that would turn `irsync -av ~/photos
theoden:` into a request for the export named `photos`. So `Hello` carries
`PATH` as typed plus a separate `name`, the basename to append once the
path is resolved, empty when the source had a trailing slash or the daemon
sends. The daemon resolves `PATH`, picks the export, then joins `name` to
the remainder. (My call; the reviewer found the collision.)

The daemon resolves `PATH` to a real directory, in this order:

1. **Short name.** `PATH` is relative and its first component is the name
   of an export in exports.toml: the export's directory plus the rest.
   `photos/2024` is `<photos' path>/2024`. This holds whether or not the
   peer may use the export; a name that exists is never reinterpreted as
   a home-relative path, so a peer learns nothing from which rule fired.
2. **Home-relative.** `PATH` is relative (including empty, `~`, and `~/x`,
   where the `~` component means the home directory): the home directory
   of the account the daemon runs as, plus `PATH`. The home directory is
   `$HOME` of the daemon process, else the account's passwd entry.
3. **Absolute.** `PATH` starts with `/`: that path.

Resolution is lexical. `.` components are dropped; a `..` component, an
empty component or a NUL byte is rejected (`BAD_PATH`). Symlinks are not
resolved at this step; an export's path is used as configured, as today.

The resolved path must lie inside an exported directory: the export's
configured path is a component-wise prefix of it (or equal to it). When
several exports contain the path, the one with the longest path governs
(the most specific export wins); when two exports have the same path, the
peer's best access among them wins, read-write over read-only. The chosen
export is the session's root, the remainder is the session's relative
path, and the export's index is the one used, exactly as if the client had
named the export. Nothing below the hello handshake changes.

Access is then as today: the peer's grant on the chosen export, with
`[all]` applying to every export and `read_only` capping writes.

### Examples, with exports `photos = /srv/photos` and `backup = /backup`

| Operand | Resolves to | Export |
| --- | --- | --- |
| `theoden:photos/2024` | `/srv/photos/2024` | photos |
| `theoden:/srv/photos/2024` | `/srv/photos/2024` | photos |
| `theoden:/backup/laptop` | `/backup/laptop` | backup |
| `theoden:Pictures` | `~irsync/Pictures` | none unless the home is exported |
| `theoden:` (as an operand of a copy) | `~irsync` | none unless the home is exported |
| `theoden:/etc` | `/etc` | none |

To make `theoden:Pictures` work the admin exports the home directory,
`irsync export add home:~` (the path is made absolute as today). To open
everything the daemon's account can read or write, `irsync export add
root:/`; the account's own permissions then bound what is reachable.

### Errors

The reply for a path the peer may not use is the same whether the path is
outside every export, inside an export the peer has no grant for, or does
not exist: `UNAUTHORIZED`, "no access to PATH" with the path as typed. Only
a peer that may use the containing export learns that the path is invalid
(`BAD_PATH`) or that the export is read-only for it (`READ_ONLY`).
`NO_SUCH_EXPORT` has no use left and is removed.

The client's message for an `UNAUTHORIZED` answer can no longer name the
export, since the user may have typed a real path. Both signals get the
same message: the close with code 1 a key with no grants receives before
`Hello`, and the `UNAUTHORIZED` refusal of a `Hello` that a key with grants
receives for a path it may not use, which is the common case now (my
call). It says that the daemon rejected this client's key for `HOST:PATH`,
prints this client's id, and
spells out what to run on the daemon's machine: `irsync grant list <id>`
to see what the key may reach, `irsync grant add EXPORT <id>` to grant an
export, `irsync export add NAME:PATH` to export a directory.

## Listing

A copy still takes `SRC... DEST` with exactly one remote side. New: a
single remote operand and no destination is a listing.

- `irsync HOST:` (nothing after the colon) prints the exports this client may reach, one per line,
  sorted by name: `NAME PATH ACCESS`, for example `photos /srv/photos rw`.
  `ACCESS` is `rw` or `ro`; a read-write grant capped by `read_only` shows
  `ro`. Only exports the peer has a grant for (directly or from `[all]`)
  appear, so a peer with no grant sees nothing and learns nothing. The
  real path is shown because the client needs it to use the absolute
  form. This is the client-side counterpart of `irsync grant list WHO` on
  the daemon.
- `irsync HOST:PATH` with a non-empty path, `/` included, prints that
  directory as a dry-run pull does today (`-n -v` output), without
  writing anything. Access rules apply as for a pull. The listing runs
  the existing dry-run pull against an empty temporary directory as the
  local root, with the index under that same temporary directory, both
  removed when the listing ends, so no state is left behind and the whole
  directory is listed rather than only what differs (my call).

A single local operand remains the usage error it is today.

## Protocol

No compatibility is owed to any released version. `Hello` loses `export`;
`path` carries `PATH` as typed (bytes), `contents_only` stays, and `name`
(bytes, the basename to append on a push) is new. The protos live in
`proto/irsync/v1/`. The export
listing needs a request and a reply (name, path, access per export); how
it rides the connection is the builder's choice. `PROTOCOL_VERSION`
becomes 2 so a mismatched pair fails with the existing version error.

## What changes where

- Client (`cli/location.rs`, `main.rs`): `Location::Remote` carries
  `host`, `path` (bytes) and `contents_only`; the "missing export" usage
  error goes away; one remote operand alone runs a listing.
- Daemon (`daemon.rs`): resolution and export selection as above, before
  the existing grant and direction checks; the error texts above.
- Session (`session/mod.rs` `resolve_roots`, `reconnect.rs` and the
  `SessionRefused` path): the basename moves to `Hello.name`; the
  `UNAUTHORIZED` hint on both signals.
- Docs: README (addresses paragraph, quick start, command table) and
  `docs/daemon.md`; a note in the 2026-10-23 spec's addressing section.
- Tests: the location parser, daemon validation (every row of the table
  above, nested exports, same-path exports, the uniform error), the two
  listings end to end, and the hint text.

## Left to the builder

- The wire shape of the export listing and whether `irsync HOST:PATH`
  listing reuses the dry-run code path directly or a thin wrapper.
- How the daemon obtains its home directory (`dirs::home_dir()` is
  already a dependency).
- Component-wise prefix matching on paths as bytes, and where the lexical
  normalization lives (`fs/paths.rs` is the natural home).
- Whether the resolved export and remainder are logged at `info` for the
  daemon's operator; the user's `-v` output on the client is unchanged.
