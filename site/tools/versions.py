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

import argparse
import io
import re
import shutil
import subprocess
import sys
import tarfile
from collections import namedtuple
from pathlib import Path

CHANNELS = ("stable", "beta", "alpha")
_RANK = {"alpha": 0, "beta": 1, "stable": 2}
_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta)\.(\d+))?")

_RELEASE_BRANCH = re.compile(r"refs/remotes/origin/release-\d+\.\d+")
_WORKSPACE_VERSION = re.compile(
    r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]*)"', re.MULTILINE)

# How the install command on the page ends, by the channel it installs.
_TAIL = {"stable": "sh", "beta": "sh -s -- --beta", "alpha": "sh -s -- --alpha"}
_MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")

# One version on the page. The label links to the channel's book, the date
# to the release on GitHub (that page appears when the release is
# published). The script in index.html turns span.version into a button that
# copies the install command ending in data-tail. Tailwind reads the classes
# from this file (@source in site/tailwind.css).
_RELEASE_URL = "https://github.com/irsync-dev/irsync/releases/tag/v{version}"
_LINE = (
    '<span class="inline-flex items-baseline gap-x-1.5 font-condensed font-normal whitespace-nowrap">'
    '<strong class="font-semibold"><a class="text-muted" href="{channel}/">{label}</a></strong> '
    '<span class="version" data-tail="{tail}">{version}</span> '
    '<span>· <a class="text-muted" href="' + _RELEASE_URL + '">{date}</a></span></span>'
)


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


def newest(versions):
    """The newest version of each channel, obsolete or not. What is not a
    release version is ignored."""
    best = {}
    for text in versions:
        key = parse(text)
        if key is None:
            continue
        c = channel(text)
        if c not in best or key > parse(best[c]):
            best[c] = text
    return best


def pick(candidates):
    """The versions to show, in the order stable, beta, alpha.

    `candidates` maps a version to its date. Keys that are not release
    versions are ignored.
    """
    best = newest(candidates)
    shown = []
    for i, c in enumerate(CHANNELS):
        if c not in best:
            continue
        key = parse(best[c])
        # Held against every higher channel that has a version.
        if all(key > parse(best[o]) for o in CHANNELS[:i] if o in best):
            shown.append(Shown(c, best[c], candidates[best[c]]))
    return shown


def _git(repo, *args):
    """Run git in `repo`: (it worked, its output)."""
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    return r.returncode == 0, (r.stdout if r.returncode == 0 else r.stderr).strip()


# Where a version comes from. `date` is an ISO day. `commit` is the commit
# its documentation is read from. `published` says whether a tag has it.
Release = namedtuple("Release", "date commit published")


def releases(repo):
    """Every release version the checkout at `repo` knows.

    A `v*` tag is a published release: GitHub creates the tag when the
    release is published. The workspace version in Cargo.toml, at HEAD and at
    the tip of every origin/release-X.Y branch, is a release that is being
    built, when no tag has it yet.

    A date is the day in the commit, in the commit's own time zone: for a
    tag, of the commit it points at; for a version being built, of the
    commit that brought it onto the branch.

    The commit is the one the tag points at; for a version being built, the
    tip of the branch it was found on, which is what the release will be cut
    from.
    """
    # For an annotated tag the starred fields are the commit's and the plain
    # date is empty; for a lightweight tag the starred ones are empty.
    ok, out = _git(repo, "for-each-ref",
                   "--format=%(refname:short) %(*committerdate:short)%(committerdate:short)"
                   " %(*objectname) %(objectname)",
                   "refs/tags/v*")
    if not ok:
        raise VersionsError(f"cannot list the tags of {repo}: {out}")
    found = {}
    for line in out.splitlines():
        fields = line.split()
        if len(fields) < 3 or parse(fields[0][1:]) is None:
            continue
        # Three fields: a lightweight tag, its commit last. Four: an
        # annotated tag, the commit before the tag object.
        found[fields[0][1:]] = Release(fields[1], fields[2], True)
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
        tip_ok, tip = _git(repo, "rev-parse", f"{ref}^{{commit}}")
        if ok and date and tip_ok:
            found[version] = Release(date, tip, False)
    return found


def from_git(repo):
    """Every release version the checkout at `repo` knows, with its date."""
    return {version: r.date for version, r in releases(repo).items()}


# One copy of the documentation on the site, at /PATH/. `commit` is where
# its pages come from. For a release, `state` is "published" or "unpublished"
# (a release that is being built). A book of the working tree has no version:
# its commit is the one that is checked out, and its state is the name of the
# branch, None when no branch is checked out.
Book = namedtuple("Book", "path version commit state")


def head(repo):
    """What is checked out: (commit, branch). The commit is what the BUILD
    file of the site names. The branch is None for a checkout of a commit."""
    ok, commit = _git(repo, "rev-parse", "HEAD^{commit}")
    if not ok:
        raise VersionsError(f"cannot read the commit of {repo}: {commit}")
    ok, branch = _git(repo, "symbolic-ref", "--short", "-q", "HEAD")
    return commit, branch if ok and branch else None


def books(found, commit=None, branch=None):
    """The books the site holds, from what `releases` found. `commit` and
    `branch` say what the working tree is (see `head`).

    /book/ is the main version, the one the install command installs.
    /stable/, /beta/ and /alpha/ are the newest of each channel, also when
    the landing page hides it as obsolete. /dev/ is the working tree. With no
    version at all, /book/ is the working tree too.
    """
    def book(path, version):
        r = found[version]
        return Book(path, version, r.commit, "published" if r.published else "unpublished")

    best = newest(found)
    shown = pick({version: r.date for version, r in found.items()})
    out = [book("book", shown[0].version) if shown else Book("book", None, commit, branch)]
    out += [book(c, best[c]) for c in CHANNELS if c in best]
    out.append(Book("dev", None, commit, branch))
    return out


def pages(repo, commit, dest):
    """Write the book's pages (book/src) as they are in `commit` to `dest`.

    All or nothing: git has to hand over the whole tree before anything is
    written, and `dest` appears only once it is complete. A book built from
    half its pages would still carry the version's name.
    """
    dest = Path(dest)
    if dest.exists():
        raise VersionsError(f"{dest} exists already")
    ok, out = _git(repo, "cat-file", "-e", f"{commit}^{{commit}}")
    if not ok:
        raise VersionsError(f"cannot read the commit {commit} in {repo}: {out}")
    ok, kind = _git(repo, "cat-file", "-t", f"{commit}:book/src")
    if not ok or kind != "tree":
        raise VersionsError(f"no book/src in {commit}")
    r = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", commit, "book/src"],
                       capture_output=True)
    if r.returncode != 0:
        raise VersionsError(f"cannot read book/src of {commit}: "
                            f"{r.stderr.decode(errors='replace').strip()}")
    tmp = dest.with_name(dest.name + ".part")
    shutil.rmtree(tmp, ignore_errors=True)
    try:
        with tarfile.open(fileobj=io.BytesIO(r.stdout)) as tar:
            tar.extractall(tmp, filter="data")
        dest.parent.mkdir(parents=True, exist_ok=True)
        (tmp / "book" / "src").rename(dest)
    except (tarfile.TarError, OSError) as e:
        raise VersionsError(f"cannot unpack book/src of {commit}: {e}") from e
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def books_text(books):
    """The VERSIONS file of a built site: PATH VERSION COMMIT STATE per book."""
    return "".join(f"{b.path} {b.version or '-'} {b.commit or '-'} {b.state or '-'}\n" for b in books)


_PATHS = ("book", *CHANNELS, "dev")
_STATES = ("published", "unpublished")


def unpublished(text):
    """The versions a built site holds that are not published, from its
    VERSIONS file (see books_text). Such a build must not be promoted: it
    names a version nobody can install, and its book was read from a branch
    that may have moved on since.

    A file that cannot be read for sure is an error, never "all published".
    """
    lines = text.splitlines()
    out = []
    for line in lines:
        fields = line.split(" ")
        if len(fields) != 4:
            raise VersionsError(f"not PATH VERSION COMMIT STATE: {line!r}")
        path, version, commit, state = fields
        if path not in _PATHS:
            raise VersionsError(f"unknown path: {line!r}")
        if version == "-":
            # A book of the working tree: its last field is a branch name,
            # whatever that is. "-" for the commit, and "dev" for the branch:
            # a build from before the line carried them.
            if path not in ("book", "dev") or not state \
                    or not re.fullmatch(r"-|[0-9a-f]{40}", commit):
                raise VersionsError(f"not a book of the working tree: {line!r}")
            continue
        if state not in _STATES:
            raise VersionsError(f"unknown state: {line!r}")
        if parse(version) is None or not re.fullmatch(r"[0-9a-f]{40}", commit):
            raise VersionsError(f"not a release version and a commit: {line!r}")
        if state == "unpublished" and version not in out:
            out.append(version)
    # The shape the build writes: the main book once, no path twice, and the
    # dev book last. Anything else is a file that was cut short or made up.
    paths = [line.split(" ")[0] for line in lines]
    if paths.count("book") != 1 or len(set(paths)) != len(paths):
        raise VersionsError("it must list every book once, the main book among them")
    if not paths or paths[-1] != "dev":
        raise VersionsError("no dev line at the end: the file is cut short")
    return out


# How often each region is on the page. prerelease: the badge and the note.
_REGIONS = {"lines": 1, "tail": 1, "channel": 1, "Channel": 1, "prerelease": 2}


def _fill(html, name, fill):
    """Replace every `versions:NAME` region of `html`, markers included.

    `fill` gets what the region holds and returns what takes its place. The
    page must hold the region as often as _REGIONS says, with every marker
    paired: a lost marker would let one region run on to the next one's end.
    """
    opening, closing = f"<!-- versions:{name} -->", f"<!-- /versions:{name} -->"
    n = html.count(opening)
    if n == 0:
        raise VersionsError(f"site/index.html: marker {name} not found")
    if n != _REGIONS[name] or html.count(closing) != n:
        raise VersionsError(
            f"site/index.html: marker {name} found {n} times, expected {_REGIONS[name]}, "
            f"with {html.count(closing)} closing")
    region = re.compile(re.escape(opening) + "(.*?)" + re.escape(closing), re.DOTALL)
    html, done = region.subn(lambda m: fill(m.group(1)), html)
    if done != n:
        raise VersionsError(f"site/index.html: marker {name} is not paired up")
    return html


def _day(iso):
    year, month, day = iso.split("-")
    return f"{int(day)} {_MONTHS[int(month) - 1]} {year}"


def render(html, shown):
    """The landing page `html` with its `versions:` regions filled in.

    lines       one line per shown version
    tail        the end of the install command, for the main version
    prerelease  kept while the main version is a beta or an alpha, else dropped
    channel     the main version's channel, inside a prerelease region
    Channel     the same, capitalised
    """
    main = shown[0].channel if shown else None
    pre = main in ("beta", "alpha")
    lines = "".join(
        _LINE.format(channel=s.channel, label=s.channel.capitalize(), tail=_TAIL[s.channel],
                     version=s.version, date=_day(s.date))
        for s in shown)
    html = _fill(html, "lines", lambda _: lines)
    html = _fill(html, "tail", lambda _: _TAIL.get(main, "sh"))
    # The names are checked on the whole page first: a dropped prerelease
    # region takes its channel markers with it.
    html = _fill(html, "channel", lambda _: main if pre else "")
    html = _fill(html, "Channel", lambda _: main.capitalize() if pre else "")
    return _fill(html, "prerelease", lambda inner: inner if pre else "")


def main(argv=None):
    ap = argparse.ArgumentParser(description="The release versions on the landing page, and the books.")
    sub = ap.add_subparsers(dest="command", required=True)
    p = sub.add_parser("render", help="write the release versions into the landing page")
    p.add_argument("repo", type=Path)
    p.add_argument("src", type=Path)
    p.add_argument("dst", type=Path)
    p = sub.add_parser("books", help="list the books to build: PATH VERSION COMMIT STATE")
    p.add_argument("repo", type=Path)
    p = sub.add_parser("pages", help="write the book/src of a commit to a new directory")
    p.add_argument("repo", type=Path)
    p.add_argument("commit")
    p.add_argument("dest", type=Path)
    p = sub.add_parser("promotable", help="fail if a built site's VERSIONS holds an unpublished version")
    p.add_argument("file", type=Path)
    args = ap.parse_args(argv)
    try:
        if args.command == "render":
            page = render(args.src.read_text(encoding="utf-8"), pick(from_git(args.repo)))
            args.dst.write_text(page, encoding="utf-8")
        elif args.command == "books":
            sys.stdout.write(books_text(books(releases(args.repo), *head(args.repo))))
        elif args.command == "pages":
            pages(args.repo, args.commit, args.dest)
        elif args.command == "promotable":
            try:
                text = args.file.read_text(encoding="utf-8")
            except FileNotFoundError:
                raise VersionsError(f"{args.file} is missing: this build was made before "
                                    "VERSIONS existed. Re-run the staging build.") from None
            try:
                held = unpublished(text)
            except VersionsError as e:
                raise VersionsError(f"{args.file}: {e}") from None
            if held:
                raise VersionsError(f"this build holds {', '.join(held)}, which is not published. "
                                    "Publish the release, re-run the staging build, then promote.")
    except VersionsError as e:
        print(f"versions.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
