# Supply chain security: decisions

This file records the decisions behind how irsync guards the way from a
commit to a user's install. They come from
`docs/superpowers/specs/2026-11-14-supply-chain-security-design.md` and
`docs/superpowers/plans/2026-11-15-supply-chain-security.md`, plus the calls
made while building it. What is in place is described in
`book/src/contributing/supply-chain.md`. Where the code and this file
differ, the code is what shipped.

## Decisions

1. **Rules a single maintainer can live with.** `main` takes changes only
   through a pull request that passed `CI result`, with 0 required
   approvals, and nobody can bypass that, admins included. The release
   environment lets the person who tagged approve. Why: irsync has one
   maintainer today, and a rule that needs a second person would block all
   work. Cost if wrong: a stolen account can still merge its own pull
   request. Two-factor login is the guard there. When maintainers join,
   approvals and `CODEOWNERS` are settings changes.

2. **Release tags can be created but never moved or deleted, with no
   bypass.** Why: a published version must always mean the same commit.
   Cost if wrong: a mistaken tag can't be cleaned up. The answer is the
   next version.

3. **Release secrets in a `release` environment, `v*` tags only, one
   approval.** Why: a change merged to `main` can't reach the signing key or
   the tokens, and a tag alone isn't enough without the click. Cost if
   wrong: one manual step per stable release.

   The gate and resume call `packages.yml` and `publish-homebrew.yml` with
   `secrets: inherit`. Without it, the called job got the environment's
   secrets empty, even though it names the environment itself (found on the
   v0.1.0-beta.2 resume dry run). `inherit` also passes repo-wide secrets, so
   there must be none: `tools/github_settings.py` checks that.

4. **The resume workflow runs on the tag, not on `main`.** Why: the
   `release` environment opens only for tags. Cost if wrong: a fix to a
   release script on `main` doesn't reach a release already tagged. That
   release gets finished by hand, or the next version carries the fix.

5. **The release gate publishes the Homebrew formula, not dist.** dist
   0.34.0 writes `uses: $/.github/workflows/<job>.yml` for a local custom
   publish job, which GitHub can't run. Its built-in Homebrew job can't use
   an environment. So `publish-jobs = []`, and the gate runs
   `publish-homebrew.yml` next to the packages job, fetching the `irsync.rb`
   that dist attached to the release. Why: `release.yml` needs no secrets,
   and one approval covers both jobs. Cost if wrong: the tap updates
   minutes after the release is announced, not before. Revisit when dist
   fixes the template.

6. **Immutable releases; packages not attached.** Once published, a
   release's files can't change, so the `.deb` and `.rpm` files live only in
   the apt/dnf repo, each with an attestation. Why: nobody, a maintainer
   included, can swap a file on a release. Cost if wrong: people who
   downloaded packages from the release page now use dist.irsync.org.

7. **Attestations, verified by hand.** dist attests every file it builds
   for the release in its host job (`github-attestations-phase = "host"`).
   The `.minisig` signatures, added later by hand, have no attestation (see
   17). dist's default attests only the per-target archives, which left
   the installer unattested up to v0.1.0-beta.2. `packages.yml` attests
   the packages. `install.sh` doesn't check them. Why: checking in the
   script would need `gh` on every user's machine. Cost if wrong: an
   install through `install.sh` trusts GitHub's release storage, as
   before.

8. **Every Action pinned to a commit.** Hand-written workflows carry a
   `# vX.Y.Z` comment, and Dependabot updates them. `release.yml` gets its
   pins from `[dist.github-action-commits]` in `dist-workspace.toml`
   (dist can't write the comment), and Dependabot leaves it alone, because
   an edit there would fail `.github/scripts/release-yml.sh check`. Cost if wrong: the
   dist table is bumped by hand.

9. **cargo-deny fails on any advisory; exceptions need a reason.**
   Unmaintained and yanked crates fail too. Only crates built for the
   platforms irsync and its tools are built for count (`targets` in
   `deny.toml`, Windows since 32); the licenses of all the others are still
   checked before a release (`deny-license-check.toml`). Cost if wrong: a
   platform we build for but forgot to list goes unchecked. One advisory is
   accepted today (31).

   Two advisories were accepted here until their crates left the build:
   `unsafe-libyaml` (RUSTSEC-2024-0449), when `filegen` moved to a YAML
   library written in safe Rust (`serde-saphyr`), and `paste`
   (RUSTSEC-2024-0436), a build-time macro, when iroh 1.5 brought a
   `netwatch` that no longer uses it.

   Cost if wrong: a known hole ships.

10. **Licenses: today's set, all compatible with MIT.** Crates offering a GPL
    or LGPL alternative are used under their MIT or Apache option. MPL-2.0
    (`attohttpc`, `option-ext`) is allowed: it's file-level copyleft, and
    crates.io keeps the sources available. Cost if wrong: one entry to
    remove.

11. **Dependabot waits 7 days**, weekly, one grouped PR per ecosystem;
    security updates don't wait. Why: most hijacked releases are found
    within days. Cost if wrong: a wanted update arrives a week later.

12. **`install.sh` refuses plain HTTP.** It uses curl with `--proto =https`,
    or wget with `--https-only`. BusyBox wget has no such flag, so with only
    that the script asks for curl. Cost if wrong: Alpine users without curl
    install it first.

13. **The signing key's primary part stays offline.** GitHub holds a
    signing subkey. Users trust the primary fingerprint. Why: a leaked
    subkey can be revoked and replaced without every user importing a new
    key. Cost if wrong: one more step when making the key.

14. **Settings as code.** `tools/github_settings.py` holds the desired
    state for the four repositories and can check it at any time. Why:
    settings changed by hand drift unseen. The org's two-factor requirement
    has no API, so the script only reports it.

15. **The tap formula is the attested `irsync.rb` plus two lines.**
    `publish-homebrew.yml` runs `.github/scripts/formula-man-pages.sh` on the
    formula it downloads from the release, before it pushes to the tap. It
    runs before the tap is checked out, so no repo code runs while
    `HOMEBREW_TAP_TOKEN` is on disk. The
    script inserts `man1.install` and `man5.install` lines ahead of dist's
    `leftover_contents` line. Why: dist has no hook for this, and its
    formula puts every leftover file in `pkgshare`, where `man` doesn't look.
    The script comes from the release's own tag. It fails if dist's template
    changes and the anchor line goes missing. Cost if wrong: someone
    comparing the tap to the release's `irsync.rb` sees a difference. The
    script and its test show exactly which lines.

16. **`irsync-installer.sh` is ours, not dist's.** Since the release after
    0.1.0-beta.3, `packaging/installer/build-installer.sh` generates it,
    run by dist through `extra-artifacts` in the `build-global-artifacts`
    job. It has the SHA-256 checksums of the three tarballs written into
    it, and it is attested with the other release files like before. Why:
    dist's installer can't install the man pages. Cost if wrong: we maintain a small POSIX
    shell script ourselves, and a change to the tarball names must change it
    too.

17. **Developer tools are downloaded and checked against hashes in the
    repo.** `tools/dev-setup.sh` fetches cargo-nextest, cargo-deny, dist,
    mdbook and actionlint as release binaries and refuses one whose SHA-256
    is not the one in the script. A refused or failed download is reported;
    it never turns into a build from source. Why: building them took
    minutes on every new machine and version bump. cargo-binstall was
    turned down: it verifies minisign signatures only for a tool whose
    `Cargo.toml` names a key, and none of these does, so it would check
    nothing beyond HTTPS. Cost if wrong: the hashes are replaced by hand on
    each bump, and mdbook publishes none, so its hashes are our own first
    download. Revisit if the tools start signing.

18. **Release files are signed with an offline minisign key** (issue #89).
    A maintainer signs every file on the draft release on their own
    computer with `.github/scripts/sign-release.sh`, then publishes the
    draft. The public key is in `packaging/signing/release-keys.pub`. Every
    signature has the trusted comment `irsync vX.Y.Z`, so an older
    release's signed files can't be passed off as a newer one. Why the key
    is offline: until now only GitHub vouched for a release, through
    attestations and checksums. Whoever takes over the organisation, a
    maintainer's token or a compromised Action can build and publish a
    release that passes both. A key that never touches GitHub closes that
    gap: nothing that happens on GitHub can make a valid signature. It also
    gives users a check that needs no `gh` and no GitHub account. The
    flow: `release.yml` makes a draft (the gate ends there), the script
    signs and publishes it, which creates the tag, and then starts the
    resume run. A draft takes new files even with immutable releases on, and
    has no tag, so a failed run leaves nothing behind. Cost if wrong: every
    release needs the maintainer, with the USB stick, at their computer.
    Losing the key means a new key and a changed `release-keys.pub`. A
    second maintainer can add their own key to that file.

19. **A stale advisory exception fails the dependency check.** `cargo deny`
    runs with `-D advisory-not-detected`, in `tools/ci.sh` and in
    `advisories.yml`. Why: an exception in `deny.toml` that no longer
    matches anything would stay for ever and hide the next advisory against
    that crate. Cost if wrong: a crate's fix can make the check fail until
    the exception is removed, which is a one-line change.

20. **Fuzzing with cargo-fuzz, in `fuzz/`.** One target per entry point that
    reads bytes from a peer or another program. The seeds checked in under
    `fuzz/seeds/` run on every pull request (`fuzz-build`). Each target is
    fuzzed for ten minutes every night (`nightly.yml`), on a corpus kept
    in the Actions cache. A crash fails the run and the failing inputs are
    uploaded as the `fuzz-artifacts` artifact, which is public. The
    maintainer chose to keep the upload: a red run is useless without the
    input, and anyone can run the same public fuzzer on the same code. The
    book says what to do with a crash that looks exploitable. No hosted
    fuzzing service for now. Cost if wrong: an input that shows a hole is
    public until the run is deleted. Revisit a hosted service if the
    nightly run finds too little or the maintainer wants crashes kept
    private.

21. **Static analysis in layers, with CodeQL advisory first.** Rules:
    `unsafe_op_in_unsafe_fn` and clippy's `undocumented_unsafe_blocks`, so
    every `unsafe` block says why it is sound; crates with no unsafe code
    forbid it. zizmor lints the workflows in the `workflows` job, with
    exceptions in `.github/zizmor.yml` that each give a reason. CodeQL runs
    on the Rust code and the workflows, but is not part of `CI result`
    until it has been quiet for a few weeks. Why: a check that cries wolf
    on its first day would block every pull request. Miri runs the tests it
    can run, on the crates that need no system calls, every night. Why not
    on every pull request: it is about a hundred times slower. Cost if
    wrong: undefined behaviour in unsafe code is found a day late. Making
    CodeQL required is a settings change once it has proved quiet.

22. **Private vulnerability reporting is on, and reports are answered within
    7 days.** `SECURITY.md` offers GitHub's **Report a vulnerability**
    button and `security@irsync.org`. The 7 days is the maintainer's
    number. Why: a reporter should have a private place that is not an
    email address, and a reply time makes the policy something people can
    rely on. `tools/github_settings.py` holds the setting and `check`
    reports it if it is off, so it can't drift unseen. Cost if wrong: one
    maintainer has to keep to 7 days. Turning the setting on needs an org
    admin.

23. **Scorecard comes last, and we do not chase the score.** The OpenSSF
    Scorecard runs weekly, on pushes to `main` and when branch protection
    changes (`scorecard.yml`), and publishes its result for the README
    badge. It was added after the other five changes, so that what it
    measures is already fixed or decided. The score describes the project;
    we change nothing only to raise it. Checks that stay low are listed in
    the book with the reason for each: one maintainer (Code-Review, parts
    of Branch-Protection, Contributors), a young repository (Maintained),
    files written by dist (Token-Permissions, some of Pinned-Dependencies),
    and release history (Signed-Releases). Why: a number we work to raise
    can drift away from what protects users. Cost if wrong: a visitor may
    read a middling score as neglect. The table is there to answer that.

24. **`ssh-key` is a release candidate, pinned exactly (`=0.7.0-rc.12`).**
    0.6 lists the old `rsa` and `crypto-bigint` as optional parts. We
    never build them, but they sit in `Cargo.lock` with open advisories
    (RUSTSEC-2023-0071, RUSTSEC-2026-0387), next to a second, older copy
    of the curve crates that iroh already brings. 0.7 uses the fixed
    versions and shares iroh's copy. Keys stay in the OpenSSH format,
    with the same cipher and key derivation. Checked by hand on
    2026-11-20: irsync 0.1.0-beta.3 and `ssh-keygen` read keys the new
    build writes, and the new build reads theirs, plain and encrypted. Why: fewer crates, and
    no known-weak code in the lock file. Cost if wrong: a release
    candidate can still change or carry a bug in how identity keys are
    read and written. The exact pin means nothing moves until we move
    it; go to 0.7.0 when it is released.

25. **aws-lc-rs in place of ring as the TLS crypto library** (2026-11-22,
    issue 240). The hybrid key exchange that keeps a recorded connection
    safe from a future quantum computer needs ML-KEM, and ring has none.
    ring leaves the build completely: iroh, noq, rustls and the OTLP
    exporter all run on aws-lc-rs. Six crates come in: `aws-lc-rs`,
    `aws-lc-sys` (AWS's fork of BoringSSL, C and assembly, compiled from
    source by `cc`; no cmake or nasm is needed), and the build helpers
    `cmake`, `dunce`, `fs_extra` and `jobserver`. Why: one crypto
    library, not two, and the one rustls itself defaults to. Cost if
    wrong: more C code than ring has; the binary is about 4% larger and a
    CPU-bound transfer about 5% slower. Going back is a change of
    features in `Cargo.toml`, and loses the hybrid. The measurements are in
    `docs/superpowers/specs/2026-11-22-post-quantum-key-exchange-design.md`.

26. **A second version of a crate fails the dependency check** (2026-11-23,
    issue 209). `deny.toml` sets `multiple-versions = "deny"` and lists
    each duplicate we accept today under `skip`, one entry per version,
    with the crate that pulls it in. `cargo deny` runs with
    `-D unmatched-skip -D unnecessary-skip`, so an entry that no longer
    matches fails the check, as a stale advisory exception does (19). Why:
    as a warning, a new duplicate was easy to miss in the CI log, and
    duplicates built up unnoticed. Single entries, not `skip-tree`:
    skip-tree accepts a crate and all it depends on, and would hide the
    next duplicate in that tree. Cost if wrong: a routine `cargo update`
    can fail the check and need a `skip` entry or a fix, and an upstream
    fix needs an entry removed.

27. **`winapi-util` reads a file's identity on Windows** (2026-11-23,
    `fileset` and `filegen`, version 0.1.11). `fileset` records each
    file's file index, volume serial number and link count, as it records
    inode, device and link count on Unix; `filegen` reads the same for its
    hard-link bookkeeping and its post-step stats.
    `winapi-util::file::information` reads all three from an open handle
    with no `unsafe` of ours. Why this crate: it was already in
    `Cargo.lock` (through `walkdir` and `same-file`), is a thin safe layer
    over `windows-sys`, and is maintained by Andrew Gallant (BurntSushi),
    who also maintains `walkdir` and `ripgrep`. Built only for Windows. Cost if wrong: one call to replace with a `windows-sys`
    call and a `// SAFETY:` comment.

28. **`windows-sys` for the Windows calls no safe crate makes**
    (2026-11-23, `fileset` and `filegen`, version 0.61.2; `fileset` with
    features `Win32_Foundation`, `Win32_Storage_FileSystem` and
    `Win32_System_SystemServices`, `filegen` with those and `Win32_System_IO`
    and `Win32_System_Ioctl`). `fileset` reads the NTFS change time and a
    reparse point's tag with `GetFileInformationByHandleEx`, in one `unsafe`
    block in `crates/fileset/src/platform/windows.rs`. The standard library
    does not expose the change time, which `fileset` records as `ctime`, as
    it does on Unix. No comparison tier uses it and `update` does not decide
    by it: a file is hashed again when its size, mtime or file identity
    differ. `filegen` reads a reparse
    point's tag the same way, and punches holes with `DeviceIoControl`
    (`FSCTL_SET_SPARSE`, `FSCTL_SET_ZERO_DATA`), one `unsafe` block each in
    `crates/filegen/src/platform/windows.rs`. Since 2026-11-24 `irsync`
    uses it too, with features `Wdk_Foundation`, `Wdk_Storage_FileSystem`,
    `Wdk_System_SystemServices`, `Win32_Foundation`, `Win32_Security`,
    `Win32_Storage_FileSystem`, `Win32_System_IO`, `Win32_System_Ioctl`,
    `Win32_System_SystemServices` and `Win32_System_Threading`: its open
    directory works on handles, through the native calls (`Wdk`) the
    standard library also uses in `remove_dir_all` on Windows: `NtCreateFile`
    relative to a directory handle, `NtQueryInformationFile`,
    `NtSetInformationFile` (rename, hard link and delete by handle),
    `NtQueryDirectoryFile` and `NtFsControlFile` (reading and setting a
    reparse point), plus `RtlNtStatusToDosError` and `WaitForSingleObject`.
    Every `unsafe` block of it is in
    `crates/irsync/src/platform/windows/native.rs`, one per call. Why this
    crate: it is
    Microsoft's own generated binding to the Win32 API (the `windows-rs`
    project), with no code beyond the declarations, and 0.61.2 is the newest
    version already in `Cargo.lock`, so no new copy comes in. Built only for
    Windows. Cost if wrong: a binding generated wrong would be wrong for
    much of the Rust ecosystem too; the block's comment lists what its
    soundness rests on.

29. **`cpu-time` for a thread's CPU time on Windows** (2026-11-23,
    `fileset`, version 1.0.0). `fileset` measures the CPU time each hashing
    thread spends on a file, for the progress line; on Unix it reads
    `CLOCK_THREAD_CPUTIME_ID`. `cpu_time::ThreadTime` wraps
    `GetThreadTimes`, so we write no `unsafe` for it. Why this crate: it is
    small, has no dependencies beyond `libc` and `winapi`, both already in
    `Cargo.lock`, and does this one thing. It is maintained by Paul
    Colomiets (tailhook); its last release is from 2019, which for a
    wrapper over a stable system call is not a worry by itself. Built only
    for Windows. Cost if wrong: if it is abandoned with a bug, the
    replacement is one `windows-sys` call (`GetThreadTimes`) in
    `platform/windows.rs`.

30. **`ctrlc` for Ctrl-C on Windows** (2026-11-23, `filegen`, version
    3.5.2, pinned exactly). `filegen` stops a run cleanly on an interrupt:
    the step in flight finishes, the state is saved, and it exits with 130;
    a second interrupt exits at once. On Unix it installs SIGINT and SIGTERM
    handlers through `nix`; on Windows `ctrlc` registers a console control
    handler, so we write no `unsafe` for it. Its handler receives every
    console event (Ctrl-C, Ctrl-Break, closing the console window, logoff,
    shutdown) and does not say which arrived, so the log names each one
    `Ctrl-C`. For Ctrl-C and Ctrl-Break the process carries on and saves
    its state; for close, logoff and shutdown Windows ends the process
    shortly after the handler returns, so a state save is not guaranteed
    there. Why this crate: it is the common choice for this one job, small,
    and on Windows depends only on `windows-sys` 0.61, already in
    `Cargo.lock`. It is maintained by Antti Keränen (Detegr). Built only
    for Windows. Why 3.5.2: 3.5.3 was under a week old on 2026-11-23.
    3.5.2 asks for `nix` 0.31, which no single target builds (`ctrlc`
    needs `nix` on Unix only, `filegen` needs `ctrlc` on Windows only), but
    which appears in `Cargo.lock` and which cargo-deny counts as a second
    `nix` version once Windows is among its targets. Move to 3.5.3 after
    2026-11-25: it takes `nix` 0.32, which removes that entry. Cost if
    wrong: the replacement is one `SetConsoleCtrlHandler` call through
    `windows-sys`, with a `// SAFETY:` comment.

31. **One advisory accepted: `chrono` is unmaintained (RUSTSEC-2026-0543)**
    (2026-11-23). Adding the Windows targets (32) made `cargo deny` look at
    every Windows-only crate of the workspace, iroh's included, and it found
    this one. `chrono` is in the build only on Windows, through `netwatch`
    (iroh); no Linux, macOS or FreeBSD build has it. The advisory says
    `chrono` is unmaintained, not that it holds a vulnerability. It is
    accepted under `[advisories]` in `deny.toml` with that reason, the other
    option being to keep Windows out of `deny.toml` until irsync itself builds
    there, which would leave `filegen` and `fileset`'s Windows build
    unchecked. We asked upstream to drop it; the tracking issue is #174. Cost
    if wrong: if a real vulnerability appears in `chrono`, the daily check
    still reports it, because the entry names this advisory and no other.
    Remove the entry when iroh no longer builds `chrono` on Windows: from then
    on the entry matches nothing, and the check fails (`-D
    advisory-not-detected`) until it is removed.

32. **Windows joins the targets in `deny.toml`, with a temporary skip of
    `nix` 0.31** (2026-11-23). `filegen` and `fileset` are built for
    `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`, so an advisory,
    a license or a duplicate in a crate only they use would otherwise go
    unseen. The check now covers the Windows-only crates of the whole
    workspace, not only those two tools'. It also showed a second `nix`:
    `ctrlc` is pinned to 3.5.2 (30), which asks for `nix` 0.31, while the rest
    is on 0.32. No single target builds both, but `Cargo.lock` holds both and
    cargo-deny counts them. The duplicate is skipped as `nix@0.31`, with
    `ctrlc` as the reason, in the same single-entry form as the others (26).
    Both this skip and the pin go when `ctrlc` moves to 3.5.3, which is not
    before 2026-11-25, because 3.5.3 was under a week old on 2026-11-23. The
    check fails on the skip once it no longer matches (`-D unmatched-skip`),
    so it cannot be forgotten. Cost if wrong: a Windows-only crate we do not
    answer for can fail the check after a routine `cargo update`, and then
    needs the usual fix or entry (see "Accepting an advisory" and "Accepting
    a second version of a crate" in the book's supply chain page).

## Left open

- `cargo-vet`, reproducible builds and signed commits. Not now, see the
  spec's non-goals.
