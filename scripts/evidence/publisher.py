"""Publishes evidence PNGs through /opt/agent-fleet/bin/evidence to Print-Partner/PrintPartner.

Threat model (README "Publishing"): whatever the user's git config or environment says,
nothing here prints a token, and the PNGs go only to Print-Partner/PrintPartner. A local
user who edits the root-owned publisher or this harness is out of scope.

How:
- Every git command, and the publisher, runs with git_env(): no GIT_* variable from the
  caller (GIT_TRACE*, GIT_CURL_VERBOSE, GIT_SSH_COMMAND, GIT_CONFIG_PARAMETERS and the
  rest), no system or global config, gh as the only credential helper, no prompts.
- The cache is a plain directory (no symlink) with no local include, includeIf or url.*
  config, and every fetch and push URL of its origin is the repository, with no
  credentials in it. A missing cache is cloned and checked the same way.
- The publisher's output is captured. Success prints only the evidence URLs it reports;
  failure prints a fixed message and the exit code.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import urllib.parse

EVIDENCE_BIN = os.environ.get("PP_EVIDENCE_BIN", "/opt/agent-fleet/bin/evidence")
EVIDENCE_REPO = "Print-Partner/PrintPartner"
EXPECTED = f"github.com/{EVIDENCE_REPO}"
CLONE_SECS = 300
PUBLISH_SECS = 600
GITHUB_REPO_PATH = re.compile(r"/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)")
SCP_REMOTE = re.compile(r"(?:([^@/:]+)@)?([^/:@]+):(.*)")
RAW_URL = re.compile(r"raw: (https://github\.com/Print-Partner/PrintPartner/raw/evidence/[A-Za-z0-9._/-]+)")
COMMENT_URL = re.compile(r"https://github\.com/Print-Partner/PrintPartner/pull/[0-9]+#issuecomment-[0-9]+")
CONFIG_REWRITES = r"^(include|includeif|url)\."


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
        GIT_CONFIG_COUNT="1",
        GIT_CONFIG_KEY_0="credential.helper",
        GIT_CONFIG_VALUE_0="!gh auth git-credential",
        GIT_TERMINAL_PROMPT="0",
    )
    return env


def git(*args, timeout=60):
    return subprocess.run(["git", *args], env=git_env(), stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=timeout)


def normalize_remote(url):
    """github.com/<owner>/<repo> for a GitHub remote without credentials, or None.

    Drops one trailing "/" and one trailing ".git", and maps git@github.com:<owner>/<repo>
    and ssh://git@github.com/<owner>/<repo> to the same form. Any userinfo other than the
    ssh user "git", another host, a port, a query or a deeper path gives None, so
    normalizing never makes a different repository match."""
    try:
        # urlsplit, username, password, hostname and port raise ValueError quoting the
        # netloc, userinfo included, for example on NFKC-unsafe characters.
        parts = urllib.parse.urlsplit(url)
        url_form = parts.scheme in ("https", "ssh") and not parts.query and not parts.fragment
        if url_form:
            if parts.port is not None or parts.password is not None:
                return None
            if parts.username is not None and (parts.scheme, parts.username) != ("ssh", "git"):
                return None
            host, path = parts.hostname, parts.path
    except ValueError:
        return None
    if not url_form:
        if parts.scheme and "://" in url:
            return None
        match = SCP_REMOTE.fullmatch(url)
        if not match or match.group(1) not in (None, "git") or match.group(3).startswith("/"):
            return None
        host, path = match.group(2).lower(), "/" + match.group(3)
    if host != "github.com":
        return None
    path = path.removesuffix("/").removesuffix(".git")
    match = GITHUB_REPO_PATH.fullmatch(path)
    if not match or ".." in path:
        return None
    return f"github.com/{match.group(1)}/{match.group(2)}"


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
        rewrites = git("-C", str(cache), "config", "--get-regexp", CONFIG_REWRITES)
        if rewrites.returncode != 1:  # 1 means no such key; 0 means one exists
            return False
        urls = remote_urls(cache)
        return urls is not None and all(normalize_remote(url) == EXPECTED for url in urls)
    except Exception:
        return False


def clone_cache(cache):
    """Clones EVIDENCE_REPO into cache the way the publisher does. Output is discarded."""
    try:
        result = git("clone", "--quiet", "--no-checkout", "--filter=blob:none",
                     f"https://{EXPECTED}", str(cache), timeout=CLONE_SECS)
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
        raise Refused(f"refusing to publish: evidence cache remote is not {EVIDENCE_REPO}, or the cache "
                      f"has include, includeIf or url config; move {cache} aside and run again")
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
