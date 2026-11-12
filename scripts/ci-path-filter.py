#!/usr/bin/env python3
"""Select required CI jobs from a complete PR diff; merge groups select only queue validation kinds."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


# Kept in sync with workaround-ratchet.mjs by test_workaround_extensions_match_ratchet.
WORKAROUND_EXTENSIONS = (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs",
                         ".rs", ".py", ".sh", ".yml", ".yaml", ".css", ".html")


def relevant(path, kind):
    # A filter change must test its own consumers.
    if path in {"scripts/ci-path-filter.py", "scripts/test_ci_path_filter.py",
                "scripts/ci-affected-nextest-filter.py",
                "scripts/test_ci_affected_nextest_filter.py"}:
        return True
    if kind == "actions":
        return (path.startswith((".github/workflows/", ".github/actions/"))
                or path in {".github/zizmor.yml", ".github/actionlint.yaml", ".github/actionlint.yml"})
    if kind == "web" and (
            path.endswith(WORKAROUND_EXTENSIONS)
            or path.startswith("docs/scripts/")
            or path in {"README.md", "docs/README.md", "docs/INSTALL.md", "docs/index.html",
                        "docs/design-system.md", "docs/product/ui-tokens.md"}):
        return True
    if kind in {"ui", "web"}:
        # Keep embedded legal text, hashed fixtures and the Docker deployment input.
        if (path.endswith(".md")
                and not path.startswith(("web/apps/server/src/data/", "fixtures/"))
                and not (kind == "web" and re.match(r"web/apps/[^/]+/src/", path))
                and path not in {"web/DEPLOY.md", "LICENSE-SUMMARY.md", "ATTRIBUTION.md",
                                 "THIRD_PARTY_NOTICES.md"}):
            return False
        shared = (path.startswith(("web/scripts/", "web/eslint", "web/knip", "web/tsconfig"))
                  or re.fullmatch(r"web/package[^/]*\.json", path) is not None
                  or path.startswith(("scripts/", "fixtures/", "manifests/", ".github/workflows/",
                                      ".github/actions/"))
                  or path in {".github/dependabot.yml", ".github/zizmor.yml",
                              ".github/actionlint.yaml", ".github/actionlint.yml"})
        if kind == "ui":
            return shared or path.startswith(("web/apps/ui/", "web/packages/"))
        # lint covers apps/ui and npm test/typecheck cover rust-client too. Keep those
        # inputs selected until their commands move to the replacement UI job.
        return (shared or path.startswith(("web/", "slicer-sidecar/", "rust/"))
                or path in {"Caddyfile", "Dockerfile", ".dockerignore", "docker-entrypoint.sh",
                            "pp-compose.yml", "rust-toolchain.toml", "LICENSE", "LICENSE-SUMMARY.md",
                            "ATTRIBUTION.md", "THIRD_PARTY_NOTICES.md"}
                or re.fullmatch(r"docker-compose[^/]*\.yml", path) is not None)
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


def select(event, base, head, kind, labels=()):
    if event == "merge_group":
        return kind in {"rust", "actions", "windows", "macos", "ui"}
    if event != "pull_request":
        return True
    if kind in {"windows", "macos"}:
        return "ci:cross-os" in labels
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha or "") for sha in (base, head)):
        return True
    try:
        paths = subprocess.check_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", errors="surrogateescape").split("\0")
    except subprocess.CalledProcessError:
        return True
    return any(relevant(path, kind) for path in paths if path)


if __name__ == "__main__":
    kind = sys.argv[1]
    if kind not in {"rust", "actions", "windows", "macos", "ui", "web"}:
        raise SystemExit("Expected rust, actions, windows, macos, ui or web")
    labels = json.loads(os.environ.get("PR_LABELS") or "[]")
    value = str(select(os.environ["EVENT_NAME"], os.environ.get("BASE_SHA"),
                       os.environ.get("HEAD_SHA"), kind, labels)).lower()
    message = f"{kind} inputs changed: {value}\n"
    print(message, end="")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"{kind if kind in {'windows', 'macos'} else 'relevant'}={value}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
        output.write(message)
