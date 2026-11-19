#!/usr/bin/env python3
"""Link to the site's pages without .html, the way Cloudflare serves them.

Cloudflare serves x.html at the address x, and answers x.html with a
redirect to x (the Worker's default html_handling). mdbook writes every
link with .html, so each click in the book cost a redirect, and mdbook's
sidebar never marked the current chapter: it compares its .html links with
the address, which has none.

This tool runs last in site/build.sh. It rewrites the links of the built
site, never the file names:

- every href (and the src of the sidebar's frame) in every page, the
  landing page included: x.html becomes x,
  and dir/index.html becomes dir/;
- the sidebar of each book (toc-HASH.js, its links as HTML text);
- the addresses in each book's search index (searchindex-HASH.js).

The books are those in the build's VERSIONS. A book without its sidebar
script or search index, or one with no link to rewrite, stops the build: a
newer mdbook that writes them differently must not leave half the links
with .html.

Usage: pretty_urls.py SITE_DIR
"""

import re
import sys
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit


class PrettyError(Exception):
    """The links cannot be rewritten."""


def pretty(href):
    """`href` without .html if it is a link to a page of the site."""
    parts = urlsplit(href)
    if parts.scheme or parts.netloc or not parts.path.endswith(".html"):
        return href
    path = parts.path[: -len(".html")]
    if path == "index":
        path = "./"
    elif path.endswith("/index"):
        path = path[: -len("index")]
    return urlunsplit(parts._replace(path=path))


_LINK = re.compile(r'\b(href|src)="([^"]*)"')


def rewrite_hrefs(text):
    """Every href="..." and src="..." in `text`, a page or the sidebar's
    HTML. A src only ever names a page in the sidebar's frame (toc.html)."""
    return _LINK.sub(lambda m: f'{m.group(1)}="{pretty(m.group(2))}"', text)


_DOC_URLS = re.compile(r'"doc_urls":\[([^\]]*)\]')
_QUOTED = re.compile(r'"([^"\\]*)"')


def rewrite_search_index(text):
    """The addresses (doc_urls) of mdbook's search index."""
    found = _DOC_URLS.search(text)
    if not found:
        raise PrettyError("no doc_urls in the search index")
    urls = _QUOTED.sub(lambda m: f'"{pretty(m.group(1))}"', found.group(1))
    return text[: found.start(1)] + urls + text[found.end(1):]


def _one(book_dir, pattern, what):
    found = sorted(book_dir.glob(pattern))
    if len(found) != 1:
        raise PrettyError(f"{book_dir.name}: no {what} ({pattern})" if not found
                          else f"{book_dir.name}: more than one {what} ({pattern})")
    return found[0]


def _rewrite(path, how, must_change):
    before = path.read_text(encoding="utf-8")
    after = how(before)
    if must_change and after == before:
        raise PrettyError(f"{path.parent.name}/{path.name}: no .html link to rewrite")
    if after != before:
        path.write_text(after, encoding="utf-8")


def apply(site_dir):
    """Rewrite the links of the site in SITE_DIR."""
    site_dir = Path(site_dir)
    try:
        lines = (site_dir / "VERSIONS").read_text(encoding="utf-8").splitlines()
    except FileNotFoundError as e:
        raise PrettyError(f"{e.filename} is missing") from e
    books = [site_dir / line.split(" ")[0] for line in lines if line.strip()]
    # Look for everything before writing anything.
    scripts = [(_one(b, "toc-*.js", "toc script"), _one(b, "searchindex-*.js", "search index"))
               for b in books]
    for toc, index in scripts:
        _rewrite(toc, rewrite_hrefs, must_change=True)
        _rewrite(index, rewrite_search_index, must_change=True)
    for page in sorted(site_dir.rglob("*.html")):
        _rewrite(page, rewrite_hrefs, must_change=False)


def main(argv):
    if len(argv) != 2:
        print("usage: pretty_urls.py SITE_DIR", file=sys.stderr)
        return 2
    try:
        apply(argv[1])
    except PrettyError as e:
        print(f"pretty_urls.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
