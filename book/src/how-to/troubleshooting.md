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

On the client, save it under the server's name, replacing the old one:

```sh
irsync server add myserver <daemon-id>
```

Then check that `irsync daemon` is running on the server.

## A server paired with irsync pair did not answer

```text
irsync: myserver did not answer. On myserver, check that the daemon is running: systemctl --user status irsync (Linux) or launchctl print gui/$(id -u)/org.irsync.daemon (macOS). Its log: journalctl --user -u irsync, or ~/.local/state/irsync/daemon.log for a daemon started in the background. To start it again from here: irsync pair myserver
```

**Cause.** The daemon is not running or cannot be reached. A sync never uses
ssh, so it cannot start the daemon for you.

**Fix.** Do what the message says. On the server, check the service. Its log is
in `journalctl --user -u irsync` when a service runs the daemon, and in
`~/.local/state/irsync/daemon.log` when `pair` started it in the background.
A background daemon does not survive a reboot. To start the daemon again from
the client, run:

```sh
irsync pair myserver
```

On a Mac, the agent starts after a reboot only once someone logs in there.

## A sync names a server irsync does not know

```text
irsync: myserver is not a saved server or an EndpointId (saved servers are in ~/.config/irsync/irsync.toml). To set it up over ssh: irsync pair myserver. To add it by hand: irsync server add myserver <daemon-id>
```

**Cause.** A sync, push, pull or listing never uses ssh, so it does not set a
server up. `myserver` is not in your saved servers, or it was mistyped.

**Fix.** Check the spelling. If the name is right and `ssh myserver` works, run
`irsync pair myserver`. Otherwise add the server by hand, as in
[Your first sync](../tutorial/first-sync.md#set-the-server-up-by-hand). A name
saved as a client only gets a similar message: this machine has never dialled
its daemon, so run `irsync pair` for it.

## pair stops: the server runs another OS or CPU

```text
irsync: pairing with myserver over ssh
irsync: "myserver" is not a server in ~/.config/irsync/irsync.toml and not an EndpointId; setting it up over ssh failed: myserver runs linux aarch64, this irsync is macos aarch64 and a development build (0.1.0-beta.1 (1a2b3c4, dirty)). Only release builds install on another OS or CPU. Install irsync on both machines and set the server up by hand: https://irsync.org/book/tutorial/first-sync#set-the-server-up-by-hand
```

**Cause.** When the server's OS or CPU differs from the client's, `irsync pair`
installs the release binary of the client's own version. That needs a
client that is itself a release: a build from source has no matching release
binary. The same stop, worded "there is no release binary for that
platform", comes for a server no release is built for, such as an Intel Mac.

**Fix.** Use a released irsync on the client
([Install](../introduction.md#install)), or install irsync on the server
yourself and
[set the server up by hand](../tutorial/first-sync.md#set-the-server-up-by-hand).
Installing it on the server does not make `pair` work, unless you run
with `--no-install`: `pair` otherwise uses its own copy of the client's
exact build.

Other messages from this step:

- `need curl or wget on this machine to download the release binary`, or
  `tar failed (it must be able to read .xz)`: the client downloads and
  unpacks the release itself. Install the missing tool on the client.
- `could not download …`: the client could not reach GitHub. Check the
  network and run again.
- `GitHub answered with an error for …`: either GitHub is having trouble
  (run again later), or there is no release with the client's version
  number, which means the client is a development build. Then use a
  released irsync.
- `release v… is not this build`: the client is a development build that
  carries a release's version number. Use a released irsync.
- `the release binary for … does not run on myserver`: the server could not
  run the binary, for example because its home directory is mounted
  `noexec`. Install irsync on the server yourself and set it up by hand.
- `the download of … is damaged (SHA-256 does not match)`: run again.
- `the release … has no signature for sha256.sum, or GitHub answered with
  an error`: run again later, in case GitHub had a problem. If the message
  stays, or it is `the release … failed the signature check`, the release
  on GitHub is not signed the way this irsync expects, so it refuses to use
  it. Report it on
  [GitHub Issues](https://github.com/irsync-dev/irsync/issues), and set the
  server up by hand meanwhile. If you built irsync yourself, this is
  expected: only release builds install on another OS or CPU.

Nothing on the server is changed in any of these cases.

## pair says there are no release files for this build

```text
irsync: downloading irsync v0.1.0-beta.1 for macos aarch64
irsync: no release files for this build (could not download release v0.1.0-beta.1: …); installing this machine's own files
```

This is a notice, not an error. The server has the same OS and CPU as the
client, so when the download fails, or the release is another build, the
setup sends the client's own files instead. The setup goes on. `fileset` and
the manual pages come along only if they sit beside the client's `irsync`;
the summary says when they did not.

## pair stops on the server

These stop the setup after the files reached the server:

- `could not unpack irsync on myserver (it needs tar and a writable home
  directory). Nothing was changed there.`: the server has no `tar`, or the
  setup cannot write its work directory in your home there. Install `tar`,
  or check that you own your home directory and can write to it, then run
  again.
- `the installer failed on myserver; see its message above`: the installer
  script ran on the server and stopped. Its own message, printed just
  before, says why, for example a `~/.local/bin` you cannot write to. Fix
  that and run again.

## pair stops: --no-install

```text
irsync: … setting it up over ssh failed: --no-install: myserver has no irsync 0.1.0-beta.1 (ae5ebe5) at ~/.local/bin/irsync (found none). Install that version there, or run without --no-install (or set ssh_install = true in the config). Nothing was changed.
```

**Cause.** With `--no-install`, or `ssh_install = false` in the config file,
the setup copies nothing. It needs the client's exact build at
`~/.local/bin/irsync` on the server, and found another version there
(`found irsync …`) or none.

**Fix.** Install the same version as the client on the server, in
`~/.local/bin`. Or let the setup install it: run without `--no-install`, and
remove `ssh_install = false` from the config file if it is there.

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

`irsync pair` does not fix this. Pairing again never changes the grants a client has lost. The refusal
is on purpose, and irsync never adds back an immediate grant you removed. See
[Identity and trust](../explanation/identity-and-trust.md).

## The transfer is slow and says `relay`

The live block (`--progress`), and the summary line at the end of each session (`-vv`), name the
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
