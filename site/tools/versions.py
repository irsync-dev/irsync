#!/usr/bin/env python3
"""The release versions on the landing page.

Works out which irsync versions the landing page shows, one per release
channel, and writes them into the page when the site is built.

A version is `X.Y.Z` (stable), `X.Y.Z-beta.N` (beta) or `X.Y.Z-alpha.N`
(alpha); anything else is not a release and is ignored. Of each channel
only the newest version counts. The stable is shown. The beta is shown if
it is newer than the stable. The alpha is shown if it is newer than the
stable and the beta, even a beta that is itself hidden. The first one shown
(stable, else beta, else alpha) is the main version: the one the install
command on the page installs.

The design is docs/superpowers/specs/2026-11-19-site-release-versions-design.md.
"""

import re
import subprocess
from collections import namedtuple

CHANNELS = ("stable", "beta", "alpha")
_RANK = {"alpha": 0, "beta": 1, "stable": 2}
_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta)\.(\d+))?")

_RELEASE_BRANCH = re.compile(r"refs/remotes/origin/release-\d+\.\d+")
_WORKSPACE_VERSION = re.compile(
    r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]*)"', re.MULTILINE)


class VersionsError(Exception):
    """The versions cannot be worked out, or the page cannot be filled."""


# One version on the page. `date` is an ISO day, YYYY-MM-DD.
Shown = namedtuple("Shown", "channel version date")


def parse(text):
    """The sort key of a release version, or None if `text` is not one."""
    m = _VERSION.fullmatch(text)
    if not m:
        return None
    x, y, z, pre, n = m.groups()
    return (int(x), int(y), int(z), _RANK[pre or "stable"], int(n or 0))


def channel(text):
    """The channel of a release version: stable, beta or alpha."""
    return CHANNELS[2 - parse(text)[3]]


def pick(candidates):
    """The versions to show, in the order stable, beta, alpha.

    `candidates` maps a version to its date. Keys that are not release
    versions are ignored.
    """
    newest = {}
    for text in candidates:
        key = parse(text)
        if key is None:
            continue
        c = channel(text)
        if c not in newest or key > parse(newest[c]):
            newest[c] = text
    shown = []
    for i, c in enumerate(CHANNELS):
        if c not in newest:
            continue
        key = parse(newest[c])
        # Held against every higher channel that has a version.
        if all(key > parse(newest[o]) for o in CHANNELS[:i] if o in newest):
            shown.append(Shown(c, newest[c], candidates[newest[c]]))
    return shown


def _git(repo, *args):
    """Run git in `repo`: (it worked, its output)."""
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    return r.returncode == 0, (r.stdout if r.returncode == 0 else r.stderr).strip()


def from_git(repo):
    """Every release version the checkout at `repo` knows, with its date.

    A `v*` tag is a published release: GitHub creates the tag when the
    release is published. The workspace version in Cargo.toml, at HEAD and at
    the tip of every origin/release-X.Y branch, is a release that is being
    built, when no tag has it yet.

    A date is the day in the commit, in the commit's own time zone: for a
    tag, of the commit it points at; for a version being built, of the
    commit that brought it onto the branch.
    """
    # For an annotated tag the starred field is the commit's date and the
    # plain one is empty; for a lightweight tag it is the other way round.
    ok, out = _git(repo, "for-each-ref",
                   "--format=%(refname:short) %(*committerdate:short)%(committerdate:short)",
                   "refs/tags/v*")
    if not ok:
        raise VersionsError(f"cannot list the tags of {repo}: {out}")
    found = {}
    for line in out.splitlines():
        tag, _, date = line.partition(" ")
        if parse(tag[1:]) is not None and date:
            found[tag[1:]] = date
    if not found:
        raise VersionsError("no release tags in this checkout; run: git fetch --tags")

    ok, out = _git(repo, "for-each-ref", "--format=%(refname)", "refs/remotes/origin/release-*")
    branches = [r for r in out.splitlines() if _RELEASE_BRANCH.fullmatch(r)] if ok else []
    for ref in ["HEAD", *branches]:
        ok, cargo = _git(repo, "show", f"{ref}:Cargo.toml")
        m = _WORKSPACE_VERSION.search(cargo) if ok else None
        if not m or parse(m.group(1)) is None or m.group(1) in found:
            continue
        version = m.group(1)
        # --first-parent shows a pull request's merge as the commit that
        # changed the version; the release tag will sit on that commit.
        ok, date = _git(repo, "log", "-1", "--first-parent", "--format=%cs",
                        f'-Sversion = "{version}"', ref, "--", "Cargo.toml")
        if not (ok and date):
            ok, date = _git(repo, "log", "-1", "--format=%cs", ref)
        if ok and date:
            found[version] = date
    return found
