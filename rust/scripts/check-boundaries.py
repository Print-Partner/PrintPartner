import json
from pathlib import Path
import shutil
import subprocess

ALLOWED = {"pp-api": {"pp-storage", "pp-services"}, "pp-source": set(), "pp-storage": {"pp-domain", "pp-contracts", "pp-os"}, "pp-core": {"pp-gateway", "pp-compat", "pp-storage", "pp-source", "pp-net", "pp-api", "pp-perf"}, "pp-gateway": {"pp-compat", "pp-drivers", "pp-net"}, "pp-compat": set(), "pp-server": {"pp-core"}, "pp-contracts": {"pp-api-types"}, "pp-desktop": {"pp-core"}, "pp-domain": set(), "pp-api-types": {"pp-domain"}, "pp-net": set(), "pp-drivers": {"pp-net", "pp-api-types"}, "pp-services": set(), "pp-perf": set(), "pp-testkit": set(), "pp-os": set()}
GATED = {"pp-core": {"pp-source", "pp-net", "pp-api"}, "pp-storage": {"pp-domain", "pp-contracts"}, "pp-api": {"pp-services"}}
assert all(GATED[package] <= ALLOWED[package] for package in GATED), GATED
# Test-support crates. Any package may use them, but only under [dev-dependencies],
# so they never reach a release binary. ALLOWED never lists them as a normal edge.
DEV_ONLY = {"pp-testkit"}
assert not any(DEV_ONLY & edges for edges in ALLOWED.values()), DEV_ONLY
# Test-only features. Only a [dev-dependencies] entry may enable one, so release
# builds never contain it. No feature table, default, or normal edge may.
TEST_ONLY_FEATURES = {"pp-storage": {"test-unmarked-writer"}}


def rebuild_edge_is_gated(package, dependency):
    if not dependency.get("optional", False):
        return False
    features = package.get("features", {})
    reachable, pending = set(), ["default"]
    while pending:
        member = pending.pop()
        if member not in reachable:
            reachable.add(member)
            pending.extend(features.get(member, []))
    name = dependency["name"]
    activation = {name, f"dep:{name}"}
    return "desktop_rebuild" not in reachable and f"dep:{name}" in features.get("desktop_rebuild", []) and all(
        feature == "desktop_rebuild" or not any(
            member in activation or member.startswith(f"{name}/")
            for member in members
        )
        for feature, members in features.items()
    )


def test_only_feature_violations(packages):
    found = []
    for package in packages:
        # Feature forwarding names a dependency by its key in Cargo.toml, which
        # is the `package = "..."` alias when one is set.
        packages_by_key = {dependency.get("rename") or dependency["name"]: dependency["name"]
                           for dependency in package["dependencies"]}
        for feature, members in package.get("features", {}).items():
            for member in members:
                owner, _, name = member.rpartition("/")
                owner = owner.removesuffix("?").removeprefix("dep:")
                owner = packages_by_key.get(owner, owner) if owner else package["name"]
                if name in TEST_ONLY_FEATURES.get(owner, set()) and not (
                        owner == package["name"] and feature == name and members == []):
                    found.append((package["name"], f"{feature} -> {member}"))
        for feature in TEST_ONLY_FEATURES.get(package["name"], set()):
            if package.get("features", {}).get(feature, []) != []:
                found.append((package["name"], f"{feature} must be an empty feature"))
        for dependency in package["dependencies"]:
            enabled = TEST_ONLY_FEATURES.get(dependency["name"], set()) & set(dependency.get("features", []))
            if enabled and dependency.get("kind") != "dev":
                found.append((package["name"], f"{dependency['name']}/{sorted(enabled)[0]}"))
    return found


def violations(packages):
    return [(package["name"], dependency["name"]) for package in packages
            for dependency in package["dependencies"]
            if dependency["name"].startswith("pp-") and not (
                # A package may dev-depend on itself to enable its test-only features.
                dependency["name"] == package["name"] and dependency.get("kind") == "dev"
            ) and (
                dependency.get("kind") != "dev" if dependency["name"] in DEV_ONLY else (
                    dependency["name"] not in ALLOWED[package["name"]]
                    or (dependency["name"] in GATED.get(package["name"], set())
                        and not rebuild_edge_is_gated(package, dependency))
                )
            )] + test_only_feature_violations(packages)


manifest = Path(__file__).resolve().parents[1] / "Cargo.toml"
cargo = shutil.which("cargo")
if cargo is None:
    raise SystemExit("cargo not found in PATH")
metadata = json.loads(subprocess.check_output([cargo, "metadata", "--manifest-path", str(manifest), "--locked", "--format-version", "1", "--no-deps"]))
package_names = {package["name"] for package in metadata["packages"]}
assert package_names == set(ALLOWED), {
    "unlisted_packages": sorted(package_names - set(ALLOWED)),
    "missing_packages": sorted(set(ALLOWED) - package_names),
}
assert not violations(metadata["packages"]), violations(metadata["packages"])


def planted_edges_rejected():
    """Fake graphs only. ALLOWED is not changed to make these pass."""
    cases = (
        ([{"name": "pp-compat", "dependencies": [{"name": "pp-gateway"}]}], [("pp-compat", "pp-gateway")]),
        ([{"name": "pp-contracts", "dependencies": [{"name": "pp-gateway"}]}], [("pp-contracts", "pp-gateway")]),
        ([{"name": "pp-domain", "dependencies": [{"name": "pp-net"}]}], [("pp-domain", "pp-net")]),
        ([{"name": "pp-storage", "dependencies": [{"name": "pp-testkit", "kind": "dev"}]}], []),
        ([{"name": "pp-storage", "dependencies": [{"name": "pp-testkit", "kind": None}]}], [("pp-storage", "pp-testkit")]),
        ([{"name": "pp-server", "dependencies": [{"name": "pp-testkit", "kind": "build"}]}], [("pp-server", "pp-testkit")]),
        ([{"name": "pp-testkit", "dependencies": [{"name": "pp-storage", "kind": None}]}], [("pp-testkit", "pp-storage")]),
        ([{"name": "pp-storage", "features": {"test-unmarked-writer": []}, "dependencies": [{"name": "pp-storage", "kind": "dev", "features": ["test-unmarked-writer"]}]}], []),
        ([{"name": "pp-storage", "dependencies": [{"name": "pp-storage", "kind": None}]}], [("pp-storage", "pp-storage")]),
        ([{"name": "pp-core", "dependencies": [{"name": "pp-storage", "kind": None, "features": ["test-unmarked-writer"]}]}], [("pp-core", "pp-storage/test-unmarked-writer")]),
        ([{"name": "pp-core", "features": {"desktop_rebuild": ["pp-storage/test-unmarked-writer"]}, "dependencies": []}], [("pp-core", "desktop_rebuild -> pp-storage/test-unmarked-writer")]),
        ([{"name": "pp-storage", "features": {"test-unmarked-writer": [], "default": ["test-unmarked-writer"]}, "dependencies": []}], [("pp-storage", "default -> test-unmarked-writer")]),
        ([{"name": "pp-core", "features": {"desktop_rebuild": ["store/test-unmarked-writer"]}, "dependencies": [{"name": "pp-storage", "rename": "store", "kind": None}]}], [("pp-core", "desktop_rebuild -> store/test-unmarked-writer")]),
        ([{"name": "pp-core", "features": {"desktop_rebuild": ["store?/test-unmarked-writer"]}, "dependencies": [{"name": "pp-storage", "rename": "store", "kind": None, "optional": True}]}], [("pp-core", "desktop_rebuild -> store?/test-unmarked-writer")]),
        ([{"name": "pp-core", "dependencies": [{"name": "pp-storage", "rename": "store", "kind": None, "features": ["test-unmarked-writer"]}]}], [("pp-core", "pp-storage/test-unmarked-writer")]),
        ([{"name": "pp-core", "dependencies": [{"name": "pp-storage", "rename": "store", "kind": "dev", "features": ["test-unmarked-writer"]}]}], []),
    )
    for package, dependencies in GATED.items():
        for dependency in sorted(dependencies):
            gated = {"name": package, "dependencies": [{"name": dependency, "optional": True}],
                     "features": {"default": [], "desktop_rebuild": [f"dep:{dependency}", f"{dependency}/desktop_rebuild"]}}
            refused = [(package, dependency)]
            cases += (([gated], []),
                      ([{**gated, "dependencies": [{"name": dependency, "optional": False}]}], refused),
                      ([{**gated, "features": {**gated["features"], "default": [f"dep:{dependency}"]}}], refused),
                      ([{**gated, "features": {**gated["features"], "default": ["desktop_rebuild"]}}], refused),
                      ([{**gated, "features": {**gated["features"], "other": [f"{dependency}/other"]}}], refused))
    return all(violations(packages) == expected for packages, expected in cases)


# Always evaluate the planted edges. The field used to mirror `--self-test` in argv,
# so a real allow-list pass printed false when that flag was omitted.
rejected = planted_edges_rejected()
if not rejected:
    raise SystemExit("intentional boundary violation was not rejected")
print(json.dumps({"packages": len(metadata["packages"]), "allowed_edges": True, "intentional_violation_rejected": rejected}))
