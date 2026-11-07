#!/usr/bin/env python3
"""Put the measured runtime's symlinks back into the Linux .deb.

tauri build --bundles deb (tauri-cli 2.12.1) copies bundle.resources but skips
symlinks. The stage that stage-desktop.py writes has the workspace links the
bundle manifest lists under dependencies.links, and the installed app refuses
to start without them because resource verification finds a different file
inventory. This helper unpacks the .deb, checks that its desktop-runtime tree
is the stage's tree minus exactly those links, adds them, rebuilds the .deb in
place and checks the rebuilt tree is identical to the stage.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

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


def dropped_links(stage, packaged, links):
    """Return the manifest links the package lacks; refuse any other difference."""
    missing = set(stage) - set(packaged)
    for name, target in links.items():
        if stage.get(name) != ("link", target):
            raise SystemExit(f"stage link {name} differs from the bundle manifest")
    explained = {name for name in missing if name in links}
    for name in missing - explained:
        inside = [other for other in stage if other.startswith(name + "/")]
        if stage[name][0] == "dir" and inside and all(other in explained or (
                other in missing and stage[other][0] == "dir") for other in inside):
            explained.add(name)
    problems = {
        "extra": sorted(set(packaged) - set(stage)),
        "changed": sorted(name for name in set(stage) & set(packaged) if stage[name] != packaged[name]),
        "missing": sorted(missing - explained),
    }
    if any(problems.values()):
        raise SystemExit(json.dumps({"error": "the .deb runtime differs from the stage",
                                     **{key: value[:20] for key, value in problems.items()}}))
    return sorted(name for name in missing if name in links)


def runtime_of(root):
    runtimes = sorted((root / "usr/lib").glob("*/desktop-runtime"))
    if len(runtimes) != 1:
        raise SystemExit(f"expected one usr/lib/*/desktop-runtime, found {len(runtimes)}")
    return runtimes[0]


def assemble(stage, deb):
    links = json.loads((stage / "bundle-manifest.json").read_text())["dependencies"]["links"]
    expected = inventory(stage)
    with tempfile.TemporaryDirectory(prefix=".deb-assembly-", dir=deb.parent) as temporary:
        work = Path(temporary) / "package"
        subprocess.run(["dpkg-deb", "-R", str(deb), str(work)], check=True)
        runtime = runtime_of(work)
        added = dropped_links(expected, inventory(runtime), links)
        for name in added:
            (runtime / name).parent.mkdir(parents=True, exist_ok=True)
            (runtime / name).symlink_to(links[name])
        if inventory(runtime) != expected:
            raise SystemExit("the assembled runtime differs from the stage")
        rebuilt = Path(temporary) / deb.name
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(work), str(rebuilt)],
                       check=True, stdout=subprocess.DEVNULL)
        check = Path(temporary) / "check"
        subprocess.run(["dpkg-deb", "-x", str(rebuilt), str(check)], check=True)
        if inventory(runtime_of(check)) != expected:
            raise SystemExit("the rebuilt .deb runtime differs from the stage")
        shutil.move(rebuilt, deb)
    return {"deb": str(deb), "sha256": hashlib.sha256(deb.read_bytes()).hexdigest(),
            "runtime_entries": len(expected), "restored_links": added}


def main(argv):
    if argv == ["--self-test"]:
        import unittest

        suite = unittest.defaultTestLoader.discover(
            str(Path(__file__).resolve().parent), pattern="test_assemble_linux_deb.py")
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--stage", type=Path, required=True, help="stage-desktop.py output")
    parser.add_argument("--deb", type=Path, required=True, help="the .deb tauri built, rebuilt in place")
    args = parser.parse_args(argv)
    print(json.dumps(assemble(args.stage.resolve(strict=True), args.deb.resolve(strict=True)), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
