import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import versions
from versions import Shown

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "site" / "tools" / "versions.py"
D = "2026-11-19"


class RepoCase(unittest.TestCase):
    """A throwaway git repository, kept clear of the developer's git settings."""

    def setUp(self):
        self.repo = Path(tempfile.mkdtemp(prefix="site-versions-"))
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")

    def git(self, *args, env=None):
        full = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
        full.update(env or {})
        r = subprocess.run(["git", "-C", str(self.repo), *args], env=full,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.strip()

    def commit(self, cargo_version, when):
        """Commit a Cargo.toml with this workspace version, dated `when`."""
        (self.repo / "Cargo.toml").write_text(
            f'[workspace]\n\n[workspace.package]\nversion = "{cargo_version}"\nedition = "2024"\n')
        self.git("add", "Cargo.toml")
        self.git("commit", "-q", "--allow-empty", "-m", f"version {cargo_version}",
                 env={"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})


class ParseTests(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(versions.parse("1.2.3"), (1, 2, 3, 2, 0))
        self.assertEqual(versions.parse("0.1.0-beta.5"), (0, 1, 0, 1, 5))
        self.assertEqual(versions.parse("0.1.0-alpha.4"), (0, 1, 0, 0, 4))

    def test_not_a_release(self):
        for t in ["1.2", "v1.2.3", "0.1.0-rc.1", "0.1.0-beta", "0.1.0-beta.x",
                  "next", "", "1.2.3 ", "1.2.3-beta.1.2"]:
            self.assertIsNone(versions.parse(t), t)

    def test_numbers_compare_as_numbers(self):
        self.assertGreater(versions.parse("0.1.0-beta.10"), versions.parse("0.1.0-beta.9"))
        self.assertGreater(versions.parse("0.10.0"), versions.parse("0.9.0"))
        self.assertGreater(versions.parse("0.1.0"), versions.parse("0.1.0-beta.99"))
        self.assertGreater(versions.parse("0.1.0-beta.1"), versions.parse("0.1.0-alpha.99"))

    def test_channel(self):
        self.assertEqual(versions.channel("1.2.3"), "stable")
        self.assertEqual(versions.channel("0.1.0-beta.5"), "beta")
        self.assertEqual(versions.channel("0.1.0-alpha.4"), "alpha")


class PickTests(unittest.TestCase):
    def shown(self, *vs):
        return [(s.channel, s.version) for s in versions.pick({v: D for v in vs})]

    def test_spec_table(self):
        self.assertEqual(self.shown("0.1.0-beta.5", "0.1.0-alpha.4"), [("beta", "0.1.0-beta.5")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.1.0-alpha.4"), [("stable", "0.1.0")])
        self.assertEqual(self.shown("0.1.0", "0.2.0-beta.1", "0.2.0-alpha.3"),
                         [("stable", "0.1.0"), ("beta", "0.2.0-beta.1")])
        self.assertEqual(self.shown("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1"),
                         [("stable", "0.1.2"), ("beta", "0.2.0-beta.1"), ("alpha", "0.3.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("alpha", "0.2.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0-alpha.4"), [("alpha", "0.1.0-alpha.4")])
        self.assertEqual(self.shown(), [])

    def test_alpha_is_held_against_a_hidden_beta(self):
        # The beta is hidden by stable 0.2.0, and still hides the alpha.
        self.assertEqual(self.shown("0.2.0", "0.2.0-beta.1", "0.2.0-alpha.9"), [("stable", "0.2.0")])
        # An alpha between a hidden beta and the stable: newer than the stable
        # is not enough, it must beat the beta too.
        self.assertEqual(self.shown("0.1.0", "0.3.0-beta.1", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("beta", "0.3.0-beta.1")])

    def test_newest_of_each_channel_and_junk_ignored(self):
        self.assertEqual(self.shown("0.1.0-beta.9", "0.1.0-beta.10", "0.1.0-rc.1", "next"),
                         [("beta", "0.1.0-beta.10")])

    def test_keeps_the_date(self):
        self.assertEqual(versions.pick({"1.0.0": "2027-03-03"}), [Shown("stable", "1.0.0", "2027-03-03")])


class FromGitTests(RepoCase):
    def test_tags_and_dates(self):
        self.commit("0.1.0-alpha.4", "2026-11-13T12:00:00+00:00")
        self.git("tag", "v0.1.0-alpha.4")
        # Just after midnight in +01:00: the commit's own day, not the UTC day before.
        self.commit("0.1.0-beta.5", "2026-11-19T00:30:00+01:00")
        self.git("tag", "v0.1.0-beta.5")
        self.assertEqual(versions.from_git(self.repo),
                         {"0.1.0-alpha.4": "2026-11-13", "0.1.0-beta.5": "2026-11-19"})

    def test_annotated_tag_uses_the_commit_date(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        self.git("tag", "-a", "-m", "irsync 0.1.0-beta.1", "v0.1.0-beta.1",
                 env={"GIT_COMMITTER_DATE": "2026-12-01T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo), {"0.1.0-beta.1": "2026-11-14"})

    def test_junk_tags_are_ignored(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        for t in ["v0.1.0-beta.1", "v1.2", "v0.1.0-rc.1", "vnext", "other"]:
            self.git("tag", t)
        self.assertEqual(list(versions.from_git(self.repo)), ["0.1.0-beta.1"])

    def test_no_tags_fails_and_says_what_to_do(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        with self.assertRaisesRegex(versions.VersionsError, "git fetch --tags"):
            versions.from_git(self.repo)

    def test_not_a_repository_fails(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(versions.VersionsError):
                versions.from_git(Path(d))

    def test_release_being_built_on_head(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.commit("0.1.0-beta.6", "2026-11-25T12:00:00+00:00")   # the bump
        self.commit("0.1.0-beta.6", "2026-11-27T12:00:00+00:00")   # a later commit, same version
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_bump_merged_through_a_pull_request(self):
        # The date is the merge commit's, as the tag will sit on it.
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.git("switch", "-q", "-c", "bump")
        self.commit("0.1.0-beta.6", "2026-11-24T12:00:00+00:00")
        self.git("switch", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "Merge bump", "bump",
                 env={"GIT_COMMITTER_DATE": "2026-11-25T12:00:00+00:00",
                      "GIT_AUTHOR_DATE": "2026-11-25T12:00:00+00:00"})
        self.assertEqual(versions.from_git(self.repo)["0.1.0-beta.6"], "2026-11-25")

    def test_head_version_that_is_not_a_release_is_skipped(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        self.commit("0.2.0-dev", "2026-11-25T12:00:00+00:00")
        self.assertEqual(versions.from_git(self.repo), {"0.1.0-beta.5": "2026-11-19"})

    def test_release_branches(self):
        self.commit("1.2.0", "2027-03-03T12:00:00+00:00")
        self.git("tag", "v1.2.0")
        base = self.git("rev-parse", "HEAD")
        self.commit("1.2.1", "2027-03-10T12:00:00+00:00")
        self.git("update-ref", "refs/remotes/origin/release-1.2", "HEAD")
        # Not a release branch: its name is not release-X.Y.
        self.commit("9.9.9", "2027-03-11T12:00:00+00:00")
        self.git("update-ref", "refs/remotes/origin/release-v0.1.0-beta.3", "HEAD")
        self.git("reset", "-q", "--hard", base)
        # A release branch with nothing to read.
        self.git("rm", "-q", "Cargo.toml")
        self.git("commit", "-q", "-m", "no cargo")
        self.git("update-ref", "refs/remotes/origin/release-1.1", "HEAD")
        self.git("reset", "-q", "--hard", base)
        self.assertEqual(versions.from_git(self.repo), {"1.2.0": "2027-03-03", "1.2.1": "2027-03-10"})

    def test_real_checkout(self):
        r = subprocess.run(["git", "-C", str(ROOT), "tag", "-l", "v0.1.0-beta.5"],
                           capture_output=True, text=True)
        if not r.stdout.strip():
            self.skipTest("this checkout has no release tags")
        self.assertEqual(versions.from_git(ROOT)["0.1.0-beta.5"], "2026-11-19")


class ReleasesTests(RepoCase):
    def test_tagged_and_being_built(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        tagged = self.git("rev-parse", "HEAD")
        self.commit("0.1.0-beta.6", "2026-11-25T12:00:00+00:00")
        self.commit("0.1.0-beta.6", "2026-11-27T12:00:00+00:00")
        tip = self.git("rev-parse", "HEAD")
        got = versions.releases(self.repo)
        self.assertEqual(got["0.1.0-beta.5"], versions.Release("2026-11-19", tagged, True))
        # The pages come from the tip; the date stays the bump's.
        self.assertEqual(got["0.1.0-beta.6"], versions.Release("2026-11-25", tip, False))
        self.assertEqual(versions.from_git(self.repo),
                         {"0.1.0-beta.5": "2026-11-19", "0.1.0-beta.6": "2026-11-25"})

    def test_annotated_tag_gives_the_commit_not_the_tag_object(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        self.git("tag", "-a", "-m", "x", "v0.1.0-beta.1")
        self.assertEqual(versions.releases(self.repo)["0.1.0-beta.1"].commit,
                         self.git("rev-parse", "HEAD"))

    def test_patch_being_built_on_a_release_branch(self):
        self.commit("1.2.0", "2027-03-03T12:00:00+00:00")
        self.git("tag", "v1.2.0")
        base = self.git("rev-parse", "HEAD")
        self.commit("1.2.1", "2027-03-10T12:00:00+00:00")
        branch_tip = self.git("rev-parse", "HEAD")
        self.git("update-ref", "refs/remotes/origin/release-1.2", "HEAD")
        self.git("reset", "-q", "--hard", base)
        self.assertEqual(versions.releases(self.repo)["1.2.1"],
                         versions.Release("2027-03-10", branch_tip, False))


R = versions.Release


class BooksTests(unittest.TestCase):
    def test_today(self):
        found = {"0.1.0-beta.5": R("2026-11-19", "b" * 40, True),
                 "0.1.0-alpha.4": R("2026-11-13", "a" * 40, True)}
        self.assertEqual(versions.books(found), [
            versions.Book("book", "0.1.0-beta.5", "b" * 40, "published"),
            versions.Book("beta", "0.1.0-beta.5", "b" * 40, "published"),
            # Obsolete on the homepage, still a book.
            versions.Book("alpha", "0.1.0-alpha.4", "a" * 40, "published"),
            versions.Book("dev", None, None, "dev")])

    def test_stable_is_the_main_book_and_old_channels_stay(self):
        found = {"1.0.0": R("2027-03-03", "c" * 40, True),
                 "0.9.0-beta.2": R("2027-01-01", "b" * 40, True),
                 "1.1.0-alpha.1": R("2027-04-01", "a" * 40, False)}
        got = versions.books(found)
        self.assertEqual([b.path for b in got], ["book", "stable", "beta", "alpha", "dev"])
        self.assertEqual(got[0].version, "1.0.0")
        self.assertEqual(got[2].version, "0.9.0-beta.2")
        self.assertEqual(got[3].state, "unpublished")

    def test_no_version_at_all(self):
        self.assertEqual(versions.books({}), [versions.Book("book", None, None, "dev"),
                                              versions.Book("dev", None, None, "dev")])

    def test_text(self):
        found = {"0.1.0-beta.5": R("2026-11-19", "b" * 40, True)}
        self.assertEqual(versions.books_text(versions.books(found)),
                         f"book 0.1.0-beta.5 {'b' * 40} published\n"
                         f"beta 0.1.0-beta.5 {'b' * 40} published\n"
                         "dev - - dev\n")

    def test_the_working_tree_books_carry_the_commit_of_the_build(self):
        # The same commit as the BUILD file.
        found = {"0.1.0-beta.5": R("2026-11-19", "b" * 40, True)}
        self.assertEqual(versions.books(found, "d" * 40)[-1],
                         versions.Book("dev", None, "d" * 40, "dev"))
        self.assertEqual(versions.books_text(versions.books({}, "d" * 40)),
                         f"book - {'d' * 40} dev\ndev - {'d' * 40} dev\n")


class NewestTests(unittest.TestCase):
    def test_per_channel_without_hiding(self):
        self.assertEqual(
            versions.newest(["1.0.0", "0.9.0-beta.2", "0.9.0-beta.10", "0.1.0-alpha.4", "junk"]),
            {"stable": "1.0.0", "beta": "0.9.0-beta.10", "alpha": "0.1.0-alpha.4"})


INDEX = (ROOT / "site" / "index.html").read_text()


class RenderTests(unittest.TestCase):
    def page(self, *vs):
        return versions.render(INDEX, versions.pick({v: "2026-11-19" for v in vs}))

    def test_beta_only(self):
        out = self.page("0.1.0-beta.5", "0.1.0-alpha.4")
        self.assertIn('<strong class="font-semibold"><a class="text-muted" href="beta/">Beta</a></strong> '
                      '<span class="version" data-tail="sh -s -- --beta">0.1.0-beta.5</span> '
                      '<span>· <a class="text-muted" '
                      'href="https://github.com/irsync-dev/irsync/releases/tag/v0.1.0-beta.5">'
                      '19 Nov 2026</a></span>', out)
        self.assertEqual(out.count('class="version"'), 1)
        self.assertIn('id="install-tail">sh -s -- --beta</span>', out)
        self.assertIn(">beta</span", out)        # the badge
        self.assertIn("Beta.</strong>", out)     # the note
        self.assertNotIn("versions:", out)       # no marker survives

    def test_stable_with_newer_beta_and_alpha(self):
        out = self.page("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1")
        order = [out.index(v) for v in (">0.1.2<", ">0.2.0-beta.1<", ">0.3.0-alpha.1<")]
        self.assertEqual(order, sorted(order))
        self.assertIn('href="stable/">Stable</a></strong> '
                      '<span class="version" data-tail="sh">0.1.2<', out)
        self.assertIn('data-tail="sh -s -- --beta">0.2.0-beta.1<', out)
        self.assertIn('data-tail="sh -s -- --alpha">0.3.0-alpha.1<', out)
        self.assertIn('id="install-tail">sh</span>', out)
        self.assertNotIn('role="note"', out)
        self.assertNotIn("bg-signal", out)       # the badge is gone
        self.assertNotIn("versions:", out)

    def test_links(self):
        out = self.page("0.1.2", "0.2.0-beta.1")
        self.assertIn('<a class="text-muted" href="stable/">Stable</a>', out)
        self.assertIn('<a class="text-muted" href="beta/">Beta</a>', out)
        self.assertIn('href="https://github.com/irsync-dev/irsync/releases/tag/v0.1.2">19 Nov 2026</a>', out)
        self.assertIn('href="https://github.com/irsync-dev/irsync/releases/tag/v0.2.0-beta.1">', out)

    def test_alpha_only(self):
        out = self.page("0.1.0-alpha.4")
        self.assertIn('id="install-tail">sh -s -- --alpha</span>', out)
        self.assertIn(">alpha</span", out)
        self.assertIn("Alpha.</strong>", out)

    def test_nothing_released(self):
        out = self.page()
        self.assertNotIn('class="version"', out)
        self.assertIn('id="install-tail">sh</span>', out)
        self.assertNotIn('role="note"', out)
        self.assertNotIn("bg-signal", out)

    def test_single_digit_day(self):
        out = versions.render(INDEX, versions.pick({"1.0.0": "2027-03-03"}))
        self.assertIn(">3 Mar 2027</a>", out)

    def test_the_rest_of_the_page_is_untouched(self):
        out = self.page("0.1.0-beta.5")
        self.assertIn("Resilient file sync over any network", out)
        self.assertIn('<h3 id="install"', out)
        self.assertIn("https://irsync.org/install.sh | ", out)

    def test_missing_marker_fails(self):
        for name in ["lines", "tail", "prerelease", "channel", "Channel"]:
            broken = INDEX.replace(f"<!-- versions:{name} -->", "")
            with self.assertRaisesRegex(versions.VersionsError, f"marker {name} not found"):
                versions.render(broken, [])

    def test_damaged_prerelease_marker_fails(self):
        # The badge and the note each have a region. Losing one marker must not
        # pass: with a stable out, one region would swallow the page between.
        stable = versions.pick({"1.0.0": "2027-03-03"})
        beta = versions.pick({"0.1.0-beta.5": "2026-11-19"})
        first_close = INDEX.replace("<!-- /versions:prerelease -->", "", 1)
        first_open = INDEX.replace("<!-- versions:prerelease -->", "", 1)
        badge_unmarked = first_close.replace("<!-- versions:prerelease -->", "", 1)
        for broken in (first_close, first_open, badge_unmarked):
            for shown in (stable, beta, []):
                with self.assertRaisesRegex(versions.VersionsError, "marker prerelease"):
                    versions.render(broken, shown)

    def test_a_marker_too_many_fails(self):
        doubled = INDEX.replace("</main>", "<!-- versions:tail -->x<!-- /versions:tail --></main>")
        with self.assertRaisesRegex(versions.VersionsError, "marker tail found 2 times, expected 1"):
            versions.render(doubled, [])


class CliTests(RepoCase):
    def run_tool(self, out, env=None):
        return subprocess.run(
            [sys.executable, str(TOOL), "render", str(self.repo), str(ROOT / "site/index.html"), str(out)],
            env=env, capture_output=True, text=True)

    def test_writes_utf8_whatever_the_locale(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        out = self.repo / "out.html"
        env = dict(os.environ, LC_ALL="C", LANG="C", PYTHONUTF8="0", PYTHONCOERCECLOCALE="0")
        r = self.run_tool(out, env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('<span>· <a class="text-muted"', out.read_text(encoding="utf-8"))

    def test_writes_the_page(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        out = self.repo / "out.html"
        r = self.run_tool(out)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(">0.1.0-beta.5<", out.read_text())

    def test_books_prints_the_list(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        self.git("tag", "v0.1.0-beta.5")
        sha = self.git("rev-parse", "HEAD")
        # main has moved on since the release: /dev/ names its newest commit.
        self.commit("0.1.0-beta.5", "2026-11-20T12:00:00+00:00")
        head = self.git("rev-parse", "HEAD")
        self.assertNotEqual(head, sha)
        r = subprocess.run([sys.executable, str(TOOL), "books", str(self.repo)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, f"book 0.1.0-beta.5 {sha} published\n"
                                   f"beta 0.1.0-beta.5 {sha} published\ndev - {head} dev\n")

    def test_no_tags_exits_1_and_writes_nothing(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        out = self.repo / "out.html"
        r = self.run_tool(out)
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: no release tags", r.stderr)
        self.assertFalse(out.exists())


class PagesTests(RepoCase):
    """The pages of a commit, for one book."""

    def book_commit(self, text):
        d = self.repo / "book" / "src"
        d.mkdir(parents=True, exist_ok=True)
        (d / "introduction.md").write_text(text)
        (d / "sub").mkdir(exist_ok=True)
        (d / "sub" / "page.md").write_text("# Page\n")
        self.git("add", "book")
        self.git("commit", "-q", "-m", "book")
        return self.git("rev-parse", "HEAD")

    def test_extracts_the_pages_of_that_commit(self):
        old = self.book_commit("# Old\n")
        self.book_commit("# New\n")
        (self.repo / "book" / "src" / "introduction.md").write_text("# Uncommitted\n")
        dest = self.repo / "out" / "src"
        versions.pages(self.repo, old, dest)
        self.assertEqual((dest / "introduction.md").read_text(), "# Old\n")
        self.assertEqual((dest / "sub" / "page.md").read_text(), "# Page\n")

    def test_commit_without_a_book(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        sha = self.git("rev-parse", "HEAD")
        dest = self.repo / "out" / "src"
        with self.assertRaisesRegex(versions.VersionsError, f"no book/src in {sha}"):
            versions.pages(self.repo, sha, dest)
        self.assertFalse(dest.exists())

    def test_unknown_commit_says_what_git_said(self):
        self.commit("0.1.0-beta.1", "2026-11-14T12:00:00+00:00")
        dest = self.repo / "out" / "src"
        with self.assertRaises(versions.VersionsError) as e:
            versions.pages(self.repo, "0" * 40, dest)
        self.assertNotIn("no book/src", str(e.exception))
        self.assertIn("0" * 40, str(e.exception))
        self.assertFalse(dest.exists())

    def test_refuses_to_write_over_something(self):
        sha = self.book_commit("# Old\n")
        dest = self.repo / "out" / "src"
        dest.mkdir(parents=True)
        with self.assertRaisesRegex(versions.VersionsError, "exists"):
            versions.pages(self.repo, sha, dest)

    def test_cli(self):
        sha = self.book_commit("# Old\n")
        dest = self.repo / "out" / "src"
        r = subprocess.run([sys.executable, str(TOOL), "pages", str(self.repo), sha, str(dest)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((dest / "introduction.md").is_file())
        r = subprocess.run([sys.executable, str(TOOL), "pages", str(self.repo), "0" * 40,
                            str(self.repo / "out2")], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: ", r.stderr)


SHA = "b" * 40


class PromotableTests(unittest.TestCase):
    def test_all_published(self):
        self.assertEqual(versions.unpublished(
            f"book 0.1.0-beta.5 {SHA} published\nbeta 0.1.0-beta.5 {SHA} published\ndev - - dev\n"), [])

    def test_names_the_unpublished_once(self):
        text = (f"book 0.1.0-beta.6 {SHA} unpublished\nbeta 0.1.0-beta.6 {SHA} unpublished\n"
                f"alpha 0.1.0-alpha.4 {SHA} published\ndev - - dev\n")
        self.assertEqual(versions.unpublished(text), ["0.1.0-beta.6"])

    def test_what_the_build_writes_is_accepted(self):
        found = {"1.0.0": versions.Release("2027-03-03", SHA, True),
                 "1.1.0-beta.1": versions.Release("2027-04-01", SHA, False)}
        self.assertEqual(versions.unpublished(versions.books_text(versions.books(found))), ["1.1.0-beta.1"])
        self.assertEqual(versions.unpublished(versions.books_text(versions.books({}))), [])
        self.assertEqual(versions.unpublished(versions.books_text(versions.books(found, "d" * 40))),
                         ["1.1.0-beta.1"])
        self.assertEqual(versions.unpublished(versions.books_text(versions.books({}, "d" * 40))), [])

    def test_a_dev_line_without_a_commit_is_still_accepted(self):
        # Builds made before the dev line carried one.
        self.assertEqual(versions.unpublished(f"book 1.0.0 {SHA} published\ndev - - dev\n"), [])

    def test_refuses_what_it_cannot_read(self):
        for text in ["", "\n", "book 1.0.0 published\ndev - - dev\n",
                     f"book 1.0.0 {SHA} maybe\ndev - - dev\n",
                     f"docs 1.0.0 {SHA} published\ndev - - dev\n",
                     f"book 1.0.0 {SHA} published\n",
                     f"book not-a-version {SHA} published\ndev - - dev\n",
                     "<html>Not found</html>\n",
                     f"book 1.0.0 {SHA} published\ndev 1.0.0 {SHA} dev\n",
                     f"book 1.0.0 {SHA} published\ndev - abc123 dev\n",
                     # No main book, the main book twice, dev not last.
                     "dev - - dev\n",
                     f"beta 1.0.0-beta.1 {SHA} published\ndev - - dev\n",
                     f"book 1.0.0 {SHA} published\nbook 1.0.0 {SHA} published\ndev - - dev\n",
                     f"book 1.0.0 {SHA} published\ndev - - dev\nbeta 1.1.0-beta.1 {SHA} unpublished\n"]:
            with self.assertRaises(versions.VersionsError, msg=repr(text)):
                versions.unpublished(text)

    def run_cli(self, text):
        d = Path(tempfile.mkdtemp(prefix="site-promotable-"))
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        f = d / "VERSIONS"
        if text is not None:
            f.write_text(text)
        return subprocess.run([sys.executable, str(TOOL), "promotable", str(f)],
                              capture_output=True, text=True)

    def test_cli_published(self):
        r = self.run_cli(f"book 0.1.0-beta.5 {SHA} published\ndev - - dev\n")
        self.assertEqual((r.returncode, r.stdout, r.stderr), (0, "", ""))

    def test_cli_unpublished(self):
        r = self.run_cli(f"book 0.1.0-beta.6 {SHA} unpublished\ndev - - dev\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: this build holds 0.1.0-beta.6, which is not published. "
                      "Publish the release, re-run the staging build, then promote.", r.stderr)

    def test_cli_missing_file(self):
        r = self.run_cli(None)
        self.assertEqual(r.returncode, 1)
        self.assertIn("is missing: this build was made before VERSIONS existed. "
                      "Re-run the staging build.", r.stderr)

    def test_cli_garbage(self):
        r = self.run_cli("garbage\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: ", r.stderr)


if __name__ == "__main__":
    unittest.main()
