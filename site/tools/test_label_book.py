import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import label_book as book

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "site" / "tools" / "label_book.py"


class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="site-book-"))
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        # The real settings: the tool must find its lines in them.
        shutil.copy(ROOT / "book" / "book.toml", self.work / "book.toml")
        (self.work / "src").mkdir()
        (self.work / "src" / "introduction.md").write_text(
            "# Introduction\n\n`irsync` is a synchroniser.\n", encoding="utf-8")

    def toml(self):
        return (self.work / "book.toml").read_text(encoding="utf-8")

    def intro(self):
        return (self.work / "src" / "introduction.md").read_text(encoding="utf-8")

    def test_released(self):
        book.prepare(self.work, "beta", "0.1.0-beta.5")
        self.assertIn('title = "irsync Documentation"\n', self.toml())
        self.assertIn('site-url = "/beta/"\n', self.toml())
        self.assertNotIn("edit-url-template", self.toml())
        self.assertEqual(self.intro(),
                         "# Introduction\n\nThis is the documentation of irsync 0.1.0-beta.5.\n\n"
                         "`irsync` is a synchroniser.\n")

    def test_dev(self):
        book.prepare(self.work, "dev", None)
        self.assertIn('title = "irsync Documentation"\n', self.toml())
        self.assertIn('site-url = "/dev/"\n', self.toml())
        self.assertIn("edit-url-template", self.toml())
        self.assertIn("\n\nThis is the documentation of the unreleased development version of irsync. "
                      "[Released documentation](../book/).\n\n", self.intro())

    def test_unreleased_main_book_has_no_link_to_itself(self):
        book.prepare(self.work, "book", None)
        self.assertIn('site-url = "/book/"\n', self.toml())
        self.assertIn("development version of irsync.\n", self.intro())
        self.assertNotIn("Released documentation", self.intro())

    def test_rest_of_the_settings_is_untouched(self):
        before = self.toml()
        book.prepare(self.work, "dev", None)
        changed = [l for l in before.splitlines() if l not in self.toml().splitlines()]
        self.assertEqual(changed, ['site-url = "/book/"'])

    def test_title_is_left_alone(self):
        toml = self.toml().replace('title = "irsync Documentation"', 'title = "Something else"')
        (self.work / "book.toml").write_text(toml, encoding="utf-8")
        book.prepare(self.work, "beta", "0.1.0-beta.5")
        self.assertIn('title = "Something else"\n', self.toml())

    def test_refuses(self):
        cases = [("docs", "1.0.0", "path"), ("beta", "1.0.0; rm", "version"),
                 ("beta", "v1.0.0", "version"), ("beta", '1.0.0"', "version")]
        for path, version, word in cases:
            with self.assertRaisesRegex(book.BookError, word):
                book.prepare(self.work, path, version)
        # Nothing was written by a refused call.
        self.assertIn('site-url = "/book/"\n', self.toml())

    def test_introduction_without_a_heading(self):
        (self.work / "src" / "introduction.md").write_text("irsync is…\n", encoding="utf-8")
        with self.assertRaisesRegex(book.BookError,
                                    "beta: introduction.md does not start with a heading"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_no_introduction(self):
        (self.work / "src" / "introduction.md").unlink()
        with self.assertRaisesRegex(book.BookError, "beta"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_setting_missing_from_book_toml(self):
        (self.work / "book.toml").write_text('[book]\ntitle = "irsync Documentation"\n',
                                             encoding="utf-8")
        with self.assertRaisesRegex(book.BookError, "site-url"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_cli(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "alpha", "0.1.0-alpha.4"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('site-url = "/alpha/"', self.toml())

    def test_cli_dash_is_no_version(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "dev", "-"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('site-url = "/dev/"', self.toml())

    def test_cli_error(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "nope", "-"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("label_book.py: ", r.stderr)


if __name__ == "__main__":
    unittest.main()
