import subprocess
import tempfile
import unittest
from pathlib import Path

from check import check_flags, check_links, check_moved


def write(root, rel, text):
    p = Path(root) / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)
    return p


def git_repo(root, files):
    """Commit `files` ({path: text}) into a fresh repository at `root`."""
    for rel, text in files.items():
        write(root, rel, text)
    for cmd in (
        ["init", "-q"],
        ["add", "."],
        ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"],
    ):
        subprocess.run(["git", "-C", str(root), *cmd], check=True)


class TmpCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.d = Path(self._tmp.name)


class LinkTests(TmpCase):
    def test_missing_file_is_reported(self):
        write(self.d, "a.html", '<a href="b.html">b</a>')
        self.assertEqual(len(check_links(self.d)), 1)

    def test_missing_anchor_is_reported(self):
        write(self.d, "a.html", '<a href="b.html#gone">b</a>')
        write(self.d, "b.html", '<h1 id="here">b</h1>')
        self.assertIn("#gone", check_links(self.d)[0])

    def test_present_anchor_and_external_pass(self):
        write(
            self.d,
            "a.html",
            '<a href="b.html#here">b</a><a href="https://x.org">x</a>'
            '<a href="mailto:a@b.c">m</a>',
        )
        write(self.d, "b.html", '<h1 id="here">b</h1>')
        self.assertEqual(check_links(self.d), [])


class MovedTests(TmpCase):
    def test_reference_to_moved_file_is_reported(self):
        git_repo(self.d, {"tools/a.sh": "# see docs/x.md\n"})
        self.assertEqual(len(check_moved(self.d, ["docs/x.md"])), 1)

    def test_reference_from_superpowers_is_ignored(self):
        git_repo(self.d, {"docs/superpowers/plan.md": "see docs/x.md\n"})
        self.assertEqual(check_moved(self.d, ["docs/x.md"]), [])

    def test_existing_moved_file_is_not_yet_enforced(self):
        git_repo(self.d, {"docs/x.md": "x\n", "tools/a.sh": "# see docs/x.md\n"})
        self.assertEqual(check_moved(self.d, ["docs/x.md"]), [])

    def test_bare_file_name_is_reported(self):
        git_repo(self.d, {"tools/a.sh": "# see x.md\n"})
        self.assertEqual(len(check_moved(self.d, ["docs/x.md"])), 1)


class FlagTests(TmpCase):
    def test_unknown_flag_is_reported(self):
        p = write(self.d, "p.md", "Run `irsync --frobnicate a b:` now.\n")
        problems = check_flags([p], {"--archive"})
        self.assertEqual(len(problems), 1)
        self.assertIn("--frobnicate", problems[0])

    def test_flag_with_value_and_non_irsync_lines(self):
        p = write(self.d, "p.md", "`irsync --timeout=30`, `fileset --frob`\n")
        self.assertEqual(check_flags([p], {"--timeout"}), [])

    def test_code_block_line_with_prompt(self):
        p = write(self.d, "p.md", "```sh\n$ irsync --bogus x\nirsync --ok y\n```\n")
        problems = check_flags([p], {"--ok"})
        self.assertEqual(len(problems), 1)
        self.assertIn("--bogus", problems[0])

    def test_command_line_table_first_column(self):
        p = write(
            self.d,
            "book/src/reference/command-line.md",
            "| Flag | Meaning |\n|---|---|\n| `--known` | a |\n"
            "| `--nope`, `--known` | b |\n| `-x` | c --prose |\n",
        )
        problems = check_flags([p], {"--known"})
        self.assertEqual(len(problems), 1)
        self.assertIn("--nope", problems[0])


if __name__ == "__main__":
    unittest.main()
