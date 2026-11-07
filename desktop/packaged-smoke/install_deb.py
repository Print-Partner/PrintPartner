#!/usr/bin/env python3
"""Unpack the Linux .deb into a disposable root and check its runtime tree.

The installed app verifies its desktop-runtime tree against the measured stage
manifest compiled into it, so the unpacked tree must match the stage that
rust/scripts/stage-desktop.py wrote. Tauri's deb bundler (tauri-cli 2.12.1)
drops symlinks from resources: the stage's workspace links under
web/node_modules/@print-partner are missing from the .deb, and the installed
app then fails resource verification. Until the Linux package keeps them, this
script restores exactly those links from the stage and records them. Any other
difference between the .deb and the stage fails.
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
        paths = [top] + (sorted(top.rglob("*")) if top.is_dir() and not top.is_symlink() else [])
        for path in paths:
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                entries[relative] = ("link", os.readlink(path))
            elif path.is_dir():
                entries[relative] = ("dir", None)
            elif path.is_file():
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                entries[relative] = ("file", (digest, os.access(path, os.X_OK)))
            else:
                entries[relative] = ("other", None)
    return entries


def missing_links(stage, packaged):
    """Return the stage links the package dropped, or raise on any other difference."""
    extra = sorted(set(packaged) - set(stage))
    changed = sorted(path for path in set(stage) & set(packaged) if stage[path] != packaged[path])
    missing = sorted(set(stage) - set(packaged))
    links = [path for path in missing if stage[path][0] == "link"]
    unexplained = []
    for path in missing:
        if stage[path][0] == "link":
            continue
        inside = [other for other in stage if other.startswith(path + "/")]
        if (
            stage[path][0] == "dir"
            and inside
            and all(other in missing and stage[other][0] in ("link", "dir") for other in inside)
        ):
            continue
        unexplained.append(path)
    if extra or changed or unexplained:
        raise SystemExit(
            json.dumps(
                {
                    "error": "the .deb runtime tree differs from the stage",
                    "extra": extra[:20],
                    "changed": changed[:20],
                    "missing": unexplained[:20],
                }
            )
        )
    return links


def install(deb, stage, root):
    root.mkdir(parents=True, exist_ok=False)
    subprocess.run(["dpkg-deb", "-x", str(deb), str(root)], check=True)
    runtimes = sorted((root / "usr/lib").glob("*/desktop-runtime"))
    if len(runtimes) != 1:
        raise SystemExit(f"expected one desktop-runtime under usr/lib, found {len(runtimes)}")
    runtime = runtimes[0]
    executable = root / "usr/bin/pp-desktop"
    if not os.access(executable, os.X_OK):
        raise SystemExit("usr/bin/pp-desktop is missing or not executable")
    links = missing_links(inventory(stage), inventory(runtime))
    for link in links:
        target = runtime / link
        target.parent.mkdir(parents=True, exist_ok=True)
        target.symlink_to(os.readlink(stage / link))
    if links and inventory(stage) != inventory(runtime):
        raise SystemExit("restoring the dropped links did not reproduce the stage")
    return {
        "executable": str(executable),
        "runtime": str(runtime),
        "restoredLinks": links,
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
    result = install(args.deb, args.stage, args.root)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
