# irsync-stress: implementation decisions

This file records the rulings made while implementing the `irsync-stress`
crate from `docs/superpowers/specs/2026-10-21-stress-harness-design.md` and
`docs/superpowers/plans/2026-10-21-stress-harness.md`, the irsync bugs the
harness has found, and the outcome of its acceptance run. Design-level
decisions live in the spec; this file covers the calls made during execution
that the spec and plan did not settle, or where the shipped code differs from
the spec. Each entry names what was decided, why, and what it costs if the
ruling turns out to be wrong. The spec itself was not amended; where it and
the code disagree, the code is what shipped and the entry says so.

## The spec's "left to the builder" items

1. **Timeouts and bounds.** The daemon has 15 s to print its startup lines.
   Stopping it sends SIGINT and waits up to 10 s, then SIGKILL. Joining a
   log-copier thread, and draining the daemon's stdout after it has exited,
   are each bounded at 1 s. `DaemonLog::tail`, used in error messages, reads
   the last 64 KiB of the log. Why: every wait must end, so that a wedged
   child cannot hang the harness (or CI), and a 64 KiB tail bounds memory on
   a long run. Cost if wrong: a slow machine could see a spurious "daemon did
   not start" (raise the 15 s), or a daemon that needs longer than 10 s to
   shut down is killed rather than stopped cleanly.

2. **Sentinel file.** The first sync carries `.irsync-stress-ping` in the
   source tree, with content `seed=<seed> started=<timestamp>\n`. Why: the
   sentinel round proves the daemon, keys and module work before any filegen
   output is involved, so a setup fault is not confused with a transfer
   fault; the content makes the file unique per run. The file stays in the
   tree and filegen may change or delete it. Cost if wrong: a name clash
   with a generated file is possible in principle; the dot-prefix makes it
   unlikely.

3. **The daemon pid is in `run.toml`** (`[daemon] pid`), next to its endpoint
   id and address. Why: the SIGINT end-to-end test needs it to check that
   the daemon is gone after the harness is signalled, and it is useful to
   anyone looking at a leftover process. Cost if wrong: none beyond an extra
   field; a stale pid is only informative.

4. **Signal handling.** A SIGINT/SIGTERM handler sends SIGINT to the daemon
   pid and calls `_exit(130)`. Both signals are blocked on the harness's
   main thread from before the daemon is spawned until its pid is stored, so
   a signal arriving during startup is delivered afterwards, with the pid
   set. Because std does not reset a child's signal mask, `process::spawn`
   unblocks both signals in every child with `pre_exec`; without it the
   daemon would inherit the block and never see the SIGINT sent to it.
   Limit: a client, filegen or fileset child that is running when the
   harness is signalled is not signalled by the harness; a terminal Ctrl-C
   still reaches it through the process group, a `kill` of the harness alone
   does not. Why: the handler can do only async-signal-safe work, so it
   signals one pid held in an atomic; tracking a changing set of short-lived
   children from a handler is not worth the risk. Cost if wrong: a stray
   client or filegen process can outlive a harness killed from outside the
   terminal, until it finishes by itself.

5. **`process::spawn` retries on ETXTBSY** (10 ms apart, up to 100
   attempts), and every child goes through it. Why: parallel unit tests
   write fake scripts and execute them, and on Linux a fork in another
   thread can briefly hold the script's write descriptor, so exec fails with
   ETXTBSY; this hit about one run in eight. Cost if wrong: a genuinely stuck
   ETXTBSY delays a failure by up to one second.

6. **The CI end-to-end test deviates from the spec's invocation.** It passes
   `--filegen-set names.max_len=200`, which the spec's CI invocation did not
   have, so the known NAME_MAX bug (below) cannot make it flaky; it uses a
   fixed seed; and it bounds each harness run to 300 s, with a guard that
   stops a stray harness on failure. Why: a test that fails because of a
   known, unfixed bug in the product is noise, and a hung child must not
   hang CI. Cost if wrong: the test does not exercise long names; once the
   bug is fixed the `--filegen-set` can be dropped.

7. **`--max-file-size` and `--filegen-set` exist.** The design review added
   them. Why: filegen refuses a total cap below its 256M per-file default,
   so a small `--max-total-size` is unusable without `--max-file-size`;
   `--filegen-set` is also the way to apply the NAME_MAX workaround. Cost if
   wrong: two more flags to maintain.

8. **Exit classification.** Client exit 23 is a failed iteration (harness
   exit 1); any other non-zero client exit is a harness error (exit 2). After
   exit 23 the manifest comparison and `fileset check` still run, because the
   check report is the main diagnostic. Currently, if the destination
   manifest is missing after an exit 23, that is exit 2 (in `verify.rs`)
   rather than a failed iteration. Why: 23 means the transfer was partial, a
   finding about irsync; other codes mean the invocation itself is wrong.
   Cost if wrong: the missing-manifest case is reported as a harness error
   when it should be a failed iteration; noted as a follow-up.

9. **Tool lookup.** Each tool is looked up next to the harness binary first,
   then on `PATH`; `--irsync`, `--filegen` and `--fileset` override. Why: a
   `cargo build --workspace` puts all binaries in one directory, so the
   freshly built tools are used rather than whatever is installed. Cost if
   wrong: an installed older tool on `PATH` is used when the sibling is
   missing; `run.toml` records the paths and versions so this is visible.
   `cargo test -p stress` alone does not build the other binaries, so
   `cargo build --workspace` is needed first.

10. **Log content.** The client's stdout is appended to the iteration log
    along with its stderr. filegen's stdout is discarded and its stderr is
    inherited (progress and errors reach the terminal). The daemon and
    client logs contain ANSI colour codes, because irsync's tracing output
    is coloured. Why: nothing the client prints should be lost, and filegen
    has its own log under `filegen/`. Cost if wrong: logs need `less -R` or
    an escape-stripping filter to grep cleanly.

## Known irsync bugs found by the harness

### NAME_MAX: long file names cannot be transferred

filegen's `long` name class draws file names of 200 to 255 bytes. irsync's
receiver stages each incoming file as `.irsync.<name>.part` in the same
directory, so a name longer than about 242 bytes makes the staging name
exceed NAME_MAX and fails with `File name too long (os error 36)`; the
client exits 23. The bug was found while checking the design by running the
loop by hand, and confirmed by the first real runs.

What the first real runs showed: in a default-size push run, iteration 1
passed and every iteration from 2 to 15 exited 23 with `check FAILED
(added=0 deleted=1..3 changed=0)`. The client log shows `File name too long
(os error 36)`; the check report lists the 200+ byte names as `DEL`. Once
such a file exists in the source it never transfers, so the failure compounds
across all later iterations rather than appearing sporadically.

Decision: the bug is not fixed in this project. The harness reports it as a
failed iteration like any other. A run that wants to get past it uses
`--filegen-set names.max_len=200`. The fix (a partial-file name that fits
whatever the entry name is) is a follow-up with its own test.

## Acceptance run

_To be filled in: the commands, the pass/fail counts for the push and pull
runs, the explanation of each failure class, and any new irsync bug as its
own entry above._
