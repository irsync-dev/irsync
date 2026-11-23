# Windows, step 2: irsync on Windows

## Intent

irsync is to support Windows and NTFS. The maintainer's goals for the whole
effort:

- **Windows to Windows:** a copy as detailed as the current user's
  permissions allow.
- **Unix to Windows and back:** usable, carrying as much as maps sensibly.
  The copy is lossy, and the book says what is lost.

The effort has five sub-projects. Step 1, `filegen` and `fileset` on
Windows, is done (`2026-11-22-windows-tooling-design.md`). This is step 2:

1. Tooling: done.
2. **Port (this spec):** `irsync` builds and passes its tests on Windows, as
   client and daemon, carrying the metadata the protocol has today.
3. Cross-platform mapping: what a mixed sync does beyond the simple rules
   set here, and how `fileset` checks across families.
4. NTFS fidelity: security descriptors, attributes, creation time,
   alternate data streams and junctions, Windows to Windows.
5. Operations: Windows service, onboarding over ssh, release files.

`irsync-stress` and `netstress` drive `irsync`. They are ported after this
step, each with a plan of its own.

Today every dependency of `irsync` builds on Windows. `irsync` itself does
not: 35 of its source files name `nix`, `libc`, `std::os::unix` or
`std::os::fd`, about 280 times, and 75 of its 78 test files use something
Unix-only.

## Goals

- `irsync` builds and passes its tests on `x86_64-pc-windows-msvc` and
  `aarch64-pc-windows-msvc`, on NTFS, on Windows 10 version 1809 or later,
  Windows 11 and the matching Server versions.
- Client and daemon both work on Windows: files, directories, content,
  mtime, hard links, symlinks where the user may create them, `--delete`,
  resume, the index, config and identity.
- **Containment is as strong as on Unix.** Nothing below a root is reached
  through a link of any kind, and swapping a directory for a link while a
  sync runs cannot redirect a write, a delete or a read.
- A session between a Unix side and a Windows side works, with the simple
  rules under "Mixed sessions".
- Behaviour on Linux, macOS and FreeBSD does not change, and neither does
  the wire format an existing peer sees.

## Non-goals

- NTFS-specific metadata: security descriptors, the read-only and other
  attributes, creation time, alternate data streams. Step 4.
- Doing better than the simple rules in a mixed session: mapping modes and
  owners, resolving case clashes, special files. Step 3.
- ssh onboarding on or from Windows, a Windows service, reloading a running
  daemon's config, release files and installers. Step 5.
- `stress` and `netstress` on Windows.
- ReFS, FAT and exFAT, network shares, and the folders a virtual machine
  shares with its host.
- The GNU toolchain and 32-bit Windows.

## Anti-goals

- **No code shared with `filegen` or `fileset`.** They check irsync with an
  independent reading of the filesystem.
- **No following of links.** Below a root, nothing on Windows opens, walks,
  writes or deletes through a reparse point.
- **No guessing at names.** Bytes that are not a valid name on this
  platform are an error for that entry, never repaired.
- **No owner taken from a side that has none.** See "Owners".

## Constraints

- **`unsafe`** only inside `crates/irsync/src/platform/`, each call wrapped
  in one small function with a safe signature and a `// SAFETY:` comment
  that is true of that signature. The crate carries `#![deny(unsafe_code)]`
  with the platform module as the one exception.
- **Miri cannot check the native calls.** They are covered by review and by
  tests on real NTFS. The tests must attack them.
- **Supply chain:** no new dependency is expected. `windows-sys` gains
  features; tokio's Windows signal support is already in the tree. If one
  is needed, the rules of step 1 apply: an entry in
  `docs/SUPPLY_CHAIN_DECISIONS.md`, a week old, no second version of a
  crate.
- **The checks live in `tools/ci.sh`,** and a change to it is tested.
- **The wire format** changes only by one optional field (see "Each side
  says what it is"). `PROTOCOL_VERSION` stays 5.
- **Minimum Windows:** Windows 10 version 1809, for replacing and deleting
  files that are open elsewhere and for ignoring the read-only attribute.

## Approaches

**Chosen: an open-directory type in a platform module, with handle-relative
native calls on Windows.** The receiver, the deleter, the scanner and the
daemon are written today against open directories and the `openat` family.
The platform module gives them one type that means the same on both
systems. On Windows it is built on `NtCreateFile` relative to a directory
handle, the exact counterpart of `openat`, and on rename, delete and link by
handle. A directory handle keeps pointing at its directory whatever happens
to the name, so the safety reasoning of the Unix code holds unchanged. The
Rust standard library uses the same calls inside `remove_dir_all` on
Windows, to close the same kind of race.

**Not chosen: path-based access to directories held open so they cannot be
renamed.** It needs almost no `unsafe`, but its guarantee rests on reading
Windows' sharing rules correctly, and it stops other programs from renaming
directories while a sync runs.

**Not chosen: the `cap-std` crate.** It offers this kind of directory on
both platforms, but it would put a large dependency tree on the
security-critical path and take the guarantee out of our hands.

**Not chosen: platform crates** (`irsync-unix`, `irsync-windows`). A crate
boundary would let the compiler enforce the rule, but the filesystem layer
is built on irsync's own types, which would have to move, and two crates
have no single definition of their interface. A module with a rule test did
the job in step 1.

## Design

### Platform module

```
crates/irsync/src/platform/
    mod.rs       the interface: types and the cfg switch
    unix.rs      today's nix, libc and std::os::unix code, moved
    windows.rs   std::os::windows, windows-sys, the native calls
```

- Outside `platform/`, no file names `nix`, `libc`, `std::os::unix`,
  `std::os::windows`, `std::os::fd` or a Windows crate. A test enforces
  this, for `src/` and `tests/` (the shared test helpers excepted).
- Dependencies are target-specific: `nix` under `cfg(unix)`, the Windows
  crates under `cfg(windows)`.

### The open directory

One type, `Dir`: a directory held open. Everything done below a root goes
through it, by a single name.

| Group | Operations on a name inside a `Dir` |
| --- | --- |
| Descend | open a subdirectory, create one |
| Files | open, create new, open a regular file and return its stat |
| Look | stat without following, read a link, list the names |
| Change | remove a file, remove an empty directory, rename into another `Dir`, hard link from another `Dir` |
| Create | symlink; on Unix only: FIFO, device |
| Metadata | set mtime; on Unix only: mode, owner |

- No operation takes a path of more than one component, and none follows a
  link.
- Walking a `RelPath` below a root stays in `fs/safe.rs`, written once for
  both platforms on top of `Dir`.
- The platform-only operations exist on both platforms. On Windows they
  return *not supported*.

**On Windows** a `Dir` is a handle from a relative native open with "do not
follow reparse points" set, checked on the handle to be a directory and not
a reparse point. Rename, delete and hard link act on handles. Listing reads
from the handle. A symlink is made and read through its reparse data.

### Errors by meaning

The platform module turns operating-system errors into a small set that
callers match on:

*link refused*, *not there*, *already there*, *not a directory*, *is a
directory*, *not empty*, *cross-device*, *out of handles*, *in use*, *not
supported*.

The roughly 40 places that match Unix error numbers today match these.
*In use* is new: a Windows sharing violation.

### Names and paths

- A path stays `RelPath`: bytes with `/` between components, no `.`, `..`
  or NUL. Sorting stays bytewise, so both ends walk in the same order.
- On Unix the bytes are the raw name. On Windows they are WTF-8: plain
  UTF-8 for a well-formed name, and a lossless three-byte form for an
  unpaired surrogate. irsync has its own copy of the conversion, safe and
  platform-neutral, tested on every platform and under Miri.
- **A name Windows cannot hold** fails that entry: bytes that are not valid
  WTF-8, or any of `< > : " | ? * \` or a character below 32. The entry is
  counted as failed, the message names it, and the session goes on.
- **Names that are legal but awkward** (a trailing dot or space, `CON`,
  `NUL.txt`) are created as they are. Handle-relative native calls do not
  apply the Win32 name rules.
- **Case.** NTFS directories are case-insensitive by default. On Windows the
  receiver refuses an entry whose name matches an existing sibling by case
  only and not exactly. That entry fails; nothing is overwritten.
- **Roots.** A root (destination, source or export) is opened by name and
  may itself be a link, as on Unix. It is resolved to the extended form
  (`\\?\`) once. A root that is not on a local NTFS volume is refused at
  start with a message that says so.

### Each side says what it is

`Hello` and `HelloAck` gain one optional field, the platform family: `unix`
or `windows`. A peer that does not send it is Unix, which is true of every
irsync that exists. All adaptation happens on the Windows side, so an older
irsync on Unix works with a Windows peer unchanged, with the one exception
under "Owners".

### Mixed sessions

What the Windows side does with each thing the protocol carries. The same
rules hold Windows to Windows.

| Carried | Windows receiving | Windows sending |
| --- | --- | --- |
| Content, size | as on Unix | as on Unix |
| mtime (`-t`) | kept, in steps of 100 ns, and compared at that grain | as stored |
| Mode (`-p`) | ignored | 0644 for files, 0755 for directories |
| Owner, group (`-o`, `-g`) | ignored | see "Owners" |
| Symlinks (`-l`) | created if the user may; each fails by itself if not | sent, with the target |
| Hard links (`-H`) | kept, up to 1023 names per file | detected by file index and volume |
| FIFOs, devices (`-D`) | skipped; one warning gives the count | none exist |
| Sparse (`-S`) | ignored; the file is written in full | not reported |

- **Comparing mtime at 100 ns** means a file is not sent again because NTFS
  dropped the last two digits.
- **Skipped special files** do not make the session fail.

**Owners.** A Unix receiver running as root applies the owner it is sent.
Windows has none to send, and every file would become root's. So owners and
groups are never transferred in a mixed session:

- a current peer knows from the platform field and leaves them alone;
- an older Unix client that asks a Windows daemon for `-o` or `-g` is
  refused, with a message that says to drop them or to update.

**Symlinks across families.**

- A relative target has `/` turned into `\` on the way in, and back on the
  way out. An absolute target is kept as text.
- Windows must know whether a link points at a file or a directory, and
  the protocol does not say. A link gets the kind its target has when it
  is created, and a file link if there is no target. Links whose target
  did not exist yet are looked at once more at the end of the session and
  recreated as directory links where the target is one.

### Reparse points in a source tree

- A symlink is sent as a symlink, when `-l` asks for links.
- Any other reparse point (a junction, a mounted folder, a cloud
  placeholder) is not followed and not sent. It is skipped, and one
  warning gives the count, as for special files.
- This means a folder synced by OneDrive with files kept online cannot be
  copied by irsync in this step. The book says so.

### Change detection

The index and the test for "has this file changed" use size, mtime, change
time and identity. On Windows these are the size, the mtime, the NTFS
change time, and the file index with the volume serial number. Mode and
owner are constant there.

### Files in flight

- **The part file** (`.irsync.<name>.part`) is created in the destination's
  `Dir`, written by offset, and locked with the same standard-library call
  as on Unix. The lock is still how a second writer and stale part files
  are found.
- **Placing it** is a rename by handle that replaces the destination in one
  step, with the options that allow replacing a file another program has
  open and that ignore the read-only attribute. Deletes use the matching
  options.
- **A destination held so that it cannot be replaced** fails by itself with
  a message that says the file is in use.
- **Short retries.** Antivirus and the indexer open files the moment they
  appear. An operation that fails as *in use* is retried for up to about a
  second before it counts as a failure, beside the retries that exist for
  running out of handles.
- **Source files** are opened so that other programs can still read, write
  and delete them.
- **Seeding a part file from a reflink** keeps its fallback: on Windows the
  old file is copied.

### Process and control

- **Stopping.** Ctrl-C and Ctrl-Break stop the client and the daemon
  cleanly, through tokio's Windows support.
- **No reload.** Unix reloads the daemon's config on SIGHUP. On Windows the
  daemon is restarted to pick up a change.
- **The daemon lock** and the address file work as on Unix.

| Unix today | Windows in this step |
| --- | --- |
| Refuses a config, key or passphrase file that others can read | Not checked. The files are created inside the user's profile. |
| Daemon refuses to run as root without `allow_root` | Not checked. |
| Raises the open-file limit and sizes its window from it | A fixed window. |
| CPU time and peak memory in telemetry | Reported where `sysinfo` provides them, otherwise left out. |

The first two rows are known gaps, written down in the book.

### Features on Windows

- **Refused with a clear message:** ssh onboarding (`ssh-setup` and the
  client side that drives it) and the legacy service-file handling.
- **`--paranoid`** works as on any system without reflinks: the file itself
  is hashed and checked again.

### Where files live on Windows

| File | Location |
| --- | --- |
| Config | `%APPDATA%\irsync\irsync.toml` |
| State (index, locks) | `%LOCALAPPDATA%\irsync` |
| Key | `%USERPROFILE%\.irsync\id_ed25519` |

- irsync on Windows neither looks in `.ssh` nor writes there by default:
  its keys usually have no passphrase and do not belong beside the user's
  ssh keys.
- `identity` in the config and `--identity` still name another file.
- Unix locations do not change.

## Testing

**Existing tests** are sorted as in step 1:

| Group | Treatment |
| --- | --- |
| Platform-neutral behaviour that happens to use a Unix call | rewritten on the platform module's test helpers, so they run everywhere |
| POSIX-only behaviour: modes, owners, FIFOs, devices, root checks | Unix only |
| The rsync comparison, ssh onboarding, fake-ssh and `sh` tests | Unix only |
| Tests that send SIGHUP or SIGTERM to a daemon | Unix only; Windows gets a stop test of its own |

No test is deleted, and on Unix every test asserts what it asserts today.

**New tests**

- **Containment under attack, on both platforms.** While a sync, a delete
  or a part-file cleanup runs, a directory on the path is swapped for a
  symlink, and on Windows for a junction, which needs no privilege.
  Nothing outside the root may be written, removed or read.
- **`Dir`:** each operation on both platforms, including that none follows
  a link, and that each error has its meaning.
- **Windows names:** paths over 260 characters; a trailing dot and space;
  `CON`; an unpaired surrogate; a refused name; a case-only clash.
- **Files in use:** a destination held open with and without permission to
  replace; a read-only destination; a source held open by a writer.
- **Mixed sessions,** in one process with one side told to act as the
  other family: owners not transferred; `-o` from an older client refused;
  symlink targets translated; the second look at link kinds; special files
  skipped; an mtime at 100 ns not causing a resend.
- **Symlink privilege:** without it the symlink tests skip and say so, and
  with `CI` set a missing privilege is a failure.
- **Platform rule** and the name conversion.

**A real mixed run,** between a Mac and a Windows machine, is done by hand
before the last pull request. CI cannot do it.

**Unix regression:** the full `tools/ci.sh check` on Linux and macOS in CI,
and the FreeBSD workflow before the last pull request merges.

## CI

- `tools/ci.sh check` on Windows covers the workspace except `stress` and
  `netstress`.
- The `windows` job stays outside the required jobs until this step is
  merged. Whether it then joins them is the maintainer's decision.
- Miri gains irsync's name conversion.

## Documentation

- **Limitations** drops "Linux and macOS only" and gains a Windows section:
  what is and is not carried, the mixed-session rules, the minimum Windows
  version, the two unchecked gaps, that a daemon is restarted to reload.
- **Files and state:** the Windows locations.
- **Configuration, command line:** what is refused on Windows.
- **Contributor pages:** the platform module, `Dir`, the attack tests.
- **Decision records** for containment, the platform field, the owner
  rule, the key location.

## Pull requests

1. The platform module on Unix: `Dir`, errors by meaning, everything
   moved. No behaviour change. The attack tests arrive here, on Unix.
2. The Windows platform file and the name conversion. `irsync` builds on
   Windows.
3. Receiver, deleter and scanner on Windows: names, case, files in use,
   the Windows tests.
4. The platform field and the mixed-session rules.
5. Process handling on Windows, file locations, refused features.
6. CI scope, the book, decision records.

Pull request 1 is a pure move and is checked by the existing tests on
Linux and macOS.

## Left to the builder

- The exact functions and types of the platform interface, within the
  table above.
- Which native calls implement each `Dir` operation, and how many
  `unsafe` functions that takes.
- How a root is checked to be on a local NTFS volume.
- The size of the fixed window on Windows.
- Whether the capture tap is ported or refused on Windows.
- How the retry for *in use* is bounded and where it sits.
- Whether the daemon's stop on Windows also covers the console being
  closed.
- How time zones and the host name are read on Windows.

## Needs the maintainer

- **A real mixed run** between a Mac and this machine, before the last
  pull request.
- **Whether the `windows` job becomes required** once this step is merged.
- **The FreeBSD workflow** before the last pull request merges.

## Later

- Step 3 replaces the simple mixed-session rules: modes, owners, case
  clashes, special files, and a link's kind carried in the protocol.
- Step 4 adds NTFS metadata, and the checks of who can read a key or
  config file.
- Step 5 adds the service, a control channel with reload, elevation
  handling, onboarding and release files.
- Cloud placeholders and other reparse points that carry data are treated
  as links here and never followed. What to do with them is a question for
  step 4.
