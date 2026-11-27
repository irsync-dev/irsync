import json
import shutil
import subprocess
import sys
import re
import tempfile
import unittest
from pathlib import Path

import label_book as book

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "site" / "tools" / "label_book.py"
SHA = "b5618f4701f405b818304bf917f7da77eb7f0a60"
URL = "https://github.com/irsync-dev/irsync"


class PrepareTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="site-book-"))
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        # The real settings: the tool must find its lines in them.
        shutil.copy(ROOT / "book" / "book.toml", self.work / "book.toml")
        (self.work / "theme").mkdir()
        (self.work / "src").mkdir()
        (self.work / "src" / "introduction.md").write_text(
            "# Introduction\n\n`irsync` is a synchroniser.\n", encoding="utf-8")

    def toml(self):
        return (self.work / "book.toml").read_text(encoding="utf-8")

    def intro(self):
        return (self.work / "src" / "introduction.md").read_text(encoding="utf-8")

    def source(self):
        """What theme/source.js tells home-link.js."""
        text = (self.work / "theme" / "source.js").read_text(encoding="utf-8")
        prefix = "window.irsyncSource = "
        line = next(l for l in text.splitlines() if l.startswith(prefix))
        return json.loads(line[len(prefix):].rstrip(";"))

    def test_dev_names_its_branch_and_commit(self):
        book.prepare(self.work, "dev", None, SHA, "main")
        self.assertEqual(self.source(), [
            {"text": "main", "url": "https://github.com/irsync-dev/irsync/tree/main"},
            {"text": "b5618f4", "url": f"https://github.com/irsync-dev/irsync/commit/{SHA}"}])

    def test_dev_without_a_branch_names_its_commit(self):
        book.prepare(self.work, "dev", None, SHA, None)
        self.assertEqual([p["text"] for p in self.source()], ["b5618f4"])

    def test_release_names_its_branch_and_tag(self):
        book.prepare(self.work, "beta", "0.1.0-beta.7", SHA)
        self.assertEqual(self.source(), [
            {"text": "release-0.1", "url": "https://github.com/irsync-dev/irsync/tree/release-0.1"},
            {"text": "v0.1.0-beta.7",
             "url": "https://github.com/irsync-dev/irsync/tree/v0.1.0-beta.7"}])

    def test_stable_patch_names_its_line(self):
        book.prepare(self.work, "book", "1.12.3", SHA)
        self.assertEqual([p["text"] for p in self.source()], ["release-1.12", "v1.12.3"])

    def test_refuses_an_odd_branch(self):
        for branch in ("a b", "x\"y", "<b>", "main?x"):
            with self.assertRaisesRegex(book.BookError, "branch"):
                book.prepare(self.work, "dev", None, SHA, branch)
        self.assertFalse((self.work / "theme" / "source.js").exists())

    def test_source_script_runs_before_home_link(self):
        book.prepare(self.work, "dev", None, SHA)
        js = re.search(r"^additional-js = \[(.*)\]$", self.toml(), re.M).group(1)
        names = [n.strip().strip('"') for n in js.split(",")]
        self.assertIn("theme/source.js", names)
        self.assertLess(names.index("theme/source.js"), names.index("theme/home-link.js"))

    def test_no_commit_no_source(self):
        book.prepare(self.work, "dev", None, None)
        self.assertFalse((self.work / "theme" / "source.js").exists())
        self.assertNotIn("source.js", self.toml())

    def test_refuses_a_bad_commit(self):
        for commit in ("b5618f4", "nope", SHA + "0", SHA.upper()):
            with self.assertRaisesRegex(book.BookError, "commit"):
                book.prepare(self.work, "dev", None, commit)
        self.assertFalse((self.work / "theme" / "source.js").exists())

    def test_released(self):
        book.prepare(self.work, "beta", "0.1.0-beta.5")
        self.assertIn('title = "irsync"\n', self.toml())
        self.assertIn('site-url = "/beta/"\n', self.toml())
        self.assertNotIn("edit-url-template", self.toml())
        self.assertEqual(self.intro(),
                         "# Introduction\n\nThis is the documentation of irsync 0.1.0-beta.5.\n\n"
                         "`irsync` is a synchroniser.\n")

    def test_dev(self):
        book.prepare(self.work, "dev", None)
        self.assertIn('title = "irsync"\n', self.toml())
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
        (self.work / "book.toml").write_text(before, encoding="utf-8")
        book.prepare(self.work, "dev", None, SHA)
        changed = [l for l in before.splitlines() if l not in self.toml().splitlines()]
        self.assertEqual([l.split(" =")[0] for l in changed], ["site-url", "additional-js"])

    def test_title_is_left_alone(self):
        toml = self.toml().replace('title = "irsync"', 'title = "Something else"')
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
        (self.work / "book.toml").write_text('[book]\ntitle = "irsync"\n',
                                             encoding="utf-8")
        with self.assertRaisesRegex(book.BookError, "site-url"):
            book.prepare(self.work, "beta", "0.1.0-beta.5")

    def test_cli(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "alpha", "0.1.0-alpha.4",
                            SHA, "published"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('site-url = "/alpha/"', self.toml())
        self.assertEqual([p["text"] for p in self.source()], ["release-0.1", "v0.1.0-alpha.4"])

    def test_cli_dev_branch(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "dev", "-", SHA, "main"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([p["text"] for p in self.source()], ["main", "b5618f4"])

    def test_cli_dash_is_no_version(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "dev", "-", "-", "-"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('site-url = "/dev/"', self.toml())
        self.assertFalse((self.work / "theme" / "source.js").exists())

    def page(self, rel, text):
        p = self.work / "src" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
        return p

    def test_source_links_follow_the_dev_commit(self):
        p = self.page("contributing/x.md",
                      "See [`label_book.py`](../../../site/tools/label_book.py#L1), "
                      "[`site/tools/`](../../../site/tools/) and [the top](../../../).\n")
        book.prepare(self.work, "dev", None, SHA, "main")
        self.assertEqual(p.read_text(encoding="utf-8"),
                         f"See [`label_book.py`]({URL}/blob/{SHA}/site/tools/label_book.py#L1), "
                         f"[`site/tools/`]({URL}/tree/{SHA}/site/tools) and [the top]({URL}/tree/{SHA}).\n")

    def test_source_links_of_a_release_go_to_its_tag(self):
        head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                              capture_output=True, text=True, check=True).stdout.strip()
        p = self.page("reference/x.md", "[`Cargo.toml`](../../../Cargo.toml)\n")
        book.prepare(self.work, "beta", "0.1.0-beta.7", head)
        self.assertEqual(p.read_text(encoding="utf-8"),
                         f"[`Cargo.toml`]({URL}/blob/v0.1.0-beta.7/Cargo.toml)\n")

    def test_source_links_without_a_commit_go_to_main(self):
        self.page("introduction.md", "# Introduction\n\nSee [`Cargo.toml`](../../Cargo.toml).\n")
        book.prepare(self.work, "dev", None)
        self.assertIn(f"See [`Cargo.toml`]({URL}/blob/main/Cargo.toml).", self.intro())
        self.assertIn("This is the documentation of the unreleased", self.intro())

    def test_other_links_are_left_alone(self):
        text = ("[Pair](../how-to/pair.md), [theme](../../theme/x.css), [web](https://example.org/)\n"
                "`[code](../../../Cargo.toml)`\n"
                "```text\n[fenced](../../../Cargo.toml)\n```\n")
        p = self.page("contributing/x.md", text)
        book.prepare(self.work, "dev", None, SHA)
        self.assertEqual(p.read_text(encoding="utf-8"), text)

    def test_a_missing_path_stops_the_build(self):
        p = self.page("contributing/x.md",
                      "[`gone.rs`](../../../crates/gone.rs)\n[up](../../../../x)\n"
                      "[dots](../../../site/../Cargo.toml)\n")
        with self.assertRaises(book.BookError) as e:
            book.prepare(self.work, "dev", None, SHA)
        msg = str(e.exception)
        self.assertIn("contributing/x.md:1: ../../../crates/gone.rs: no such file", msg)
        self.assertIn("contributing/x.md:2: ../../../../x goes above the repository", msg)
        self.assertIn("contributing/x.md:3: ../../../site/../Cargo.toml: write the path", msg)
        # Nothing was written by a refused call.
        self.assertIn('site-url = "/book/"\n', self.toml())
        self.assertIn("gone.rs`](../../../crates/gone.rs)", p.read_text(encoding="utf-8"))

    def test_a_release_checks_the_paths_of_its_commit(self):
        first = subprocess.run(["git", "-C", str(ROOT), "rev-list", "--max-parents=0", "HEAD"],
                               capture_output=True, text=True, check=True).stdout.split()[0]
        self.page("contributing/x.md", "[`label_book.py`](../../../site/tools/label_book.py)\n")
        with self.assertRaisesRegex(book.BookError, "no such file or directory at v0.1.0-beta.7"):
            book.prepare(self.work, "beta", "0.1.0-beta.7", first)

    def test_the_book_links_only_to_files_that_exist(self):
        shutil.rmtree(self.work / "src")
        shutil.copytree(ROOT / "book" / "src", self.work / "src")
        book.prepare(self.work, "dev", None, SHA)
        for page in (self.work / "src").rglob("*.md"):
            # Fenced blocks and code spans are examples, left as they are.
            text = re.sub(r"^```.*?^```", "", page.read_text(encoding="utf-8"), flags=re.M | re.S)
            text = re.sub(r"`[^`\n]*`(?!\])", "", text)
            for target in re.findall(r"\]\((\.\./[^)]*)\)", text):
                depth = len(page.relative_to(self.work / "src").parts) + 1
                self.assertLess(target.count("../"), depth, f"{page}: {target}")

    def test_cli_error(self):
        r = subprocess.run([sys.executable, str(TOOL), str(self.work), "nope", "-", "-", "-"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("label_book.py: ", r.stderr)


if __name__ == "__main__":
    unittest.main()
