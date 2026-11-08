# Sessions and resume

A sync in `irsync` is a session, not a connection. The session is the job: the
plan of which files to send and how far each one got. A connection is only the
current way of moving it forward. When the connection drops, the session
waits; when the process dies, the session stays on disk; the next connection,
or the next run of the same command, continues it. This page explains what a
session holds, how it is found again, and what a resumed session skips.

## What a session is

A session has a random 128-bit id, which the daemon assigns in its `HelloAck`,
a plan and a record of progress. Both sides keep the plan and the progress in
the index of the root they work on, under `state_dir` (see
[Files and state](../reference/files-and-state.md#state-directory)), and write
them at most every 2 seconds and once more at the end:

- the plan: each file the receiver decided to request, with its action, size,
  hash and listing entry, and once the listing is over, a flag that the plan is
  complete, with the directories and the errors found on the way;
- the progress: the files acknowledged as done, the bytes transferred over all
  connections, and the deletions so far, which count against `--max-delete`.

Separately from the session, the receiver records for each partly received
file which byte ranges of its `.part` file it has verified. That record is
written after every 64 newly verified chunks or every 2 seconds, and belongs to
the file, not to the session. The exact messages are in
[Protocol](../reference/protocol.md#persisted-state).

A finished session's record is deleted.

## When the connection drops

The client dials again and sends `Hello` with the same session id. Both sides
load what they stored and carry on. A file acknowledged as done is not touched
again. A file that was partly received continues from its verified chunks: the
receiver reads each recorded chunk of the `.part` back, checks it against its
hash, and requests only the rest. Chunks that were written but not yet
recorded are requested again.

The client redials with exponential backoff: 1 second, doubling up to 60
seconds, plus up to a quarter of random jitter, so that many clients do not
redial in step. It keeps going for `--reconnect`, 24 hours by default, measured
from the last time the session made progress. A connection that the daemon
accepts but that moves no data and places no file does not count as progress,
so a daemon that accepts and drops every connection cannot keep a client
retrying forever. `--reconnect 0` gives up at once, as rsync does.

Some answers end the session instead of a retry: an `Error` from the daemon
(the key may not use the path, the export is read-only, another protocol
version), and close codes 1 to 3 (see
[Protocol](../reference/protocol.md#connection)). A daemon that no longer knows
the session answers `BAD_SESSION`, and the client starts a fresh session at
once.

## When the process dies

The client stores the session under a key derived from what the command means,
not how it was typed: the daemon's `EndpointId` (so an alias and the raw id
are the same), both locations (a local one by its canonical path, a remote one
as typed, each with its trailing-slash rule),
the preserve flags, `-c`, `-u`, `--inplace`, `--append` or `--append-verify`,
the deletion mode with `--delete-excluded`, `-n`, and the filter rules in order.
Flags that do not change the result, such as `--parallel`, `--bwlimit` or
`-v`, are not part of the key.

The next run of the same command computes the same key, finds the stored
session and prints `resuming session <id> from <time>`, with the first eight
hex digits of the id and the time of the last write. A command that differs in
any part of the key starts its own session. There is one stored session per
key: a new one replaces an old one.

`--no-resume` ignores the stored session and plans afresh. Partly received
files are still picked up, since their records belong to the files: a `.part`
whose record names the same expected size and hash continues from its verified
chunks.

## What the daemon checks

The daemon keys its copy of the session by the client's `EndpointId`, the
export and the path inside it as resolved, the direction, the trailing-slash
rule, and the receiver-side options. A resume must match all of them.

Only the key that started a session may resume it. A `Hello` with the id of a
session that another key started, that is finished, or that the daemon has
collected, gets `BAD_SESSION`, with no hint which of these it was.

When a second connection resumes a session that an older connection still
holds, as when the same command runs twice at once, the daemon closes the older
one as superseded, waits for it to let go, and continues with the newer. The
older client stops with an error rather than taking the session back.

## What a resumed session skips

On a resume with a complete plan, neither side lists again: the receiver
recreates the planned directories where they are missing and requests the
files not yet done, and nothing else. This is what makes a
resume after an interruption cheap for a large tree. Two cases list afresh
even then: a run with deletions, because the stored plan holds no deletion
candidates, and a dry run, which stores nothing worth resuming. They still
skip the files the progress marks as done.

A resume with an incomplete plan lists both sides again and plans the files
that are not done yet, using the index to skip unchanged ones.

## How long a session waits

A session that nobody resumes is dormant. Garbage collection removes dormant
sessions and their partial files after `dormant_session_timeout`, 7 days by
default, on the daemon, and after 7 days on the client. A partial file that a
stored session still plans to write is kept. When collection runs is in
[Clean up state](../how-to/clean-up.md#what-runs-on-its-own).

## Timeouts

Three timers decide when a connection is given up on, and each leads to a
redial, not to the end of the session:

- **QUIC's idle timeout**, 60 seconds, with keep-alives every 5 seconds: the
  path is gone. See
  [Connections](connections.md#keeping-a-connection-alive).
- **The daemon's `idle_timeout`**, 30 minutes by default: nothing has happened
  on the session for that long.
- **The client's `--timeout SECS`**, off by default: no control message, no
  data and no local progress for `SECS` seconds. It also bounds the wait for
  the daemon's reply to `Hello`.

Local progress (hashing, writing, finalizing, deleting) counts as activity, so
that a side busy with a large file does not look dead. The busy side tells the
other with a `Heartbeat` every 500 ms in which it made progress; an idle side
sends none. Since heartbeats come every half second, a `--timeout` below a few
seconds is unreliable.
