#!/usr/bin/env python3
"""Unpack the shipped Linux .deb into a disposable root and check its runtime.

The .deb is the one rust/scripts/desktop-deb.sh produced, used as shipped. This
script extracts it with dpkg-deb -x and refuses it unless its desktop-runtime
tree equals the stage it was built from: the same files with the same SHA-256
and executable bit, the same symlinks with the same targets, and nothing else.
It changes nothing in the package or the unpacked tree.
"""

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys

RUNTIME_ENTRIES = ("web", "bin", "release.json")


def inventory(root):
    entries = {}
    for name in RUNTIME_ENTRIES:
        top = root / name
        if not os.path.lexists(top):
            continue
        walk = sorted(top.rglob("*")) if top.is_dir() and not top.is_symlink() else []
        for path in [top, *walk]:
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                entries[relative] = ("link", os.readlink(path))
            elif path.is_dir():
                entries[relative] = ("dir", None)
            elif path.is_file():
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                entries[relative] = ("file", digest, os.access(path, os.X_OK))
            else:
                entries[relative] = ("other", None)
    return entries


def differences(stage, packaged):
    """Return the entries that are extra, changed or missing in the package."""
    return {
        "extra": sorted(set(packaged) - set(stage)),
        "changed": sorted(
            name for name in set(stage) & set(packaged) if stage[name] != packaged[name]
        ),
        "missing": sorted(set(stage) - set(packaged)),
    }


def install(deb, stage, root):
    root.mkdir(parents=True, exist_ok=False)
    subprocess.run(["dpkg-deb", "-x", str(deb), str(root)], check=True)
    runtimes = sorted((root / "usr/lib").glob("*/desktop-runtime"))
    if len(runtimes) != 1:
        raise SystemExit(f"expected one desktop-runtime under usr/lib, found {len(runtimes)}")
    executable = root / "usr/bin/pp-desktop"
    if not os.access(executable, os.X_OK):
        raise SystemExit("usr/bin/pp-desktop is missing or not executable")
    expected = inventory(stage)
    found = differences(expected, inventory(runtimes[0]))
    if any(found.values()):
        raise SystemExit(
            json.dumps(
                {"error": "the .deb runtime differs from the stage"}
                | {key: names[:20] for key, names in found.items()}
            )
        )
    return {
        "executable": str(executable),
        "runtime": str(runtimes[0]),
        "runtimeEntries": len(expected),
        "debSha256": hashlib.sha256(deb.read_bytes()).hexdigest(),
    }


def main(argv):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--deb", type=pathlib.Path, required=True)
    parser.add_argument("--stage", type=pathlib.Path, required=True, help="stage-desktop.py output")
    parser.add_argument("--root", type=pathlib.Path, required=True, help="a new directory")
    args = parser.parse_args(argv)
    print(json.dumps(install(args.deb, args.stage, args.root), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
