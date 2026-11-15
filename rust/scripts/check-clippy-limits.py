"""Limit Rust complexity findings on Linux, with desktop_rebuild off and on."""

import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys
import tempfile

LINTS = (
    "clippy::cognitive_complexity",
    "clippy::too_many_lines",
    "clippy::match_same_arms",
)
MODES = ("off", "on")
BASE_REF = "origin/main"
BASELINE = "clippy-limits-baseline.json"
ROOT = Path(__file__).resolve().parents[1]


def parse_baseline(text):
    baseline = json.loads(text)
    if not isinstance(baseline, dict) or baseline.keys() != set(MODES):
        raise ValueError("baseline must contain desktop_rebuild off and on counts")
    for counts in baseline.values():
        if (
            not isinstance(counts, dict)
            or counts.keys() != set(LINTS)
            or any(type(n) is not int or n < 0 for n in counts.values())
        ):
            raise ValueError("baseline must contain nonnegative integer counts for every lint")
    return baseline


def main_baseline():
    subprocess.run(
        ["git", "rev-parse", "--verify", BASE_REF + "^{commit}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    # A missing file is allowed only for the first landing, not a missing ref.
    listed = subprocess.run(
        ["git", "ls-tree", "--name-only", BASE_REF, "--", BASELINE],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    if not listed.stdout.strip():
        return None
    shown = subprocess.run(
        ["git", "show", f"{BASE_REF}:./{BASELINE}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return parse_baseline(shown.stdout)


def clippy_command(mode):
    command = [
        "cargo",
        "clippy",
        "--manifest-path",
        "Cargo.toml",
        "--locked",
        "--workspace",
        "--all-targets",
        "--keep-going",
        "--message-format=json",
    ]
    # GRE-1646 makes desktop_rebuild the default on the image crates. The off
    # scan has to opt out, or it counts the same findings as the on scan.
    if mode == "off":
        command.append("--no-default-features")
    elif mode == "on":
        command += ["--features", "desktop_rebuild"]
    command += ["--"]
    for lint in LINTS:
        command += ["-W", lint]
    return command


def read_diagnostics(lines, members):
    """Count each source span once, not once per lib/test compilation."""
    hits = {}
    finished = False
    for line in lines:
        message = json.loads(line)
        if message.get("reason") == "build-finished":
            if message.get("success") is not True:
                raise ValueError("Clippy compilation failed")
            finished = True
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message["message"]
        code = (diagnostic.get("code") or {}).get("code")
        if diagnostic["level"] == "error":
            raise ValueError(diagnostic.get("rendered") or diagnostic["message"])
        if code in {"unknown_lints", "renamed_and_removed_lints"}:
            raise ValueError(diagnostic.get("rendered") or diagnostic["message"])
        if message["package_id"] not in members or code not in LINTS:
            continue
        spans = tuple(
            (span["file_name"], span["byte_start"], span["byte_end"])
            for span in diagnostic["spans"]
            if span["is_primary"]
        )
        if not spans:
            raise ValueError(f"{code} has no primary source span")
        hits[(code, spans)] = diagnostic.get("rendered") or diagnostic["message"]
    if not finished:
        raise ValueError("Clippy output has no successful build-finished record")
    counts = Counter(code for code, _ in hits)
    return {lint: counts[lint] for lint in LINTS}, hits


def scan(mode, members):
    print(f"Checking Clippy limits, desktop_rebuild {mode}", flush=True)
    with tempfile.TemporaryFile(mode="w+") as output:
        # Keep build progress on stderr and stream JSON through a temporary file.
        result = subprocess.run(clippy_command(mode), cwd=ROOT, stdout=output)
        output.seek(0)
        diagnostics = read_diagnostics(output, members)
        result.check_returncode()
        return diagnostics


def compare(actual, baseline, previous):
    failures = []
    for mode in MODES:
        for lint in LINTS:
            count, allowed = actual[mode][lint], baseline[mode][lint]
            print(f"desktop_rebuild {mode}: {lint}: {count} findings (baseline {allowed})")
            if count != allowed:
                action = "fix new findings" if count > allowed else "lower the baseline"
                failures.append(f"{mode} {lint}: {action}")
            if previous is not None and allowed > previous[mode][lint]:
                failures.append(f"{mode} {lint}: baseline rose over {BASE_REF}")
    for failure in failures:
        print(f"Clippy limits FAILED: {failure}")
    return 1 if failures else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="regenerate counts; increases over origin/main still fail",
    )
    args = parser.parse_args()
    if sys.platform != "linux":
        raise ValueError("Clippy limits use a Linux baseline; run on Linux")
    previous = main_baseline()
    baseline = None if args.update else parse_baseline((ROOT / BASELINE).read_text())
    metadata = subprocess.run(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            "Cargo.toml",
            "--locked",
            "--no-deps",
            "--format-version=1",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    members = set(json.loads(metadata.stdout)["workspace_members"])
    if not members:
        raise ValueError("Cargo metadata has no workspace members")
    scans = {mode: scan(mode, members) for mode in MODES}
    actual = {mode: result[0] for mode, result in scans.items()}
    if baseline is None:
        baseline = actual
    status = compare(actual, baseline, previous)
    for mode, (_, hits) in scans.items():
        for (lint, _), rendered in hits.items():
            if actual[mode][lint] > baseline[mode][lint]:
                print(rendered)
    if previous is None:
        print(f"{BASE_REF} has no {BASELINE} yet; first landing.")
    if status == 0:
        if args.update:
            (ROOT / BASELINE).write_text(json.dumps(actual, indent=2) + "\n")
        print("Clippy limits passed.")
    return status


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Clippy limits error: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(2)
