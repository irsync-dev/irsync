"""Tests for tools/build-man.sh, which builds the man pages from the book.

Needs pandoc and mandoc. A missing tool skips the tests, unless
MAN_TESTS_REQUIRED=1, which makes it a failure.
"""
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "tools", "build-man.sh")
DATE = "2026-01-02"
TOOLS = ["pandoc", "mandoc"]
# Everything the script reads, relative to the repository root.
INPUTS = [
    "tools/build-man.sh",
    "packaging/man",
    "packaging/dist/dist.toml",
    "book/src/reference/command-line.md",
    "book/src/reference/management.md",
    "book/src/reference/configuration.md",
    "book/src/fileset/reference.md",
]


def read_version():
    with open(os.path.join(ROOT, "packaging", "dist", "dist.toml")) as f:
        return re.search(r'^version = "([^"]+)"', f.read(), re.M).group(1)


VERSION = read_version()


def setUpModule():
    for tool in TOOLS:
        if shutil.which(tool) is None:
            if os.environ.get("MAN_TESTS_REQUIRED") == "1":
                raise AssertionError(f"required tool missing: {tool}")
            raise unittest.SkipTest(f"tool not installed: {tool}")


def build(script, out):
    env = dict(os.environ, IRSYNC_MAN_DATE=DATE)
    return subprocess.run([script, out], capture_output=True, text=True, env=env)


def render(page):
    man = subprocess.run(["mandoc", "-T", "utf8", page],
                         capture_output=True, text=True, check=True).stdout
    return subprocess.run(["col", "-b"], input=man,
                          capture_output=True, text=True, check=True).stdout


def read(path):
    with open(path) as f:
        return f.read()


class BuildManTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="build-man-")
        cls.out = os.path.join(cls.tmp, "out")
        r = build(SCRIPT, cls.out)
        if r.returncode:
            raise AssertionError(f"build-man.sh failed ({r.returncode}):\n{r.stderr}")
        cls.irsync = os.path.join(cls.out, "man1", "irsync.1")
        cls.fileset = os.path.join(cls.out, "man1", "fileset.1")
        cls.toml = os.path.join(cls.out, "man5", "irsync.toml.5")
        cls.pages = [cls.irsync, cls.fileset, cls.toml]

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_builds_three_pages(self):
        for rel in ("man1/irsync.1", "man1/fileset.1", "man5/irsync.toml.5"):
            self.assertTrue(os.path.isfile(os.path.join(self.out, rel)), rel)

    def test_lint_is_clean(self):
        for page in self.pages:
            r = subprocess.run(["mandoc", "-T", "lint", "-W", "warning", page],
                               capture_output=True, text=True)
            self.assertEqual((r.returncode, r.stdout), (0, ""), page)

    def test_title_block(self):
        # version from packaging/dist/dist.toml, date from IRSYNC_MAN_DATE
        text = render(self.irsync)
        self.assertIn("IRSYNC(1)", text)
        self.assertIn(f"irsync {VERSION}", text)
        self.assertIn(DATE, text)
        self.assertIn("IRSYNC.TOML(5)", render(self.toml))

    def test_flags_keep_their_dashes(self):
        text = render(self.irsync)
        for flag in ("--dry-run", "--archive", "--no-recursive", "--dial-retries"):
            self.assertIn(flag, text)
        for page in self.pages:
            self.assertNotIn("–", render(page))  # en dash
            self.assertNotIn("—", render(page))  # em dash

    def test_irsync_sections_in_order(self):
        heads = re.findall(r"^\.SH (.+)$", read(self.irsync), re.M)
        self.assertEqual(heads[0], "NAME")
        self.assertIn("MANAGEMENT COMMANDS", heads)
        self.assertEqual(heads[-3:], ["ENVIRONMENT", "EXIT STATUS", "SEE ALSO"])
        for sub in ("HOST", "EXPORT", "GRANT"):  # subsections, not sections
            self.assertNotIn(sub, heads)

    def test_cross_references(self):
        self.assertIn("irsync.toml(5)", render(self.irsync))
        self.assertIn("irsync(1)", render(self.toml))
        self.assertIn("https://irsync.org/book/reference/remote-addresses.html",
                      render(self.irsync))
        for page in self.pages:  # no raw Markdown link targets
            self.assertNotRegex(read(page), r"\w\.md[#)]")

    def test_fileset_page_has_no_install_block(self):
        self.assertNotIn("cargo install", render(self.fileset))

    def test_mdbook_directive_fails(self):
        tree = os.path.join(self.tmp, "tree")
        for rel in INPUTS:
            src, dst = os.path.join(ROOT, rel), os.path.join(tree, rel)
            if os.path.isdir(src):
                shutil.copytree(src, dst)
            else:
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(src, dst)
        chapter = os.path.join(tree, "book/src/reference/management.md")
        with open(chapter, "a") as f:
            f.write("\n{{#include x.md}}\n")
        r = build(os.path.join(tree, "tools", "build-man.sh"),
                  os.path.join(self.tmp, "tree-out"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("{{#", r.stderr)
        self.assertIn("management.md", r.stderr)

    def test_same_inputs_same_output(self):
        out2 = os.path.join(self.tmp, "out2")
        r = build(SCRIPT, out2)
        self.assertEqual(r.returncode, 0, r.stderr)
        for page in self.pages:
            rel = os.path.relpath(page, self.out)
            with open(page, "rb") as a, open(os.path.join(out2, rel), "rb") as b:
                self.assertEqual(a.read(), b.read(), rel)


if __name__ == "__main__":
    unittest.main()
