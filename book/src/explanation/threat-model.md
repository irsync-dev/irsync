# Threat model

This page says who can send irsync data, what each of them could try, what
stops them, and what irsync does not defend against. It is the map for
anyone who wants to judge whether irsync is safe for their use, or who wants
to look for holes.

[Identity and trust](identity-and-trust.md) explains the keys and grants in
detail. This page builds on it.

## What irsync sets out to protect

1. **Data on the way.** Nobody between the client and the daemon can read or
   change it.
2. **Who you talk to.** A client reaches the daemon whose key it named, or
   nobody.
3. **Who gets in.** A key reaches only the exports it has a grant for, and
   only in the way the grant allows: reading, or reading and writing.
4. **Everything outside the sync.** A sync changes files only inside its
   destination directory. A daemon reads and writes only inside the export.
5. **Content.** What is placed in the destination is what the source had.
   [How we verify irsync's correctness](correctness.md) covers this one.

## Who can send irsync data

| Who | What they control | How far they are trusted |
| --- | --- | --- |
| The network, relays and address lookups | Packets, and the answers to "where is this daemon" | Not at all |
| A key without a grant | A connection attempt | Not at all |
| A client with a read-only grant | Requests for files in an export | To read that export |
| A client with a read-write grant | File names, contents and metadata pushed into an export | To change that export, and nothing else |
| A client with the immediate grant | The same, for any path | As much as the daemon's account itself |
| The daemon a client talks to | The listing and the data on a pull; the answers on a push | To hold the files you give it, and to supply the files you ask for |
| Whoever made a tree you sync | File names, symlinks, permissions, and any `.fileset` manifest in it | Not at all, beyond the content you chose to copy |
| Other users of the same machine | Files in directories they can write | Not at all |

The sections below take them in turn.

## The network, relays and lookups

**What they could try:** read the data, change it, pretend to be the
daemon, or send the client to another machine.

**What stops them:**

- Every connection is QUIC with TLS, between two Ed25519 keys. The handshake
  proves each end holds its private key before any irsync message is sent.
- The client dials one key and accepts no other. There is no trust on first
  use and no prompt to accept a new key.
- A relay forwards packets that are encrypted end to end. It cannot read or
  change them. See [Connections](connections.md#relays).
- DNS, DHT and mDNS records only say where to knock. A wrong record leads to
  a failed connection, never to another daemon.

**What is left:**

- A relay, and anyone who watches the network, sees that two keys talk, when,
  and roughly how much. The public lookup records tie a daemon's key to its
  relay. irsync does not hide who talks to whom.
- Anyone on the path can block or slow the connection.
- **A recording of a connection could be decrypted later** by someone with a
  large quantum computer. No such machine exists today. The key exchange
  irsync uses now (X25519) would not hold against one. irsync is to move to
  a hybrid key exchange that does; see
  [issue 219](https://github.com/irsync-dev/irsync/issues/219).
- **The keys are Ed25519,** which such a machine could forge too. That would
  let it pretend to be a daemon or a client from then on. It does not open
  up connections recorded before.

## A key without a grant

**What they could try:** connect and see what the daemon reveals, or keep
it busy.

**What stops them:**

- The daemon closes the connection at once, with no message, when no grant
  names the key. It reads its grants afresh for every connection.
- There is no open access: `all` and `*` are refused as a grantee.

**What is left:** the daemon still pays for the handshake of every attempt.
irsync has no rate limit of its own for strangers; use a firewall if that
matters.

## A client with a grant

**What they could try:** reach files outside the export, write to an export
they may only read, learn where other exports are, take over another
client's session, or exhaust the daemon.

**What stops them:**

- A path is resolved and matched against the exports the key may use before
  anything is said about it. `..` and NUL are refused. A path outside those
  exports gets `no access`, the same answer as for a path that does not
  exist, so a key cannot map the daemon by probing.
- Below the export's directory, every existing component of the session's
  root must be a real directory, not a symlink.
- A read-only grant, or an export marked `read_only`, refuses every push.
- Every path in a listing must be relative, with no `..`, no `.`, no empty
  part and no NUL. A listing must be in order, with no path twice.
- Symlinks are stored as they are sent and are not followed when irsync
  walks a tree. A file listed below a symlink is refused.
- A file is read, written, renamed and deleted through its directory, and
  that directory is opened step by step from the
  export's own directory with no symlink followed. A directory that was
  swapped for a symlink after it was listed, for example by a second
  session of the same client, makes the operation fail. It does not lead out
  of the export. The same holds for a `--partial-dir` that is a symlink.
- The walk that lists a tree checks every entry through its directory in
  the same way. A session root that has become a symlink is not walked.
- Deleting is done relative to a directory that was opened without following
  symlinks, never by descending into a link.
- A session can be resumed only by the key that started it.
- A key may leave only `max_dormant_sessions_per_key` interrupted sessions
  stored in an export, 25 by default. The oldest is dropped when there are
  more.
- Messages are limited to 16 MiB, the queues between the two sides have
  fixed sizes, and an idle connection is dropped after `idle_timeout`.
- The filter rules a client sends are limited in number and size before the
  daemon compiles them, and so is a path the other side names for a file.

**What is left:**

- **A read-write grant is real power inside the export.** The client can
  replace or delete everything in it, fill the disk, and leave symlinks that
  point anywhere. irsync does not follow those links, but another program
  that reads the export might.
- **Metadata is applied as sent,** as with `rsync -a`: permission bits
  including setuid, and, when the daemon runs as root, owners and device
  files. Do not run the daemon as root for keys you do not trust that far.
- **A granted client can use the daemon's resources:** CPU for hashing, disk
  reads, memory, and the sessions it has open at once. The limits above
  bound single messages, queues and stored sessions, not a client that works
  at being expensive. irsync does not treat a granted key as hostile to the daemon's
  uptime.
- **The immediate grant is the whole account.** Such a key can reach any
  path the daemon's account can. Give it only to machines you would give
  ssh access to. See
  [Immediate grants](../reference/configuration.md#immediate-grants).

## The daemon you connect to

A daemon you run yourself is as trustworthy as its machine. This section is
about a daemon you do not control, or one that was broken into.

**What it could try:** on a pull, send other files than you asked for,
write outside your destination, or read your local files. On a push, keep
or change what you send.

**What stops it:**

- The same listing checks as above apply on the client: relative paths
  only, nothing below a symlink, nothing outside the destination directory.
- The client deletes only with a delete flag from its own command line, and
  only inside the destination. A listing that did not arrive whole stops
  the delete pass.
- Every chunk and every whole file is checked against its BLAKE3 hash before
  it is placed.
- When a file you already have changes, the sender may ask for parts of
  your old copy to be reused. The client reuses only whole blocks it
  described to the sender itself, so a sender cannot probe your old file
  for its content.

**What is left:**

- **On a pull, the daemon decides what is in the listing.** It can send
  files you did not expect, inside the destination. With a delete flag, a
  daemon that lists less makes you delete more, inside the destination.
  That is what the flag means. Pull from a daemon you do not trust into a
  directory of its own, never into your home directory, and look before you
  use what arrived.
- **A daemon learns what you push,** and can keep, change or drop it. irsync
  does not encrypt files at rest, and it cannot prove that a daemon still
  has your data. [fileset](../fileset/introduction.md) can check a copy
  later against a manifest you keep yourself.
- **Hashes show what the source sent, not what it should have sent.** They
  catch damage on the way, not a source that lies.
- **On a pull, the daemon can learn whether your destination already holds
  some content.** The daemon sends the hashes of each file's chunks, and
  the client asks only for the chunks it finds in no file of the
  destination tree. That is what makes a renamed or copied file cost
  nothing. It also means that a daemon which guesses the whole content of a
  chunk can see whether the guess was right, also for files it never sent
  you. A chunk is a whole small file, or 64 KiB to 1 MiB of a larger one.
  The daemon learns nothing about content it cannot guess. Pulling into a directory of its own leaves nothing there to ask
  about. `--no-rename-detection` makes the client look only at the old
  version of the same file, at the price of fetching moved and copied
  files again. On a push there is nothing to learn this way: a key that
  may write to an export may read all of it anyway.

## Whoever made the tree you sync

File names, symlinks and permissions travel with a tree. So does a
`.fileset.sqlite`, `.fileset.fjall` or `.fileset.redb` manifest inside it.

**What they could try:** names that confuse the terminal or a log, symlinks
that point out of the tree, setuid files, or a manifest that says the copy
is fine.

**What stops them:**

- Paths are shown escaped: control characters, bytes that are not UTF-8,
  and the invisible characters that reverse the direction of text are
  printed as `\ooo`.
- A log event stays on one line. Text from the other side cannot end a
  line and start one that looks like irsync wrote it.
- irsync never follows a symlink it copies.
- A manifest is compared with the files as they are now. It cannot make
  `fileset` skip a file that is on disk.
- `fileset` refuses a manifest in the tree that is a symlink, or that has
  symlinks inside. It opens a SQLite manifest with SQLite's settings for
  files that are not trusted, and refuses one with views, triggers or
  virtual tables. In a redb manifest every entry carries a checksum of its
  path and content, the whole file is verified before `update` writes to
  it, and a failure inside the database engine is reported as a damaged
  manifest.

**What is left:**

- **A manifest is only as good as where it came from.** `fileset check`
  proves that a tree matches its manifest, not that the manifest is the
  original. Someone who can change the files can change the manifest too.
  Keep a copy you trust somewhere else, or compare root hashes over another
  channel. See [How fileset works](../fileset/how-it-works.md).
- **A manifest is a database file from someone else.** The checks above
  narrow what a crafted one can do; they do not make the database engines
  safe against every crafted file. A `.fileset.fjall` manifest is repaired
  when it is opened, even by `check`, so `check` can write into it. redb
  does not check a file it only reads, and says that it may stop on one that
  was altered; `fileset` turns that into an error, but nothing fuzzes redb's
  reading of a crafted file here.
- Symlinks and setuid files arrive as they were. What other programs do with
  them is up to you.

## Other users of the same machine

**What they could try:** read your key or state, or swap a directory for a
symlink while a sync runs in a tree they can write.

**What stops them:**

- A new private key is created readable by its owner only, and an existing
  key is never overwritten.
- irsync stops when the config file, a private key or a passphrase file can
  be read or written by other users, or belongs to another user. The config
  file is checked before anything else happens. Setting
  `unsafe_config_allowed = true` in the config file turns these checks off.
- A file being received is readable by its owner only until it is placed.
- The index of file names and hashes sits in a directory only its owner can
  enter. A new config file and the daemon's log are for the owner only.

**What is left:**

- Whoever can read a private key file is that identity. An encrypted key
  (`irsync id generate --passphrase-file`) helps against a copied file.
- irsync does not defend a sync against another user who can write in the
  source or destination tree while it runs. Do not sync as root into, or
  out of, a tree that other users can change.
- irsync's state directory holds the names and hashes of the files it
  synced. Protect it as you protect the files.

## Setting up over ssh, and installing

- [The setup over ssh](../how-to/ssh-setup.md) trusts ssh: the server you
  log in to is the one irsync installs on and grants access to. It gives
  this client the immediate grant on that server.
- Releases are signed. [Verify a download](../how-to/verify-a-download.md)
  shows how to check one, and
  [Supply chain security](../contributing/supply-chain.md) how they are
  built.

## What irsync does not defend against

In one place:

- A machine that is already broken into, on either side, or a stolen key
  file.
- A daemon you chose to trust that turns out to be hostile, beyond keeping
  it inside your destination directory. It can also check guesses about
  what that directory already holds.
- Traffic analysis: who talks to whom, when, and how much.
- A future quantum computer: it could decrypt a connection recorded today,
  and forge a key.
- A client with a grant, or a stranger, who sets out to use up the daemon's
  CPU, memory, disk or network.
- Other users of the same machine who can write in the tree being synced.
- Files at rest. irsync encrypts the connection, not the disk.
- What other programs do with the symlinks, setuid files and device files a
  sync was asked to copy.

## Where the review stands

The code was checked against this page in November 2026. The work is
tracked in [issue 126](https://github.com/irsync-dev/irsync/issues/126).

The review looked at the limits on what the other side may send, file
permissions, logs and terminal output, `fileset` manifests, the reuse of old
file content, how files are reached by name, and how the daemon resolves a
client's path. What it found there is fixed, and the checks above describe
the code as it is now.

No gap found by the review is still open. What irsync leaves to you is
listed under "What is left" in each section above, and in one place under
[What irsync does not defend against](#what-irsync-does-not-defend-against).

To report a security problem, see the
[security policy](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md).
Please do not open a public issue for one.
