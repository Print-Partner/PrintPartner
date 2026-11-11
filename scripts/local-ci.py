#!/usr/bin/env python3
"""Run the required Rust CI checks locally, in CI order, and report every step.

Each step copies its command lines from the workflow step named in `ci`;
scripts/test_local_ci.py fails when a copied line no longer matches the workflow.
A step whose tool, pinned version or host is missing is reported as SKIP with the
reason, never dropped silently. Usage: just ci [--fast] [--list] [--only id,...]
[--skip id,...], or python3 scripts/local-ci.py with the same flags. --fast leaves
out the full nextest and doctest passes and runs nextest with desktop_rebuild on
for the crates this branch changes, using CI's affected-crate filter. See
docs/development/ci.md.
"""

import argparse
from dataclasses import dataclass, field, replace
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
CROSS_OS = "rust-cross-os.yml#"
CONTRACTS = "desktop-contracts.yml#"


@dataclass(frozen=True)
class Step:
    id: str
    title: str
    # Workflow lines, copied verbatim from the `ci` sources ("file.yml#Step name").
    commands: tuple = ()
    ci: tuple = ()
    # Local-only lines that run before `commands`, where CI's own line cannot run locally.
    setup: tuple = ()
    # (tool, version text its --version output must contain, or None).
    requires: tuple = ()
    cwd: str = "."
    env: dict = field(default_factory=dict)
    node22: bool = False
    # Why the step cannot run on this machine at all.
    host_only: str = ""
    # Nonzero exit status that means the step ran but a check in it could not run
    # (a missing browser, say). Reported as SKIP, so --fail-on-skip fails on it.
    skip_status: int = 0


# rust-cross-os.yml sets INSTA_UPDATE workflow-wide so a snapshot mismatch fails
# instead of writing .snap.new files.
def cargo(step_id, title, ci_step, command, requires=(("cargo", None),)):
    return Step(step_id, title, (command,), (CROSS_OS + ci_step,), requires=requires,
                env={"NEXTEST_FILTER": "all()", "INSTA_UPDATE": "no"})


NEXTEST = (("cargo", None), ("cargo-nextest", None))
# The workflow's install step owns the scanner version.
MACHETE_VERSION = re.search(r"tool: cargo-machete@([0-9.]+)",
                            (ROOT / ".github/workflows/rust-deps-weekly.yml").read_text())[1]
RUFF_VERSION = re.search(
    r'^required-version = "==([0-9.]+)"$',
    (ROOT / "ruff.toml").read_text(),
    re.MULTILINE,
)[1]
CARGO = "cargo {} --manifest-path rust/Cargo.toml --locked --workspace"

STEPS = [
    Step("local-ci-self-test", "this command's own tests",
         setup=("python3 -B -m unittest discover -s scripts -p test_local_ci.py",),
         requires=(("python3", None),)),
    Step("selector-tests", "CI input-selection tests",
         ("python3 -B -m unittest discover -s scripts -p 'test_*path_filter.py'",
          "python3 -B -m unittest discover -s scripts -p test_alert_macos_main.py",
          "python3 -B -m unittest discover -s scripts -p test_cancel_stale_merge_groups.py"),
         (CROSS_OS + "Select changed inputs", "actionlint.yml#Select changed inputs"), requires=(("python3", None),)),
    # No hosted job runs these yet, so the line is local-only.
    Step("perf-baseline-tests", "Node-versus-Rust baseline harness tests",
         setup=("python3 -B -m unittest discover -s scripts/perf-baseline -p 'test_*.py' -v",),
         requires=(("python3", None),)),
    Step("actionlint", "actionlint (required check)", setup=("actionlint -color",),
         requires=(("actionlint", "1.7.12"),)),
    Step("agent-guidance", "links and anchors in every tracked Markdown page, and agent guidance rules",
         ("python3 -B -m unittest discover -s scripts -p test_agent_guidance.py -v",
          "python3 -B scripts/check-agent-guidance.py"),
         ("actionlint.yml#Validate shared agent guidance",), requires=(("python3", None), ("git", None))),
    Step("large-files", "tracked file size limit",
         ("python3 -B -m unittest discover -s scripts -p test_check_large_files.py -v",
          "python3 -B scripts/check-large-files.py"),
         ("actionlint.yml#Check tracked file sizes",), requires=(("python3", None), ("git", None))),
    Step("python-lint", "Ruff unused-import and complexity lint",
         ("python3 -B -m unittest discover -s scripts -p test_ruff_lint.py -v",
          "python3 -B -m unittest discover -s scripts -p test_operation_inventory.py -v",
          "ruff check scripts rust/scripts web/scripts slicer-sidecar .claude/hooks"),
         ("actionlint.yml#Lint Python with Ruff",),
         requires=(("python3", None), ("ruff", RUFF_VERSION))),
    Step("zizmor", "zizmor (required check)",
         setup=("zizmor --min-severity informational --config .github/zizmor.yml .",),
         requires=(("zizmor", "1.31.1"),)),
    Step("ast-grep", "ast-grep rule tests and ratchet",
         ("ast-grep test --config rust/sgconfig.yml --skip-snapshot-tests",
          "python3 -m unittest discover -s rust/scripts -p 'test_ast_grep.py' -v",
          "python3 rust/scripts/check-ast-grep.py"),
         ("desktop-fleet-rust.yml#Check Rust ast-grep ratchet",),
         # CI fetches with --depth=1, which would make a full local clone shallow.
         setup=("git fetch --no-tags origin '+refs/heads/main:refs/remotes/origin/main'",),
         requires=(("git", None), ("ast-grep", "0.50.0"), ("python3", None))),
    Step("boundaries", "crate boundary and pp-domain dependency guards",
         ("python3 rust/scripts/check-boundaries.py --self-test",
          "python3 rust/scripts/check-domain-deps.py --self-test"),
         ("chain-tip-gate.yml#check-boundaries", "chain-tip-gate.yml#pp-domain dependency guard"),
         requires=(("python3", None),)),
    # pp-storage tests `git archive` pinned commits that a shallow clone lacks.
    Step("historical-schema", "fetch pinned historical schema commits",
         ("rust/scripts/fetch-historical-schema-commits.sh",),
         (CROSS_OS + "Fetch pinned historical schema commits",), requires=(("git", None),)),
    Step("fmt", "cargo fmt", ("cargo fmt --manifest-path rust/Cargo.toml --all --check",),
         (CROSS_OS + "cargo fmt",), requires=(("cargo", None),)),
    Step("rust-deps", "unused Rust dependencies",
         ("python3 -B -m unittest discover -s scripts -p test_cargo_machete.py -v",
          "cargo machete rust"),
         ("rust-deps-weekly.yml#Check unused Rust dependencies",),
         requires=(("python3", None), ("cargo", None), ("cargo-machete", MACHETE_VERSION))),
    Step("web-assets", "desktop web assets and rust/bundle-manifest.json",
         ("node_path=$(node -p process.execPath)",
          "npm --prefix web ci",
          "npm --prefix web run build",
          '"$MANIFEST_PYTHON" rust/scripts/build-desktop-manifest.py --web web --node "$node_path" '
          '--output rust/bundle-manifest.json --commit "$GITHUB_SHA"'),
         (CROSS_OS + "Build desktop assets and generate measured manifest",),
         requires=(("node", "v24.21.0"), ("npm", None), ("python3", None)),
         env={"VITE_PRINT_PARTNER_DESKTOP": "1", "MANIFEST_PYTHON": "python3"}),
    # The replacement UI package. No hosted job runs these yet, so they are local-only
    # lines. After web-assets, which installs web/node_modules on Node 24. Vitest
    # exits nonzero when it finds no tests.
    Step("ui-package", "replacement UI package typecheck, lint, tests, build and font requests",
         # The package's build script runs its typecheck first, so there is no separate
         # typecheck line. The desktop does not ship apps/ui, so the measured manifest
         # does not follow it and its tests and build leave rust/bundle-manifest.json valid.
         setup=("npm --prefix web run lint --workspace @print-partner/ui",
                "npm --prefix web run test --workspace @print-partner/ui",
                "npm --prefix web run build --workspace @print-partner/ui",
                # Serves the build and fails on any request to another origin. Last, so
                # its exit 77 for a missing browser engine is the step's status.
                "npm --prefix web run test:browser --workspace @print-partner/ui"),
         requires=(("node", "v24.21.0"), ("npm", None), ("python3", None)),
         skip_status=77),
    # After web-assets, which installs the parser it loads from web/node_modules, and
    # before node-fixtures reinstalls them. Web CI runs it on Node 22.
    Step("workaround-ratchet", "TODO, eslint-disable and Rust #[allow] ratchet (Node 22)",
         ("node ../scripts/workaround-ratchet.mjs && node --test ../scripts/workaround-ratchet.test.mjs",),
         ("web-ci.yml#Check workaround ratchet",),
         requires=(("node", "v22.23.3"),), cwd="web", node22=True),
    Step("duplication", "duplicated-code baseline and negative controls (Node 22)",
         ("node --test scripts/check-duplication.test.mjs",
          "node scripts/check-duplication.mjs"),
         ("duplication.yml#Check duplicated code",),
         setup=("git fetch --no-tags origin '+refs/heads/main:refs/remotes/origin/main'",),
         requires=(("git", None), ("node", "v22.23.3")), node22=True),
    cargo("clippy-off", "clippy, desktop_rebuild off", "clippy (desktop_rebuild off)",
          CARGO.format("clippy") + " --all-targets --keep-going -- -D warnings"),
    cargo("clippy-on", "clippy, desktop_rebuild on", "clippy (desktop_rebuild on)",
          CARGO.format("clippy") + " --all-targets --keep-going --features desktop_rebuild -- -D warnings"),
    Step("clippy-limits", "Clippy complexity limits, both feature modes",
         ("python3 -B -m unittest discover -s rust/scripts -p test_clippy_limits.py -v",
          "python3 -B rust/scripts/check-clippy-limits.py"),
         (CROSS_OS + "Check Rust Clippy limits",),
         setup=("git fetch --no-tags origin '+refs/heads/main:refs/remotes/origin/main'",),
         requires=(("git", None), ("cargo", None), ("python3", None)),
         host_only="" if sys.platform == "linux" else "needs Linux for the Clippy baseline"),
    cargo("nextest-off", "nextest, desktop_rebuild off", "nextest (desktop_rebuild off)",
          CARGO.format("nextest run") + ' --no-fail-fast --no-tests=warn -E "$NEXTEST_FILTER"', NEXTEST),
    cargo("nextest-on", "nextest, desktop_rebuild on", "nextest (desktop_rebuild on)",
          CARGO.format("nextest run") + ' --no-fail-fast --no-tests=warn -E "$NEXTEST_FILTER" --features desktop_rebuild', NEXTEST),
    cargo("doc-off", "doctests, desktop_rebuild off", "doctests (desktop_rebuild off)",
          CARGO.format("test") + " --no-fail-fast --doc"),
    cargo("doc-on", "doctests, desktop_rebuild on", "doctests (desktop_rebuild on)",
          CARGO.format("test") + " --no-fail-fast --doc --features desktop_rebuild"),
    # Last: `npm ci` here rebuilds web/node_modules for Node 22.
    Step("node-fixtures", "Node parity fixture and route inventory freshness (Node 22)",
         ("npm ci && npm rebuild better-sqlite3",
          "npm run build -w @print-partner/contracts && npm run build -w @print-partner/domain",
          "node --conditions=development ../fixtures/parity/auth-admission/generate.mjs --check",
          "node ../fixtures/parity/auth-key/generate.mjs --check",
          "node ../fixtures/parity/auth-login/generate.mjs --check",
          "node ../fixtures/parity/auth-registration/generate.mjs --check",
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/accepted-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/catalog-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/drafts-http/generate.mjs --check',
          "node ../fixtures/parity/github-sync/generate.mjs --check",
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/jobs-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/jobs-ws/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/layers-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/manifest-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/manifest-checkouts-http/generate.mjs --check',
          'TMPDIR="$RUNNER_TEMP" node ../fixtures/parity/plans-http/generate.mjs --check',
          "node ../rust/crates/pp-storage/tests/fixtures/required-units/messages-node.mjs --check",
          'TMPDIR="$RUNNER_TEMP" node --conditions=development --import tsx '
          "../rust/crates/pp-storage/tests/fixtures/required-units/gaps-node.mjs --check",
          "node ../rust/crates/pp-storage/tests/fixtures/manifest-globs-node.mjs --check",
          "node ../rust/crates/pp-storage/tests/fixtures/manifest-case-node.mjs --check",
          "npm run build -w @print-partner/server",
          'node ../rust/scripts/inspect-node-routes.mjs "${RUNNER_TEMP}/routes-inventory.json"',
          'diff -u ../fixtures/http/routes-inventory.json "${RUNNER_TEMP}/routes-inventory.json"'),
         (CONTRACTS + "Install locked dependencies", CONTRACTS + "Build authority packages",
          "auth-admission-freshness.yml#Check auth admission golden on canonical Node",
          CONTRACTS + "Verify auth Node fixtures", CONTRACTS + "Verify GRE-451 parity Node fixtures",
          CONTRACTS + "Verify required-unit Node fixtures",
          CONTRACTS + "Verify builder glob Node fixtures", CONTRACTS + "Verify golden Node HTTP route inventory"),
         requires=(("node", "v22.23.3"), ("npm", None)), cwd="web", node22=True),
    Step("rust-windows", "rust (windows) (advisory check)",
         host_only="needs a Windows runner; CI runs it on windows-latest"),
    Step("rust-macos", "rust (macos) (advisory check)",
         host_only="needs a macOS runner with a case-sensitive TMPDIR; add the ci:cross-os label to run it in CI"),
    Step("pr-evidence", "Lint Evidence block (required check)",
         host_only="reads the pull request body; CI checks it on the PR"),
]


# --fast leaves these out and runs FAST_NEXTEST in place of nextest-on.
FAST_LEAVES_OUT = ("nextest-off", "nextest-on", "doc-off", "doc-on")
FAST_NEXTEST = "nextest-affected"
FILTER_SCRIPT = ROOT / "scripts/ci-affected-nextest-filter.py"
# The agent fleet installs sccache here; PATH wins when both exist.
FLEET_SCCACHE = Path("/opt/agent-fleet/bin/sccache")


def fast_steps():
    """STEPS for --fast: nextest-on becomes FAST_NEXTEST, the other full passes go."""
    steps = []
    for step in STEPS:
        if step.id == "nextest-on":
            # Same command line; main() sets NEXTEST_FILTER to the affected filter.
            steps.append(replace(step, id=FAST_NEXTEST,
                                 title="nextest, desktop_rebuild on, crates this branch affects"))
        elif step.id not in FAST_LEAVES_OUT:
            steps.append(step)
    return steps


def git_output(env, *args):
    """Return git's stdout, or None when git fails or is missing."""
    try:
        result = subprocess.run(["git", *args], cwd=ROOT, env=env, capture_output=True)
    except OSError:
        return None
    return result.stdout.decode("utf-8", errors="surrogateescape") if result.returncode == 0 else None


def load_filter_module():
    spec = importlib.util.spec_from_file_location("ci_affected_nextest_filter", FILTER_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def affected_nextest_filter(env, git=git_output):
    """Return CI's nextest filterset for this branch against origin/main, or all().

    The changed paths are the diff from `git merge-base origin/main HEAD` to the
    working tree, plus untracked files, so uncommitted edits count. The filter
    script's own rules decide, so shared inputs still force all(), as in CI.
    """
    base = (git(env, "merge-base", "origin/main", "HEAD") or "").strip()
    if not base:
        print("affected nextest filter: no merge-base with origin/main, using all()", flush=True)
        return "all()"
    changed = git(env, "diff", "--name-only", "--no-renames", "-z", base)
    untracked = git(env, "ls-files", "--others", "--exclude-standard", "-z")
    if changed is None or untracked is None:
        print("affected nextest filter: git diff failed, using all()", flush=True)
        return "all()"
    paths = [path for path in (changed + untracked).split("\0") if path]
    try:
        module = load_filter_module()
        crates = ROOT / "rust/crates"
        expression = module.filter_expression("pull_request", paths, module.workspace_packages(crates),
                                              module.source_readers(crates))
    except Exception as error:  # noqa: BLE001 - any filter error means run everything
        print(f"affected nextest filter: {FILTER_SCRIPT.name} failed ({error!r}), using all()", flush=True)
        return "all()"
    print(f"affected nextest filter against {base[:12]}: {expression}", flush=True)
    return expression


def unmet(step, env):
    """Return why the step cannot run with this environment, or ''."""
    if step.host_only:
        return step.host_only
    if step.node22 and not env.get("PP_SCHEMA_PARITY_NODE"):
        return "set PP_SCHEMA_PARITY_NODE to a Node v22.23.3 executable"
    env = step_env(step, env)
    for tool, version in step.requires:
        path = shutil.which(tool, path=env.get("PATH", ""))
        if not path:
            return f"{tool} not on PATH" + (f" (CI uses {tool} {version})" if version else "")
        if version:
            probe = subprocess.run([path, "--version"], capture_output=True, text=True, env=env)
            if version not in probe.stdout:
                found = probe.stdout.strip() or probe.stderr.strip()
                hint = " (PP_SCHEMA_PARITY_NODE)" if step.node22 and tool == "node" else ""
                return f"needs {tool} {version}{hint}, found {found!r}"
    return ""


def step_env(step, env):
    env = {**env, **step.env}
    if step.node22:
        node_dir = str(Path(env["PP_SCHEMA_PARITY_NODE"]).parent)
        env["PATH"] = node_dir + os.pathsep + env.get("PATH", "")
    return env


def run_bash(step_id, script, cwd, env):
    return subprocess.run(["bash", "--noprofile", "--norc", "-euo", "pipefail", "-c", script],
                          cwd=cwd, env=env).returncode


def base_env(env):
    env = dict(os.environ if env is None else env)
    node = shutil.which("node", path=env.get("PATH", ""))
    if node:
        env.setdefault("PP_TEST_NODE", os.path.realpath(node))
    if "GITHUB_SHA" not in env:
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True)
        env["GITHUB_SHA"] = sha.stdout.strip()
    target = Path(env.get("CARGO_TARGET_DIR") or ROOT / "rust/target")
    env.setdefault("RUNNER_TEMP", str(target / "local-ci"))
    return use_sccache(env)


def use_sccache(env):
    """Wrap rustc in sccache when it is installed and the caller set no wrapper.

    An empty RUSTC_WRAPPER counts as set: it is how a caller turns a wrapper off.
    """
    if "RUSTC_WRAPPER" in env:
        return env
    sccache = shutil.which("sccache", path=env.get("PATH", ""))
    if not sccache and FLEET_SCCACHE.is_file() and os.access(FLEET_SCCACHE, os.X_OK):
        sccache = str(FLEET_SCCACHE)
    if sccache:
        # sccache cannot cache incremental builds.
        env["RUSTC_WRAPPER"] = sccache
        env["CARGO_INCREMENTAL"] = "0"
    return env


def ids(value):
    return [item for item in value.split(",") if item] if value else []


def main(argv=None, env=None, run=run_bash, affected=affected_nextest_filter):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fast", action="store_true",
                        help="leave out the full nextest and doctest passes; run nextest with "
                             "desktop_rebuild on for the crates this branch affects")
    parser.add_argument("--list", action="store_true", help="print the steps and exit")
    parser.add_argument("--only", help="comma-separated step ids to run")
    parser.add_argument("--skip", help="comma-separated step ids to leave out")
    parser.add_argument("--fail-on-skip", action="store_true",
                        help="exit nonzero if any selected step cannot run")
    args = parser.parse_args(argv)

    steps = fast_steps() if args.fast else STEPS
    known = {step.id for step in steps}
    for step_id in ids(args.only) + ids(args.skip):
        if step_id not in known:
            mode = " --fast" if args.fast else ""
            print(f"unknown step: {step_id} (see --list{mode})", file=sys.stderr)
            return 2
    if args.list:
        for step in steps:
            print(f"{step.id:20} {step.title}")
        return 0

    only, skip = set(ids(args.only)), set(ids(args.skip))
    plan = [step for step in steps if (not only or step.id in only) and step.id not in skip]
    env = base_env(env)
    if env.get("RUSTC_WRAPPER"):
        print(f"RUSTC_WRAPPER={env['RUSTC_WRAPPER']} CARGO_INCREMENTAL={env.get('CARGO_INCREMENTAL', '')}",
              flush=True)
    Path(env["RUNNER_TEMP"]).mkdir(parents=True, exist_ok=True)
    results = []
    for step in plan:
        reason = unmet(step, env)
        if reason:
            print(f"\n== SKIP {step.id}: {reason}", flush=True)
            results.append(("SKIP", step, reason))
            continue
        if step.id == FAST_NEXTEST:
            # The cargo steps' own NEXTEST_FILTER=all() would override the caller's.
            step = replace(step, env={**step.env, "NEXTEST_FILTER": affected(env)})
        print(f"\n== RUN {step.id}: {step.title}", flush=True)
        started = time.monotonic()
        code = run(step.id, "\n".join(step.setup + step.commands), ROOT / step.cwd, step_env(step, env))
        detail = f"{time.monotonic() - started:.0f}s"
        if code == 0:
            status = "PASS"
        elif step.skip_status and code == step.skip_status:
            status, detail = "SKIP", f"{detail}; a check in the step printed SKIP"
        else:
            status = "FAIL"
        results.append((status, step, detail))

    print("\n== local CI summary ==")
    for status, step, detail in results:
        print(f"{status} {step.id:20} {detail}")
    counts = {s: sum(1 for r in results if r[0] == s) for s in ("PASS", "FAIL", "SKIP")}
    print(f"{counts['PASS']} passed, {counts['FAIL']} failed, {counts['SKIP']} skipped")
    return 1 if counts["FAIL"] or (args.fail_on_skip and counts["SKIP"]) else 0


if __name__ == "__main__":
    sys.exit(main())
