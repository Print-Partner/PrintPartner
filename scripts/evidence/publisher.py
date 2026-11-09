"""Publishes evidence PNGs through /opt/agent-fleet/bin/evidence to Print-Partner/PrintPartner.

Threat model (README "Publishing"): whatever the user's git config or environment says,
nothing here prints a token, and the PNGs go only to Print-Partner/PrintPartner. A local
user who edits the root-owned publisher or this harness is out of scope.

How:
- Every git command, and the publisher, runs with git_env(): no GIT_* variable from the
  caller (GIT_TRACE*, GIT_CURL_VERBOSE, GIT_SSH_COMMAND, GIT_CONFIG_PARAMETERS and the
  rest), no system or global config, gh as the only credential helper, https as the only
  transport, no ssh command and no prompts.
- The cache is a plain directory (no symlink) whose local config holds only the keys a
  fresh clone writes (an allowlist), and every fetch and push URL of its origin is exactly
  https://github.com/Print-Partner/PrintPartner, with or without .git. A missing cache is
  cloned and checked the same way.
- The publisher's output is captured. Success prints only the evidence URLs it reports;
  failure prints a fixed message and the exit code.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess

EVIDENCE_BIN = os.environ.get("PP_EVIDENCE_BIN", "/opt/agent-fleet/bin/evidence")
EVIDENCE_REPO = "Print-Partner/PrintPartner"
ORIGIN = f"https://github.com/{EVIDENCE_REPO}"
# The only origin URLs accepted: https, no credentials, no other spelling.
ORIGIN_URLS = frozenset({ORIGIN, ORIGIN + ".git"})
CLONE_SECS = 300
PUBLISH_SECS = 600
RAW_URL = re.compile(r"raw: (https://github\.com/Print-Partner/PrintPartner/raw/evidence/[A-Za-z0-9._/-]+)")
COMMENT_URL = re.compile(r"https://github\.com/Print-Partner/PrintPartner/pull/[0-9]+#issuecomment-[0-9]+")
# The cache's local config may hold only these keys: what `git clone --no-checkout
# --filter=blob:none` writes (git 2.53), branch tracking the publisher's `checkout -B`
# adds, and a few keys other git versions or file systems write. Anything else, such as
# core.sshCommand, http.proxy, http.*.extraHeader, credential.*, protocol.*, url.*,
# include.* or remote.origin.pushurl, refuses the cache.
LOCAL_CONFIG_KEYS = frozenset({
    "core.repositoryformatversion", "core.filemode", "core.bare", "core.logallrefupdates",
    "core.ignorecase", "core.precomposeunicode",
    "remote.origin.url", "remote.origin.fetch",
    "remote.origin.promisor", "remote.origin.partialclonefilter", "extensions.partialclone",
})
BRANCH_KEY = re.compile(r"branch\.[^\n]+\.(remote|merge)")


class Refused(SystemExit):
    """A refusal with a fixed message: never a URL, a token or a command's output."""


def git_env(environ=None):
    """The environment for every git command and the publisher. Drops every GIT_*
    variable, disables system and global config, and adds gh as credential helper."""
    environ = os.environ if environ is None else environ
    env = {key: value for key, value in environ.items() if not key.startswith("GIT_")}
    env.update(
        GIT_CONFIG_NOSYSTEM="1",
        # /dev/null is an empty config no other process can write to.
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_TERMINAL_PROMPT="0",
    )
    # Command-line config outranks the cache's local config, so even a missed check
    # cannot add a helper, an ssh command or another transport.
    pinned = [
        ("credential.helper", ""),  # an empty value clears any helper list before ours
        ("credential.helper", "!gh auth git-credential"),
        ("core.sshCommand", "false"),
        ("protocol.allow", "never"),
        ("protocol.https.allow", "always"),
    ]
    env["GIT_CONFIG_COUNT"] = str(len(pinned))
    for index, (key, value) in enumerate(pinned):
        env[f"GIT_CONFIG_KEY_{index}"] = key
        env[f"GIT_CONFIG_VALUE_{index}"] = value
    return env


def git(*args, timeout=60):
    return subprocess.run(["git", *args], env=git_env(), stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=timeout)


def local_config_ok(cache):
    """Whether the cache's own config holds only allowlisted keys. --local reads .git/config
    alone and does not follow includes, so an include.path key shows and is refused."""
    result = git("-C", str(cache), "config", "--local", "--list", "--null")
    if result.returncode != 0:
        return False
    for entry in filter(None, result.stdout.split("\0")):
        key = entry.split("\n", 1)[0]
        if key not in LOCAL_CONFIG_KEYS and not BRANCH_KEY.fullmatch(key):
            return False
    return True


def remote_urls(cache):
    """Every fetch URL and every effective push URL of the cache's origin, or None."""
    urls = []
    for extra in ([], ["--push"]):
        result = git("-C", str(cache), "remote", "get-url", *extra, "--all", "origin")
        if result.returncode != 0:
            return None
        urls += result.stdout.split()
    return urls or None


def cache_ok(cache):
    """Whether the cache is a clean clone of EVIDENCE_REPO. Any error counts as no, and is
    dropped, because its message can quote a URL and its token."""
    try:
        dot_git = cache / ".git"
        if os.path.islink(cache) or os.path.islink(dot_git) or not dot_git.is_dir():
            return False
        if not local_config_ok(cache):
            return False
        urls = remote_urls(cache)
        return urls is not None and all(url in ORIGIN_URLS for url in urls)
    except Exception:
        return False


def clone_cache(cache):
    """Clones EVIDENCE_REPO into cache the way the publisher does. Output is discarded."""
    try:
        result = git("clone", "--quiet", "--no-checkout", "--filter=blob:none",
                     ORIGIN, str(cache), timeout=CLONE_SECS)
        return result.returncode == 0
    except Exception:
        return False


def evidence_cache():
    """The checked publisher clone for EVIDENCE_REPO, as a real path. A missing cache is
    cloned first, so the first publish is checked too."""
    base = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    name = f"evidence-repo-{EVIDENCE_REPO.replace('/', '-')}"
    if os.path.islink(base) or os.path.islink(base / name):
        raise Refused(f"refusing to publish: the evidence cache {base / name} or its parent is a symlink")
    base.mkdir(parents=True, exist_ok=True)
    if os.path.islink(base):
        raise Refused(f"refusing to publish: the evidence cache parent {base} is a symlink")
    cache = Path(os.path.realpath(base / name))
    fresh = not os.path.lexists(cache)
    if fresh and not clone_cache(cache):
        shutil.rmtree(cache, ignore_errors=True)
        raise Refused(f"refusing to publish: cloning {EVIDENCE_REPO} into {cache} failed")
    if not cache_ok(cache):
        if fresh:
            shutil.rmtree(cache, ignore_errors=True)
            raise Refused(f"refusing to publish: the fresh clone of {EVIDENCE_REPO} does not fetch and "
                          f"push only there; removed {cache}")
        raise Refused(f"refusing to publish: evidence cache origin is not {ORIGIN}, or its local git "
                      f"config has a key outside the allowlist; move {cache} aside and run again")
    return cache


def publish(pr, shots, note):
    """Publishes shots to PR pr. Returns the evidence URLs the publisher reported."""
    env = git_env()
    env.update(EVIDENCE_REPO=EVIDENCE_REPO, EVIDENCE_CACHE=str(evidence_cache()))
    command = [EVIDENCE_BIN, "publish", str(pr), *map(str, shots), *(["--note", note] if note else [])]
    status = None
    try:
        result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL, capture_output=True,
                                text=True, timeout=PUBLISH_SECS)
        status = result.returncode
    except Exception:
        pass
    if status != 0:
        # Raised outside the except block. The output is withheld: it can quote a remote URL.
        raise Refused(f"publish failed (exit {status if status is not None else 'none: it did not finish'}); "
                      "its output is withheld because it can quote a remote URL")
    urls = []
    for line in result.stdout.splitlines():
        if match := RAW_URL.fullmatch(line.strip()):
            urls.append(match.group(1))
        elif COMMENT_URL.fullmatch(line.strip()):
            urls.append(line.strip())
    for url in urls:
        print(f"evidence-run: published {url}", flush=True)
    if not urls:
        print("evidence-run: published; the publisher reported no evidence URLs", flush=True)
    return urls


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description=f"Publish PNGs to a {EVIDENCE_REPO} PR comment (see README).")
    parser.add_argument("pr", type=int)
    parser.add_argument("files", nargs="+", type=Path)
    parser.add_argument("--note", default="")
    args = parser.parse_args(argv)
    for path in args.files:
        if path.suffix.lower() != ".png" or not path.is_file():
            parser.error(f"not a PNG file: {path}")
    publish(args.pr, [path.resolve() for path in args.files], args.note)


if __name__ == "__main__":
    main()
