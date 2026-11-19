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
from collections import namedtuple

CHANNELS = ("stable", "beta", "alpha")
_RANK = {"alpha": 0, "beta": 1, "stable": 2}
_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta)\.(\d+))?")

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
