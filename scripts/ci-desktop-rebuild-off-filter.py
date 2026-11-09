#!/usr/bin/env python3
"""Select the tests the desktop_rebuild-off nextest pass must run, from `cargo metadata` only.

The on pass runs every test. A workspace package sees the feature when it
declares a `desktop_rebuild` feature, or when its resolved feature set differs
between `cargo metadata` without and with `--features desktop_rebuild` (another
member's `desktop_rebuild` turns on one of its features). A package leaves the
off pass only when nothing in its transitive workspace dependency closure (every
dependency kind: normal, dev and build, off and on edges) sees the feature. Its
off and on builds then compile the same source with the same cfg. Every other
package keeps the full off pass. The script reads no Rust source.

With the feature on, Cargo also unifies more features of external crates into
the build, so a left-out package's on-pass tests link those crates with extra
features. ADDITIVE_FEATURES lists the extra features reviewed as additive; any
other one keeps the full off pass.

Fail safe: on any error (cargo metadata failing or printing something that is not
JSON, an unreviewed external feature) and when PP_NEXTEST_OFF_ALL is true (a run
without the on pass), the script prints all(), so the off pass runs every test.
It prints the filterset without a newline, because on Windows print() would end
it with a carriage return inside $(...). docs/development/ci.md describes the split.
"""

import json
import os
from pathlib import Path
import subprocess
import sys

FEATURE = "desktop_rebuild"
FULL = "all()"
# External features the on build adds to packages the off build already resolves. Each adds
# API (an extractor, a re-export, trait impls, a codec) or, for simdutf8 aarch64_neon, picks
# an implementation with the same results. Reviewed 2026-11-09 for GRE-1278.
ADDITIVE_FEATURES = {
    "axum": {"multipart"},
    "chrono": {"oldtime"},
    "futures-channel": {"futures-sink", "sink"},
    "futures-util": {"channel", "futures-channel"},
    "schemars": {"chrono04", "default", "derive", "schemars_derive"},
    "simdutf8": {"aarch64_neon", "public_imp"},
    "tokio-util": {"codec"},
}


class Unsafe(Exception):
    """Anything that stops the script from vouching for a narrower off pass."""


def cargo_metadata(manifest, *features):
    command = ["cargo", "metadata", "--format-version", "1", "--locked", "--manifest-path", str(manifest), *features]
    # cargo writes UTF-8; Windows would otherwise decode it with the console code page.
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8")
    if result.returncode:
        raise Unsafe(f"{' '.join(command)} exited {result.returncode}:\n{result.stderr}")
    try:
        return json.loads(result.stdout)
    except (TypeError, ValueError) as error:
        raise Unsafe(f"{' '.join(command)} printed no JSON: {error}") from error


def member_nodes(metadata):
    """Map each workspace member id to (enabled features, workspace members it depends on)."""
    members = set(metadata["workspace_members"])
    nodes = {node["id"]: (set(node["features"]), {dep["pkg"] for dep in node["deps"] if dep["pkg"] in members})
             for node in metadata["resolve"]["nodes"] if node["id"] in members}
    if set(nodes) != members:
        raise Unsafe("cargo metadata did not resolve every workspace member.")
    return nodes


def skipped_packages(metadata, on_metadata=None):
    """Workspace packages whose dependency closure, themselves included, never sees desktop_rebuild.

    on_metadata is `cargo metadata --features desktop_rebuild`; without it only declarations count.
    """
    on_metadata = metadata if on_metadata is None else on_metadata
    members = set(metadata["workspace_members"])
    if set(on_metadata["workspace_members"]) != members:
        raise Unsafe("The off and on metadata list different workspace members.")
    names = {package["id"]: package["name"] for package in metadata["packages"] if package["id"] in members}
    off_nodes, on_nodes = member_nodes(metadata), member_nodes(on_metadata)
    declares = {package["id"] for package in metadata["packages"]
                if package["id"] in members and FEATURE in package.get("features", {})}
    if not declares:
        raise Unsafe(f"No workspace package declares {FEATURE}; the metadata looks wrong.")
    # A member whose resolved features change with the feature on compiles different cfg.
    sees = declares | {member for member in members if off_nodes[member][0] != on_nodes[member][0]}
    deps = {member: off_nodes[member][1] | on_nodes[member][1] for member in members}
    skipped = []
    for member in members:
        closure, todo = {member}, [member]
        while todo:
            for dep in deps[todo.pop()] - closure:
                closure.add(dep)
                todo.append(dep)
        if not closure & sees:
            skipped.append(names[member])
    return sorted(skipped)


def resolved_features(metadata):
    """Map each resolved non-workspace package id to (name, enabled features)."""
    members = set(metadata["workspace_members"])
    names = {package["id"]: package["name"] for package in metadata["packages"]}
    return {node["id"]: (names[node["id"]], set(node["features"]))
            for node in metadata["resolve"]["nodes"] if node["id"] not in members}


def unreviewed_features(off, on):
    """Return 'crate/feature' for each feature the on build adds to a package the off build has."""
    return sorted({f"{name}/{feature}" for package, (name, enabled) in on.items() if package in off
                   for feature in enabled - off[package][1] - ADDITIVE_FEATURES.get(name, set())})


def off_filter(off_metadata, on_metadata):
    """Return the off-pass filterset; raise Unsafe when it cannot be narrowed safely."""
    unreviewed = unreviewed_features(resolved_features(off_metadata), resolved_features(on_metadata))
    if unreviewed:
        raise Unsafe(f"{FEATURE} adds external features {', '.join(unreviewed)}; list each in "
                     "ADDITIVE_FEATURES once it is checked to only add API.")
    skipped = skipped_packages(off_metadata, on_metadata)
    if not skipped:
        return FULL
    return "not (" + " + ".join(f"package(={name})" for name in skipped) + ")"


def main(manifest, env=os.environ, metadata=cargo_metadata):
    """Return (filterset, message for the log)."""
    if env.get("PP_NEXTEST_OFF_ALL") == "true":
        return FULL, "PP_NEXTEST_OFF_ALL is true, so the off pass runs every test."
    try:
        expression = off_filter(metadata(manifest), metadata(manifest, "--features", FEATURE))
    except Exception as error:  # noqa: BLE001 - any failure must widen the pass, never fail or narrow it
        return FULL, f"::warning::The desktop_rebuild-off pass runs every test: {error}"
    return expression, f"desktop_rebuild-off nextest filter: {expression}"


if __name__ == "__main__":
    filterset, message = main(Path(__file__).resolve().parents[1] / "rust" / "Cargo.toml")
    print(message, file=sys.stderr)
    sys.stdout.write(filterset)
