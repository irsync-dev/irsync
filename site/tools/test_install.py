import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "install.sh"
BASE = "https://github.com/irsync-dev/irsync/releases/download"
ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "minisign"


class InstallTest(unittest.TestCase):
    def run_install(self, tags, *args, releases_file=None):
        with tempfile.TemporaryDirectory() as tmp:
            path = releases_file or str(Path(tmp) / "releases.json")
            if releases_file is None:
                Path(path).write_text(json.dumps([{"tag_name": t, "draft": False} for t in tags]))
            env = {
                "PATH": "/usr/bin:/bin",
                "IRSYNC_INSTALL_RELEASES_FILE": path,
                "IRSYNC_INSTALL_DRY_RUN": "1",
            }
            return subprocess.run(
                ["sh", str(SCRIPT), *args], env=env, capture_output=True, text=True
            )

    def picked(self, tags, *args):
        r = self.run_install(tags, *args)
        self.assertEqual(r.returncode, 0, r.stderr)
        words = r.stdout.split()
        self.assertEqual(words[:2], ["would", "install"], r.stdout)
        return words[2]

    def test_default_picks_latest_stable(self):
        self.assertEqual(self.picked(["v0.2.1", "v0.3.0-beta.2", "v0.2.0"]), "v0.2.1")

    def test_beta_newer_than_stable(self):
        tags = ["v0.2.1", "v0.3.0-beta.2", "v0.3.0-alpha.5"]
        self.assertEqual(self.picked(tags, "--beta"), "v0.3.0-beta.2")

    def test_beta_falls_back_when_stable_is_newer(self):
        self.assertEqual(self.picked(["v0.3.0", "v0.3.0-beta.2"], "--beta"), "v0.3.0")

    def test_alpha_takes_newest_of_all(self):
        tags = ["v0.2.1", "v0.3.0-beta.1", "v0.3.0-alpha.7"]
        self.assertEqual(self.picked(tags, "--alpha"), "v0.3.0-beta.1")

    def test_alpha_newer_than_beta(self):
        tags = ["v0.2.1", "v0.3.0-beta.1", "v0.4.0-alpha.1"]
        self.assertEqual(self.picked(tags, "--alpha"), "v0.4.0-alpha.1")

    def test_numeric_ordering(self):
        tags = ["v0.9.0", "v0.10.0", "v0.10.0-beta.9", "v0.10.0-beta.10"]
        self.assertEqual(self.picked(tags), "v0.10.0")
        self.assertEqual(self.picked(tags, "--beta"), "v0.10.0")

    def test_beta_number_ordering(self):
        tags = ["v0.1.0-beta.9", "v0.1.0-beta.10"]
        self.assertEqual(self.picked(tags, "--beta"), "v0.1.0-beta.10")

    def test_no_stable_release_yet(self):
        r = self.run_install(["v0.1.0-alpha.1"])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("--alpha", r.stderr)

    def test_args_pass_through(self):
        r = self.run_install(["v0.2.1"], "--no-modify-path")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.strip().endswith("--no-modify-path"), r.stdout)

    def test_args_pass_through_after_channel(self):
        r = self.run_install(["v0.2.1", "v0.3.0-beta.1"], "--beta", "--no-modify-path")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("v0.3.0-beta.1", r.stdout)
        self.assertNotIn("--beta", r.stdout)
        self.assertTrue(r.stdout.strip().endswith("--no-modify-path"), r.stdout)

    def test_url(self):
        r = self.run_install(["v0.2.1"])
        self.assertIn(f"{BASE}/v0.2.1/irsync-installer.sh", r.stdout.split())

    def test_github_unreachable(self):
        r = self.run_install([], releases_file="/nonexistent/releases.json")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("could not reach GitHub", r.stderr)
        self.assertIn("rate limit", r.stderr)

    def test_ignores_unknown_tag_shapes(self):
        self.assertEqual(self.picked(["v0.2.1", "nightly", "v0.3.0-rc.1"], "--alpha"), "v0.2.1")


class SignatureTest(unittest.TestCase):
    """install.sh checks the installer's minisign signature before running it."""

    # What install.sh needs besides a verifier.
    TOOLS = ["sh", "grep", "sed", "awk", "mktemp", "rm", "cat", "printf"]

    def setUp(self):
        self.tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmpdir.cleanup)
        self.tmp = Path(self.tmpdir.name)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        for tool in self.TOOLS:
            (self.bin / tool).symlink_to(shutil.which(tool))
        self.test_key = (FIXTURES / "test.pub").read_text().splitlines()[1]

    def add_minisign(self):
        real = shutil.which("minisign")
        if real is None:
            if os.environ.get("CI"):
                self.fail("minisign is not installed, and CI is set")
            self.skipTest("minisign is not installed")
        (self.bin / "minisign").symlink_to(real)

    def add_stub(self, name, body):
        stub = self.bin / name
        stub.write_text("#!/bin/sh\n" + body)
        stub.chmod(0o755)

    def download_dir(self, sig="irsync-installer.sh.minisig", installer=None):
        d = self.tmp / "dl"
        d.mkdir(exist_ok=True)
        text = (installer if installer is not None
                else (FIXTURES / "irsync-installer.sh").read_text())
        (d / "irsync-installer.sh").write_text(text)
        if sig is not None:
            shutil.copy(FIXTURES / sig, d / "irsync-installer.sh.minisig")
        return d

    def run_install(self, *args, tag="v9.9.9", sig="irsync-installer.sh.minisig",
                    keys=None, installer=None):
        releases = self.tmp / "releases.json"
        releases.write_text(json.dumps([{"tag_name": tag, "draft": False}]))
        env = {
            "PATH": str(self.bin),
            "TMPDIR": str(self.tmp),
            "IRSYNC_INSTALL_RELEASES_FILE": str(releases),
            "IRSYNC_INSTALL_DOWNLOAD_DIR": str(self.download_dir(sig, installer)),
            "IRSYNC_INSTALL_PUBLIC_KEYS": keys or self.test_key,
        }
        return subprocess.run([str(self.bin / "sh"), str(SCRIPT), *args], env=env,
                              capture_output=True, text=True)

    def assert_ran(self, r):
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("fake installer ran", r.stdout)

    def assert_stopped(self, r, message):
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout, "", "the installer must not run")
        self.assertIn(message, r.stderr)

    def test_keys_match_release_keys_file(self):
        pub = (ROOT / "packaging" / "signing" / "release-keys.pub").read_text()
        wanted = [l for l in pub.splitlines() if l and not l.startswith("untrusted comment:")]
        m = re.search(r'^\s*keys="([^"]*)"', SCRIPT.read_text(), re.MULTILINE)
        self.assertIsNotNone(m, "no keys= in install.sh")
        self.assertEqual(m.group(1).split(), wanted)

    def test_good_signature_with_minisign(self):
        self.add_minisign()
        self.assert_ran(self.run_install())

    def test_good_signature_with_irsync(self):
        log = self.tmp / "irsync.log"
        self.add_stub("irsync", f"""
if [ "$1 $2" = "verify-signature --help" ]; then echo "check a minisign signature"; exit 0; fi
printf '%s\\n' "$*" > {log}
exit 0
""")
        self.assert_ran(self.run_install())
        call = log.read_text()
        self.assertIn(f"--public-key {self.test_key}", call)
        self.assertIn("--trusted-comment irsync v9.9.9", call)
        self.assertIn("--signature", call)

    def test_bad_signature_fails(self):
        self.add_minisign()
        other = "RWR6ngoFwyY7D9i4MAL2T4n1UkGwrCdSGA/SMowtANaffYWkTZdrAtAZ"
        self.assert_stopped(self.run_install(keys=other), "does not match")

    def test_tampered_installer_fails(self):
        self.add_minisign()
        r = self.run_install(installer="#!/bin/sh\necho fake installer ran: evil\n")
        self.assert_stopped(r, "does not match")

    def test_failing_irsync_stops(self):
        self.add_stub("irsync", """
if [ "$1" = verify-signature ] && [ "$2" = --help ]; then echo minisign; exit 0; fi
exit 1
""")
        self.assert_stopped(self.run_install(), "does not match")

    def test_irsync_usage_error_is_not_a_bad_signature(self):
        self.add_stub("irsync", """
if [ "$1" = verify-signature ] && [ "$2" = --help ]; then echo minisign; exit 0; fi
echo "irsync: unexpected argument" >&2
exit 2
""")
        r = self.run_install()
        self.assert_stopped(r, "irsync could not check the signature")
        self.assertIn("irsync: unexpected argument", r.stderr)
        self.assertNotIn("does not match", r.stderr)

    def test_missing_signature_fails(self):
        self.add_minisign()
        r = self.run_install(sig=None)
        self.assert_stopped(r, "missing signature")
        self.assertIn("irsync-installer.sh.minisig", r.stderr)

    def test_wrong_tag_comment_fails(self):
        self.add_minisign()
        r = self.run_install(sig="irsync-installer.sh.v9.9.8.minisig")
        self.assert_stopped(r, "does not match")

    def test_old_irsync_on_path_falls_through(self):
        self.add_minisign()
        self.add_stub("irsync", f"""
if [ "$1 $2" = "verify-signature --help" ]; then echo "error: unrecognized subcommand"; exit 2; fi
echo "old irsync was called: $*" >&2
exit 99
""")
        r = self.run_install()
        self.assert_ran(r)
        self.assertNotIn("old irsync was called", r.stderr)

    def test_no_verifier_warns(self):
        r = self.run_install()
        self.assert_ran(r)
        self.assertIn("cannot check the installer's signature (install minisign to check it); "
                      "relying on HTTPS and checksums", r.stderr)

    def test_no_verifier_require_signature_fails(self):
        r = self.run_install("--require-signature")
        self.assert_stopped(r, "cannot check the installer's signature")

    def test_no_verifier_require_signature_downloads_nothing(self):
        r = self.run_install("--require-signature", sig=None)
        self.assert_stopped(r, "cannot check the installer's signature")
        self.assertNotIn("missing signature", r.stderr)

    def test_require_signature_not_passed_on(self):
        self.add_minisign()
        for flags in (["--require-signature", "--beta"], ["--beta", "--require-signature"]):
            r = self.run_install(*flags, "--no-modify-path")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), "fake installer ran: --no-modify-path")

    def test_require_signature_with_a_verifier_runs(self):
        self.add_minisign()
        self.assert_ran(self.run_install("--require-signature"))

    def test_before_first_signed_skips_check(self):
        # No signature file and no verifier.
        r = self.run_install("--beta", tag="v0.1.0-beta.4", sig=None)
        self.assert_ran(r)
        self.assertIn("not checking", r.stderr)

    def test_before_first_signed_require_signature_fails(self):
        r = self.run_install("--beta", "--require-signature", tag="v0.1.0-beta.4", sig=None)
        self.assert_stopped(r, "has no signature to check")

    def test_first_signed_is_checked(self):
        r = self.run_install("--beta", "--require-signature", tag="v0.1.0-beta.5")
        self.assert_stopped(r, "cannot check the installer's signature")


class ShellTest(unittest.TestCase):
    def test_truncated_script_does_nothing(self):
        lines = SCRIPT.read_text().rstrip("\n").split("\n")
        self.assertEqual(lines[-1], 'main "$@"')
        with tempfile.TemporaryDirectory() as tmp:
            releases = Path(tmp) / "releases.json"
            releases.write_text(json.dumps([{"tag_name": "v0.2.1"}]))
            env = {
                "PATH": "/usr/bin:/bin",
                "IRSYNC_INSTALL_RELEASES_FILE": str(releases),
                "IRSYNC_INSTALL_DRY_RUN": "1",
            }
            # Cut at every line boundary and mid-line before the last line.
            for n in range(1, len(lines)):
                part = "\n".join(lines[:n]) + "\n"
                r = subprocess.run(
                    ["sh", "-s", "--", "--no-modify-path"],
                    input=part, env=env, capture_output=True, text=True,
                )
                # A syntax error on stderr is fine; installing anything is not.
                self.assertEqual(r.stdout, "", f"cut after line {n}")
                self.assertNotIn("installing", r.stderr, f"cut after line {n}")

    def test_posix_sh(self):
        subprocess.run(["sh", "-n", str(SCRIPT)], check=True)
        dash = shutil.which("dash")
        if dash:
            subprocess.run([dash, "-n", str(SCRIPT)], check=True)


FAKE_FETCHER = """#!/bin/sh
# Logs its arguments, then prints a releases list for the API URL and a
# do-nothing installer for anything else. `--help` lists --https-only
# unless FAKE_NO_HTTPS_ONLY is set (BusyBox wget has no such flag).
if [ "$1" = --help ]; then
  [ -n "${FAKE_NO_HTTPS_ONLY-}" ] || echo "  --https-only   only follow secure HTTPS links"
  exit 0
fi
printf '%s\\n' "$*" >> "$FAKE_LOG"
for a in "$@"; do
  case $a in
    https://api.github.com/*) printf '[{"tag_name": "v1.2.3", "draft": false}]\\n'; exit 0 ;;
  esac
done
printf 'exit 0\\n'
"""

# What install.sh runs besides the downloader.
TOOLS = ["sh", "grep", "sed", "awk", "mktemp", "rm", "cat", "printf"]


class FetchFlagsTest(unittest.TestCase):
    """install.sh's own downloads must refuse plain HTTP, even after a redirect."""

    def run_with(self, fetcher, extra_env=None, expect_ok=True):
        with tempfile.TemporaryDirectory() as tmp:
            bindir = Path(tmp) / "bin"
            bindir.mkdir()
            for tool in TOOLS:
                (bindir / tool).symlink_to(shutil.which(tool))
            fake = bindir / fetcher
            fake.write_text(FAKE_FETCHER)
            fake.chmod(0o755)
            log = Path(tmp) / "calls.log"
            env = {"PATH": str(bindir), "FAKE_LOG": str(log), "TMPDIR": tmp,
                   **(extra_env or {})}
            r = subprocess.run([str(bindir / "sh"), str(SCRIPT)], env=env,
                               capture_output=True, text=True)
            if not expect_ok:
                return r, log
            self.assertEqual(r.returncode, 0, r.stderr)
            calls = log.read_text().splitlines()
            self.assertEqual(len(calls), 2, calls)
            return calls

    def test_curl_downloads_are_https_only(self):
        for call in self.run_with("curl"):
            self.assertIn("--proto =https", call)
            self.assertIn("--tlsv1.2", call)

    def test_wget_downloads_are_https_only(self):
        for call in self.run_with("wget"):
            self.assertIn("--https-only", call)

    def test_wget_without_https_only_refuses(self):
        r, log = self.run_with("wget", {"FAKE_NO_HTTPS_ONLY": "1"}, expect_ok=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("install curl", r.stderr)
        self.assertNotIn("could not reach GitHub", r.stderr)
        self.assertFalse(log.exists(), "nothing may be downloaded")


if __name__ == "__main__":
    unittest.main()
