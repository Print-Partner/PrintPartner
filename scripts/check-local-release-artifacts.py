"""Check local runtime assets against the manifest before starting nextest."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
REBUILD = "just ci --only web-assets"


def fresh(root, env, run=subprocess.run):
    """Use the release generator's inventories rather than a second inventory format."""
    try:
        stored = json.loads((root / "rust/bundle-manifest.json").read_text())
        with tempfile.TemporaryDirectory(prefix="pp-release-check-") as scratch:
            measured = Path(scratch) / "manifest.json"
            result = run(
                [sys.executable, str(root / "rust/scripts/build-desktop-manifest.py"),
                 "--web", str(root / "web"), "--node", env["PP_TEST_NODE"],
                 "--output", str(measured), "--commit", env["GITHUB_SHA"]],
                cwd=root, env=env, capture_output=True, text=True,
            )
            return result.returncode == 0 and json.loads(measured.read_text()) == stored
    except (OSError, ValueError, KeyError):
        return False


def main():
    if fresh(ROOT, os.environ):
        print("Release artifacts match rust/bundle-manifest.json")
        return 0
    print(f"Stale or missing rust/bundle-manifest.json or web assets; nextest was not started. "
          f"Rebuild with Node 24.21.0: {REBUILD}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
