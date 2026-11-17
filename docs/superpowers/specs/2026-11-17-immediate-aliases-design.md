# Host aliases in immediate mode (#33)

## Why

A first sync to an unknown host saves whatever was typed before the `:` as
the alias, and the server only ever sees the client as a bare EndpointId.
Feedback on #33: the saved names should be ones a person would type and
recognise.

- `mike@myserver:` saves `mike@myserver`; a later `myserver:` does not find
  it and runs ssh setup again.
- `192.168.1.5:` saves an alias that says nothing about the machine and
  goes stale. An IPv6 address cannot be saved at all.
- On the server, `[immediate].rw` and automatic exports list a long id with
  nothing that says which machine it is.

## Goals

1. `user@host` saves two aliases: `user@host`, and plain `host` when that
   name is free. Both have `ssh = "user@host"`.
2. An IP address is saved under the server's own host name, with the
   address as the ssh destination. Typing the address again finds that
   entry.
3. The client sends its host name during ssh setup. The server saves it in
   `[hosts]` and uses the name in `[immediate].rw` and in automatic
   exports.

## Non-goals

- No migration of bare-id grants already on a server. They keep working.
- No protocol change: the name travels over ssh only.
- No renaming when a client's host name changes later.

## Anti-goals

- The client's name must never decide access. It is a label that resolves
  to an id through `[hosts]`; access is checked on the id.
- The name must never be written into the config unquoted or unchecked.
- The server must not be changed for a name that is then refused.

## Approaches

One way fits; the alternatives were dropped for these reasons.

- **Server host name: from the probe (picked)** or from the `ssh-setup`
  report. The probe runs before anything is installed or changed, so a
  name that cannot be saved stops setup early. The report arrives after
  the server has been changed.
- **Client host name: a flag on `ssh-setup` (picked)** or sent over the
  irsync connection. The flag lets the server check the name before it
  writes the grant. The connection would mean a protocol change, and the
  grant would already exist as a bare id.
- **Typing the IP again: find the host by its ssh destination (picked)**,
  save the IP as a second alias, or do nothing. The lookup keeps one entry
  per machine and works for IPv6. A second alias keeps the stale name and
  still fails for IPv6. Doing nothing runs ssh setup on every sync.

## Design

### Client: finding a saved host (`connect_for`, `main.rs`)

Order of lookup for the text before the `:`:

1. an alias with that name (as today);
2. an EndpointId (as today);
3. **new:** a saved host whose `ssh` value is exactly that text. Use it,
   and print `irsync: 192.168.1.5 is saved as myserver`. If several
   entries match and they do not all have the same id, refuse and name
   them;
4. otherwise ssh setup (as today).

`--ssh` and `--no-remote-dht` on a host found by step 3 recheck that entry.

### Client: which aliases to save (`onboard`, `ssh/onboard.rs`)

- **`user@host`:** save `user@host`. Also save `host` if no alias of that
  name exists (first one wins; an existing `host` is left alone, whatever
  its id). Both get `ssh = "user@host"`.
- **IP address** (the host part parses as IPv4 or IPv6, with or without a
  `user@`): the alias is the server's short host name from the probe.
  `ssh` is the text as typed. If that alias exists with a different id,
  use `name-2`, `name-3`, ... as automatic exports do. If it exists with
  the same id, update it.
  If the server gives no usable name: IPv4 falls back to today's
  behaviour (the address is the alias); IPv6 fails with a clear message,
  before the server is changed.
- **Anything else:** as today.

The report lists every alias that was saved.

### Probe (`ssh/client.rs`)

Add `hostname` to the probe command and a `hostname: Option<String>` to
`Probe`. Keep the part before the first `.`. It is usable when it passes
`valid_alias`.

### Server: `ssh-setup --client-name-hex HEX` (`ssh/setup.rs`)

The client sends its own short host name. Before any change:

- NAME is not a valid alias: refuse.
- `[hosts.NAME]` exists with a different id: refuse.
- `[hosts.NAME]` exists with the client's id: go on as before.

Then add `[hosts.NAME]` with the client's id, and grant `NAME` (not the
id) in `[immediate].rw`. A client already granted by bare id counts as
granted. Its `[hosts]` entry is still added, and saving the config then
writes the grant by that name, as every save already does for a peer that
`[hosts]` has a name for. A client the server already has a name for keeps
that name: no second entry is added and its own host name is not checked.
With immediate mode off, nothing is granted, named or refused. A recheck follows the same rules and
still never adds back a removed grant.

If the client cannot work out its own host name, it fails before ssh
setup with a message saying so.

All writes go through `toml_edit` keys and string values, so the name is
always quoted where TOML needs it. A test covers a name that needs quotes
(for example one with a space or a `.`).

Known consequence: a client with a new key but the same host name is
refused until the old `[hosts.NAME]` entry is removed on the server. The
error message says which entry to remove.

### Server: automatic exports (`authz.rs`, `manage/exports.rs`)

`add_auto_export` writes the alias from `[hosts]` for the peer's id into
`rw` when there is one (`Hosts::alias_of`), else the id as today.

### Docs

Update the book pages that describe the first sync and grants:
`tutorial/first-sync.md`, `how-to/ssh-setup.md`,
`reference/remote-addresses.md`, `reference/configuration.md`,
`reference/management.md`. Examples use `myclient` and `myserver`.

## Calls made by the author, not by Mike

- Short host names only (cut at the first `.`).
- `-2` suffix when the server's name is taken on the client by another id.
- IPv4 fallback and IPv6 failure when the server reports no usable name.
- Refuse when two saved hosts share an ssh destination but not an id.
- Fail on the client when its own host name cannot be found.
- A bare-id grant already present counts as granted. (It is rewritten to
  the name on save; that is how the config writer already works.)
- The name goes over ssh as hex digits (`--client-name-hex`), because the
  command is read by whatever login shell the server account has.
- `localhost`, all-digit names and names with control characters count as
  no usable host name.
- The server's name comes from `uname -n`.
- IPv6 hosts are typed in brackets: `[fe80::1]:PATH`.

## Left to the builder

- Wording of messages and the report lines.
- How the short client host name is read (an existing dependency if one
  does it, else the smallest addition).
- Test layout: unit tests beside each function; an end-to-end test of the
  onboarding path where the existing ssh tests allow it.
