# Security policy

## Report a vulnerability

There are two ways to report a vulnerability. Please do not open a public
GitHub issue for a security problem.

- On GitHub, open the repository's **Security** tab and press **Report a
  vulnerability**. The report stays private between you and the maintainers.
- Or email **security@irsync.org**.

We answer within 7 days.

Say what you found, which irsync version (`irsync --version`) and OS it
affects, and how to reproduce it. We will confirm we got your report and keep
you posted while we work on a fix.

## Supported versions

irsync is beta software. Only the latest commit on `main` gets security
fixes.

## How irsync protects data

How keys, grants and relays work is explained in
[Identity and trust](https://irsync.org/book/explanation/identity-and-trust)
and [Connections](https://irsync.org/book/explanation/connections).

## How releases are built

Every release is built by GitHub Actions from a tag that can't be moved
after it's pushed. Each archive, installer and package carries a signed
attestation of the commit and workflow that built it, and a published
release can't be changed. To check a download, see
[Verify a download](https://irsync.org/book/how-to/verify-a-download).

The apt and dnf repositories at dist.irsync.org are signed by the key
`irsync releases <releases@irsync.org>`, with this fingerprint:

```
58A0 F740 B554 8CE7 3D16  1CCD ABDF 7107 434D F0E0
```

The signatures come from a signing subkey of that key. The primary key is
kept offline.

The project's OpenSSF Scorecard result is at
[scorecard.dev](https://scorecard.dev/viewer/?uri=github.com/irsync-dev/irsync).
The low checks we accept are listed in
[Supply chain security](https://irsync.org/dev/contributing/supply-chain).
