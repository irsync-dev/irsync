# Identity and trust

`irsync` has no passwords, no user accounts and no certificate authority. Each
client and each daemon is an Ed25519 key pair, and its `EndpointId` is the
public half. Who may connect is a list of `EndpointId`s on the daemon; whom a
client connects to is the `EndpointId` it dials. This page explains where the
keys come from, why the client trusts nothing it has not been given, and how
the daemon decides what a key may reach.

## Keys and EndpointIds

A key is an OpenSSH Ed25519 private key file. The `EndpointId` is the 32-byte
public key, printed in hex or z-base-32. iroh's QUIC handshake proves that each
end holds the private key for the `EndpointId` it presents, so the client knows
it reached the daemon it dialled, and the daemon knows which key connected,
before any `irsync` message is exchanged.

The client and the daemon use different keys, so both can run in one account
on one machine and reach each other:

- The client's key is looked up the way ssh picks one, ending with
  `~/.ssh/id_ed25519` if it exists and with a newly generated
  `~/.config/irsync/id_ed25519` if nothing else serves. The order is in
  [Configuration](../reference/configuration.md#identity-and-state).
- The daemon's key is `[daemon] identity`, `~/.config/irsync/daemon_ed25519`
  by default, generated the first time the daemon, `irsync export add` or
  `irsync show` needs it.

A key is the identity for good. Replacing it with `irsync id generate --force`
gives the machine a new `EndpointId`: every grant made for the old one stops
matching, and every client that pinned the old daemon id fails to connect.
Only file-based keys work; see
[Known limitations](../reference/limitations.md).

## The client pins the daemon

A remote is named by its `EndpointId`, or by an alias that stores one. The
client dials that key and nothing else. A daemon with another key fails the
connection; it is not a warning to confirm.

There is no trust on first use. The client never learns a daemon's key from
the network, has no `known_hosts` file to add to, and never asks whether to
trust a key it has not seen. Both ids are copied by hand once, each in its own
direction, as in [Your first sync](../tutorial/first-sync.md): the client's id
to the daemon's machine, for the grant, and the daemon's id to the client, for
the alias. In exchange, nothing on the way can redirect a client: a DNS or DHT
record, a relay or an address given by hand only says where to knock, and a
wrong one leads to a failed connection, never to the wrong daemon (see
[Connections](connections.md#finding-the-daemon)).

## The daemon checks grants

A grant names a key and an export, read-write or read-only. Grants live in the
daemon's configuration, under each `[exports.NAME]` or under `[all]` for every
export, as aliases or raw `EndpointId`s. `irsync grant add` also takes a pasted
`ssh-ed25519` line or a `.pub` file and writes the id it holds. A grant that
names an alias follows the alias: pointing `laptop` at a new id with
`irsync host add` moves every grant that names `laptop`.

The daemon reads its configuration afresh for every connection, once the
handshake has proved the client's key, and checks the key against it before it
accepts any stream:

- A key that no grant names is closed on at once, with no message. There is no
  open access: `all` and `*` are refused as a grantee.
- A configuration that cannot be read rejects the connection; the daemon does
  not fall back to an earlier copy.
- A change to the grants applies from the next connection; a session in
  progress keeps running. See
  [Change exports and settings](../how-to/change-exports.md#exports-grants-and-aliases).

Access combines in a fixed way. A key's entry in an export's own table decides
its access there; without one, its `[all]` entry does, so an export's `ro`
entry limits a key that `[all]` grants `rw`. Within one table, `rw` wins over
`ro`. An export marked `read_only`, which `irsync export add --ro` sets,
refuses pushes from everyone, whatever they are granted. The rules are listed
in [Configuration](../reference/configuration.md#exports).

## Paths, exports and "no access"

A remote path is resolved on the daemon's machine, as an export name, a path
under the daemon account's home, or an absolute path (see
[Remote addresses](../reference/remote-addresses.md#resolution-order)). Then
the export that contains it is chosen, among the exports the key has a grant
for only. Exports may be nested:

- The most specific export, the one with the longest path, decides the
  session's root and access. Between exports with the same path, the one the
  operand named wins, then the one with the better access.
- A grant on an outer export reaches everything below it, the inner exports
  included. An inner export the key has no grant for does not stop it, and
  neither does an inner `read_only` export, since an export without a grant
  takes no part in the choice.

A path that no export the key may use contains is refused with
`no access to PATH`. The answer is the same whether the path lies outside
every export, inside an export without a grant, or does not exist, so a key
cannot learn the daemon's layout by probing. Only a key that may use the
export learns more: that the export is read-only, or that the path cannot be
used as a session root.

A `..` component or a NUL byte is refused in every path, before any lookup.
Below the export's directory, every existing component of the session's root must
be a directory and not a symlink to one, so the root cannot leave its export
either.

## Sessions belong to their key

A session is resumed only by the key that started it. A `Hello` that names the
session id of another key gets `BAD_SESSION`, the same answer as for an id the
daemon never knew. See
[Sessions and resume](sessions-and-resume.md#what-the-daemon-checks).
