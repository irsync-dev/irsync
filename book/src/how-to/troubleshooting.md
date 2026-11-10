# Troubleshoot a first sync

The errors people meet on a first sync, what they mean and what to do.

## A host added by hand never answers

The sync waits, then stops with one of two messages. When no address was
ever published for the ID, which is the usual case for a wrong one:

```text
irsync: ~/Photos: gave up after 5 attempts: the daemon never answered: connecting to 4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff: no address found for it. Is this the daemon's EndpointId? On the server, `irsync id show --daemon` prints it (plain `irsync id show` prints the client's). Also check that the daemon is running there.
```

When an address is known but nothing answers there, for example a daemon
that has stopped:

```text
irsync: ~/Photos: gave up after 5 attempts: the daemon never answered: connecting to 4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff: timed out; if you copied this host's ID by hand, it must be the daemon's EndpointId: on the server, irsync id show --daemon prints it (plain irsync id show prints the client's). Also check that the daemon is running there
```

The listing, `irsync myserver:`, ends with the same advice.

**Cause.** Most often the client was given the server's *client* ID instead
of its daemon's. A server has two keys: plain `irsync id show` prints the one
it uses as a client; clients must connect to the daemon's. Nothing answers to
the wrong one, so the client can only wait. The other cause is a daemon that
is not running.

**Fix.** On the server, print the daemon's ID:

```sh
irsync id show --daemon
```

On the client, save it under the host's name, replacing the old one:

```sh
irsync host add myserver <daemon-id>
```

Then check that `irsync daemon` is running on the server.

## ssh setup stops: the server runs another OS or CPU

```text
irsync: myserver is not a saved alias; setting it up over ssh
irsync: "myserver" is not an alias in ~/.config/irsync/irsync.toml and not an EndpointId; setting it up over ssh failed: myserver runs linux aarch64, this irsync is macos aarch64, so it cannot copy itself there. Install irsync on both machines and set the server up by hand: https://irsync.org/book/tutorial/first-sync.html#set-the-server-up-by-hand
```

**Cause.** The ssh setup installs irsync by copying the client's own binary,
which only runs on the same OS and CPU.

**Fix.** [Install](../introduction.md#install) irsync on the server yourself
and [set the server up by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).
Installing it on the server does not make the ssh setup work, because the
setup only uses its own copy of the client's exact build. Release binaries,
planned for the beta, will remove this step.

## The daemon rejected this client's key

```text
irsync: the daemon rejected this client's key: no access to myserver:photos/. On the daemon's machine, irsync grant list a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba shows what this key may reach; irsync grant add EXPORT a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba grants an export, irsync export add NAME:PATH exports a directory (irsync id show prints this id)
```

**Cause.** The daemon answered, but no grant lets this client reach that
path: the client was never granted an export, the path lies outside every
export it may use, or a grant was removed.

**Fix.** On the server, grant the export to the client's ID, which the message
names:

```sh
irsync grant add photos <client-id>
```

A host set up over ssh is not "fixed" over ssh when this happens. The refusal
is on purpose, and irsync never adds back an immediate grant you removed. See
[Identity and trust](../explanation/identity-and-trust.md).

## The transfer is slow and says `relay`

The live block, and the summary line at the end of each session, name the
path the connection took: `direct` or `relay`. See
[Connect directly or through a relay](direct-and-relay.md#see-which-path-a-sync-took).

**Cause.** The two machines could not reach each other directly (hole
punching failed, often when both sit behind strict NATs), so the data goes
through a relay server. Relays are a fallback and much slower than a direct
path.

**Fix.** [Connections](../explanation/connections.md#relays) explains when
hole punching fails. When the connection has to stay on a relay, run
[your own relay](direct-and-relay.md#use-your-own-relay) close to both
machines.
