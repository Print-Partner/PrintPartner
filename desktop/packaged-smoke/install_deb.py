#!/usr/bin/env python3
"""Unpack the shipped Linux .deb into a disposable root and check its runtime.

The .deb is the one rust/scripts/desktop-deb.sh produced, used as shipped. This
script extracts it with dpkg-deb -x and refuses it unless its desktop-runtime
tree equals the stage it was built from, using the inventory of
rust/scripts/assemble-linux-deb.py: the same files with the same SHA-256 and the
mode Tauri's bundler writes, the same directories, the same symlinks with the
same targets, and nothing else. It changes nothing in the package or the
unpacked tree.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import sys

ASSEMBLER = pathlib.Path(__file__).resolve().parents[2] / "rust/scripts/assemble-linux-deb.py"
_spec = importlib.util.spec_from_file_location("assemble_linux_deb", ASSEMBLER)
assembler = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(assembler)


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
    runtime = assembler.runtime_of(root)
    executable = root / "usr/bin/pp-desktop"
    if not os.access(executable, os.X_OK):
        raise SystemExit("usr/bin/pp-desktop is missing or not executable")
    names = assembler.runtime_entries(stage)
    expected = assembler.inventory(stage, names, assembler.bundled_mode)
    found = differences(expected, assembler.inventory(runtime, names))
    if any(found.values()):
        raise SystemExit(
            json.dumps(
                {"error": "the .deb runtime differs from the stage"}
                | {key: names[:20] for key, names in found.items()}
            )
        )
    return {
        "executable": str(executable),
        "runtime": str(runtime),
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
    print(json.dumps(install(args.deb, args.stage.resolve(), args.root), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
