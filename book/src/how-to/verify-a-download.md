# Verify a download

Every irsync release is built by GitHub Actions from a tagged commit. GitHub
signs a record of that build, called an attestation: which file, which
commit, which workflow. You can check any file you downloaded against it.
Doing so shows that the file was built by irsync's own release workflow, and
not swapped somewhere on the way to you.

Every file on a release is also signed with irsync's release key. The key
is kept offline, away from GitHub, so a signature is the stronger proof. The
minisign check below is the simplest, and needs no GitHub account.

The attestation checks need the [GitHub CLI](https://cli.github.com/)
(`gh`), logged in with `gh auth login`.

## Check the signature with minisign

Install [minisign](https://jedisct1.github.io/minisign/) 0.10 or newer
(`apt install minisign`, `dnf install minisign`, `brew install minisign`).
Download the file and its signature, which has the same name with
`.minisig` added, from the
[releases page](https://github.com/irsync-dev/irsync/releases). Then:

```sh
minisign -Vm irsync-x86_64-unknown-linux-musl.tar.xz -P RWR6ngoFwyY7D9i4MAL2T4n1UkGwrCdSGA/SMowtANaffYWkTZdrAtAZ
```

`-P` is irsync's public key, the line above. It is the same key as in the
[README](https://github.com/irsync-dev/irsync#verify-a-download) and in
`packaging/signing/release-keys.pub` in the repository.

A good file prints `Signature and comment signature verified`, and a line
`Trusted comment: irsync vX.Y.Z`. **Check that the trusted comment names the
version you downloaded.** Otherwise someone could give you an older release's
signed files. A failure means: don't use the file, and please
[report it](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md).

The same works for every file on the release, the checksum files included.
Releases from before signing started have no `.minisig` files; use the
checks below for them.

## Check the signature with irsync

An installed irsync can check a download too, with the release keys built in:

```sh
irsync verify-signature --trusted-comment "irsync vX.Y.Z" irsync-x86_64-unknown-linux-musl.tar.xz
```

The signature file, `.minisig` added to the name, must be next to the file.
Exit status 0 means a good signature, 1 a bad or missing one. This command is
hidden from `irsync --help`.

## An archive or the installer

Download the file from the
[releases page](https://github.com/irsync-dev/irsync/releases), then:

```sh
gh attestation verify irsync-x86_64-unknown-linux-musl.tar.xz \
  --repo irsync-dev/irsync \
  --signer-workflow irsync-dev/irsync/.github/workflows/release.yml
```

`--signer-workflow` matters: without it, a file attested by any workflow in
the repository would pass, including one someone pushed on a branch. A good
file prints `Verification succeeded!` and the commit it was built from. That commit
is the one the release tag points to. The release workflow runs on a
temporary branch, `pinned/vX.Y.Z`, which is deleted afterwards, so the
attestation names that branch and not the tag. Don't use
`--source-ref refs/tags/vX.Y.Z`: it will not match. A
failure means: don't use the file, and please
[report it](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md).

The same works for every other file on the release: the other archives,
`irsync-installer.sh`, `irsync.rb` and the checksum files. Releases up to
v0.1.0-beta.2 have attestations for the archives only.

## A `.deb` or `.rpm` package

The packages live in the apt and dnf repository at
[dist.irsync.org](https://dist.irsync.org), not on the GitHub release. apt
and dnf already check every download against the repository's signing key.
To check a package file by hand as well:

```sh
gh attestation verify irsync_1.2.3_amd64.deb \
  --repo irsync-dev/irsync \
  --signer-workflow irsync-dev/irsync/.github/workflows/packages.yml
```

## The apt and dnf signing key

apt and dnf trust whatever key you imported. Check its fingerprint once,
when you add the repository:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://dist.irsync.org/irsync.asc | gpg --show-keys
```

Compare the 40-character fingerprint it prints with the one in
[SECURITY.md](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md#how-releases-are-built).
The two must match exactly.

If irsync's signing key ever changes its subkey, apt and dnf need the key
file again: download `irsync.gpg` (apt) or `irsync.asc` (dnf) as when you
added the repository. The fingerprint stays the same.

## Homebrew

The formula in `irsync-dev/tap` pins the SHA-256 of each archive, and
`brew` refuses a download that doesn't match. To check the archive itself,
find it in Homebrew's cache and verify it as above:

```sh
gh attestation verify "$(brew --cache irsync)" \
  --repo irsync-dev/irsync \
  --signer-workflow irsync-dev/irsync/.github/workflows/release.yml
```
