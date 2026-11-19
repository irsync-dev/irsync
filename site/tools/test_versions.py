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
        self.assertIn('<strong class="font-semibold">Beta</strong> '
                      '<span class="version" data-tail="sh -s -- --beta">0.1.0-beta.5</span> '
                      '<span>· 19 Nov 2026</span>', out)
        self.assertEqual(out.count('class="version"'), 1)
        self.assertIn('id="install-tail">sh -s -- --beta</span>', out)
        self.assertIn(">beta</span", out)        # the badge
        self.assertIn("Beta.</strong>", out)     # the note
        self.assertNotIn("versions:", out)       # no marker survives

    def test_stable_with_newer_beta_and_alpha(self):
        out = self.page("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1")
        order = [out.index(v) for v in (">0.1.2<", ">0.2.0-beta.1<", ">0.3.0-alpha.1<")]
        self.assertEqual(order, sorted(order))
        self.assertIn('<strong class="font-semibold">Stable</strong> '
                      '<span class="version" data-tail="sh">0.1.2<', out)
        self.assertIn('data-tail="sh -s -- --beta">0.2.0-beta.1<', out)
        self.assertIn('data-tail="sh -s -- --alpha">0.3.0-alpha.1<', out)
        self.assertIn('id="install-tail">sh</span>', out)
        self.assertNotIn('role="note"', out)
        self.assertNotIn("bg-signal", out)       # the badge is gone
        self.assertNotIn("versions:", out)

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
        self.assertIn("· 3 Mar 2027", out)

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
        self.assertIn("· 19 Nov 2026", out.read_text(encoding="utf-8"))

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
        r = subprocess.run([sys.executable, str(TOOL), "books", str(self.repo)],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, f"book 0.1.0-beta.5 {sha} published\n"
                                   f"beta 0.1.0-beta.5 {sha} published\ndev - - dev\n")

    def test_no_tags_exits_1_and_writes_nothing(self):
        self.commit("0.1.0-beta.5", "2026-11-19T12:00:00+00:00")
        out = self.repo / "out.html"
        r = self.run_tool(out)
        self.assertEqual(r.returncode, 1)
        self.assertIn("versions.py: no release tags", r.stderr)
        self.assertFalse(out.exists())


if __name__ == "__main__":
    unittest.main()
