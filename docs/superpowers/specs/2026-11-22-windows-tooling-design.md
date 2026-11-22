# Windows, step 1: filegen and fileset on Windows

## Intent

irsync is to support Windows and NTFS. The maintainer's goals for the whole
effort:

- **Windows to Windows:** a copy as detailed as the current user's
  permissions allow.
- **Unix to Windows and back:** usable, carrying as much as maps sensibly.
  The copy is lossy, and the book says what is lost.

The effort is split into five sub-projects, each with its own spec, plan
and pull requests, in this order:

1. **Tooling (this spec):** `filegen` and `fileset` run on Windows. The
   tests of every later step need them.
2. **Port:** `irsync` builds and passes its tests on Windows, as client and
   daemon, carrying the metadata the protocol has today.
3. **Cross-platform mapping:** what a Unix to Windows sync does with mode
   bits, names, symlinks and special files, and how `fileset` checks a tree
   against a manifest from the other platform family.
4. **NTFS fidelity:** security descriptors, attributes, creation time,
   alternate data streams and junctions, Windows to Windows.
5. **Operations:** Windows service, onboarding over ssh, release files.

`irsync-stress` and `netstress` drive an irsync that does not run on
Windows yet. They are ported after step 2, not here.

Today nothing builds on Windows. `filegen` and `fileset` call `nix`, `libc`
and `std::os::unix` directly in 19 source files, with almost no `cfg`
gates, and both store file names as raw bytes.

## Goals

- `filegen`, `fileset` and `irsync-glob` build and pass their tests on
  `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`, on NTFS, on
  Windows 10, Windows 11 and the matching Server versions.
- Behaviour on Linux, macOS and FreeBSD does not change.
- On Windows the tools cover what the two worlds share: files,
  directories, content, sizes, mtime, hard links, sparse files, and
  symlinks where the user may create them.
- The manifest, state and log formats record the platform now, so that
  steps 3 and 4 add to them without a format break.
- Every guarantee `fileset` gives about a manifest that came with someone
  else's tree holds on Windows too.
- CI builds and tests the three crates on both Windows architectures.

## Non-goals

- `irsync`, `irsync-proto`, `stress` and `netstress` on Windows.
- NTFS-specific metadata in either tool: security descriptors, the
  read-only and other attributes, creation time, alternate data streams.
  Junctions and other reparse points are recognised and never followed,
  but not generated and not recorded beyond their kind. Step 4.
- Checking a tree against a manifest from the other platform family.
  Step 3; see "Cross-platform check" for what this step prepares.
- ReFS, FAT and exFAT, network shares, and the folders a virtual machine
  shares with its host.
- The GNU toolchain (`*-pc-windows-gnu`) and 32-bit Windows.
- A polite stop of `filegen` from another process on Windows. The stress
  harness needs one; its spec decides how.

## Anti-goals

- **No code shared between `filegen`, `fileset` and `irsync`.** `fileset`
  checks irsync with code that shares nothing with it, and `filegen`
  shares no code with either. A shared platform crate would let one bug
  hide from the tool meant to find it.
- **No guessing at names.** Bytes that are not a valid stored name are an
  error, never repaired or replaced.
- **No following of links.** Nothing on Windows opens, walks or hashes
  through a reparse point of any kind.

## Constraints

- **`unsafe`:** use a wrapper crate where one does the job. Where none
  does, call Win32 through `windows-sys` inside `platform/windows.rs`, with
  the `// SAFETY:` comment clippy requires
  (`undocumented_unsafe_blocks`). This step has two such places: hole
  punching and the change time.
- **Miri cannot check those two.** It cannot make system calls and the
  nightly job runs on Linux. They are covered by their tests only.
- **Supply chain:** each new dependency gets an entry in
  `docs/SUPPLY_CHAIN_DECISIONS.md` and is vetted by `cargo deny`. A
  release less than a week old is not taken; pin behind it.
- **The checks live in `tools/ci.sh`,** not in the workflow, and a change
  to that script is tested in `tools/tests/test_ci_script.py`.
- **A new workflow job** follows the workflow policy tests, actionlint and
  zizmor.
- **Path length:** the MSVC linker fails on a path over 260 characters.
  Keep the checkout and the build directory short in CI.

## Approaches

**Chosen: a platform module in each tool.** All platform code sits behind
one small interface per crate. The roughly 70 lines that touch a Unix API
in `filegen` and 30 in `fileset` shrink to one file per platform, and that
interface is where step 4 adds the NTFS operations. The cost is a
refactor of working Unix code, guarded by the existing tests.

**Not chosen: `cfg` gates at each call site.** A smaller diff, but
platform logic stays scattered through operation code, and every later
addition repeats the scatter.

**Ruled out: one platform crate for all three programs.** See the first
anti-goal.

## Design

### Platform module

Each tool gets its own:

```
crates/<tool>/src/platform/
    mod.rs       the interface, and the cfg switch
    unix.rs      today's nix, libc and std::os::unix code, moved here
    windows.rs   std::os::windows and the Windows crates
```

- Outside `platform/`, no file names `nix`, `libc`, `xattr`,
  `std::os::unix`, `std::os::windows`, `std::os::fd` or a Windows crate.
  A test enforces this.
- The interface is plain functions. One implementation exists per build,
  so there is nothing to dispatch on.
- Dependencies are target-specific in each `Cargo.toml`: `nix`, `libc`
  and `xattr` under `cfg(unix)`, the Windows crates under `cfg(windows)`.

| Function group | `filegen` | `fileset` |
| --- | --- | --- |
| Name to and from stored bytes | yes | yes |
| Read an entry's metadata: kind, size, mtime, change time, link identity, sparse | yes | yes |
| Is this a link of any kind | yes | yes |
| Open a regular file without following a link | no | yes |
| Set mtime without following a link | yes | no |
| Create a symlink, a hard link | yes | no |
| Punch a hole | yes | no |
| POSIX-only: chmod, chown, mkfifo, mknod, xattrs | Unix only | xattr read, Unix only |
| Which capabilities this machine has | yes | no |
| Interrupt handling | yes | no |
| Thread CPU time | no | yes |

### Windows calls

| Need | How | `unsafe` of ours |
| --- | --- | --- |
| Metadata, attributes, mtime, symlink and hard link creation, setting times, opening without following | standard library | no |
| File identity and link count | `winapi-util` (already in `Cargo.lock`) | no |
| Ctrl-C | `ctrlc` (new) | no |
| Thread CPU time | `cpu-time` (new) | no |
| Mark a file sparse and zero a range | `windows-sys`: `DeviceIoControl` with `FSCTL_SET_SPARSE` and `FSCTL_SET_ZERO_DATA` | yes |
| Change time | `windows-sys`: `GetFileInformationByHandleEx` | yes |

### Names

- **Outside `platform/` a name is bytes,** as today. On Unix they are the
  raw name. On Windows they are WTF-8: plain UTF-8 for a well-formed name,
  and a lossless three-byte form for an unpaired surrogate, which NTFS
  allows. A well-formed name therefore has the same bytes in a Windows
  manifest as in a Unix one.
- **The conversion is platform-neutral, safe code** in each tool, working
  on 16-bit units: units to bytes, and bytes to units. It is compiled and
  tested on every platform and runs under Miri. Only the last step, between
  units and an `OsString`, is in `platform/windows.rs`.
- **Bytes that are not valid WTF-8 are an error.** This includes a
  surrogate pair written as two three-byte forms, so every name has
  exactly one stored form.
- `OsStr::as_encoded_bytes` is not used: its encoding is documented as
  unspecified, and the way back is `unsafe`.
- **Stored paths use `/`** on both platforms. `platform/windows.rs`
  translates at the filesystem boundary.
- **Every path is opened in the extended form** (`\\?\`), so the
  260-character limit does not apply and names the shell refuses work.
- **A symlink's target is content, not a path:** it is stored exactly as
  it is on disk, in WTF-8.

### filegen on Windows

| Group | Operations | On Windows |
| --- | --- | --- |
| Content | `create_file`, `append`, `truncate`, `overwrite_range`, `rewrite` | yes |
| Sparse | `punch_hole` | yes |
| Tree | `mkdir`, `rmdir`, `remove_tree`, `delete_file`, `rename` | yes |
| Replace | `rename_over` | files only: Windows cannot rename over a directory |
| Links | `hardlink` | yes, up to 1023 links per file |
| Symlinks | `symlink`, `repoint_symlink`, `delete_symlink` | if the capability is there |
| Time | `set_mtime` | yes, in steps of 100 ns |
| POSIX-only | `chmod`, `chown`, `mkfifo`, `mknod`, `set_xattr` | disabled |

- **Capabilities** are found once, at the start of a run: `symlinks` by
  creating one in the target directory, `sparse` by marking a file there.
  Creating a symlink needs Developer Mode, an elevated process or the
  symlink privilege.
- **A disabled operation** gets weight zero and one notice at startup, so
  one YAML config runs on both platforms.
- **Reproducibility is per platform family and capability set.** The same
  seed gives the same run on any Windows machine with the same
  capabilities, and a different run than on Unix.
- **Symlinks:** Windows has file symlinks and directory symlinks.
  `filegen` creates the kind matching the target, and a file symlink for a
  dangling target. Targets are written with `\`.
- **Names:**
  - never the characters NTFS forbids (`< > : " | ? * \` and code points
    below 32);
  - lengths counted in 16-bit units, 255 per name and 32,767 per path;
  - no two names in a directory that differ only by case or
    normalization, which the model already ensures;
  - at a low rate set in the config, on by default: unpaired surrogates,
    a trailing dot or space, and reserved device names (`CON`, `NUL.txt`).
    These are legal on NTFS and irsync will have to carry them. They are
    the counterpart of the non-UTF-8 names made on Unix.
- **Interrupts:** the first Ctrl-C, Ctrl-Break or other console event
  lets the step in flight finish, saves the state and exits with 130. A
  second one exits at once. `ctrlc` does not say which event arrived, so
  the log names all of them `Ctrl-C` and the status is 130 for each. On
  Unix nothing changes.

### fileset on Windows

| Manifest field | Value on Windows |
| --- | --- |
| `kind` | `dir`, `file`, `symlink`; any other reparse point is `other` |
| `size`, `mtime`, `blake3`, `target` | as on Unix |
| `ino`, `dev`, `nlink` | file index, volume serial number, link count |
| `ctime` | the NTFS change time |
| `mode`, `uid`, `gid`, `rdev`, xattrs | zero or empty, never compared |

- **The change time** is what `update` uses to decide whether a file must
  be hashed again. Without it, a file rewritten with its old mtime
  restored would be missed.
- **Reparse points are never followed.** The walk does not descend
  through a symlink, a junction or any other kind.
- **The untrusted-manifest rules, on Windows:**
  - where Unix refuses a manifest that is a symlink, or a fjall manifest
    with a symlink inside, Windows refuses any reparse point. A junction
    redirects a directory and needs no privilege to create;
  - hashing opens the file without following a reparse point and checks
    that it is a regular file, so a link swapped in after the walk is an
    error, not another file's hash.
- **Known blind spot until step 4:** the read-only attribute and the
  security descriptor are not recorded, so a change to either is not
  reported.

### Formats

**fileset manifest**

- A new meta key, `platform`, with the value `unix` or `windows`. `init`
  writes it and `update` keeps it, in all three stores (SQLite, fjall,
  redb).
- `SCHEMA_VERSION` stays 1. A manifest without the key is a Unix manifest,
  which is true of every manifest that exists. A bump would make `check`
  refuse every existing manifest until `update` had run, and that update
  would absorb the drift the check was meant to find.
- The cost: an older `fileset` on Unix does not refuse a Windows manifest
  and reports false differences in `mode`, `uid` and `gid`. The book says
  to use a current `fileset` across platforms.
- The key is read from a manifest that may come from someone else. It can
  only cause a refusal, never relax a check, and a value other than the
  two above is refused.
- The protobuf `Entry` is unchanged.

**filegen state**

- `format_version` goes from 3 to 4, adding `platform` and the
  capabilities found at the first run.
- Resuming refuses when either differs from the current machine: the run
  would no longer be the one the seed gave.
- A version 3 state is read as a Unix state with every capability, so
  runs in progress resume unchanged.

**filegen log**

- The `# run` line gains `platform=` and `caps=`. No existing field
  changes.
- Step lines keep their shape. `mode` in the post-step stat is 0 on
  Windows.
- Name escaping is unchanged. It works on bytes, and the bytes of an
  unpaired surrogate are escaped like a non-UTF-8 name on Unix.

### Cross-platform check

The maintainer's long-term requirement: `fileset check` must work on a
tree whose manifest was made on the other platform family.

- **This step refuses it.** `check`, `update` and `list` stop with one
  message naming both platforms, with the exit code `fileset` already
  uses for a manifest it cannot use.
- **The decision is one function** that takes the manifest's platform and
  the running platform. Step 3 replaces its refusal with comparison rules.
  The manifest format does not change then, because the origin is already
  recorded.

## Testing

**Existing tests** fall into three groups:

| Group | Treatment |
| --- | --- |
| Platform-neutral behaviour that happens to use a Unix call (most symlink and walk tests) | rewritten against the platform interface or a test helper, so they run everywhere |
| POSIX-only behaviour (chmod, chown, FIFOs, devices, xattrs) | `#[cfg(unix)]` |
| Tests that need an unreadable file (`chmod 000` today) | on Windows, hold the file open with sharing denied |

**New tests**

- **Names:** the conversion round-trips seeded random 16-bit sequences,
  unpaired surrogates included; invalid WTF-8 is rejected; a pair written
  as two three-byte forms is rejected. On every platform.
- **Paths:** a tree deeper than 260 characters; `CON`, a trailing dot, a
  trailing space and an unpaired surrogate created, listed and removed.
- **Hard links:** two names of one file share an identity and a link
  count of 2.
- **Sparse:** after `punch_hole` the file has the sparse attribute, the
  range reads as zeros, and `fsutil sparse queryrange` shows the hole. The
  test runs `fsutil` so that checking adds no `unsafe`.
- **Change time:** content rewritten with the old mtime restored is hashed
  again by `update`.
- **Symlinks:** file and directory kinds and a dangling target, none
  followed by the walk.
- **Reparse points:** a junction in the tree is recorded as `other` and
  not entered; a manifest that is a junction, and a fjall manifest with a
  junction inside, are refused; hashing a file swapped for a symlink
  fails.
- **Manifest platform:** a manifest of the other family is refused, and so
  is an unknown value. On every platform, for each store.
- **State:** a version 3 state resumes; a state from the other platform or
  with other capabilities is refused.
- **Platform rule:** a test fails when a file outside `platform/` names
  one of the crates or modules listed under "Platform module".

**Symlink privilege:** without it, the symlink tests skip and say so. With
`CI` set, a missing privilege is a failure, so CI cannot pass by skipping
(as `MAN_TESTS_REQUIRED` does for the man pages).

**Both tools together:** `tools/ci.sh check` on Windows ends with a smoke
run: `filegen` fills and mutates a tree, then `fileset init`, `update` and
`check`. A script step, because the tools must not depend on each other.

**Unix regression:** the full `tools/ci.sh check` on Linux and macOS in CI.
FreeBSD has only the hand-started workflow; run it before the last pull
request merges.

## CI

- **`tools/ci.sh check` on Windows** (Git Bash) runs `cargo fmt --all
  --check`, then clippy, nextest and the doc tests for a list of crates
  kept in one variable: `filegen`, `fileset`, `irsync-glob`. Later steps
  extend the list until it is the workspace.
- It leaves out the dev-setup and man page tests and does not require
  rsync, so it needs no Python, pandoc or mandoc.
- A bare `tools/ci.sh` on Windows runs `check` only and says what it left
  out, as it does on macOS and FreeBSD.
- **`ci.yml`** gets a `windows` job with two entries: `windows-latest`
  (x86_64) and `windows-11-arm` (ARM64). It reports on every pull request
  and does not block: it is not among the jobs `CI result` requires.
- **`deny.toml`** lists both Windows targets, so the Windows-only crates
  are checked.
- **Miri:** the name conversion's tests join `miri_packages` for
  `fileset`.

## Documentation

- **Book, fileset:** the reference and "How it works" say what is recorded
  on Windows, what is not, the `platform` key, the refusal across
  platforms, and the blind spot.
- **Book, filegen** (under contributing): the operations on Windows, the
  capabilities, the names, the state and log additions.
- **Book, setup:** building on Windows: the MSVC build tools, Git Bash for
  `tools/ci.sh`, Developer Mode for symlinks, a short checkout path, and
  keeping trees on an NTFS drive.
- **`AGENTS.md`:** the Windows form of the test commands.
- **Limitations** keeps "Linux and macOS only": that line is about irsync.
- **Decision records:** entries in `docs/FILESET_DECISIONS.md`,
  `docs/FILEGEN_DECISIONS.md` and `docs/SUPPLY_CHAIN_DECISIONS.md`.

## Pull requests

0. **Before any code:** run `cargo deny` with the two Windows targets
   added. See "Needs the maintainer".
1. `fileset`: the platform module, Unix only, no behaviour change.
2. `filegen`: the same.
3. `fileset` on Windows: the name conversion, `platform/windows.rs`, the
   `platform` key and the refusal, the tests.
4. `filegen` on Windows: the same parts, the state and log changes, the
   tests.
5. CI job, `deny.toml`, the book, `AGENTS.md`, the decision records.

1 and 2 are pure moves. The CI job arrives last, so until then the Windows
tests run on the maintainer's machine, which runs both architectures.

## Left to the builder

- The exact functions of each platform interface, within the table above.
- Whether the Windows metadata read uses one handle for identity, change
  time and attributes, or several calls.
- The config key and default rate for edge names.
- The spelling of capabilities in the state file and the log.
- How the smoke run is sized so that it takes seconds.
- Whether the name conversion gets a fuzz target in `fuzz/`.
- Confirm that `stress/src/replay.rs` ignores `# run` fields it does not
  know, and adjust it if not.
- Confirm which console events `ctrlc` catches, and correct the book's
  wording to match.

## Needs the maintainer

- **The dependency check.** `deny.toml` accepts no advisory today, and the
  `deny` job is required. Adding the Windows targets makes it look at
  every Windows-only crate of the workspace, including iroh's. If step 0
  finds an advisory, choose: fix it, accept it with a reason, or keep
  Windows out of `deny.toml` until the port.
- **`windows-11-arm` runners** must be available to the organisation.
- **Developer Mode** on the development machine, or the symlink tests
  skip there.

## Later

- Step 2 inherits the name form (WTF-8, `/`), the extended path form and
  the rule against following reparse points.
- Step 3 replaces the refusal across platforms with comparison rules.
- Step 4 adds NTFS metadata to both tools through the platform interface
  and the formats' open ends.
- The stress harness needs a polite stop for a background `filegen` on
  Windows: Ctrl-Break to a process group, a stop file, or closed standard
  input.
