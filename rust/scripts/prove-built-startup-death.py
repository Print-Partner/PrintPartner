"""Prove production Rust parent-death cleanup during the full built entry's setup.

Only the isolated measured preload receives a test observer. The actual entry,
frontend, workspace outputs and complete runtime dependency tree are retained.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
TEST = "ticket_t_2::built_startup::ticket_t_3_drain_built_release_startup_parent_death"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--negative-control", action="store_true")
    args = parser.parse_args()
    require(sys.platform == "linux", "full built startup proof requires Linux /proc and subreaping")
    node = args.node.resolve(strict=True)
    with node.open("rb") as binary:
        require(binary.read(4) == b"\x7fELF", "--node must resolve to native Linux ELF, not a dispatcher")
    node_identity = json.loads(subprocess.check_output([str(node), "-p",
        "JSON.stringify({version:process.version,abi:process.versions.modules,os:process.platform,arch:process.arch})"], text=True))
    require(node_identity == {"version": "v24.21.0", "abi": "137", "os": "linux",
                              "arch": {"x86_64": "x64", "aarch64": "arm64"}.get(platform.machine())},
            f"wrong native Node identity: {node_identity}")
    args.evidence.mkdir(parents=True, exist_ok=True)
    evidence = Path(tempfile.mkdtemp(prefix="run-", dir=args.evidence.resolve()))
    print(f"Evidence: {evidence}", flush=True)
    stage = evidence / "stage"
    stage.mkdir()
    env = dict(os.environ, PP_COMPAT_BUILT_STARTUP_STAGE=str(stage),
               PP_DESKTOP_RELEASE_MANIFEST=str(stage / "bundle-manifest.json"))
    env.pop("PP_COMPAT_MEASURED_STARTUP_STAGE", None)

    def run(label, command, timeout=900):
        with (evidence / "commands.log").open("a") as log:
            log.write(f"{label}: {shlex.join(command)}\n")
        print(f"{label}: {shlex.join(command)}", flush=True)
        with (evidence / f"{label}.log").open("w") as log:
            result = subprocess.run(command, cwd=REPO, env=env, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=timeout)
        print(f"{label}: exit={result.returncode}", flush=True)
        return result.returncode

    web = REPO / "web"
    entry = web / "apps/server/dist/current/desktop.js"
    preload = entry.with_name("desktop-resolution.js")
    require(entry.is_file() and preload.is_file(), "build full desktop web assets first")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()

    def manifest_command(root, runtime, output):
        return [sys.executable, str(REPO / "rust/scripts/build-desktop-manifest.py"),
                "--web", str(root), "--node", str(runtime), "--package-root", str(root.parent),
                "--output", str(output), "--commit", commit]

    original_manifest = evidence / "original-manifest.json"
    require(run("original-manifest", manifest_command(web, node, original_manifest)) == 0,
            "original full build manifest failed")
    require(run("copy-real-web", ["cp", "-a", "--reflink=auto", str(web), str(stage / "web")]) == 0,
            "real web staging failed")
    shutil.copy2(node, stage / "node")
    staged_entry = stage / "web/apps/server/dist/current/desktop.js"
    staged_preload = staged_entry.with_name("desktop-resolution.js")
    observer = REPO / "rust/crates/pp-compat/tests/fixtures/built-startup-observer.mjs"
    staged_preload.write_bytes(preload.read_bytes() + b"\n" + observer.read_bytes())
    require(run("instrumented-manifest", manifest_command(stage / "web", stage / "node", stage / "bundle-manifest.json")) == 0,
            "instrumented full build manifest failed")
    original = json.loads(original_manifest.read_text())
    measured = json.loads((stage / "bundle-manifest.json").read_text())
    expected = json.loads(json.dumps(original))
    expected["backend"]["desktop-resolution.js"] = digest(staged_preload)
    preload_dependency = preload.resolve().relative_to(REPO).as_posix()
    require(expected["dependencies"]["files"][preload_dependency] == digest(preload),
            "workspace preload dependency alias missing")
    expected["dependencies"]["files"][preload_dependency] = digest(staged_preload)
    require(expected == measured, "staged manifest differs beyond the declared startup observer")
    artifact = {"proof_class": "headless_unsigned_full_built_startup_production_rust_spawn",
                "release_manifest_verified_by_rust": True, "commit": commit,
                "node": str(node), "node_identity": node_identity, "node_sha256": digest(node),
                "entry": str(entry.resolve()), "entry_sha256": digest(entry),
                "staged_entry": str(staged_entry.resolve()), "staged_entry_sha256": digest(staged_entry),
                "original_preload_sha256": digest(preload), "instrumented_preload_sha256": digest(staged_preload),
                "observer_sha256": digest(observer), "manifest_sha256": digest(stage / "bundle-manifest.json"),
                "dependency_files": len(measured["dependencies"]["files"]),
                "instrumentation": "original preload followed by setup-reader observer; all other manifest entries identical"}
    shutil.copy2(stage / "bundle-manifest.json", evidence / "instrumented-manifest.json")
    shutil.copy2(staged_preload, evidence / "instrumented-preload.js")
    write_json(evidence / "artifact-identity.json", artifact)
    write_json(evidence / "environment.json", {key: env[key] for key in
        ("PP_COMPAT_BUILT_STARTUP_STAGE", "PP_DESKTOP_RELEASE_MANIFEST")})
    command = ["cargo", "test", "--locked", "--manifest-path", "rust/Cargo.toml", "-p", "pp-compat",
               "--lib", TEST, "--", "--exact", "--ignored", "--nocapture", "--test-threads=1"]

    def proof(label, test_command=None):
        env["PP_COMPAT_BUILT_STARTUP_EVIDENCE"] = str(evidence / label)
        status = run(label, command if test_command is None else test_command)
        output = (evidence / f"{label}.log").read_text()
        if status == 0:
            require("test result: ok. 1 passed; 0 failed" in output, "exact built startup test did not run")
            receipts = []
            for cycle in range(2):
                root = evidence / label / f"cycle-{cycle}"
                receipt = json.loads((root / "receipt.json").read_text())
                cleanup = json.loads((root / "guard-cleanup.json").read_text())
                require(receipt["cycle"] == cycle and receipt["case"] == "built_startup_parent_death", "wrong receipt")
                require(receipt["armed"]["state"]["milestone"]["entry_sha256"] == artifact["entry_sha256"], "executed entry hash differs")
                require(not cleanup["external_sigkill"] and cleanup["positive_verified_before_cleanup"], "positive relied on fallback cleanup")
                require(receipt["shutdown_ms"] <= 5000 and receipt["group_absent"] and receipt["pids_fully_reaped"], "bounded cleanup unproved")
                receipts.append(receipt)
            write_json(evidence / label / "receipts.json", receipts)
        return status, output

    require(proof("baseline")[0] == 0, "full built startup baseline failed")
    if args.negative_control:
        tracked_source = REPO / "rust/crates/pp-compat/src/unix.rs"
        original_source = tracked_source.read_bytes()
        isolated_rust = stage / "negative-source/rust"
        shutil.copytree(REPO / "rust", isolated_rust, ignore=shutil.ignore_patterns("target"))
        source = isolated_rust / "crates/pp-compat/src/unix.rs"
        require(source.read_bytes() == original_source, "isolated production source differs")
        negative_command = command.copy()
        negative_command[negative_command.index("--manifest-path") + 1] = str(isolated_rust / "Cargo.toml")
        separator = negative_command.index("--")
        negative_command[separator:separator] = ["--target-dir", str(stage / "negative-target")]
        before = b"    _guard: &mut ParentDeathGuard,\n"
        require(original_source.count(before) == 1, "ambiguous production guard declaration")
        changed = original_source.replace(before, b"    guard: &mut ParentDeathGuard,\n", 1)
        before = b"    drop(setup);\n"
        require(changed.count(before) == 1, "ambiguous production guard disarm point")
        changed = changed.replace(before, before + b"    guard.release();\n", 1)
        try:
            source.write_bytes(changed)
            write_json(evidence / "negative-isolation.json", {
                "tracked_source": str(tracked_source), "tracked_source_sha256": digest(tracked_source),
                "isolated_source": str(source), "mutated_source_sha256": digest(source),
                "cargo_target": str(stage / "negative-target"), "tracked_source_unmodified": tracked_source.read_bytes() == original_source})
            require(tracked_source.read_bytes() == original_source, "tracked source changed before negative control")
            status, output = proof("negative", negative_command)
        finally:
            source.write_bytes(original_source)
            require(source.read_bytes() == original_source, "production source restoration failed")
            require(tracked_source.read_bytes() == original_source, "negative control changed tracked source")
            write_json(evidence / "source-restoration.json", {"lib_sha256": digest(source), "byte_for_byte_restored": True,
                       "tracked_source_unmodified": True, "isolated_source": str(source)})
        require(status == 101 and "built startup group outlived force-killed owner" in output,
                "negative did not fail intended production parent-death invariant")
        require("test result: FAILED. 0 passed; 1 failed" in output, "negative ran no exact test")
        failure = json.loads((evidence / "negative/cycle-0/invariant-failure.json").read_text())
        require(failure["backend_state"] == "T" and failure["peer_state"] not in (None, "Z", "X"),
                "negative did not preserve stopped backend and live peer")
        require(failure["owner_reaped"] and not failure["fallback_cleanup_started"], "negative observation followed fallback cleanup")
        cleanup = json.loads((evidence / "negative/cycle-0/guard-cleanup.json").read_text())
        require(cleanup["external_sigkill"] and cleanup["backend_group_absent"] and cleanup["owner_absent"], "negative cleanup unproved")
        write_json(evidence / "negative-control.json", {"expected_failure": "built startup group outlived force-killed owner", "observation": failure, "cleanup": cleanup})
        require(proof("restored")[0] == 0, "restored full built startup proof failed")
    require(digest(entry) == artifact["entry_sha256"] == digest(staged_entry), "actual built entry changed")
    require(digest(preload) == artifact["original_preload_sha256"], "original preload changed")
    require(digest(staged_preload) == artifact["instrumented_preload_sha256"], "instrumented preload changed")
    write_json(evidence / "artifact-unchanged.json", {"entry_sha256": digest(entry), "original_preload_unchanged": True,
                                                   "instrumented_preload_unchanged": True})
    print(f"Full built startup proof passed. Retained receipts: {evidence}", flush=True)


if __name__ == "__main__":
    main()
