#!/usr/bin/env python3
"""Select required CI jobs from a complete PR diff; non-PR events always run."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


def relevant(path, kind):
    # A filter change must test its own consumers.
    if path in {"scripts/ci-path-filter.py", "scripts/test_ci_path_filter.py",
                "scripts/ci-affected-nextest-filter.py",
                "scripts/test_ci_affected_nextest_filter.py"}:
        return True
    if kind == "native" and (path.startswith("scripts/verify-rust-gateway/")
                             or path in {".github/workflows/desktop-native-macos.yml",
                                         "scripts/native_image_diagnostics.py",
                                         "scripts/test_native_image_cleanup.py"}):
        return True
    if kind == "actions":
        return (path.startswith((".github/workflows/", ".github/actions/"))
                or path in {".github/zizmor.yml", ".github/actionlint.yaml", ".github/actionlint.yml"})
    # Markdown is not a Rust input, except the legal texts that pp-api and pp-gateway embed with
    # include_str!/include_bytes! and the fixture READMEs that sit inside hashed fixture trees.
    if path.endswith(".md") and not path.startswith(("web/apps/server/src/data/", "fixtures/")):
        return False
    return (path.startswith(("rust/", "web/apps/", "web/packages/", "web/scripts/storage-fixtures/",
                             "fixtures/"))
            or path in {"rust-toolchain.toml", ".github/workflows/rust-cross-os.yml",
                        "web/scripts/atomic-package-build.mjs", "scripts/setup-ci-linux.sh",
                        "scripts/ci-rust-runner.py", "scripts/test_ci_rust_runner.py",
                        "scripts/ci-pp-local-sccache.sh",
                        ".github/actions/linux-gui-deps/action.yml"}
            or re.fullmatch(r"web/tsconfig[^/]*\.json", path) is not None
            or re.fullmatch(r"web/package[^/]*\.json", path) is not None)


WINDOWS_LABELS = {"ci:full", "ci:windows"}
WINDOWS_MARKERS = (b"windows", b"std::os::unix", b"cfg(unix)", b"target_family", b"target_os", b"libc::")
WINDOWS_PATHS = ("rust/crates/pp-os/", ".github/workflows/", ".github/actions/")
WINDOWS_FILES = {"rust/Cargo.lock", "web/scripts/atomic-package-build.mjs",
                 "rust/scripts/build-desktop-manifest.py", "rust-toolchain.toml", ".github/actionlint.yaml",
                 "scripts/ci-path-filter.py", "scripts/test_ci_path_filter.py"}


def windows_relevant(path, base, head):
    if path in WINDOWS_FILES or path.startswith(WINDOWS_PATHS):
        return True
    if not path.startswith("rust/") or not path.endswith((".rs", ".toml")):
        return False
    for sha in (head, base):
        try:
            text = subprocess.check_output(["git", "show", f"{sha}:{path}"], stderr=subprocess.DEVNULL)
        except subprocess.CalledProcessError:
            if sha == head:
                return True
            continue
        text = text.lower()
        if any(marker in text for marker in WINDOWS_MARKERS):
            return True
    return False


def select(event, base, head, kind, labels=()):
    if event != "pull_request":
        return True
    if kind == "windows" and WINDOWS_LABELS.intersection(labels):
        return True
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha or "") for sha in (base, head)):
        return True
    try:
        paths = subprocess.check_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", errors="surrogateescape").split("\0")
    except subprocess.CalledProcessError:
        return True
    if kind == "windows":
        return any(windows_relevant(path, base, head) for path in paths if path)
    return any(relevant(path, kind) for path in paths if path)


if __name__ == "__main__":
    kind = sys.argv[1]
    if kind not in {"rust", "actions", "native", "windows"}:
        raise SystemExit("Expected rust, actions, native or windows")
    labels = json.loads(os.environ.get("PR_LABELS") or "[]")
    value = str(select(os.environ["EVENT_NAME"], os.environ.get("BASE_SHA"),
                       os.environ.get("HEAD_SHA"), kind, labels)).lower()
    message = f"{kind} inputs changed: {value}\n"
    print(message, end="")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"{'windows' if kind == 'windows' else 'relevant'}={value}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
        output.write(message)
