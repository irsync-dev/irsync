#!/usr/bin/env python3
"""Label one copy of the book with the version it documents.

The site holds the book several times: /book/, /stable/, /beta/, /alpha/
and /dev/ (see site/tools/versions.py). site/build.sh builds each from a
working copy under target/: today's book.toml and theme, and the pages
(book/src) of the version's commit. This tool makes such a copy say what it
is, before mdbook builds it:

- the title in the bar at the top of every page: "irsync VERSION", or
  "irsync unreleased";
- the address the book is served at, for mdbook's 404 page;
- "Edit this page" only for the unreleased book: a released page cannot be
  edited at its tag;
- one sentence under the heading of the title page.

It only ever writes into the working copy. The book/ directory of the
repository, and the pages of a release, stay as they are.

Usage: label_book.py WORK PATH VERSION    (VERSION is - for the unreleased book)
"""

import sys
from pathlib import Path

import versions

PATHS = ("book", "stable", "beta", "alpha", "dev")


class BookError(Exception):
    """The working copy cannot be labelled."""


def _settings(text, path, version):
    """book.toml with the title and the address of this copy."""
    title = f"irsync {version}" if version else "irsync unreleased"
    lines = text.splitlines(keepends=True)

    def only(name, start):
        found = [i for i, line in enumerate(lines) if line.startswith(start)]
        if len(found) != 1:
            raise BookError(f"{path}: book.toml must have the line `{start.strip()}` once, "
                            f"to set the {name}")
        return found[0]

    lines[only("title", 'title = "irsync"\n')] = f'title = "{title}"\n'
    lines[only("site-url", 'site-url = "/book/"\n')] = f'site-url = "/{path}/"\n'
    edit = only("edit-url-template", "edit-url-template = ")
    if version:
        del lines[edit]
    return "".join(lines)


def _title_page(text, path, version):
    """The title page with the sentence that names the version."""
    heading, _, rest = text.partition("\n")
    if not heading.startswith("# "):
        raise BookError(f"{path}: introduction.md does not start with a heading")
    if version:
        sentence = f"This is the documentation of irsync {version}."
    else:
        sentence = "This is the documentation of the unreleased development version of irsync."
        if path == "dev":
            sentence += " [Released documentation](../book/)."
    # The text below keeps one empty line between it and the sentence.
    body = rest if rest.startswith("\n") else "\n" + rest
    return f"{heading}\n\n{sentence}\n{body}"


def prepare(work, path, version):
    """Label the working copy at `work`, served at /PATH/. `version` is None
    for the unreleased book."""
    if path not in PATHS:
        raise BookError(f"unknown path {path!r}: one of {', '.join(PATHS)}")
    if version is not None and versions.parse(version) is None:
        raise BookError(f"{path}: {version!r} is not a release version")
    toml, intro = Path(work) / "book.toml", Path(work) / "src" / "introduction.md"
    try:
        settings = _settings(toml.read_text(encoding="utf-8"), path, version)
        page = _title_page(intro.read_text(encoding="utf-8"), path, version)
    except FileNotFoundError as e:
        raise BookError(f"{path}: {e.filename} is missing") from e
    # Both are worked out before either is written.
    toml.write_text(settings, encoding="utf-8")
    intro.write_text(page, encoding="utf-8")


def main(argv):
    if len(argv) != 4:
        print("usage: label_book.py WORK PATH VERSION", file=sys.stderr)
        return 2
    work, path, version = argv[1:]
    try:
        prepare(Path(work), path, None if version == "-" else version)
    except BookError as e:
        print(f"label_book.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
