#!/usr/bin/env python3
"""Select the packages the desktop_rebuild-off nextest pass must run.

The on pass runs every test. A workspace package needs the off pass too when its
code can behave differently with the feature off. The scan marks a package
sensitive when any `.rs` file in it (src, tests, benches, build.rs) mentions
desktop_rebuild or DESKTOP_REBUILD anywhere except a plain
`#[cfg(feature = "desktop_rebuild")]` (or `#![cfg(...)]` heading a file) that
gates one of these, which only add code with the feature on:

- a whole `mod` at module level or in a `mod` block;
- a free `fn`, `struct`, `enum`, `const`, `static`, `type` or `trait` there, when
  its name is defined once in the whole workspace, is not in SHADOWING_NAMES (the
  prelude and common std trait methods), and its module has no glob import
  (`use x::*;`), so it cannot shadow anything ungated code uses;
- an enum variant, when the enum derives only Debug, Clone, Copy, PartialEq, Eq
  or Hash and has no `repr` or explicit discriminants, so no order or index moves;
- a match arm with one pattern on such a gated variant of the same enum.

Anything else (`not(...)`, `cfg!`, `cfg_attr`, `any`/`all`, other spacing, the
runtime switch, a gated `use`, `impl` block, method, statement, field, list
element, trait impl or macro) marks the package sensitive. Inherent methods win
method resolution over trait methods from any crate, so a gated method or impl
block could change what an ungated call does; the scan does not try to prove it
cannot. Every package that depends on a sensitive one, by
any dependency kind, is selected too, because it compiles against the off build.
A package left out therefore compiles off as a strict subset of its on build,
and the on pass runs each of its tests with the same code. Tests compiled only
with the feature off sit behind `not(feature = ...)`, so their package is always
selected.

With the feature on, Cargo also unifies more features of external crates into
the build. The script compares `cargo metadata` off and on per resolved package
and fails on any extra feature not listed in ADDITIVE_FEATURES, so a person
reviews it before a left-out package loses its off pass.

Prints the filterset without a newline, or all() when PP_NEXTEST_OFF_ALL is true
(a run without the on pass). docs/development/ci.md describes the split.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

FEATURE = "desktop_rebuild"
MENTION = re.compile(r"desktop_rebuild|DESKTOP_REBUILD")
PLAIN = re.compile(r'^\s*#(!?)\[cfg\(feature = "desktop_rebuild"\)\]\s*$')
ITEM = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern)\s+)*"
                  r"(mod|use|fn|struct|enum|impl|static|const|type|trait)\b\s*(?:<[^>]*>\s*)?(\w*)")
VARIANT = re.compile(r"^\s*([A-Z]\w*)\s*(?:\(.*\)|\{.*\})?\s*,?\s*$")
ARM = re.compile(r"^\s*(?:(\w+)::)+([A-Z]\w*)\b\s*(?:\(.*\)|\{.*\})?\s*(?:if\b[^|]*)?=>")
SAFE_DERIVES = {"Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash"}
GLOB = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+[^;]*\*\s*;")
# Names a gated item must not take even when unique: it could shadow the prelude or, as a
# method, a std trait method that ungated code calls.
SHADOWING_NAMES = {
    "Box", "Clone", "Copy", "Default", "Drop", "Eq", "Err", "Fn", "FnMut", "FnOnce", "From", "Into",
    "IntoIterator", "Iterator", "None", "Ok", "Option", "Ord", "PartialEq", "PartialOrd", "Result", "Send",
    "Sized", "Some", "String", "Sync", "ToOwned", "ToString", "TryFrom", "TryInto", "Unpin", "Vec",
    "as_mut", "as_ref", "borrow", "clone", "cmp", "default", "deref", "drop", "eq", "flush", "fmt", "from",
    "from_str", "hash", "into", "into_iter", "iter", "len", "next", "partial_cmp", "read", "to_owned",
    "to_string", "try_from", "try_into", "write",
}
DEFINITION = re.compile(r"\b(?:fn|const|static|struct|enum|type|trait|mod|union)\s+(\w+)|\bas\s+(\w+)\s*[;,}]")
STRING = re.compile(r'"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'')
# External features the on build adds to packages the off build already resolves, from
# `cargo metadata` off and on. Reviewed 2026-11-09 for GRE-1278: each adds API (an
# extractor, a re-export, trait impls, a codec) or, for simdutf8 aarch64_neon, picks an
# implementation with the same results. Packages only the on build has are reached only
# through gated code.
ADDITIVE_FEATURES = {
    "axum": {"multipart"},
    "chrono": {"oldtime"},
    "futures-channel": {"futures-sink", "sink"},
    "futures-util": {"channel", "futures-channel"},
    "schemars": {"chrono04", "default", "derive", "schemars_derive"},
    "simdutf8": {"aarch64_neon", "public_imp"},
    "tokio-util": {"codec"},
}


def blocks(lines):
    """Yield, for each line, the stack of enclosing blocks as (kind, name, header line) tuples.

    kind is mod, enum, impl (inherent), trait-impl, trait or other. String literals and
    `//` comments are dropped before counting braces.
    """
    stack, pending, start = [], "", 0
    for number, line in enumerate(lines):
        yield list(stack)
        code = STRING.sub('""', line).split("//", 1)[0]
        for char in code:
            if char == "{":
                header = " ".join(pending.split())
                if not pending.strip():
                    start = number
                match = re.search(r"\b(mod|enum|impl|trait)\b(.*)", header)
                if not match or re.search(r"\b(fn|struct|match|if|for|while|loop)\b", header.split(match.group(1))[0]):
                    kind, name = "other", ""
                elif match.group(1) == "impl":
                    rest = re.sub(r"^\s*<[^>]*>", "", match.group(2))
                    trait_impl = re.search(r"\bfor\b", rest)
                    kind = "trait-impl" if trait_impl else "impl"
                    name = re.findall(r"(\w+)\s*(?:<[^{]*)?$", rest.split(" where ")[0].strip()) or [""]
                    name = name[0]
                else:
                    kind, name = match.group(1), (re.findall(r"\w+", match.group(2)) or [""])[0]
                stack.append((kind, name, start))
                pending = ""
            elif char == "}":
                if stack:
                    stack.pop()
                pending = ""
            elif char == ";":
                pending = ""
            else:
                if not pending.strip() and not char.isspace():
                    start = number
                pending += char
        pending += " "


def attributes_before(lines, index):
    """The attribute text directly above line index (doc comments and #[...] lines)."""
    found = []
    for line in reversed(lines[:index]):
        stripped = line.strip()
        if stripped.startswith(("#[", "//")) or (found and not stripped.endswith(("}", ";", "{"))
                                                 and stripped and not found[-1].startswith("//")):
            found.append(stripped)
        else:
            break
    return " ".join(found)


def safe_enum(lines, header):
    """Whether a gated variant in the enum starting at line header moves no order, index or repr."""
    # The block's header starts at its first attribute, so read those lines too.
    keyword = next(index for index in range(header, len(lines)) if re.search(r"\benum\b", lines[index]))
    attrs = attributes_before(lines, header) + " " + " ".join(lines[header:keyword + 1])
    derives = set(re.findall(r"\w+", " ".join(re.findall(r"derive\(([^)]*)\)", attrs))))
    body = []
    depth = 0
    for line in lines[header:]:
        body.append(line)
        depth += line.count("{") - line.count("}")
        if depth <= 0 and "{" in "".join(body):
            break
    return derives <= SAFE_DERIVES and "repr(" not in attrs and not re.search(r"^\s*[A-Z]\w*\s*=", "\n".join(body), re.M)


def gates(text):
    """Yield (line number, gated line, enclosing blocks) for each plain outer cfg in a file."""
    lines = text.splitlines()
    contexts = list(blocks(lines))
    for number, line in enumerate(lines):
        match = PLAIN.match(line)
        if not match or match.group(1):
            continue
        for index in range(number + 1, len(lines)):
            stripped = lines[index].strip()
            if stripped and not stripped.startswith(("#[", "//")):
                yield number + 1, index, contexts[index]
                break
        else:
            yield number + 1, None, contexts[number]


def defined_names(crates_root):
    """Count how often each name is defined or imported under an alias in the workspace's sources."""
    counts = {}
    for path in sorted(Path(crates_root).rglob("*.rs")):
        if "target" in path.relative_to(crates_root).parts:
            continue
        for match in DEFINITION.finditer(path.read_text(encoding="utf-8", errors="replace")):
            name = match.group(1) or match.group(2)
            counts[name] = counts.get(name, 0) + 1
    return counts


def off_reasons(package_dir, names=None):
    """Return why a package's code can differ with the feature off; empty when it cannot.

    names counts the definitions in the whole workspace (defined_names); by default only
    this package's own sources are counted.
    """
    package_dir = Path(package_dir)
    sources = {}
    for path in sorted(package_dir.rglob("*.rs")):
        relative = path.relative_to(package_dir)
        if "target" not in relative.parts:
            sources[relative] = path.read_text(encoding="utf-8", errors="replace")
    variants = set()
    names = defined_names(package_dir) if names is None else names
    for text in sources.values():
        lines = text.splitlines()
        for _, index, context in gates(text):
            if index is not None and context and context[-1][0] == "enum":
                found = VARIANT.match(lines[index])
                if found and safe_enum(lines, context[-1][2]):
                    variants.add((context[-1][1], found.group(1)))
    reasons = []
    for relative, text in sources.items():
        lines = text.splitlines()
        contexts = list(blocks(lines))
        globs = {scope(contexts[index]) for index, line in enumerate(lines) if GLOB.match(line)}
        for number, line in enumerate(lines, 1):
            if MENTION.search(line) and not PLAIN.match(line):
                reasons.append(f"{relative}:{number}: {line.strip()}")
        for number, index, context in gates(text):
            gated = lines[index] if index is not None else ""
            if not additive(gated, context, scope(context) in globs, variants, names):
                reasons.append(f"{relative}:{number}: gates `{gated.strip()}`")
    return reasons


def scope(context):
    """The module a line sits in: the start lines of its enclosing blocks."""
    return tuple(start for _, _, start in context)


def additive(gated, context, glob, variants, names):
    """Whether one gated line only adds code with the feature on (see the module docstring)."""
    kinds = [kind for kind, _, _ in context]
    top = context[-1] if context else ("mod", "", 0)
    item = ITEM.match(gated)
    if all(kind == "mod" for kind in kinds):
        if not item or item.group(1) in ("impl", "use"):
            return False
        return item.group(1) == "mod" or (not glob and item.group(2) not in SHADOWING_NAMES
                                          and names.get(item.group(2), 0) == 1)
    if top[0] == "enum" and all(kind in ("mod", "enum") for kind in kinds):
        found = VARIANT.match(gated)
        return bool(found) and (top[1], found.group(1)) in variants
    arm = ARM.match(gated)
    if arm and "|" not in gated.split("=>", 1)[0]:
        owner = arm.group(1)
        if owner == "Self":
            owner = next((name for kind, name, _ in reversed(context) if kind in ("impl", "trait-impl")), "")
        return (owner, arm.group(2)) in variants
    return False


def packages(crates_root):
    """Map each package name under crates_root to its directory."""
    found = {}
    for manifest in sorted(Path(crates_root).glob("*/Cargo.toml")):
        name = re.search(r'(?m)^name\s*=\s*"([^"]+)"', manifest.read_text(encoding="utf-8")).group(1)
        found[name] = manifest.parent
    return found


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


def workspace_graph(metadata):
    """Map each workspace package to the workspace packages it depends on, by any kind."""
    members = {package["id"]: package["name"] for package in metadata["packages"]
               if package["id"] in set(metadata["workspace_members"])}
    return {members[node["id"]]: {members[dep["pkg"]] for dep in node["deps"] if dep["pkg"] in members}
            for node in metadata["resolve"]["nodes"] if node["id"] in members}


def selected(sensitive, graph):
    """The sensitive packages and every package that depends on one, directly or not."""
    chosen = set(sensitive)
    while True:
        more = {name for name, deps in graph.items() if name not in chosen and deps & chosen}
        if not more:
            return chosen
        chosen |= more


def off_filter(crates_root, graph, off, on):
    """Return the package() filterset for the off pass; fail on anything the scan cannot vouch for."""
    found = packages(crates_root)
    if set(found) != set(graph):
        raise SystemExit(f"The scan read {sorted(found)} but the workspace has {sorted(graph)}; "
                         "teach scripts/ci-desktop-rebuild-off-filter.py where the other members live.")
    unreviewed = unreviewed_features(off, on)
    if unreviewed:
        raise SystemExit(f"desktop_rebuild adds external features {', '.join(unreviewed)}. Check that each "
                         "only adds API, then list it in ADDITIVE_FEATURES in "
                         "scripts/ci-desktop-rebuild-off-filter.py.")
    names = defined_names(crates_root)
    sensitive = {name for name, folder in found.items() if off_reasons(folder, names)}
    if not sensitive:
        raise SystemExit("No package mentions desktop_rebuild beyond additive gates; the scan is probably "
                         "reading the wrong files.")
    return " + ".join(f"package(={name})" for name in sorted(selected(sensitive, graph)))


def cargo_metadata(manifest, *features):
    command = ["cargo", "metadata", "--format-version", "1", "--locked", "--manifest-path", str(manifest), *features]
    # cargo writes UTF-8; Windows would otherwise decode it with the console code page.
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8")
    if result.returncode:
        raise SystemExit(f"{' '.join(command)} failed:\n{result.stderr}")
    return json.loads(result.stdout)


if __name__ == "__main__":
    if os.environ.get("PP_NEXTEST_OFF_ALL") == "true":
        print("PP_NEXTEST_OFF_ALL is true, so the off pass runs every test.", file=sys.stderr)
        sys.stdout.write("all()")
        sys.exit(0)
    rust = Path(__file__).resolve().parents[1] / "rust"
    off_metadata = cargo_metadata(rust / "Cargo.toml")
    on_metadata = cargo_metadata(rust / "Cargo.toml", "--features", FEATURE)
    graph = workspace_graph(off_metadata)
    expression = off_filter(rust / "crates", graph, resolved_features(off_metadata), resolved_features(on_metadata))
    names = defined_names(rust / "crates")
    for name, folder in packages(rust / "crates").items():
        reasons = off_reasons(folder, names)
        why = (reasons[0] if reasons else "depends on a sensitive package" if f"(={name})" in expression
               else "on pass only: additive gates")
        print(f"{name}: {why}", file=sys.stderr)
    # No newline: on Windows print() would end the filterset with \r inside $(...).
    sys.stdout.write(expression)
