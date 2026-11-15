# Security policy

## Report a vulnerability

Email **security@irsync.org**. Please do not open a public GitHub issue for a
security problem.

Say what you found, which irsync version (`irsync --version`) and OS it
affects, and how to reproduce it. We will confirm we got your report and keep
you posted while we work on a fix.

## Supported versions

irsync is alpha software. Only the latest commit on `main` gets security
fixes.

## How irsync protects data

How keys, grants and relays work is explained in
[Identity and trust](https://irsync.org/book/explanation/identity-and-trust.html)
and [Connections](https://irsync.org/book/explanation/connections.html).

## How releases are built

Every release is built by GitHub Actions from a tag that can't be moved
after it's pushed. Each archive, installer and package carries a signed
attestation of the commit and workflow that built it, and a published
release can't be changed. To check a download, see
[Verify a download](https://irsync.org/book/how-to/verify-a-download.html).

The apt and dnf repositories at dist.irsync.org are signed by the key
`irsync releases <releases@irsync.org>`. Its fingerprint will be listed here
with the first stable release.

