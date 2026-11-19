#!/usr/bin/env python3
"""Add the version switcher to the books of a built site.

The site holds the book several times (see site/tools/versions.py and the
VERSIONS file the build writes). This tool runs after mdbook has built all
of them. It puts a button in the top bar of every page, which opens a list
of the books: Stable, Beta, Alpha (those that exist) and Dev. /book/ is
not in the list: it is a copy of one of the channel books, and that one is
ticked there.

Each link is worked out here, for the page it is on: the same page in the
other book if that book has the file, else that book's title page. So the
build's link check (check.py) checks every one of them, and the browser
only opens and closes the list (book/theme/switcher.js).

Usage: switcher.py SITE_DIR    (SITE_DIR holds VERSIONS and the books)
"""

import html
import sys
from collections import namedtuple
from pathlib import Path

import versions

ORDER = (*versions.CHANNELS, "dev")
LABELS = {"stable": "Stable", "beta": "Beta", "alpha": "Alpha", "dev": "Dev"}

# One book in the list. `version` is None for dev.
Entry = namedtuple("Entry", "path label version")


class SwitcherError(Exception):
    """The switcher cannot be added."""


def _lines(versions_text):
    """{path: version or None} from a VERSIONS text, which must be sound."""
    try:
        versions.unpublished(versions_text)
    except versions.VersionsError as e:
        raise SwitcherError(f"VERSIONS: {e}") from e
    out = {}
    for line in versions_text.splitlines():
        path, version, _, _ = line.split(" ")
        out[path] = None if version == "-" else version
    return out


def entries(versions_text):
    """The books in the list, in the order stable, beta, alpha, dev."""
    found = _lines(versions_text)
    return [Entry(p, LABELS[p], found[p]) for p in ORDER if p in found]


def ticked(versions_text, path):
    """The entry ticked in the book at /PATH/. /book/ ticks the channel of
    its version, or dev when it has none."""
    if path != "book":
        return path
    version = _lines(versions_text)["book"]
    return versions.channel(version) if version else "dev"


def menu(entries, here, page, pages_of):
    """The switcher's markup for `page` (its path inside its book, such as
    tutorial/first-sync.html). `here` is the ticked entry; `pages_of` holds
    the HTML files of each entry's book."""
    up = "../" * (page.count("/") + 1)
    items = []
    for e in entries:
        same = page != "404.html" and page in pages_of.get(e.path, ())
        href = f"{up}{e.path}/{page if same else 'index.html'}"
        attrs = [f'href="{html.escape(href)}"']
        if e.path == here:
            attrs.append('aria-current="true"')
        if same:
            attrs.append("data-same-page")
        version = html.escape(e.version or "unreleased")
        items.append(f'<li><a {" ".join(attrs)}>{e.label} '
                     f'<span class="irsync-book-version">{version}</span></a></li>')
    label = LABELS.get(here, here)
    return (
        '<div class="irsync-book">'
        '<button id="irsync-book-toggle" class="icon-button" type="button" '
        'title="Switch version" aria-haspopup="true" aria-expanded="false" '
        f'aria-controls="irsync-book-list">{label}</button>'
        '<ul id="irsync-book-list" aria-label="Versions" hidden>'
        + "".join(items) + "</ul></div>"
    )


_BAR = '<div class="right-buttons">'


def insert(page_html, menu_html):
    """The page with the menu at the start of its top bar's right buttons.
    A page without a top bar comes back unchanged."""
    count = page_html.count(_BAR)
    if count == 0:
        return page_html
    if count > 1:
        raise SwitcherError("the page has more than one top bar")
    return page_html.replace(_BAR, _BAR + menu_html)


def apply(site_dir):
    """Add the switcher to every page of every book in SITE_DIR/VERSIONS."""
    site_dir = Path(site_dir)
    try:
        text = (site_dir / "VERSIONS").read_text(encoding="utf-8")
    except FileNotFoundError as e:
        raise SwitcherError(f"{e.filename} is missing") from e
    found = entries(text)
    paths = list(_lines(text))
    pages_of = {p: {f.relative_to(site_dir / p).as_posix() for f in (site_dir / p).rglob("*.html")}
                for p in paths}
    for path in paths:
        here = ticked(text, path)
        # A book whose title page has no top bar would get no switcher at
        # all, and every link check would still pass: stop instead.
        index = site_dir / path / "index.html"
        if not index.is_file() or _BAR not in index.read_text(encoding="utf-8"):
            raise SwitcherError(f"{path}/index.html: no top bar ({_BAR}) to put the switcher in")
        for page in sorted(pages_of[path]):
            f = site_dir / path / page
            before = f.read_text(encoding="utf-8")
            try:
                after = insert(before, menu(found, here, page, pages_of))
            except SwitcherError as e:
                raise SwitcherError(f"{path}/{page}: {e}") from e
            if after != before:
                f.write_text(after, encoding="utf-8")


def main(argv):
    if len(argv) != 2:
        print("usage: switcher.py SITE_DIR", file=sys.stderr)
        return 2
    try:
        apply(argv[1])
    except SwitcherError as e:
        print(f"switcher.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
