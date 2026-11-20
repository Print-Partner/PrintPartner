#!/usr/bin/env python3
"""Write a receipt for one Playwright e2e run on an exact head SHA.

collect: copy the run's PNG screenshot attachments from the Playwright JSON report into
<out>/screenshots, in report order.
write: count the report's tests, check the copied screenshots, and write <out>/receipt.json.
It writes a receipt only when the Playwright step's outcome is `success`; for any other outcome
it exits 1 and writes nothing, so a receipt never stands for a failed, cancelled or skipped run.
Identical inputs give identical bytes: the timestamp is an input, not the clock.

The two steps are split so CI can upload the screenshots first and put that artifact's URL
in the receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
from datetime import datetime
from pathlib import Path

FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
REPO = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
PR = re.compile(r"^[1-9][0-9]{0,9}$")
RUN_ID = re.compile(r"^[1-9][0-9]*$")
TIMESTAMP = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
# Playwright's per-test outcome: expected passed, flaky passed on a retry, unexpected failed.
STATUSES = ("expected", "flaky", "unexpected", "skipped")
SCREENSHOTS = "screenshots"


class ReceiptError(Exception):
    """The report or the receipt is not consistent; no receipt is written."""


def walk_tests(suite):
    for spec in suite.get("specs") or []:
        for test in spec.get("tests") or []:
            yield spec, test
    for child in suite.get("suites") or []:
        yield from walk_tests(child)


def all_tests(report) -> list[tuple[dict, dict]]:
    if not isinstance(report, dict) or not isinstance(report.get("suites"), list):
        raise ReceiptError("not a Playwright JSON report")
    return [pair for suite in report["suites"] for pair in walk_tests(suite)]


def count(report) -> dict:
    """pass, fail, skipped and flaky from the tests, cross-checked against the report's stats."""
    tally = dict.fromkeys(STATUSES, 0)
    for _, test in all_tests(report):
        status = test.get("status")
        if status not in tally:
            raise ReceiptError(f"unknown test status {status!r}")
        tally[status] += 1
    stats = report.get("stats")
    if not isinstance(stats, dict):
        raise ReceiptError("stats must be an object")
    for status in STATUSES:
        value = stats.get(status)
        if type(value) is not int or value < 0:
            raise ReceiptError(f"stats.{status} must be a non-negative integer, not {value!r}")
        if value != tally[status]:
            raise ReceiptError(f"stats.{status}={value!r} but the report lists {tally[status]}")
    if not isinstance(report.get("errors"), list):
        raise ReceiptError("errors must be a list")
    return {
        "pass": tally["expected"] + tally["flaky"],
        "fail": tally["unexpected"],
        "skipped": tally["skipped"],
        "flaky": tally["flaky"],
        "errors": len(report["errors"]),
    }


def slug(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "-", text).strip("-").lower()[:60] or "test"


def screenshot_sources(report, report_dir: Path) -> list[tuple[Path, str]]:
    """Every PNG attachment with a file path, in report order, with its copy's file name.

    The index is zero-padded to the width of the count, so file-name order is report order.
    """
    found = []
    for spec, test in all_tests(report):
        for result in test.get("results") or []:
            for attachment in result.get("attachments") or []:
                if attachment.get("contentType") != "image/png" or not attachment.get("path"):
                    continue
                source = Path(attachment["path"])
                if not source.is_absolute():
                    source = report_dir / source
                found.append((source, f"{slug(spec.get('title') or '')}-r{result.get('retry', 0)}.png"))
    width = max(3, len(str(len(found))))
    return [(source, f"{i:0{width}d}-{rest}") for i, (source, rest) in enumerate(found, 1)]


def collect(report_path: Path, out: Path) -> list[Path]:
    report = json.loads(report_path.read_text(encoding="utf-8"))
    target = out / SCREENSHOTS
    # A staging directory is never reused, even when empty: each run collects into a new one.
    if target.exists():
        raise ReceiptError(f"{target} already exists")
    out.mkdir(parents=True, exist_ok=True)
    target.mkdir()
    copied = []
    for source, name in screenshot_sources(report, report_path.parent):
        if not source.is_file() or source.stat().st_size == 0:
            raise ReceiptError(f"screenshot {source} is missing or empty")
        copied.append(Path(shutil.copyfile(source, target / name)))
    return copied


def describe(path: Path, root: Path) -> dict:
    data = path.read_bytes()
    return {"path": path.relative_to(root).as_posix(), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest()}


def check(receipt: dict, root: Path) -> None:
    """Refuse a receipt whose fields or screenshot files do not hold together."""
    if not isinstance(receipt.get("head"), str) or not FULL_SHA.fullmatch(receipt["head"]):
        raise ReceiptError("head must be a 40-character lowercase SHA")
    if not REPO.fullmatch(receipt.get("repo") or ""):
        raise ReceiptError("repo must be owner/name")
    if receipt.get("pr") is not None and not (type(receipt["pr"]) is int and receipt["pr"] > 0):
        raise ReceiptError("pr must be a positive integer or null")
    for key in ("pass", "fail", "skipped", "flaky", "errors"):
        if type(receipt.get(key)) is not int or receipt[key] < 0:
            raise ReceiptError(f"{key} must be a non-negative integer")
    if receipt["flaky"] > receipt["pass"]:
        raise ReceiptError("flaky tests are a subset of pass")
    if receipt["pass"] + receipt["fail"] + receipt["skipped"] == 0 and receipt["errors"] == 0:
        raise ReceiptError("the report ran no tests")
    shots = receipt.get("screenshots")
    if not isinstance(shots, list):
        raise ReceiptError("screenshots must be a list")
    if not shots and receipt["fail"] == 0 and receipt["errors"] == 0:
        raise ReceiptError("a passing run must carry at least one screenshot")
    url = receipt.get("artifact_url")
    artifact = re.compile(rf"^https://[^/]+/{re.escape(receipt['repo'])}/actions/runs/{receipt.get('run_id')}"
                          r"/artifacts/[1-9][0-9]*$")
    if shots and not url:
        raise ReceiptError("screenshots need the URL of their uploaded artifact")
    if url is not None and not artifact.fullmatch(url):
        raise ReceiptError("artifact_url must be an artifact of this run")
    for shot in shots:
        path = root / shot["path"]
        if not path.is_file() or path.stat().st_size == 0:
            raise ReceiptError(f"screenshot {shot['path']} is missing or empty")
        if describe(path, root) != shot:
            raise ReceiptError(f"screenshot {shot['path']} does not match its size or sha256")


def pr_head(pr: int, remote: str = "origin", cwd: Path | None = None) -> str:
    """The PR's live head from refs/pull/<pr>/head; works anonymously on a public repository."""
    out = subprocess.check_output(["git", "ls-remote", remote, f"refs/pull/{pr}/head"],
                                  cwd=cwd, text=True, timeout=60)
    shas = [line.split()[0] for line in out.splitlines() if line.strip()]
    if len(shas) != 1 or not FULL_SHA.fullmatch(shas[0]):
        raise ReceiptError(f"cannot read the head of PR #{pr}")
    return shas[0]


def build(args) -> dict:
    # Only a Playwright step that succeeded gets a receipt; failed, cancelled and skipped get none.
    if args.e2e_outcome != "success":
        raise ReceiptError(f"the e2e step's outcome is {args.e2e_outcome!r}, not 'success'")
    if not TIMESTAMP.fullmatch(args.timestamp):
        raise ReceiptError("timestamp must be YYYY-MM-DDTHH:MM:SSZ")
    try:
        datetime.strptime(args.timestamp, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        raise ReceiptError("timestamp must be a real UTC time") from None
    report = json.loads(Path(args.report).read_text(encoding="utf-8"))
    out = Path(args.out)
    sources = screenshot_sources(report, Path(args.report).parent)
    expected = [name for _, name in sources]
    present = sorted(p.name for p in (out / SCREENSHOTS).glob("*")) if (out / SCREENSHOTS).is_dir() else []
    if present != sorted(expected):
        raise ReceiptError(f"{out / SCREENSHOTS} holds {len(present)} files; the report lists {len(expected)}")
    # The staged copies are what CI uploaded; each must still equal the attachment Playwright wrote.
    for source, name in sources:
        if not source.is_file() or hashlib.sha256(source.read_bytes()).digest() != \
                hashlib.sha256((out / SCREENSHOTS / name).read_bytes()).digest():
            raise ReceiptError(f"{SCREENSHOTS}/{name} differs from the report's attachment {source}")
    if args.pr and not PR.fullmatch(args.pr):
        raise ReceiptError("pr must be a positive integer without leading zeros")
    if not RUN_ID.fullmatch(args.run_id):
        raise ReceiptError("run-id must be a positive integer")
    if not RUN_ID.fullmatch(args.run_attempt):
        raise ReceiptError("run-attempt must be a positive integer")
    # The receipt names the commit Playwright tested, and only a PR whose live head is that commit.
    if not FULL_SHA.fullmatch(args.head):
        raise ReceiptError("head must be a 40-character lowercase SHA")
    if not FULL_SHA.fullmatch(str(args.checkout_sha)):
        raise ReceiptError("checkout_sha must be a 40-character lowercase SHA")
    if args.head != args.checkout_sha:
        raise ReceiptError(f"head {args.head[:12]} is not the tested checkout {str(args.checkout_sha)[:12]}")
    if args.pr and pr_head(int(args.pr)) != args.head:
        raise ReceiptError(f"PR #{args.pr}'s head is not {args.head[:12]}")
    receipt = {
        "repo": args.repo,
        "pr": int(args.pr) if args.pr else None,
        "head": args.head,
        "checkout_sha": args.checkout_sha,
        "run_id": int(args.run_id),
        "run_attempt": int(args.run_attempt),
        "run_url": f"{args.server_url.rstrip('/')}/{args.repo}/actions/runs/{args.run_id}",
        "suite": args.suite,
        **count(report),
        "screenshots": [describe(out / SCREENSHOTS / name, out) for name in expected],
        "artifact_url": args.artifact_url or None,
        "timestamp": args.timestamp,
        "runner_host": args.runner_host,
        "runner_environment": args.runner_environment,
    }
    check(receipt, out)
    return receipt


def git_head(cwd: Path) -> str:
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=cwd, text=True).strip()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect", help="copy the report's PNG screenshots into <out>/screenshots")
    w = sub.add_parser("write", help="write <out>/receipt.json")
    for p in (c, w):
        p.add_argument("--report", required=True, help="Playwright JSON report")
        p.add_argument("--out", required=True, help="receipt directory")
    w.add_argument("--repo", required=True)
    w.add_argument("--pr", default="", help="PR number, or empty when the run has none")
    w.add_argument("--head", required=True, help="the exact head SHA the run tested")
    w.add_argument("--checkout-sha", help="the checked-out commit (default: git rev-parse HEAD)")
    w.add_argument("--run-id", required=True)
    w.add_argument("--run-attempt", default="1")
    w.add_argument("--server-url", default="https://github.com")
    w.add_argument("--suite", required=True)
    w.add_argument("--artifact-url", default="", help="URL of the uploaded screenshots artifact")
    w.add_argument("--runner-host", required=True)
    w.add_argument("--runner-environment", default="")
    w.add_argument("--e2e-outcome", required=True,
                   help="the Playwright step's outcome; only 'success' gets a receipt")
    w.add_argument("--timestamp", required=True, help="UTC start time, YYYY-MM-DDTHH:MM:SSZ")
    args = parser.parse_args(argv)
    try:
        if args.cmd == "collect":
            copied = collect(Path(args.report), Path(args.out))
            print(f"collected {len(copied)} screenshots into {Path(args.out) / SCREENSHOTS}")
            return 0
        args.checkout_sha = args.checkout_sha or git_head(Path.cwd())
        receipt = build(args)
    except (ReceiptError, OSError, ValueError, KeyError) as err:
        print(f"e2e receipt refused: {err}", file=sys.stderr)
        return 1
    path = Path(args.out) / "receipt.json"
    path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
