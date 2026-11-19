#!/usr/bin/env python3
"""Check the built site: links, no fonts, references to moved files, documented
flags."""

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
            elif not target.exists() and target.with_name(target.name + ".html").is_file():
                # x for x.html, as Cloudflare serves the page (pretty_urls.py).
                target = target.with_name(target.name + ".html")
            if not target.is_file():
                problems.append(f"{rel_src}: {ref}: no such file")
                continue
            if parts.fragment and parts.fragment != "top" and target.suffix == ".html":
                if target not in pages:
                    pages[target] = _parse(target)
                if unquote(parts.fragment) not in pages[target].ids:
                    problems.append(f"{rel_src}: {ref}: no id #{parts.fragment}")
    return problems


def check_html_links(site_dir):
    """No link inside the site ends in .html. Cloudflare serves x.html at x
    and redirects x.html there; mdbook's sidebar only marks the current
    chapter when the link and the address agree (site/tools/pretty_urls.py)."""
    site_dir = Path(site_dir).resolve()
    problems = []
    for src in sorted(site_dir.rglob("*.html")):
        for ref in _parse(src).refs:
            parts = urlsplit(ref)
            if parts.scheme or parts.netloc or not parts.path.endswith(".html"):
                continue
            better = parts.path[: -len(".html")]
            if better == "index" or better.endswith("/index"):
                better = better[: -len("index")] or "./"
            problems.append(f"{src.relative_to(site_dir)}: {ref}: link to {better} instead; "
                            "the site redirects .html addresses")
    return problems


# ---- fonts ----------------------------------------------------------------

_FONT_SUFFIXES = {".woff", ".woff2", ".ttf", ".otf", ".eot"}

# The web fonts that ship: Barlow, Inter and JetBrains Mono, in the
# top-level fonts/ directory (see site/build.sh), which the book shares.
_ALLOWED_FONT = re.compile(
    r"fonts/(barlow|barlow-semi-condensed|inter|jetbrains-mono)-latin-\d{3}-(normal|italic)\.woff2"
)


def check_fonts(site_dir):
    """No font files and no `fonts/` directory ship, except the site's own
    web fonts in the top-level `fonts/`. The book loads those from there;
    this keeps mdbook's web fonts out."""
    site_dir = Path(site_dir).resolve()
    problems = []
    for p in sorted(site_dir.rglob("*")):
        rel = p.relative_to(site_dir)
        if p.is_dir() and p.name == "fonts":
            if rel.as_posix() != "fonts":
                problems.append(f"{rel}: a fonts directory ships")
        elif p.is_file() and p.suffix.lower() in _FONT_SUFFIXES:
            if not _ALLOWED_FONT.fullmatch(rel.as_posix()):
                problems.append(f"{rel}: a font file ships")
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


def _is_command(binary, name):
    """Whether `name` is a command of `binary`, listed in its help or not.

    `binary NAME --help` describes the command when NAME is one, and the
    whole program when NAME is only an argument; the usage line tells.
    """
    r = subprocess.run([str(binary), name, "--help"], capture_output=True, text=True)
    usage = re.search(r"^Usage: \S+ (\S+)", r.stdout, re.MULTILINE)
    return r.returncode == 0 and usage is not None and usage.group(1) == name


def cli_flags(binary, _path=(), also=()):
    """Every --long and -s option of `binary --help`, and of its subcommands.

    The help does not list a hidden command, such as `irsync
    verify-signature`. `also` names words to try as top-level commands too;
    those that are not commands are ignored.
    """
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
    commands += sorted(n for n in set(also) - set(commands) if _is_command(binary, n))
    for name in commands:
        flags |= cli_flags(binary, (*_path, name))
    return flags


_LONG_FLAG = re.compile(r"(?<![\w-])(--[A-Za-z0-9][A-Za-z0-9-]*)")
_SPAN = re.compile(r"`([^`]+)`")
_COMMAND_LINE = re.compile(r"^(?:\$ )?irsync ")


def _flags_in(text):
    return _LONG_FLAG.findall(text)


def _documented(paths):
    """(path, line number, text) of every place whose flags are checked: the
    irsync command lines in code blocks and inline code, and the first
    column of the tables of book/src/reference/command-line.md."""
    for path in map(Path, paths):
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        is_table_page = path.as_posix().endswith("book/src/reference/command-line.md")
        in_fence = False
        for lineno, line in enumerate(text.splitlines(), 1):
            if line.lstrip().startswith(("```", "~~~")):
                in_fence = not in_fence
                continue
            if in_fence:
                stripped = line.strip()
                if _COMMAND_LINE.match(stripped):
                    yield path, lineno, stripped
            else:
                for span in _SPAN.findall(line):
                    if _COMMAND_LINE.match(span.strip()):
                        yield path, lineno, span
                if is_table_page and line.lstrip().startswith("|"):
                    cells = line.strip().strip("|").split("|")
                    if cells:
                        yield path, lineno, cells[0]


_FIRST_WORD = re.compile(r"^(?:\$ )?irsync ([a-z][a-z0-9-]*)(?:\s|$)")


def documented_commands(paths):
    """The first word after `irsync` on every documented command line: the
    commands the pages use, and some words that are only arguments."""
    words = set()
    for _path, _lineno, text in _documented(paths):
        m = _FIRST_WORD.match(text.strip())
        if m:
            words.add(m.group(1))
    return words


def check_flags(paths, flags):
    """--flags on documented irsync command lines, and in the first column of
    the tables of book/src/reference/command-line.md, must exist in `flags`."""
    problems = []
    for path, lineno, text in _documented(paths):
        for flag in _flags_in(text):
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
    problems = check_links(args.site_dir) + check_html_links(args.site_dir)
    problems += check_fonts(args.site_dir)
    problems += check_moved(repo, MOVED_FILES)
    if args.irsync:
        pages = sorted((repo / "book/src").rglob("*.md"))
        pages += [repo / "site/index.html", repo / "README.md"]
        # The pages may use a command the help hides; its flags count too.
        problems += check_flags(pages, cli_flags(args.irsync, also=documented_commands(pages)))
    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
