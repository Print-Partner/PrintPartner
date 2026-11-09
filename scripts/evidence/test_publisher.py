"""Tests for publisher.py against the README threat model: whatever the user's git config or
environment says, no token reaches the output and the PNGs go only to Print-Partner/PrintPartner.
No test touches the network: clones are stood in for and the publisher is a stub."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import publisher

TOKEN = "ghp_canaryEvidenceToken0123456789"
GOOD = "https://github.com/Print-Partner/PrintPartner"
FOREIGN = f"https://x-access-token:{TOKEN}@github.com/poitee/PrintPartner"


def plain_git(*args):
    """git as the test sets things up: no isolation, but never the user's real config."""
    subprocess.run(["git", *args], check=True, capture_output=True,
                   env=dict(os.environ, GIT_CONFIG_NOSYSTEM="1"))


class Base(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = Path(directory.name)
        # The "user's" global config, which the harness must ignore. Tests write hostile rules here.
        self.user_config = self.base / "user-gitconfig"
        self.user_config.write_text("")
        self.env = {"XDG_CACHE_HOME": str(self.base / "cache"), "GIT_CONFIG_GLOBAL": str(self.user_config),
                    "HOME": str(self.base / "home")}
        patcher = mock.patch.dict(os.environ, self.env)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.cache = self.base / "cache" / "evidence-repo-Print-Partner-PrintPartner"

    def make_repo(self, path, url=GOOD):
        plain_git("init", "-q", str(path))
        plain_git("-C", str(path), "remote", "add", "origin", url)

    def fake_clone(self, cache):
        """Stands in for the network clone: a repository whose origin is the real URL."""
        self.make_repo(cache)
        return True

    def user_rule(self, key, value):
        plain_git("config", "--file", str(self.user_config), key, value)

    def refused(self):
        with mock.patch.object(publisher, "clone_cache", side_effect=self.fake_clone), \
                self.assertRaises(publisher.Refused) as refusal:
            publisher.evidence_cache()
        return str(refusal.exception)

    def stub_publisher(self, script):
        stub = self.base / "evidence"
        stub.write_text("#!/bin/sh\n" + script)
        stub.chmod(0o755)
        return stub

    def publish_in_child(self, stub, extra_env=None):
        """Runs publish() in a fresh process, as run.sh does, with the stub as publisher.
        The clone is stood in for by the cache the test prepared."""
        program = "import publisher; publisher.publish(1622, [], '')"
        env = dict(os.environ, **self.env, PP_EVIDENCE_BIN=str(stub), **(extra_env or {}))
        return subprocess.run([sys.executable, "-c", program], cwd=Path(publisher.__file__).parent,
                              capture_output=True, text=True, env=env)


class GitEnv(Base):
    def test_drops_every_git_variable_and_isolates_config(self):
        hostile = {"GIT_TRACE": "1", "GIT_TRACE_CURL": "1", "GIT_TRACE_PACKET": "1", "GIT_CURL_VERBOSE": "1",
                   "GIT_SSH_COMMAND": "ssh -v", "GIT_CONFIG_PARAMETERS": "'url.x.insteadof'='y'",
                   "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "url.x.insteadOf", "GIT_CONFIG_VALUE_0": "y",
                   "GIT_DIR": "/elsewhere", "PATH": "/usr/bin"}
        env = publisher.git_env(hostile)
        self.assertEqual(env, {
            "PATH": "/usr/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_COUNT": "5",
            "GIT_CONFIG_KEY_0": "credential.helper", "GIT_CONFIG_VALUE_0": "",
            "GIT_CONFIG_KEY_1": "credential.helper", "GIT_CONFIG_VALUE_1": "!gh auth git-credential",
            "GIT_CONFIG_KEY_2": "core.sshCommand", "GIT_CONFIG_VALUE_2": "false",
            "GIT_CONFIG_KEY_3": "protocol.allow", "GIT_CONFIG_VALUE_3": "never",
            "GIT_CONFIG_KEY_4": "protocol.https.allow", "GIT_CONFIG_VALUE_4": "always"})

    def test_only_https_transport_and_no_ssh_command_even_with_local_config(self):
        # A local core.sshCommand and protocol.allow lose to the pinned command-line config.
        self.make_repo(self.cache)
        plain_git("-C", str(self.cache), "config", "core.sshCommand", "touch /nonexistent/ran")
        plain_git("-C", str(self.cache), "config", "protocol.allow", "always")
        for url in ("git@github.com:Print-Partner/PrintPartner", str(self.base / "elsewhere.git")):
            with self.subTest(url=url):
                result = publisher.git("-C", str(self.cache), "ls-remote", url)
                self.assertNotEqual(result.returncode, 0)
        effective = publisher.git("-C", str(self.cache), "config", "--get", "core.sshCommand")
        self.assertEqual(effective.stdout.strip(), "false")

    def test_the_users_global_rewrites_have_no_effect(self):
        self.user_rule(f"url.{FOREIGN}.insteadOf", GOOD)
        self.user_rule(f"url.{FOREIGN}.pushInsteadOf", GOOD)
        self.make_repo(self.cache)
        self.assertEqual(publisher.remote_urls(self.cache), [GOOD, GOOD])
        self.assertEqual(publisher.evidence_cache(), Path(os.path.realpath(self.cache)))


class Remotes(Base):
    def origin_accepted(self, url):
        self.make_repo(self.cache, url)
        try:
            return publisher.cache_ok(self.cache)
        finally:
            shutil.rmtree(self.cache)

    def test_accepts_only_the_exact_https_origin(self):
        for url in (GOOD, GOOD + ".git"):
            with self.subTest(url=url):
                self.assertTrue(self.origin_accepted(url))

    def test_refuses_every_other_origin(self):
        for url in ("git@github.com:Print-Partner/PrintPartner.git", "ssh://git@github.com/Print-Partner/PrintPartner",
                    "github.com:Print-Partner/PrintPartner", GOOD + "/", GOOD + ".git/",
                    f"https://x-access-token:{TOKEN}@github.com/Print-Partner/PrintPartner",
                    "https://user@github.com/Print-Partner/PrintPartner", "http://github.com/Print-Partner/PrintPartner",
                    "https://github.com/poitee/PrintPartner", "https://github.com/Print-Partner/PrintPartner2",
                    "https://github.com/print-partner/printpartner", "https://github.com:443/Print-Partner/PrintPartner",
                    "https://github.com/Print-Partner/PrintPartner/extra", "https://github.com.evil/Print-Partner/PrintPartner",
                    "https://github.com/Print-Partner/PrintPartner?x=1", "https://github.com/Print-Partner/Print%50artner",
                    "https://github.com\u2100/Print-Partner/PrintPartner",
                    "https://github.com\uff1a443/Print-Partner/PrintPartner", "file:///github.com/Print-Partner/PrintPartner",
                    "/tmp/github.com/Print-Partner/PrintPartner"):
            with self.subTest(url=url.encode("unicode_escape").decode().replace(TOKEN, "<token>")):
                self.assertFalse(self.origin_accepted(url))

    def test_a_cache_whose_origin_has_a_token_is_refused(self):
        self.make_repo(self.cache, f"https://x-access-token:{TOKEN}@github.com/Print-Partner/PrintPartner")
        message = self.refused()
        self.assertNotIn(TOKEN, message)

    def test_refuses_another_origin_a_foreign_pushurl_or_no_origin(self):
        cases = {
            "another origin": [("remote", "set-url", "origin", "https://github.com/poitee/PrintPartner")],
            "foreign pushurl": [("remote", "set-url", "--push", "origin", "https://github.com/poitee/PrintPartner")],
            "one of several push URLs": [("remote", "set-url", "--add", "--push", "origin", GOOD),
                                         ("remote", "set-url", "--add", "--push", "origin",
                                          "https://github.com/poitee/PrintPartner")],
            "no origin": [("remote", "remove", "origin")],
        }
        for name, commands in cases.items():
            with self.subTest(name):
                self.make_repo(self.cache)
                try:
                    for command in commands:
                        plain_git("-C", str(self.cache), *command)
                    self.assertIn("refusing to publish", self.refused())
                finally:
                    shutil.rmtree(self.cache)


class CacheShape(Base):
    def test_refuses_any_local_config_outside_the_allowlist(self):
        for key, value in (("core.sshCommand", "ssh -o ProxyCommand=elsewhere"),
                           ("http.proxy", "http://127.0.0.1:9"),
                           ("credential.helper", "store --file /tmp/captured"),
                           ("credential.https://github.com.helper", "store"),
                           ("http.extraHeader", f"Authorization: bearer {TOKEN}"),
                           ("http.https://github.com/.extraHeader", "X-Leak: 1"),
                           ("protocol.allow", "always"), ("protocol.ssh.allow", "always"),
                           ("remote.origin.pushurl", GOOD), ("remote.backup.url", GOOD),
                           (f"url.{FOREIGN}.insteadOf", GOOD), (f"url.{FOREIGN}.pushInsteadOf", GOOD),
                           ("include.path", "/dev/null"), ("includeIf.onbranch:evidence/**.path", "/dev/null"),
                           ("core.hooksPath", "/tmp/hooks"), ("core.fsmonitor", "true"),
                           ("extensions.worktreeConfig", "true")):
            with self.subTest(key=key.replace(TOKEN, "<token>")):
                self.make_repo(self.cache)
                try:
                    plain_git("-C", str(self.cache), "config", key, value)
                    message = self.refused()
                    self.assertIn("a key outside the allowlist", message)
                    self.assertNotIn(TOKEN, message)
                finally:
                    shutil.rmtree(self.cache)

    def test_a_partial_clone_passes_the_allowlist(self):
        # The same clone command the harness runs, from a local source (file:// is only for
        # this setup; the isolated env blocks it), then pointed at the real origin.
        source = self.base / "source.git"
        plain_git("init", "-q", "--bare", str(source))
        plain_git("-C", str(source), "config", "uploadpack.allowFilter", "true")
        work = self.base / "work"
        plain_git("init", "-q", "-b", "main", str(work))
        plain_git("-C", str(work), "-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q",
                  "--allow-empty", "-m", "seed")
        plain_git("-C", str(work), "push", "-q", str(source), "main")
        plain_git("clone", "--quiet", "--no-checkout", "--filter=blob:none", source.as_uri(), str(self.cache))
        plain_git("-C", str(self.cache), "remote", "set-url", "origin", GOOD)
        keys = publisher.git("-C", str(self.cache), "config", "--local", "--name-only", "--list").stdout.split()
        self.assertIn("remote.origin.promisor", keys)
        self.assertTrue(any(key.startswith("branch.") and key.endswith(".merge") for key in keys), keys)
        self.assertTrue(publisher.local_config_ok(self.cache), keys)
        # The publisher's `checkout -B evidence/<branch> origin/...` adds branch tracking keys.
        plain_git("-C", str(self.cache), "config", "branch.evidence/gre-1281.remote", "origin")
        plain_git("-C", str(self.cache), "config", "branch.evidence/gre-1281.merge", "refs/heads/evidence/gre-1281")
        self.assertEqual(publisher.evidence_cache(), Path(os.path.realpath(self.cache)))

    @unittest.skipUnless(os.environ.get("PP_EVIDENCE_NETWORK_TESTS") == "1",
                         "set PP_EVIDENCE_NETWORK_TESTS=1 to clone from GitHub")
    def test_a_real_harness_clone_passes_the_allowlist(self):
        self.assertTrue(publisher.clone_cache(self.cache))
        self.assertTrue(publisher.local_config_ok(self.cache))
        self.assertTrue(publisher.cache_ok(self.cache))

    def test_refuses_a_symlinked_cache(self):
        elsewhere = self.base / "elsewhere"
        self.make_repo(elsewhere)
        self.cache.parent.mkdir(parents=True)
        self.cache.symlink_to(elsewhere)
        self.assertIn("is a symlink", self.refused())

    def test_refuses_a_symlinked_cache_directory(self):
        real = self.base / "real-cache-home"
        real.mkdir()
        (self.base / "cache").symlink_to(real)
        self.assertIn("is a symlink", self.refused())

    def test_refuses_a_gitfile_in_place_of_dot_git(self):
        elsewhere = self.base / "elsewhere"
        self.make_repo(elsewhere)
        self.cache.mkdir(parents=True)
        (self.cache / ".git").write_text(f"gitdir: {elsewhere / '.git'}\n")
        self.assertIn("refusing to publish", self.refused())

    def test_a_missing_cache_is_cloned_then_checked(self):
        with mock.patch.object(publisher, "clone_cache", side_effect=self.fake_clone) as clone:
            self.assertEqual(publisher.evidence_cache(), Path(os.path.realpath(self.cache)))
        clone.assert_called_once_with(Path(os.path.realpath(self.cache)))

    def test_a_failed_clone_is_refused_and_removed(self):
        def half_clone(cache):
            cache.mkdir()
            return False
        with mock.patch.object(publisher, "clone_cache", side_effect=half_clone), \
                self.assertRaises(publisher.Refused) as refusal:
            publisher.evidence_cache()
        self.assertIn("cloning Print-Partner/PrintPartner", str(refusal.exception))
        self.assertFalse(self.cache.exists())

    def test_a_fresh_clone_that_fetches_elsewhere_is_removed(self):
        def foreign_clone(cache):
            self.make_repo(cache, "https://github.com/poitee/PrintPartner")
            return True
        with mock.patch.object(publisher, "clone_cache", side_effect=foreign_clone), \
                self.assertRaises(publisher.Refused):
            publisher.evidence_cache()
        self.assertFalse(self.cache.exists())

    def test_any_error_in_the_check_becomes_the_fixed_refusal(self):
        self.make_repo(self.cache)
        for error in (ValueError(f"netloc 'x-access-token:{TOKEN}@github.com' is invalid"),
                      UnicodeError(TOKEN), OSError(TOKEN)):
            with self.subTest(error=type(error).__name__), \
                    mock.patch.object(publisher, "remote_urls", side_effect=error):
                message = self.refused()
                self.assertNotIn(TOKEN, message)


class Publish(Base):
    def setUp(self):
        super().setUp()
        self.make_repo(self.cache)
        self.record = self.base / "record"

    def test_git_trace_and_the_publishers_own_output_never_reach_the_output(self):
        # The stub echoes the canary the way a git trace would, records its GIT_* variables,
        # and reports one evidence URL and the comment URL.
        stub = self.stub_publisher(f"""env | grep '^GIT_' | sort > '{self.record}'
cd "$EVIDENCE_CACHE" && git remote -v
echo "trace: Cookie {TOKEN}" >&2
echo "pushing to {FOREIGN}"
echo "raw: https://github.com/Print-Partner/PrintPartner/raw/evidence/b/20261109T000000Z/a.png"
echo "https://github.com/Print-Partner/PrintPartner/pull/1622#issuecomment-1"
""")
        result = self.publish_in_child(stub, {"GIT_TRACE": "1", "GIT_TRACE_CURL": "1", "GIT_CURL_VERBOSE": "1",
                                              "GIT_SSH_COMMAND": f"ssh -o User={TOKEN}"})
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, output)
        self.assertNotIn(TOKEN, output)
        self.assertEqual(result.stdout.splitlines(), [
            "evidence-run: published https://github.com/Print-Partner/PrintPartner/raw/evidence/b/20261109T000000Z/a.png",
            "evidence-run: published https://github.com/Print-Partner/PrintPartner/pull/1622#issuecomment-1",
        ])
        seen = self.record.read_text()
        self.assertNotIn("GIT_TRACE", seen)
        self.assertNotIn("GIT_CURL_VERBOSE", seen)
        self.assertNotIn("GIT_SSH_COMMAND", seen)
        self.assertIn(f"GIT_CONFIG_GLOBAL={os.devnull}\n", seen)

    def test_an_includeif_onbranch_rewrite_in_the_users_config_has_no_effect(self):
        # The rule turns on only after the publisher checks out evidence/<branch>, as Sol found.
        rule = self.base / "onbranch.gitconfig"
        rule.write_text(f'[url "{FOREIGN}"]\n\tpushInsteadOf = {GOOD}\n')
        self.user_rule("includeIf.onbranch:evidence/**.path", str(rule))
        stub = self.stub_publisher(f"""cd "$EVIDENCE_CACHE"
git checkout -q --orphan evidence/gre-1281
git remote get-url --push --all origin > '{self.record}'
""")
        result = self.publish_in_child(stub)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.record.read_text(), GOOD + "\n")
        # The same rule does rewrite the push URL for git that reads the user's config.
        check = subprocess.run(["git", "-C", str(self.cache), "remote", "get-url", "--push", "origin"],
                               capture_output=True, text=True, env=dict(os.environ, GIT_CONFIG_NOSYSTEM="1"))
        self.assertIn(TOKEN, check.stdout)

    def test_a_failed_publish_prints_only_a_fixed_message(self):
        stub = self.stub_publisher(f"""echo "fatal: unable to access '{FOREIGN}'" >&2
echo "{FOREIGN}"
exit 3
""")
        result = self.publish_in_child(stub)
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1)
        self.assertIn("publish failed (exit 3); its output is withheld", result.stderr)
        self.assertNotIn(TOKEN, output)
        self.assertNotIn("fatal:", output)

    def test_publishes_from_the_real_cache_path(self):
        stub = self.stub_publisher(f'printf %s "$EVIDENCE_CACHE" > \'{self.record}\'\n')
        self.assertEqual(self.publish_in_child(stub).returncode, 0)
        self.assertEqual(self.record.read_text(), os.path.realpath(self.cache))


if __name__ == "__main__":
    unittest.main()
