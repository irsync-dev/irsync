#!/usr/bin/env python3
"""Check the built site: links, references to moved files, documented flags."""

import argparse
import re
import subprocess
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

# Repository paths that the book replaces; Tasks 3-7 delete them one by one.
MOVED_FILES = [
    "docs/daemon.md",
    "docs/protocol.md",
    "docs/telemetry.md",
    "docs/DELTA_RECEIVE.md",
    "docs/IRSYNC_VS_RSYNC.md",
    "docs/NETSTRESS_HOW_IT_WORKS.md",
    "docs/STRESS_BUG_HUNTING.md",
    "DEVELOPMENT.md",
]

# Files that name the moved paths on purpose, and trees that are history.
MOVED_IGNORED_FILES = {"site/tools/check.py", "site/tools/test_check.py"}
MOVED_IGNORED_PREFIXES = ("docs/superpowers/", "target/")


# ---- links ----------------------------------------------------------------


class _Page(HTMLParser):
    """Collects the ids and the relative references of one HTML page."""

    def __init__(self):
        super().__init__()
        self.ids = set()
        self.refs = []

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if value is None:
                continue
            if name == "id" or (tag == "a" and name == "name"):
                self.ids.add(value)
            elif name in ("href", "src"):
                self.refs.append(value)


def _parse(path):
    page = _Page()
    page.feed(path.read_text(encoding="utf-8", errors="replace"))
    return page


def check_links(site_dir):
    """Every relative href/src resolves to a file; a #fragment to an id in it."""
    site_dir = Path(site_dir).resolve()
    pages = {p: _parse(p) for p in sorted(site_dir.rglob("*.html"))}
    problems = []
    for src, page in pages.items():
        rel_src = src.relative_to(site_dir)
        for ref in page.refs:
            parts = urlsplit(ref)
            if parts.scheme or parts.netloc:
                continue
            if parts.path.startswith("/"):
                target = site_dir / unquote(parts.path).lstrip("/")
            elif parts.path:
                target = src.parent / unquote(parts.path)
            else:
                target = src
            target = target.resolve()
            if target.is_dir():
                target = target / "index.html"
            if not target.is_file():
                problems.append(f"{rel_src}: {ref}: no such file")
                continue
            if parts.fragment and parts.fragment != "top" and target.suffix == ".html":
                if target not in pages:
                    pages[target] = _parse(target)
                if unquote(parts.fragment) not in pages[target].ids:
                    problems.append(f"{rel_src}: {ref}: no id #{parts.fragment}")
    return problems


# ---- moved files ----------------------------------------------------------


def _tracked_text_files(repo):
    out = subprocess.run(
        ["git", "-C", str(repo), "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout.decode()
    for rel in filter(None, out.split("\0")):
        if rel in MOVED_IGNORED_FILES or rel.startswith(MOVED_IGNORED_PREFIXES):
            continue
        path = Path(repo) / rel
        if not path.is_file():
            continue
        try:
            yield rel, path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue


def check_moved(repo, moved):
    """References, in tracked text files, to moved paths that are already gone."""
    repo = Path(repo)
    patterns = []
    for rel in moved:
        if (repo / rel).exists():
            continue
        name = re.escape(Path(rel).name)
        # By full path, or by bare name; neither as the tail of another path.
        # A bare name is captured with the rest of its path-like token, so a
        # link such as `x.md` or `x.md#top` can be resolved.
        path = re.compile("(?<![\\w./-])" + re.escape(rel))
        bare = re.compile("(?<![\\w./-])(" + name + ")[\\w./#-]*")
        patterns.append((rel, path, bare))
    problems = []
    if not patterns:
        return problems
    for rel, text in _tracked_text_files(repo):
        here = (repo / rel).parent
        for lineno, line in enumerate(text.splitlines(), 1):
            for moved_rel, path, bare in patterns:
                hit = path.search(line) or any(
                    not (here / m.group(0).split("#")[0].rstrip(".")).is_file()
                    for m in bare.finditer(line)
                )
                if hit:
                    problems.append(f"{rel}:{lineno}: refers to moved file {moved_rel}")
    return problems


# ---- command-line flags ---------------------------------------------------

_OPTION = re.compile(
    r"^\s+(?:(-[A-Za-z0-9]),?\s*)?(--[A-Za-z0-9][A-Za-z0-9-]*)?(?:\.\.\.)?(?:[ <\[=]|$)"
)
_COMMAND = re.compile(r"^  ([a-z][a-z0-9-]*)(?:\s|$)")


def _help(binary, path):
    return subprocess.run(
        [str(binary), *path, "--help"], check=True, capture_output=True, text=True
    ).stdout


def cli_flags(binary, _path=()):
    """Every --long and -s option of `binary --help`, and of its subcommands."""
    text = _help(binary, _path)
    flags = set()
    commands = []
    section = None
    for line in text.splitlines():
        if re.match(r"^[A-Z][A-Za-z ]*:$", line):
            section = line[:-1]
            continue
        if section == "Options":
            m = _OPTION.match(line)
            if m:
                flags.update(f for f in m.groups() if f)
        elif section == "Commands":
            m = _COMMAND.match(line)
            if m and m.group(1) != "help":
                commands.append(m.group(1))
    for name in commands:
        flags |= cli_flags(binary, (*_path, name))
    return flags


_LONG_FLAG = re.compile(r"(?<![\w-])(--[A-Za-z0-9][A-Za-z0-9-]*)")
_SPAN = re.compile(r"`([^`]+)`")
_COMMAND_LINE = re.compile(r"^(?:\$ )?irsync ")


def _flags_in(text):
    return _LONG_FLAG.findall(text)


def check_flags(paths, flags):
    """--flags on documented irsync command lines, and in the first column of
    the tables of book/src/reference/command-line.md, must exist in `flags`."""
    problems = []
    for path in map(Path, paths):
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        is_table_page = path.as_posix().endswith("book/src/reference/command-line.md")
        in_fence = False
        for lineno, line in enumerate(text.splitlines(), 1):
            candidates = []
            if line.lstrip().startswith(("```", "~~~")):
                in_fence = not in_fence
                continue
            if in_fence:
                stripped = line.strip()
                if _COMMAND_LINE.match(stripped):
                    candidates.append(stripped)
            else:
                for span in _SPAN.findall(line):
                    if _COMMAND_LINE.match(span.strip()):
                        candidates.append(span)
                if is_table_page and line.lstrip().startswith("|"):
                    cells = line.strip().strip("|").split("|")
                    if cells:
                        candidates.append(cells[0])
            for cand in candidates:
                for flag in _flags_in(cand):
                    if flag not in flags:
                        problems.append(f"{path}:{lineno}: unknown flag {flag}")
    return problems


# ---- CLI ------------------------------------------------------------------


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--irsync", type=Path, help="irsync binary, to check documented flags")
    ap.add_argument("site_dir", type=Path)
    args = ap.parse_args(argv)

    repo = Path(
        subprocess.run(
            ["git", "-C", str(Path(__file__).resolve().parent), "rev-parse", "--show-toplevel"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    )
    problems = check_links(args.site_dir) + check_moved(repo, MOVED_FILES)
    if args.irsync:
        pages = sorted((repo / "book/src").rglob("*.md"))
        pages += [repo / "site/index.html", repo / "README.md"]
        problems += check_flags(pages, cli_flags(args.irsync))
    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
