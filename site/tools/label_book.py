#!/usr/bin/env python3
"""Label one copy of the book with the version it documents.

The site holds the book several times: /book/, /stable/, /beta/, /alpha/
and /dev/ (see site/tools/versions.py). site/build.sh builds each from a
working copy under target/: today's book.toml and theme, and the pages
(book/src) of the version's commit. This tool makes such a copy say what it
is, before mdbook builds it:

- the address the book is served at, for mdbook's 404 page;
- "Edit this page" only for the unreleased book: a released page cannot be
  edited at its tag;
- one sentence under the heading of the title page;
- theme/source.js, which tells theme/home-link.js where the book was built
  from: BRANCH@COMMIT for the unreleased book (the commit alone when no
  branch is checked out), release-X.Y@vVERSION for a release, each part
  linked to that branch, commit or tag on GitHub;
- links to files of the repository: a page links to a source file with a
  relative path from book/src up to the repository, such as
  [`delta.rs`](../../../crates/irsync/src/delta.rs). Each becomes a link to
  that file on GitHub at the book's own version: the tag for a release, the
  commit for the unreleased book, main when the commit is not known. A path
  that is not in that version stops the build.

It only ever writes into the working copy. The book/ directory of the
repository, and the pages of a release, stay as they are.

Usage: label_book.py WORK PATH VERSION COMMIT BRANCH
    (VERSION is - for the unreleased book; COMMIT is - when it is not
    known; BRANCH, the branch checked out, is - when there is none, and is
    not used for a release)
"""

import json
import re
import subprocess
import sys
from pathlib import Path

import versions

PATHS = ("book", "stable", "beta", "alpha", "dev")
REPO_URL = "https://github.com/irsync-dev/irsync"
SOURCE_JS = "theme/source.js"
REPO = Path(__file__).resolve().parents[2]

# A link whose target starts with ../ (the climb) and then a path. Only the
# target is matched: the text of a link may be anything, code included.
_LINK = re.compile(r"\]\(((?:\.\./)+)([^()\s#]*)(#[^()\s]*)?\)")
_FENCE = re.compile(r"^\s*(```|~~~)")


class BookError(Exception):
    """The working copy cannot be labelled."""


def _settings(text, path, version):
    """book.toml with the address of this copy. The title is the same in
    every copy; the version switcher says which one it is."""
    lines = text.splitlines(keepends=True)

    def only(name, start):
        found = [i for i, line in enumerate(lines) if line.startswith(start)]
        if len(found) != 1:
            raise BookError(f"{path}: book.toml must have the line `{start.strip()}` once, "
                            f"to set the {name}")
        return found[0]

    lines[only("site-url", 'site-url = "/book/"\n')] = f'site-url = "/{path}/"\n'
    edit = only("edit-url-template", "edit-url-template = ")
    if version:
        del lines[edit]
    return "".join(lines)


def _source(version, commit, branch):
    """Where the book was built from: the parts home-link.js joins with "@",
    each with its link."""
    def tree(ref):
        return {"text": ref, "url": f"{REPO_URL}/tree/{ref}"}

    if version:
        x, y = versions.parse(version)[:2]
        return [tree(f"release-{x}.{y}"), tree(f"v{version}")]
    parts = [tree(branch)] if branch else []
    return parts + [{"text": commit[:7], "url": f"{REPO_URL}/commit/{commit}"}]


def _with_source_js(text, path):
    """book.toml with theme/source.js loaded before theme/home-link.js."""
    old = '"theme/home-link.js"'
    found = [line for line in text.splitlines() if line.startswith("additional-js = ")]
    if len(found) != 1 or old not in found[0]:
        raise BookError(f"{path}: book.toml must load {old} in its one additional-js line")
    return text.replace(found[0], found[0].replace(old, f'"{SOURCE_JS}", {old}'))


def _repo_paths(repo, commit, worktree):
    """The files and the directories of the repository at `repo`: of its
    working tree (tracked and new files) when `worktree` is true, else of
    `commit`."""
    if worktree:
        args = ["ls-files", "-z", "--cached", "--others", "--exclude-standard"]
        what = f"the working tree of {repo}"
    else:
        args = ["ls-tree", "-r", "-z", "--name-only", commit]
        what = f"commit {commit}"
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    if r.returncode != 0:
        raise BookError(f"cannot list the files of {what}: "
                        f"{r.stderr.decode(errors='replace').strip()}")
    files = {f for f in r.stdout.decode().split("\0") if f}
    dirs = {"/".join(f.split("/")[:i]) for f in files for i in range(1, f.count("/") + 1)}
    return files, dirs


def _source_links(src, ref, paths):
    """Each page under `src` with its links to files of the repository
    turned into links to GitHub at `ref`, for the pages that have any.
    `paths` is called, once, for the files and directories to check the
    links against. Raises BookError naming every link that does not work."""
    found, problems, listing = {}, [], None
    for page in sorted(src.rglob("*.md")):
        rel = page.relative_to(src)
        # From book/src/DIR/page.md: one ../ per DIR, then book/src.
        climb = len(rel.parts) + 1
        text = page.read_text(encoding="utf-8")
        lines, fenced, changed = text.split("\n"), False, False
        for n, line in enumerate(lines):
            if _FENCE.match(line):
                fenced = not fenced
            if fenced or "](../" not in line:
                continue

            def link(m):
                nonlocal listing, changed
                ups, path, anchor = len(m.group(1)) // 3, m.group(2), m.group(3) or ""
                if ups < climb:
                    return m.group(0)  # a page or file of the book
                where = f"{rel.as_posix()}:{n + 1}: {m.group(1)}{path}"
                if ups > climb:
                    problems.append(f"{where} goes above the repository")
                    return m.group(0)
                path = path.rstrip("/")
                if any(part in (".", "..") for part in path.split("/")):
                    problems.append(f"{where}: write the path without . or ..")
                    return m.group(0)
                if listing is None:
                    listing = paths()
                files, dirs = listing
                if path in files:
                    kind = "blob"
                elif path in dirs or path == "":
                    kind = "tree"
                else:
                    problems.append(f"{where}: no such file or directory at {ref}")
                    return m.group(0)
                changed = True
                url = f"{REPO_URL}/{kind}/{ref}/{path}".rstrip("/")
                return f"]({url}{anchor})"

            # Code spans hold no links: only the parts between them are read.
            parts = line.split("`")
            parts[::2] = [_LINK.sub(link, p) for p in parts[::2]]
            lines[n] = "`".join(parts)
        if changed:
            found[page] = "\n".join(lines)
    if problems:
        raise BookError("links to the repository that do not work:\n  " + "\n  ".join(problems))
    return found


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


def prepare(work, path, version, commit=None, branch=None, repo=REPO):
    """Label the working copy at `work`, served at /PATH/. `version` is None
    for the unreleased book; `commit`, the full commit the pages are from, is
    None when it is not known, and then the title names no source. `branch`
    is the branch checked out for the unreleased book, None when there is
    none; a release names its release branch instead. `repo` is the
    repository the links to source files are checked against: its working
    tree for the unreleased book, `commit` for a release."""
    if path not in PATHS:
        raise BookError(f"unknown path {path!r}: one of {', '.join(PATHS)}")
    if version is not None and versions.parse(version) is None:
        raise BookError(f"{path}: {version!r} is not a release version")
    if commit is not None and not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise BookError(f"{path}: {commit!r} is not a full commit")
    if branch is not None and not re.fullmatch(r"[A-Za-z0-9._/-]+", branch):
        raise BookError(f"{path}: {branch!r} is not a branch name this tool links to")
    toml, intro = Path(work) / "book.toml", Path(work) / "src" / "introduction.md"
    try:
        settings = _settings(toml.read_text(encoding="utf-8"), path, version)
        page = _title_page(intro.read_text(encoding="utf-8"), path, version)
        if commit:
            settings = _with_source_js(settings, path)
    except FileNotFoundError as e:
        raise BookError(f"{path}: {e.filename} is missing") from e
    ref = f"v{version}" if version else commit or "main"
    if version and not commit:
        def paths():
            raise BookError(f"{path}: the pages link to the repository, "
                            f"but the commit of {version} is not known")
    else:
        def paths():
            return _repo_paths(repo, commit, worktree=version is None)
    try:
        pages = _source_links(Path(work) / "src", ref, paths)
    except BookError as e:
        raise BookError(f"{path}: {e}") from e
    if intro in pages:
        page = _title_page(pages.pop(intro), path, version)
    # Everything is worked out before anything is written.
    for p, text in pages.items():
        p.write_text(text, encoding="utf-8")
    toml.write_text(settings, encoding="utf-8")
    intro.write_text(page, encoding="utf-8")
    if commit:
        (Path(work) / SOURCE_JS).write_text(
            "// Written by site/tools/label_book.py: where this book was built from.\n"
            f"window.irsyncSource = {json.dumps(_source(version, commit, branch))};\n",
            encoding="utf-8")


def main(argv):
    if len(argv) != 6:
        print("usage: label_book.py WORK PATH VERSION COMMIT BRANCH", file=sys.stderr)
        return 2
    work, path, version, commit, branch = argv[1:]

    def given(arg):
        return None if arg == "-" else arg

    try:
        prepare(Path(work), path, given(version), given(commit),
                None if version != "-" else given(branch))
    except BookError as e:
        print(f"label_book.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
