# Changelog

What changed in each irsync release, for people who run it: what is new,
what to do when upgrading, and what was fixed. The newest release is on top.
Each section is also the top of that release's notes on GitHub, where the
full list of changes follows it.

## v0.1.0-beta.6

This section was written after the release, from its list of changes.

### New

- `--bwlimit` now limits a pull too, not only a push.
- irsync and the install script check the signature of a release before
  they install it. The install script never installs an unsigned release.
- The website holds the documentation once per release channel, and the
  book's header switches between them. The homepage shows the current
  versions.
- The man pages name the authors and the license.

### Fixed

- After a resume, the progress block and the summary report the right
  totals, percent and number of created files.
- A failed session says "failed" in the progress block, not "done".

### Behind the scenes

- The code that reads data from a peer is fuzzed every night.
- CodeQL, Miri and zizmor check the code and the workflows, and shellcheck
  checks every shell script.
