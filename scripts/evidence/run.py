#!/usr/bin/env python3
"""Screenshot replacement-UI paths from a Rust-only core on a seeded throwaway data directory.

One run: build (or reuse) pp-server with desktop_rebuild, generate the synthetic dataset,
start the core with PP_DESKTOP_REBUILD=1 on loopback through verify.py launch, serve the
built replacement UI with vite preview, sign in a throwaway account, run the named setup
steps, screenshot each path, then stop everything and delete the data. See README.md.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

from publisher import EVIDENCE_BIN

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
STEP_NAMES = ("settle-reconciliation", "snapshot", "duplicate-order")
MODULES = ("farm", "orders", "bambu", "spoolman")
SHOT_PATH = re.compile(r"^/(?!/)[A-Za-z0-9/_.~-]*$")
HARNESS_DIR = ROOT / "rust/target/evidence-harness"
STAMP = HARNESS_DIR / "stamp.json"
# Other builds (verify.py, cargo build -p pp-server) overwrite target/debug/pp-server
# without desktop_rebuild, so the harness keeps its own copy and hashes what it reuses.
BINARY = HARNESS_DIR / "pp-server"
BUILT_FILES = (BINARY, ROOT / "rust/bundle-manifest.json", ROOT / "web/apps/ui/dist/index.html")
# Inputs whose changes need a rebuild of the server, its bundle or the replacement UI.
BUILD_INPUTS = ("web/apps", "web/packages", "web/package.json", "web/package-lock.json",
                "rust/crates", "rust/Cargo.toml", "rust/Cargo.lock", "rust/scripts")
# What the core verifies at start (pp-compat release.rs, verify_bundle) against the manifest
# embedded in pp-server: these build directories under web/ file by file, by name and
# sha256, plus the metadata files and the Node executable. Same map as
# rust/scripts/desktop_workspaces.py ARTIFACTS; test_run.py checks they agree.
RELEASE_DIRS = {"frontend": "apps/web/dist", "backend": "apps/server/dist/current",
                "contracts": "packages/contracts/dist/current", "domain": "packages/domain/dist/current"}
READY_SECS = 300
STOP_SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
CAPTURE_SECS = 900


class Stopped(Exception):
    """Raised by SIGTERM or SIGHUP so cleanup still runs."""


def say(message):
    print(f"evidence-run: {message}", flush=True)


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], check=True, capture_output=True).stdout


def fingerprint():
    digest = hashlib.sha256(git("rev-parse", "HEAD"))
    digest.update(git("diff", "HEAD", "--binary", "--", *BUILD_INPUTS))
    for name in git("ls-files", "--others", "--exclude-standard", "-z", "--", *BUILD_INPUTS).split(b"\0"):
        if name:
            digest.update(name + b"\0" + (ROOT / name.decode()).read_bytes())
    return digest.hexdigest()


def run(*args, env=None, cwd=ROOT, stdout=None, timeout=None):
    say("$ " + " ".join(str(a) for a in args))
    return subprocess.run([str(a) for a in args], cwd=cwd, env=env, check=True, stdout=stdout, text=True,
                          timeout=timeout)


def file_hashes():
    return {str(path): hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
            for path in BUILT_FILES}


def sha256(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def release_matches(manifest_path, web, node):
    """Whether the web build and Node still match the manifest pp-server embedded, so the
    core's start-up release check will pass. A plain `npm run build` rewrites them."""
    try:
        manifest = json.loads(manifest_path.read_text())
        for key, directory in RELEASE_DIRS.items():
            # The core canonicalizes each root (dist/current links to a versioned directory)
            # and refuses a symlink inside it.
            root = (web / directory).resolve(strict=True)
            if not root.is_dir() or not manifest.get(key):
                return False
            actual = {}
            for path in root.rglob("*"):
                if path.is_symlink():
                    return False
                if path.is_file():
                    actual[path.relative_to(root).as_posix()] = sha256(path)
            if actual != manifest[key]:
                return False
        if any(sha256(web / name) != digest for name, digest in manifest["metadata"].items()):
            return False
        return sha256(Path(node)) == manifest["node_sha256"]
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return False


def build(node, rebuild):
    """Returns a pp-server built with desktop_rebuild against the current checkout."""
    current = fingerprint()
    if not rebuild and STAMP.is_file():
        stamp = json.loads(STAMP.read_text())
        if stamp.get("fingerprint") == current and stamp.get("files") == file_hashes():
            if release_matches(ROOT / "rust/bundle-manifest.json", ROOT / "web", node):
                say(f"reusing {BINARY} (checkout, build outputs and web build unchanged since its build)")
                return BINARY
            say("rebuilding: the web build or Node no longer matches the manifest pp-server was built with")
    if not (ROOT / "web/node_modules").is_dir():
        raise SystemExit("web/node_modules is missing; run npm --prefix web ci first")
    commit = git("rev-parse", "HEAD").decode().strip()
    run("npm", "--prefix", "web", "run", "build", env=dict(os.environ, VITE_PRINT_PARTNER_DESKTOP="1"))
    run("npm", "--prefix", "web", "run", "build", "--workspace", "@print-partner/ui")
    run(sys.executable, "rust/scripts/build-desktop-manifest.py", "--web", "web", "--node", node,
        "--output", "rust/bundle-manifest.json", "--commit", commit)
    out = run("cargo", "build", "--locked", "--manifest-path", "rust/Cargo.toml", "-p", "pp-server",
              "--features", "desktop_rebuild", "--message-format=json-render-diagnostics",
              stdout=subprocess.PIPE).stdout
    binary = None
    for line in out.splitlines():
        artifact = json.loads(line)
        if (artifact.get("reason") == "compiler-artifact" and artifact["target"]["name"] == "pp-server"
                and artifact.get("executable")):
            binary = Path(artifact["executable"])
    if binary is None:
        raise SystemExit("cargo did not report a pp-server executable")
    HARNESS_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, BINARY)
    STAMP.write_text(json.dumps({"fingerprint": current, "files": file_hashes(), "commit": commit}))
    return BINARY


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_for(predicate, what, process, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{what}: process exited with {process.returncode}")
        value = predicate()
        if value:
            return value
        time.sleep(0.5)
    raise RuntimeError(f"{what}: not ready after {seconds}s")


def wait_for_core(log, launcher):
    """The core's origin once it is ready. A core that exits at start often found a web
    build that no longer matches its manifest ("Release file inventory mismatch")."""
    try:
        return wait_for(lambda: server_origin(log), "Rust-only core", launcher, READY_SECS)
    except RuntimeError as error:
        raise RuntimeError(f"{error}; see launcher.log, and run again with --rebuild") from None


def server_origin(log):
    match = re.search(r"^PP_SERVER_URL=(http://127\.0\.0\.1:\d+) ", log.read_text(), re.M)
    return match.group(1) if match else None


def answers(url):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            return response.status == 200
    except (urllib.error.URLError, OSError):
        return False


def stop(process, name, grace):
    """SIGTERM the process group, SIGKILL after grace seconds. Returns the exit status."""
    if process is None:
        return 0
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            say(f"{name} ignored SIGTERM for {grace}s; killing it")
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
    return process.returncode


def shot_name(path):
    """File name for a shot: /builds/2/history -> builds-2-history.png."""
    return (re.sub(r"[^A-Za-z0-9]+", "-", path).strip("-") or "root") + ".png"


def parse_step(text):
    name, _, arg = text.partition(":")
    if name not in STEP_NAMES:
        raise argparse.ArgumentTypeError(f"unknown step {name!r}; known: {', '.join(STEP_NAMES)}")
    if not re.fullmatch(r"[1-9][0-9]{0,15}", arg):
        raise argparse.ArgumentTypeError(f"{name} takes a Build ID, as {name}:2")
    return {"name": name, "arg": int(arg)}


def parse_shot(text):
    if not SHOT_PATH.match(text):
        raise argparse.ArgumentTypeError(f"--shot takes an app path such as /builds/2/history, not {text!r}")
    return text


def parse_args(argv):
    parser = argparse.ArgumentParser(prog="scripts/evidence/run.sh", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--seed", type=int, default=1138, help="synthetic dataset seed (default 1138)")
    parser.add_argument("--shot", type=parse_shot, action="append", default=[],
                        help="replacement-UI path to screenshot; repeat for more")
    parser.add_argument("--step", type=parse_step, action="append", default=[],
                        help="setup step before the shots, in order: " + ", ".join(f"{n}:<build-id>" for n in STEP_NAMES))
    parser.add_argument("--module", choices=MODULES, action="append", default=[],
                        help="turn this Module on in the seed before start; repeat for more")
    parser.add_argument("--out", type=Path, help="directory for the PNGs (default: a new temporary directory)")
    parser.add_argument("--review-plan", action="store_true", help="open Review and accept on Plan shots")
    parser.add_argument("--full-page", action="store_true", help="capture the whole scrolled page")
    parser.add_argument("--rebuild", action="store_true", help="build even if the checkout is unchanged")
    parser.add_argument("--display", nargs="?", const=os.environ.get("EVIDENCE_DISPLAY", ":99"),
                        help="show the browser on this X display (default :99) and capture with the "
                             "evidence tool instead of headless screenshots")
    parser.add_argument("--self-test", action="store_true",
                        help="run safety.test.mjs and steps.test.mjs, capture /builds once, and check the PNG and the cleanup")
    args = parser.parse_args(argv)
    if args.self_test:
        if args.shot or args.step:
            parser.error("--self-test takes no --shot or --step")
        args.shot = ["/builds"]
    if not args.shot:
        parser.error("give at least one --shot")
    return args


def turn_on_modules(seed, modules):
    """Turns Modules on in the disposable seed, before the core starts.

    No Rust route writes tenant Module choices yet (Node owns PATCH /settings/features),
    so this writes the same `feature_toggles` row that route and Rust's /health read.
    """
    if not modules:
        return
    with sqlite3.connect(seed / "print-partner.db") as db:
        row = db.execute("SELECT value FROM app_settings WHERE tenant_id='default' AND key='feature_toggles'").fetchone()
        toggles = json.loads(row[0]) if row else {}
        toggles.update({module: True for module in modules})
        db.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles',?) "
                   "ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", (json.dumps(toggles),))
    db.close()
    say(f"turned on in the seed: {', '.join(modules)}")


def seed_order_holder(seed, steps):
    """Gives duplicate-order captures a holder in the disposable seed before startup."""
    for step in steps:
        if step["name"] != "duplicate-order":
            continue
        with sqlite3.connect(seed / "print-partner.db") as db:
            holder = db.execute(
                "SELECT id FROM build_profiles WHERE archived_at IS NULL AND id <> ? ORDER BY id LIMIT 1",
                (step["arg"],),
            ).fetchone()
            if holder is None:
                raise RuntimeError("no other synthetic Build for an order holder")
            db.execute("UPDATE build_profiles SET order_number=? WHERE id=?",
                       (f"EVIDENCE-ORDER-{step['arg']}", holder[0]))
        db.close()
    if any(step["name"] == "duplicate-order" for step in steps):
        say("prepared an order holder in the disposable seed")


def display_geometry(name):
    out = subprocess.run(["xdpyinfo", "-display", name], check=True, capture_output=True, text=True).stdout
    width, height = re.search(r"dimensions:\s+(\d+)x(\d+)", out).groups()
    return int(width), int(height)


def capture(args, origin, ui_origin, credential, out_dir, node, browser_tmp):
    config = {"root": str(ROOT), "coreOrigin": origin, "uiOrigin": ui_origin, "shots": [{"path": path, "file": shot_name(path)} for path in args.shot],
              "reviewPlan": args.review_plan, "steps": args.step, "outDir": str(out_dir), "fullPage": args.full_page, "display": None}
    if args.display:
        width, height = display_geometry(args.display)
        config["display"] = {"name": args.display, "width": width, "height": height, "evidenceBin": EVIDENCE_BIN}
    # Chromium's profile, cookies included, goes under browser_tmp, which cleanup removes.
    env = dict(os.environ, CREDENTIAL_FILE=str(credential), TMPDIR=str(browser_tmp))
    run(node, HERE / "capture.mjs", json.dumps(config), env=env, timeout=CAPTURE_SECS)


def spawn(procs, key, args, **kwargs):
    """Starts a process and records it in procs[key] with stop signals held, so a signal
    cannot land after the process exists but before cleanup can see it. The child gets
    the caller's signal mask back before exec, so it can still be stopped."""
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
    try:
        procs[key] = subprocess.Popen(
            args, start_new_session=True,
            preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, previous), **kwargs)
    finally:
        # A held signal is delivered here, after the process is recorded.
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    return procs[key]


def session(args, work, out_dir, node, binary):
    """Starts the core and the UI server, captures, and always stops both. Returns failures."""
    seed, launch_tmp, private = work / "seed", work / "tmp", work / "private"
    launch_tmp.mkdir()
    private.mkdir(mode=0o700)
    (private / "browser").mkdir(mode=0o700)
    log = work / "launcher.log"
    procs = {}
    failures = []
    try:
        run("npm", "--prefix", "web", "run", "--silent", "fixture:synthetic", "--", "--seed", args.seed, seed,
            stdout=subprocess.DEVNULL)
        turn_on_modules(seed, args.module)
        seed_order_holder(seed, args.step)
        # verify.py copies the seed into its own temporary data directory under launch_tmp,
        # adds the Rust ownership marker, and removes the copy when it stops.
        env = dict(os.environ, PP_DESKTOP_REBUILD="1", TMPDIR=str(launch_tmp))
        with log.open("w") as handle:
            launcher = spawn(
                procs, "launcher",
                [sys.executable, str(ROOT / "scripts/verify-rust-gateway/verify.py"), "launch",
                 "--repo", str(ROOT), "--binary", str(binary), "--seed", str(seed),
                 "--credential-file", str(private / "credential.json")],
                cwd=ROOT, env=env, stdout=handle, stderr=subprocess.STDOUT)
        origin = wait_for_core(log, launcher)
        say(f"core ready at {origin} (PP_DESKTOP_REBUILD=1)")
        port = free_port()
        preview = spawn(
            procs, "preview",
            [str(ROOT / "web/node_modules/.bin/vite"), "preview", "--host", "127.0.0.1", "--port", str(port),
             "--strictPort"],
            cwd=ROOT / "web/apps/ui", env=dict(os.environ, PP_UI_CORE_URL=origin),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        ui_origin = f"http://127.0.0.1:{port}"
        wait_for(lambda: answers(ui_origin + "/"), "replacement UI server", preview, 60)
        capture(args, origin, ui_origin, private / "credential.json", out_dir, node,
                private / "browser")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, RuntimeError) as error:
        failures.append(str(error))
    finally:
        for signum in STOP_SIGNALS:
            signal.signal(signum, signal.SIG_IGN)
        launcher = procs.get("launcher")
        stop(procs.get("preview"), "vite preview", 10)
        status = stop(launcher, "launcher", 90)
        text = log.read_text() if log.exists() else ""
        if launcher is not None and (status != 0 or "STOP: exit=0" not in text):
            failures.append(f"core did not stop cleanly (launcher exit {status})")
            # The log holds server stderr, never the bootstrap URL; keep it for diagnosis.
            shutil.copy(log, out_dir / "launcher.log")
            failures.append(f"launcher log: {out_dir / 'launcher.log'}")
        elif launcher is not None:
            say("core stopped cleanly (STOP: exit=0)")
        leftovers = sorted(p.name for p in launch_tmp.iterdir()) if launch_tmp.exists() else []
        if leftovers:
            failures.append(f"launcher left data behind: {leftovers}")
    return failures


def stop_label(error):
    """The signal that stopped the run. Stopped carries its signal name; Ctrl-C raises a
    KeyboardInterrupt with no message, and an exception instance is always truthy."""
    return str(error) or "SIGINT"


def check_png(path):
    with path.open("rb") as handle:
        if handle.read(8) != b"\x89PNG\r\n\x1a\n":
            raise SystemExit(f"self-test: {path} is not a PNG")
    if path.stat().st_size < 10_000:
        raise SystemExit(f"self-test: {path} is suspiciously small")


def main(argv=None):
    args = parse_args(sys.argv[1:] if argv is None else argv)
    node = shutil.which("node")
    if node is None:
        raise SystemExit("node not found")
    node = str(Path(node).resolve())

    def on_signal(signum, _frame):
        raise Stopped(signal.Signals(signum).name)

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, on_signal)

    if args.self_test:
        # The redaction tests (no cookie in a failed request) and the setup step tests.
        run(node, "--test", HERE / "safety.test.mjs", HERE / "steps.test.mjs")
    try:
        binary = build(node, args.rebuild)
    except (KeyboardInterrupt, Stopped) as stop_reason:
        say(f"FAIL: stopped by {stop_label(stop_reason)} while building")
        return 1
    if args.out is None:
        out_dir = Path(tempfile.mkdtemp(prefix=f"pp-evidence-{args.seed}-"))
    else:
        out_dir = args.out.resolve()
        out_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="pp-evidence-work-"))
    failures = []
    try:
        failures = session(args, work, out_dir, node, binary)
    except (KeyboardInterrupt, Stopped) as stop_reason:
        failures = [f"stopped by {stop_label(stop_reason)}"]
    finally:
        # Cleanup must finish even if a second signal arrives.
        for signum in STOP_SIGNALS:
            signal.signal(signum, signal.SIG_IGN)
        shutil.rmtree(work, ignore_errors=True)
        if work.exists():
            failures.append(f"could not remove {work}")
        else:
            say(f"removed {work} (seed, data directory, credentials and browser profile)")
        for signum in STOP_SIGNALS:
            signal.signal(signum, signal.SIG_DFL)
        signal.signal(signal.SIGINT, signal.default_int_handler)
    if failures:
        for failure in failures:
            say(f"FAIL: {failure}")
        return 1
    # Only this run's files: --out may hold PNGs from earlier runs.
    shots = list(dict.fromkeys(out_dir / shot_name(path) for path in args.shot))
    for shot in shots:
        say(f"saved {shot}")
    if args.self_test:
        check_png(out_dir / "builds.png")
        say("self-test: PASS (PNG written, core stopped cleanly, data directory removed)")
    # A capture run never publishes: the repository is public, so a person opens each PNG
    # first and then publishes on purpose.
    files = " ".join(str(shot) for shot in shots)
    say(f"open each PNG, then publish with: python3 scripts/evidence/publisher.py <pr> {files}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
