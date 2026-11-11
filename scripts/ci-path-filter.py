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


WINDOWS_LABELS = {"ci:full", "ci:windows", "ci:cross-os"}
MACOS_LABELS = {"macos", "ci:full", "ci:cross-os"}
# Platform-gated code: Windows names, Unix-only code that needs a Windows counterpart, Linux-only
# code whose fallback runs on Windows, and path-separator handling: node:path's `sep` (namespaced or
# destructured), `path.win32`, a backslash string literal ("\\" or '\\') and a backslash regex (/\\/).
WINDOWS_MARKERS = re.compile(rb"windows|win32|unix|linux|os::fd|target_family|target_os|\blibc\b|named.?pipe|"
                             rb"\\\\\.\\pipe|process\.platform|/proc/|\bsep\b|path\.win32|"
                             rb"[\"']\\\\[\"']|/\\\\/")
WINDOWS_PATHS = ("rust/crates/pp-os/", "rust/crates/pp-desktop/icons/", ".github/workflows/", ".github/actions/")
WINDOWS_FILES = {".gitattributes", "rust/Cargo.lock", "web/package.json", "web/package-lock.json", "web/scripts/atomic-package-build.mjs",
                 "rust/scripts/build-desktop-manifest.py", "rust/scripts/desktop_workspaces.py",
                 "rust/crates/pp-desktop/tauri.conf.json",
                 "rust/scripts/fetch-historical-schema-commits.sh", "rust-toolchain.toml", ".github/actionlint.yaml",
                 "scripts/ci-path-filter.py", "scripts/test_ci_path_filter.py"}


def windows_relevant(path, base, head, kind="windows"):
    if kind == "macos" and (path.startswith(("rust/scripts/", "scripts/verify-rust-gateway/"))
                            or path in {"scripts/native_image_diagnostics.py",
                                        "scripts/test_native_image_cleanup.py",
                                        "scripts/ci-rust-runner.py", "scripts/test_ci_rust_runner.py"}):
        return True
    # The Windows job builds every web workspace (npm ci, npm run build) and launches the Node child.
    if (path in WINDOWS_FILES or path.startswith(WINDOWS_PATHS)
            or re.fullmatch(r"web/(apps|packages)/[^/]+/package\.json", path)):
        return True
    rust = path.startswith("rust/") and path.endswith((".rs", ".toml"))
    # The server and the shared packages it imports run inside that Node child.
    server = path.startswith(("web/apps/server/src/", "web/packages/")) and path.endswith(".ts")
    if not rust and not server:
        return False
    # A deleted file has no head copy and an added file no base copy; each is judged on the copy
    # that exists. Only a path that neither side can read selects Windows unseen.
    read = False
    for sha in (head, base):
        try:
            text = subprocess.check_output(["git", "show", f"{sha}:{path}"], stderr=subprocess.DEVNULL)
        except subprocess.CalledProcessError:
            continue
        read = True
        if WINDOWS_MARKERS.search(text.lower()) or (kind == "macos" and
                re.search(rb"macos|darwin|apple|appkit|corefoundation", text.lower())):
            return True
    return not read


def select(event, base, head, kind, labels=()):
    if event != "pull_request":
        return True
    if ((kind == "windows" and WINDOWS_LABELS.intersection(labels))
            or (kind == "macos" and MACOS_LABELS.intersection(labels))):
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
    if kind in {"windows", "macos"}:
        try:
            old = subprocess.check_output(["git", "merge-base", base, head],
                                          stderr=subprocess.DEVNULL).decode().strip()
        except subprocess.CalledProcessError:
            return True
        return any(windows_relevant(path, old, head, kind) for path in paths if path)
    return any(relevant(path, kind) for path in paths if path)


if __name__ == "__main__":
    kind = sys.argv[1]
    if kind not in {"rust", "actions", "native", "windows", "macos"}:
        raise SystemExit("Expected rust, actions, native, windows or macos")
    labels = json.loads(os.environ.get("PR_LABELS") or "[]")
    value = str(select(os.environ["EVENT_NAME"], os.environ.get("BASE_SHA"),
                       os.environ.get("HEAD_SHA"), kind, labels)).lower()
    message = f"{kind} inputs changed: {value}\n"
    print(message, end="")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"{kind if kind in {'windows', 'macos'} else 'relevant'}={value}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
        output.write(message)
