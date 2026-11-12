"""Tests for the scripts in .github/scripts, with a fake gh (fake_gh.py)."""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(HERE)
ROOT = os.path.dirname(os.path.dirname(SCRIPTS))
CHECK = os.path.join(SCRIPTS, "check-release-tag.sh")
NOTES = os.path.join(SCRIPTS, "release-notes.sh")
RESUME = os.path.join(SCRIPTS, "resume-check.sh")
BUMP = os.path.join(SCRIPTS, "bump-version.sh")
HEADING = "## What's changed"


class FakeGh(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="gh-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        bindir = os.path.join(self.tmp, "bin")
        os.mkdir(bindir)
        os.symlink(os.path.join(HERE, "fake_gh.py"), os.path.join(bindir, "gh"))
        self.state_file = os.path.join(self.tmp, "state.json")
        self.env = dict(os.environ, PATH=bindir + os.pathsep + os.environ["PATH"],
                        FAKE_GH_STATE=self.state_file, GH_REPO="irsync-dev/irsync")
        self.set_state(releases={}, tags=[], generated="* a change by @someone")

    def set_state(self, **kw):
        with open(self.state_file, "w") as f:
            json.dump(kw, f)

    def state(self):
        with open(self.state_file) as f:
            return json.load(f)

    def run_script(self, *args, cwd=None):
        return subprocess.run(list(args), env=self.env, cwd=cwd,
                              capture_output=True, text=True)


class CheckTagTests(unittest.TestCase):
    def check(self, *args, cwd=None):
        return subprocess.run([CHECK, *args], cwd=cwd, capture_output=True, text=True)

    def test_release_tags(self):
        for tag, pre in [("v1.2.3", "false"), ("v0.10.0", "false"),
                         ("v1.2.3-beta.1", "true"), ("v1.2.3-alpha.12", "true")]:
            r = self.check(tag)
            self.assertEqual(r.returncode, 0, tag)
            self.assertEqual(r.stdout, f"prerelease={pre}\n", tag)

    def test_other_tags(self):
        for tag in ["v1.2", "1.2.3", "v1.2.3-rc.1", "v1.2.3-beta", "v1.2.3-beta.1.2",
                    "v1.2.3 ", "v1.2.3;id", "v1.2.3\nv1.2.4", "vx.y.z", ""]:
            r = self.check(tag)
            self.assertEqual(r.returncode, 1, repr(tag))
            self.assertIn("not a release tag", r.stderr, repr(tag))
            self.assertEqual(r.stdout, "", repr(tag))

    def fixture(self, cargo, dist):
        d = tempfile.mkdtemp(prefix="ver-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        os.makedirs(os.path.join(d, "packaging", "dist"))
        with open(os.path.join(d, "Cargo.toml"), "w") as f:
            f.write(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{cargo}"\n'
                    f'\n[package]\nversion = "9.9.9"\n')
        with open(os.path.join(d, "packaging", "dist", "dist.toml"), "w") as f:
            f.write(f'[package]\nname = "irsync"\nversion = "{dist}"\n')
        return d

    def test_versions_match(self):
        d = self.fixture("1.2.3-beta.1", "1.2.3-beta.1")
        r = self.check("--versions", "v1.2.3-beta.1", cwd=d)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout, "prerelease=true\n")

    def test_cargo_version_differs(self):
        d = self.fixture("1.2.2", "1.2.3")
        r = self.check("--versions", "v1.2.3", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("Cargo.toml has version 1.2.2", r.stderr)
        self.assertNotIn("dist.toml", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_dist_version_differs(self):
        d = self.fixture("1.2.3", "1.2.3-beta.1")
        r = self.check("--versions", "v1.2.3", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("dist.toml has version 1.2.3-beta.1", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_bad_tag_with_versions(self):
        d = self.fixture("1.2.3", "1.2.3")
        r = self.check("--versions", "v1.2", cwd=d)
        self.assertEqual(r.returncode, 1)
        self.assertIn("not a release tag", r.stderr)

    def test_this_repo(self):
        with open(os.path.join(ROOT, "Cargo.toml")) as f:
            m = re.search(r'^\[workspace\.package\]\nversion = "([^"]+)"', f.read(), re.M)
        r = self.check("--versions", "v" + m.group(1), cwd=ROOT)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


@unittest.skipUnless(shutil.which("cargo"), "needs cargo")
class BumpVersionTests(unittest.TestCase):
    """bump-version.sh, in a temporary git repo holding a tiny Cargo workspace."""

    def setUp(self):
        d = self.dir = tempfile.mkdtemp(prefix="bump-")
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        self.env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                        GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com",
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.write("Cargo.toml",
                   '[workspace]\nmembers = ["a"]\nresolver = "2"\n\n'
                   '[workspace.metadata.other]\nversion = "9.9.9"\n\n'
                   '[workspace.package]\nedition = "2021"\nversion = "1.2.3"\n')
        self.write("a/Cargo.toml",
                   '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n')
        self.write("a/src/lib.rs", "")
        self.write("packaging/dist/dist.toml",
                   '# Keep `version` equal to the workspace version.\n\n'
                   '[package]\nname = "irsync"\nversion = "1.2.3"\nlicense = "MIT"\n')
        self.run_ok("git", "init", "-q", "-b", "main")
        self.run_ok("cargo", "generate-lockfile", "--quiet")
        self.run_ok("git", "add", "-A")
        self.run_ok("git", "commit", "-q", "-m", "start")

    def write(self, path, text):
        path = os.path.join(self.dir, path)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)

    def read(self, path):
        with open(os.path.join(self.dir, path)) as f:
            return f.read()

    def run_ok(self, *args):
        r = subprocess.run(args, cwd=self.dir, env=self.env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r.stdout

    def bump(self, version):
        return subprocess.run([BUMP, version], cwd=os.path.join(self.dir, "a"), env=self.env,
                              capture_output=True, text=True)

    def test_bumps_and_commits(self):
        r = self.bump("1.3.0-beta.1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        cargo = self.read("Cargo.toml")
        self.assertIn('[workspace.package]\nedition = "2021"\nversion = "1.3.0-beta.1"\n', cargo)
        self.assertIn('[workspace.metadata.other]\nversion = "9.9.9"\n', cargo)
        dist = self.read("packaging/dist/dist.toml")
        self.assertTrue(dist.startswith("# Keep `version` equal"), dist)
        self.assertIn('version = "1.3.0-beta.1"\nlicense = "MIT"\n', dist)
        self.assertIn('name = "a"\nversion = "1.3.0-beta.1"\n', self.read("Cargo.lock"))
        self.assertEqual(self.run_ok("git", "log", "-1", "--format=%s"), "Release v1.3.0-beta.1\n")
        self.assertEqual(self.run_ok("git", "status", "--porcelain"), "")
        self.assertEqual(self.run_ok("git", "show", "--name-only", "--format=").split(),
                         ["Cargo.lock", "Cargo.toml", "packaging/dist/dist.toml"])
        self.assertIn("git tag -a v1.3.0-beta.1 -m \"irsync 1.3.0-beta.1\"", r.stdout)
        self.run_ok(CHECK, "--versions", "v1.3.0-beta.1")

    def assert_refused(self, r, message):
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(message, r.stderr)
        self.assertEqual(self.run_ok("git", "log", "-1", "--format=%s"), "start\n")
        self.assertIn('version = "1.2.3"', self.read("packaging/dist/dist.toml"))

    def test_bad_versions(self):
        for v in ["1.2", "v1.3.0", "1.3.0-rc.1", "1.3.0;id"]:
            self.assert_refused(self.bump(v), "not a release tag")

    def test_same_version(self):
        self.assert_refused(self.bump("1.2.3"), "already 1.2.3")

    def test_uncommitted_changes(self):
        self.write("a/src/lib.rs", "// edit\n")
        self.assert_refused(self.bump("1.3.0"), "uncommitted changes")


class NotesTests(FakeGh):
    def notes(self, tag, tags, body="Intro."):
        self.set_state(releases={tag: {"body": body, "isDraft": False, "isPrerelease": False}},
                       tags=tags, generated="* a change by @someone")
        r = self.run_script(NOTES, tag)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return self.state()

    def test_adds_list(self):
        s = self.notes("v0.2.0", ["v0.1.0", "v0.2.0"])
        self.assertEqual(s["releases"]["v0.2.0"]["body"],
                         f"Intro.\n\n{HEADING}\n\n* a change by @someone\n")

    def test_empty_body(self):
        s = self.notes("v0.2.0", ["v0.2.0"], body="")
        self.assertEqual(s["releases"]["v0.2.0"]["body"],
                         f"{HEADING}\n\n* a change by @someone\n")

    def test_stable_starts_at_previous_stable(self):
        tags = ["v0.1.0", "v0.2.0-alpha.1", "v0.2.0-beta.1", "v0.2.0", "v0.3.0-beta.1",
                "v0.3.0-beta.2", "v0.3.0"]
        s = self.notes("v0.3.0", tags)
        self.assertEqual(s["generate"], {"tag_name": "v0.3.0", "previous_tag_name": "v0.2.0"})

    def test_sorts_by_version(self):
        tags = ["v0.9.0", "v0.10.0", "v0.11.0", "v0.10.1-beta.1", "v0.2.0"]
        self.assertEqual(self.notes("v0.11.0", tags)["generate"]["previous_tag_name"], "v0.10.0")
        self.assertEqual(self.notes("v0.10.0", tags)["generate"]["previous_tag_name"], "v0.9.0")

    def test_ignores_later_tags(self):
        # Running the notes for an older release after a newer one exists.
        tags = ["v0.1.0", "v0.2.0", "v0.3.0", "v1.0.0"]
        self.assertEqual(self.notes("v0.2.0", tags)["generate"]["previous_tag_name"], "v0.1.0")

    def test_first_stable(self):
        tags = ["v0.1.0-alpha.1", "v0.1.0-beta.1", "v0.1.0", "v-odd", "vnext"]
        self.assertEqual(self.notes("v0.1.0", tags)["generate"], {"tag_name": "v0.1.0"})

    def test_prerelease_keeps_default(self):
        s = self.notes("v0.2.0-beta.1", ["v0.1.0", "v0.2.0-beta.1"])
        self.assertEqual(s["generate"], {"tag_name": "v0.2.0-beta.1"})
        self.assertFalse([c for c in s["calls"] if "matching-refs" in " ".join(c)])

    def test_rerun_does_nothing(self):
        body = f"Intro.\n\n{HEADING}\n\n* old list\n"
        s = self.notes("v0.2.0", ["v0.2.0"], body=body)
        self.assertEqual(s["releases"]["v0.2.0"]["body"], body)
        self.assertNotIn("generate", s)
        self.assertFalse([c for c in s["calls"] if c[:2] == ["release", "edit"]])

    def test_twice_adds_once(self):
        self.notes("v0.2.0", ["v0.1.0", "v0.2.0"])
        r = self.run_script(NOTES, "v0.2.0")
        self.assertEqual(r.returncode, 0)
        self.assertEqual(self.state()["releases"]["v0.2.0"]["body"].count(HEADING), 1)

    def test_empty_list_fails(self):
        self.set_state(releases={"v0.2.0": {"body": "x", "isDraft": False, "isPrerelease": False}},
                       tags=["v0.2.0"], generated="")
        r = self.run_script(NOTES, "v0.2.0")
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.state()["releases"]["v0.2.0"]["body"], "x")

    def test_no_release_fails(self):
        r = self.run_script(NOTES, "v0.2.0")
        self.assertNotEqual(r.returncode, 0)


class ResumeTests(FakeGh):
    def resume(self, tag, dry_run, release=None):
        releases = {tag: release} if release else {}
        self.set_state(releases=releases, tags=[tag], generated="x")
        return self.run_script(RESUME, tag, "true" if dry_run else "false")

    def out(self, r):
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return dict(l.split("=", 1) for l in r.stdout.splitlines())

    PUBLISHED = {"body": "", "isDraft": False, "isPrerelease": False}
    PRE = {"body": "", "isDraft": False, "isPrerelease": True}

    def test_stable(self):
        self.assertEqual(self.out(self.resume("v1.2.3", False, self.PUBLISHED)),
                         {"packages": "true", "notes": "true"})

    def test_prerelease_tag_gets_notes_only(self):
        self.assertEqual(self.out(self.resume("v1.2.3-beta.1", False, self.PRE)),
                         {"packages": "false", "notes": "true"})

    def test_stable_tag_marked_prerelease_gets_no_packages(self):
        self.assertEqual(self.out(self.resume("v1.2.3", False, self.PRE)),
                         {"packages": "false", "notes": "true"})

    def test_dry_run_on_alpha(self):
        self.assertEqual(self.out(self.resume("v0.1.0-alpha.1", True, self.PRE)),
                         {"packages": "true", "notes": "false"})

    def test_dry_run_on_stable(self):
        self.assertEqual(self.out(self.resume("v1.2.3", True, self.PUBLISHED)),
                         {"packages": "true", "notes": "false"})

    def test_draft_fails(self):
        r = self.resume("v1.2.3", False, {"body": "", "isDraft": True, "isPrerelease": False})
        self.assertEqual(r.returncode, 1)
        self.assertIn("draft", r.stderr)
        self.assertEqual(r.stdout, "")

    def test_missing_release_fails(self):
        for dry in (False, True):
            r = self.resume("v1.2.3", dry)
            self.assertEqual(r.returncode, 1)
            self.assertIn("no GitHub Release", r.stderr)
            self.assertEqual(r.stdout, "")

    def test_bad_tag_fails_before_gh(self):
        r = self.resume("v1.2.3;id", True, self.PUBLISHED)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, "")
        self.assertEqual(self.state().get("calls", []), [])


if __name__ == "__main__":
    unittest.main()
