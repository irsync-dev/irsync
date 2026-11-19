import subprocess
import tempfile
import unittest
from pathlib import Path

from check import (check_flags, check_fonts, check_html_links, check_links, check_moved, cli_flags,
                   documented_commands)


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

    def test_address_without_html_resolves_to_the_page(self):
        # The site links to x for x.html, as Cloudflare serves it.
        write(self.d, "a.html", '<a href="b#here">b</a><a href="sub/">s</a><a href="c">c</a>')
        write(self.d, "b.html", '<h1 id="here">b</h1>')
        write(self.d, "sub/index.html", "<p>s</p>")
        problems = check_links(self.d)
        self.assertEqual(problems, ["a.html: c: no such file"])

    def test_html_addresses_are_reported(self):
        write(self.d, "a.html", '<a href="b.html">b</a><a href="b">b</a>'
                                '<a href="https://x.org/b.html">x</a>')
        write(self.d, "b.html", "<p>b</p>")
        self.assertEqual(check_html_links(self.d),
                         ["a.html: b.html: link to b instead; the site redirects .html addresses"])


class FontTests(TmpCase):
    def test_font_files_and_fonts_directory_are_reported(self):
        write(self.d, "book/fonts/fonts.css", "")
        write(self.d, "book/x/open-sans.woff2", "")
        problems = check_fonts(self.d)
        self.assertEqual(len(problems), 2, problems)

    def test_barlow_in_top_level_fonts_passes(self):
        write(self.d, "fonts/barlow-latin-400-normal.woff2", "")
        write(self.d, "fonts/barlow-latin-600-normal.woff2", "")
        write(self.d, "fonts/barlow-semi-condensed-latin-400-normal.woff2", "")
        write(self.d, "fonts/barlow-semi-condensed-latin-600-normal.woff2", "")
        write(self.d, "fonts/barlow-OFL.txt", "")
        self.assertEqual(check_fonts(self.d), [])

    def test_inter_and_jetbrains_mono_in_top_level_fonts_pass(self):
        for f in (
            "inter-latin-400-normal.woff2",
            "inter-latin-400-italic.woff2",
            "inter-latin-600-normal.woff2",
            "jetbrains-mono-latin-400-normal.woff2",
            "jetbrains-mono-latin-400-italic.woff2",
            "jetbrains-mono-latin-600-normal.woff2",
            "inter-OFL.txt",
            "jetbrains-mono-OFL.txt",
        ):
            write(self.d, f"fonts/{f}", "")
        self.assertEqual(check_fonts(self.d), [])

    def test_the_site_fonts_in_the_book_are_reported(self):
        write(self.d, "book/fonts/inter-latin-400-normal.woff2", "")
        problems = check_fonts(self.d)
        # the book's fonts directory and the file in it
        self.assertEqual(len(problems), 2, problems)

    def test_other_fonts_next_to_barlow_are_reported(self):
        write(self.d, "fonts/barlow-latin-400-normal.woff2", "")
        write(self.d, "fonts/open-sans.woff2", "")
        write(self.d, "book/fonts/barlow-latin-400-normal.woff2", "")
        problems = check_fonts(self.d)
        # the stray top-level font, and the book's fonts directory and file
        self.assertEqual(len(problems), 3, problems)

    def test_site_without_fonts_passes(self):
        write(self.d, "index.html", "<p>x</p>")
        write(self.d, "book/css/general.css", "body { font-family: sans-serif; }")
        self.assertEqual(check_fonts(self.d), [])


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

    def test_sibling_link_with_moved_name_is_not_reported(self):
        git_repo(
            self.d,
            {"book/src/a/x.md": "x\n", "book/src/a/b.md": "see [x](x.md)\n"},
        )
        self.assertEqual(check_moved(self.d, ["docs/x.md"]), [])

    def test_bare_name_without_existing_target_is_reported(self):
        git_repo(self.d, {"tools/a.sh": "# see x.md\n"})
        self.assertEqual(len(check_moved(self.d, ["docs/x.md"])), 1)

    def test_longer_path_is_not_a_match(self):
        git_repo(self.d, {"notes.md": "see book/docs/x.md\n"})
        self.assertEqual(check_moved(self.d, ["docs/x.md"]), [])


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


    def test_cli_flags_reads_repeatable_option(self):
        # clap marks a counted or repeatable flag with `...`: `-v, --verbose...`.
        help_text = (
            "Usage: x [OPTIONS]\n\nOptions:\n"
            "  -v, --verbose...    Log more\n"
            "      --exclude <PATTERN>  Exclude\n"
            "  -h, --help          Print help\n"
        )
        binary = Path(self.d) / "fakebin"
        binary.write_text("#!/bin/sh\ncat <<'EOF'\n" + help_text + "EOF\n")
        binary.chmod(0o755)
        flags = cli_flags(binary)
        self.assertTrue({"-v", "--verbose", "--exclude", "-h", "--help"} <= flags, flags)

    def fake_cli(self):
        """A CLI with a listed command `sync` and a hidden command `secret`.
        Any other first word is a positional: it gets the top-level help."""
        binary = Path(self.d) / "fakebin"
        binary.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "sync) printf 'Usage: fakebin sync [OPTIONS]\\n\\nOptions:\\n      --archive  Keep\\n' ;;\n"
            "secret) printf 'Usage: fakebin secret [OPTIONS] <FILE>\\n\\nOptions:\\n      --deep <X>  Dig\\n' ;;\n"
            "*) printf 'Usage: fakebin [OPTIONS] [COMMAND]\\n\\nCommands:\\n  sync  Copy\\n\\nOptions:\\n  -q, --quiet  Hush\\n' ;;\n"
            "esac\n")
        binary.chmod(0o755)
        return binary

    def test_cli_flags_misses_a_hidden_command_unless_named(self):
        binary = self.fake_cli()
        self.assertEqual(cli_flags(binary), {"-q", "--quiet", "--archive"})
        self.assertEqual(cli_flags(binary, also=["secret"]), {"-q", "--quiet", "--archive", "--deep"})

    def test_cli_flags_ignores_a_named_word_that_is_no_command(self):
        # `fakebin photos --help` prints the top-level help: photos is an argument.
        binary = self.fake_cli()
        self.assertEqual(cli_flags(binary, also=["photos", "sync"]), {"-q", "--quiet", "--archive"})

    def test_documented_commands(self):
        p = write(self.d, "page.md",
                  "Run `irsync secret --deep 1 file` or `irsync -av a b`.\n\n"
                  "```sh\n$ irsync sync -a x y\nirsync photos host:\nrsync other --x\n```\n")
        self.assertEqual(documented_commands([p]), {"secret", "sync", "photos"})

    def test_flag_of_a_hidden_command_is_known(self):
        binary = self.fake_cli()
        p = write(self.d, "page.md", "```sh\nirsync secret --deep 1 file\n```\n")
        flags = cli_flags(binary, also=documented_commands([p]))
        self.assertEqual(check_flags([p], flags), [])


if __name__ == "__main__":
    unittest.main()
