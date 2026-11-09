#!/usr/bin/env python3
"""Select a nextest filterset from the PR diff; non-PR events always run all().

On pull requests, map changed rust/crates/<pkg>/ paths to
`rdeps(=<pkg>)` unions so nextest runs the changed crates and their reverse
dependents. Shared workspace inputs (Cargo.lock, web assets, fixtures, the
workflow, this script, …) force `all()` because they can affect any crate.
Push, merge_group, schedule and workflow_dispatch keep the full suite.
"""

import os
from pathlib import Path
import re
import subprocess


CRATE_PATH = re.compile(r"^rust/crates/([^/]+)/")


def package_for_path(path):
    match = CRATE_PATH.match(path)
    return match.group(1) if match else None


def forces_full_suite(path):
    if path.startswith(("web/", "fixtures/")):
        return True
    if path in {
        "rust/Cargo.toml",
        "rust/Cargo.lock",
        "rust-toolchain.toml",
        "rust/.config/nextest.toml",
        ".github/workflows/rust-cross-os.yml",
        "scripts/ci-path-filter.py",
        "scripts/test_ci_path_filter.py",
        "scripts/ci-affected-nextest-filter.py",
        "scripts/test_ci_affected_nextest_filter.py",
        "scripts/ci-desktop-rebuild-off-filter.py",
        "scripts/test_ci_desktop_rebuild_off_filter.py",
        "scripts/setup-ci-linux.sh",
        ".github/actions/linux-gui-deps/action.yml",
        "web/scripts/atomic-package-build.mjs",
    }:
        return True
    if path.startswith(("rust/tests/", "rust/scripts/", "rust/rules/", "rust/fuzz/",
                        "rust/rule-tests/")):
        return True
    if path.startswith("rust/") and not path.startswith("rust/crates/"):
        return True
    if re.fullmatch(r"web/(package[^/]*|tsconfig[^/]*)\.json", path):
        return True
    return False


def workspace_packages(crates_root):
    packages = set()
    root = Path(crates_root)
    if not root.is_dir():
        return packages
    for cargo in sorted(root.glob("*/Cargo.toml")):
        text = cargo.read_text(encoding="utf-8", errors="replace")
        match = re.search(r'(?m)^name\s*=\s*"([^"]+)"', text)
        if match:
            packages.add(match.group(1))
    return packages


def source_readers(crates_root):
    """Map each crate to the other crates whose sources read its files by path.

    Tests such as pp-gateway's drift guards `include_str!("../../pp-storage/...")`
    without a Cargo dependency, so rdeps() alone would skip them.
    """
    root = Path(crates_root)
    if not root.is_dir():
        return {}
    crates = {path.name for path in root.iterdir() if path.is_dir()}
    reference = re.compile(r"\.\./(" + "|".join(map(re.escape, sorted(crates))) + r")/")
    readers = {}
    for reader in sorted(crates):
        for source in sorted(root.joinpath(reader).rglob("*.rs")):
            text = source.read_text(encoding="utf-8", errors="replace")
            for read in reference.findall(text):
                if read != reader:
                    readers.setdefault(read, set()).add(reader)
    return readers


def filter_expression(event, paths, packages, readers=None):
    if event != "pull_request":
        return "all()"
    selected = set()
    for path in paths:
        if not path:
            continue
        if forces_full_suite(path):
            return "all()"
        package = package_for_path(path)
        if package is None:
            continue
        if package not in packages:
            return "all()"
        selected.add(package)
        selected.update((readers or {}).get(package, ()))
    if not selected:
        return "all()"
    return " + ".join(f"rdeps(={name})" for name in sorted(selected))


def changed_paths(base, head):
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha or "") for sha in (base, head)):
        return None
    try:
        return subprocess.check_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}"],
            stderr=subprocess.DEVNULL,
        ).decode("utf-8", errors="surrogateescape").split("\0")
    except subprocess.CalledProcessError:
        return None


if __name__ == "__main__":
    event = os.environ["EVENT_NAME"]
    base = os.environ.get("BASE_SHA")
    head = os.environ.get("HEAD_SHA")
    crates_root = os.environ.get(
        "PP_AFFECTED_CRATES_ROOT",
        str(Path(__file__).resolve().parents[1] / "rust" / "crates"),
    )
    packages = workspace_packages(crates_root)
    paths = changed_paths(base, head)
    if paths is None:
        expression = "all()"
    else:
        expression = filter_expression(event, paths, packages, source_readers(crates_root))
    message = f"nextest filter: {expression}\n"
    print(message, end="")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"nextest_filter={expression}\n")
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
        output.write(message)
