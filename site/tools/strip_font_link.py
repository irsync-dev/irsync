#!/usr/bin/env python3
"""Remove mdbook's web-font stylesheet link from every page of a built book.

Usage: strip_font_link.py BOOK_DIR
"""

import re
import sys
from pathlib import Path

# mdbook 0.5 writes it on a line of its own, with a content hash in the name.
_LINK = re.compile(
    r'[ \t]*<link rel="stylesheet" href="[^"]*fonts/fonts(?:-[0-9a-f]+)?\.css">\n'
)


def strip_font_link(book_dir):
    """Remove the link from every HTML page under `book_dir`; return how many
    pages changed."""
    changed = 0
    for page in sorted(Path(book_dir).rglob("*.html")):
        text = page.read_text(encoding="utf-8")
        new = _LINK.sub("", text)
        if new != text:
            page.write_text(new, encoding="utf-8")
            changed += 1
    return changed


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__.strip())
    strip_font_link(sys.argv[1])
