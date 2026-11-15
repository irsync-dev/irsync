# Verify a download

Every irsync release is built by GitHub Actions from a tagged commit. GitHub
signs a record of that build, called an attestation: which file, which
commit, which workflow. You can check any file you downloaded against it.
Doing so shows that the file was built by irsync's own release workflow, and
not swapped somewhere on the way to you.

You need the [GitHub CLI](https://cli.github.com/) (`gh`), logged in with
`gh auth login`.

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
file prints `Verification succeeded!` and the tag it was built from. A
failure means: don't use the file, and please
[report it](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md).

The same works for `irsync-installer.sh` and the other archives on the
release.

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
