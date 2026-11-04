#!/usr/bin/env python3
"""Build, drive and stop an isolated Rust gateway; stdout is the proof.

Modes: `verify` (default) runs the full proof. `launch` builds, starts pp-server
on loopback, prints `PP_SERVER_URL=...` and stays up until SIGTERM, SIGINT or SIGHUP,
then stops it cleanly. `launch --binary` skips the build and starts a pp-server built
against the existing rust/bundle-manifest.json.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


STOP_SIGNALS = frozenset({signal.SIGTERM, signal.SIGINT, signal.SIGHUP})


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Stop(Exception):
    """Raised from a SIGTERM/SIGINT/SIGHUP handler so launch mode can clean up."""


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


SHAPE_FIXTURE = Path(__file__).with_name("health-runtime-shape.json")
SCALAR_TYPES = {str: "string", bool: "boolean", int: "number", float: "number", type(None): "null"}


def type_name(value):
    if isinstance(value, dict):
        return "object"
    if isinstance(value, list):
        return "array"
    return SCALAR_TYPES[type(value)]


def shape_diff(expected, actual, path):
    if isinstance(expected, list):
        if not isinstance(actual, list):
            return [f"{path}: expected array, got {type_name(actual)}"]
        return [problem for index, item in enumerate(actual)
                for problem in shape_diff(expected[0], item, f"{path}[{index}]")]
    if not isinstance(expected, dict):
        return [] if type_name(actual) == expected else [f"{path}: expected {expected}, got {type_name(actual)}"]
    if not isinstance(actual, dict):
        return [f"{path}: expected object, got {type_name(actual)}"]
    problems = [f"{path}.{key}: missing key" for key in sorted(expected.keys() - actual.keys())]
    problems += [f"{path}.{key}: unexpected key" for key in sorted(actual.keys() - expected.keys())]
    for key in sorted(expected.keys() & actual.keys()):
        problems += shape_diff(expected[key], actual[key], f"{path}.{key}")
    return problems


def check_shape(name, actual, shapes):
    problems = shape_diff(shapes[name], actual, name)
    check(not problems, f"{name} shape drifted from {SHAPE_FIXTURE.name}: " + "; ".join(problems))
    print(f"SHAPE: {name} keys and types match {SHAPE_FIXTURE.name}", flush=True)


def default_repo_root():
    helper_dir = Path(__file__).resolve().parent
    try:
        top = subprocess.check_output(
            ["git", "-C", str(helper_dir), "rev-parse", "--show-toplevel"],
            text=True,
            timeout=15,
        ).strip()
    except subprocess.CalledProcessError as error:
        raise SystemExit(
            "git rev-parse --show-toplevel failed from helper checkout; pass --repo explicitly"
        ) from error
    return Path(top)


def ignore_signals():
    # Once cleanup starts, further signals must not interrupt it.
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
    try:
        for signum in STOP_SIGNALS:
            signal.signal(signum, signal.SIG_IGN)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


def begin_stop(name):
    ignore_signals()
    return Stop(name)


# While held, a signal is recorded and raised only after release_signals(), so
# pp-server is never stopped before it is recorded or before its origin is printed.
# Holding also means no stop can escape between statements of a cleanup path; work that
# must react promptly polls `pending` or registers `on_stop`, called from the handler.
held = {"active": False, "pending": None, "on_stop": None}


def stop_on_signal(signum, _frame):
    name = signal.Signals(signum).name
    if held["active"]:
        held["pending"] = name
        if held.get("on_stop") is not None:
            held["on_stop"]()
        return
    raise begin_stop(name)


def signal_server(process):
    # Called from the stop handler: the proof's next request then fails fast. Once reaped,
    # the pid is never signalled again, so it cannot hit a reused pid.
    if process.returncode is None:
        try:
            os.kill(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


def install_stop_handlers():
    # Handler installation is atomic; preserve unrelated masks inherited from the caller.
    signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
    for signum in STOP_SIGNALS:
        signal.signal(signum, stop_on_signal)
    signal.pthread_sigmask(signal.SIG_UNBLOCK, STOP_SIGNALS)


def release_signals():
    held["active"] = False
    if held["pending"] is not None:
        raise begin_stop(held["pending"])


def stop_group(process, grace=5):
    # Build workers are already spawned, so blocking cannot leak a mask into them.
    # Deliver any additional stop only after the whole group has been cleaned up.
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
    try:
        _stop_group(process, grace)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


def _stop_group(process, grace):
    # The build runs in its own session, so its pid is also its process group id. Terminate the
    # whole group (npm, node, rustc, ...), escalate to SIGKILL after `grace` seconds, and return
    # only once no live member is left, so a replacement launch cannot overlap leftover writers.
    group = process.pid

    def signal_group(signum):
        try:
            os.killpg(group, signum)
        except ProcessLookupError:
            pass

    def group_gone():
        process.poll()  # reaps the leader if it has exited; its descendants keep the group alive
        try:
            os.killpg(group, 0)
        except ProcessLookupError:
            return True
        # An orphan can remain as a zombie until PID 1 reaps it. Such a member cannot
        # execute or write files; killpg(0) alone cannot distinguish it from a live worker.
        try:
            members = subprocess.check_output(["ps", "-axo", "pgid=,stat="], text=True, timeout=1)
            for line in members.splitlines():
                fields = line.split()
                if len(fields) == 2 and int(fields[0]) == group and not fields[1].startswith("Z"):
                    return False
        except (OSError, subprocess.SubprocessError, ValueError):
            # Failed inspection cannot establish that workers exited. Keep waiting and
            # escalate normally rather than abandoning cleanup before SIGKILL.
            return False
        return True

    def wait_gone(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if group_gone():
                return True
            time.sleep(0.05)
        return group_gone()

    signal_group(signal.SIGTERM)
    if not wait_gone(grace):
        signal_group(signal.SIGKILL)
        check(wait_gone(5), f"build process group {group} survived SIGKILL")
    if process.stdout is not None:
        process.stdout.close()


# Written before a normal close so the watcher does not SIGKILL a pgid that has
# already been reaped and reused. A bare EOF is still parent death.
GUARD_RELEASE = b"\x00"
GUARD_SOURCE = ("import os, signal, sys\n"
                "fd, group = int(sys.argv[1]), int(sys.argv[2])\n"
                "if group == 0:\n"
                "    number = bytearray()\n"
                "    while True:\n"
                "        chunk = os.read(fd, 1)\n"
                "        if not chunk:\n"
                "            raise SystemExit(0)\n"
                "        if chunk == b'\\n':\n"
                "            break\n"
                "        number.extend(chunk)\n"
                "    group = int(number)\n"
                "released = False\n"
                "while True:\n"
                "    chunk = os.read(fd, 1)\n"
                "    if chunk == b'\\x00':\n"
                "        released = True\n"
                "        break\n"
                "    if not chunk:\n"
                "        break\n"
                "if released:\n"
                "    raise SystemExit(0)\n"
                "try:\n"
                "    os.killpg(group, signal.SIGKILL)\n"
                "except ProcessLookupError:\n"
                "    pass\n")


def guard_group(group, read_fd, done_fd):
    return subprocess.Popen([sys.executable, "-I", "-c", GUARD_SOURCE, str(read_fd), str(group)],
                            pass_fds=(read_fd,), start_new_session=True,
                            stdin=subprocess.DEVNULL, stdout=done_fd, stderr=subprocess.DEVNULL)


def guarded_popen(args, **kwargs):
    read_fd, write_fd = os.pipe()
    done_fd, done_write = os.pipe()
    watcher = None

    def publish_group_before_exec():
        os.write(write_fd, f"{os.getpgrp()}\n".encode())
        os.close(write_fd)

    try:
        watcher = guard_group(0, read_fd, done_write)
        process = subprocess.Popen(args, start_new_session=True, preexec_fn=publish_group_before_exec,
                                   pass_fds=(write_fd,), **kwargs)
    except BaseException:
        os.close(write_fd)
        os.close(done_fd)
        if watcher is not None:
            watcher.wait()
        raise
    finally:
        os.close(read_fd)
        os.close(done_write)
    process.guard = (done_fd, write_fd, watcher)
    return process


def release_guard(guard):
    done_fd, write_fd, *watchers = guard
    try:
        try:
            pending = GUARD_RELEASE
            while pending:
                try:
                    written = os.write(write_fd, pending)
                except InterruptedError:
                    continue
                pending = pending[written:]
        finally:
            os.close(write_fd)
        while os.read(done_fd, 1):
            pass
    finally:
        os.close(done_fd)
        for watcher in watchers:
            watcher.wait()


def run(root, *args, env=None, stdout=None, timeout=600):
    print("BUILD:", " ".join(args), flush=True)
    # Signals stay held for the whole build: a stop is only recorded, so it can never escape
    # between the leader exiting and its group being reaped. Waiting in short slices still
    # cancels promptly. Keeping the OS mask unchanged lets build workers handle SIGTERM normally.
    was_held = held["active"]
    held["active"] = True
    process = None
    try:
        process = guarded_popen(args, cwd=root, env=env, stdout=stdout, text=True)
        deadline = time.monotonic() + timeout
        while True:
            try:
                output, _ = process.communicate(timeout=0.1)
                break
            except subprocess.TimeoutExpired:
                if held["pending"] is not None:
                    output = None
                    break
                if time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(args, timeout) from None
    finally:
        # Even successful/nonzero leaders can leave background workers behind. Finish each
        # group before another build starts; a stop recorded meanwhile is raised afterwards.
        try:
            if process is not None:
                stop_group(process)
        finally:
            if process is not None:
                release_guard(process.guard)
            held["active"] = was_held
    if not was_held:
        release_signals()
    if process.returncode:
        if output:
            print(output, end="", flush=True)
        raise subprocess.CalledProcessError(process.returncode, args, output=output)
    return subprocess.CompletedProcess(args, process.returncode, output, None)


def checkout(root):
    if not (root / "rust").is_dir():
        raise SystemExit("rust/ not found; check out the desktop chain")
    check((root / "rust/crates/pp-server/Cargo.toml").is_file(), "Rust pp-server checkout required")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True, timeout=15).strip()
    print(f"Rust verification checkout: {root}\nCommit: {commit}", flush=True)
    node_path = shutil.which("node")
    check(node_path is not None, "node not found; install the Node version documented in rust/README.md")
    return commit, str(Path(node_path).resolve())


def build(root, node, commit):
    run(root, "npm", "--prefix", "web", "ci")
    run(root, "npm", "--prefix", "web", "run", "build", env=dict(os.environ, VITE_PRINT_PARTNER_DESKTOP="1"))
    run(root, sys.executable, "rust/scripts/build-desktop-manifest.py", "--web", "web", "--node", node,
        "--output", "rust/bundle-manifest.json", "--commit", commit)
    build = run(root, "cargo", "build", "--manifest-path", "rust/Cargo.toml", "--locked", "-p", "pp-server",
                "--message-format=json", stdout=subprocess.PIPE)
    binary = None
    for line in build.stdout.splitlines():
        artifact = json.loads(line)
        if artifact["reason"] == "compiler-message":
            print(artifact["message"].get("rendered", ""), end="", flush=True)
        if (artifact["reason"] == "compiler-artifact" and artifact["target"]["name"] == "pp-server"
                and artifact.get("executable")):
            binary = Path(artifact["executable"])
    check(binary is not None, "Cargo did not report a pp-server executable")
    print(f"EXECUTABLE: {binary}", flush=True)
    return binary


def start_server(binary, root, node, commit, fixture, errors, credentials=None, isolated=False):
    args = [str(binary), "--data", str(fixture / "data"), "--web", str(root / "web"),
            "--node", node, "--commit", commit]
    if credentials is not None:
        args += ["--test-credential-file", str(credentials)]
    # A new session keeps terminal Ctrl-C from reaching pp-server before its signal handlers exist.
    spawn = guarded_popen if isolated else subprocess.Popen
    return spawn(args, cwd=root, stdout=subprocess.PIPE, stderr=errors, text=True)


def read_origin(process, errors):
    # Read only available bytes, so a silent server or an unterminated origin line
    # cannot postpone a held stop request until the full startup timeout.
    deadline = time.monotonic() + 60
    data = bytearray()
    while b"\n" not in data:
        remaining = deadline - time.monotonic()
        check(remaining > 0, "Rust startup timed out")
        # With a stop pending, drain what is already queued first: an origin printed before
        # the stop means pp-server is ready, and its shutdown must still be checked.
        stopping = held["active"] and held["pending"] is not None
        if not select.select([process.stdout], [], [], 0 if stopping else min(0.1, remaining))[0]:
            if stopping:
                release_signals()
            continue
        chunk = os.read(process.stdout.fileno(), 4096)
        if not chunk:
            break
        data.extend(chunk)
    # A stop arriving with the origin line stays pending: the caller has a running server
    # to stop and must still check its shutdown, so it honours the stop after this returns.
    origin = bytes(data).split(b"\n", 1)[0].decode().strip()
    if not origin:
        # A stop that arrived before EOF is honoured here, so the caller still runs shutdown checks.
        if held["pending"] is not None:
            release_signals()
        process.wait(timeout=5)
        errors.seek(0)
        raise RuntimeError(f"Rust startup failed (exit={process.returncode}): {errors.read().strip()}")
    parsed = urllib.parse.urlsplit(origin)
    check(parsed.scheme == "http" and parsed.hostname == "127.0.0.1" and parsed.port,
          "Rust did not emit a loopback origin")
    return origin


def terminate(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=25)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
        raise RuntimeError("Rust shutdown timed out")


def verify_shutdown(process, fixture, runtime_dir, require_lock=True):
    check(process.returncode == 0, f"Rust exited {process.returncode}")
    check(not (fixture / "data/.desktop-lease").exists(), "Compatibility reservation survived")
    check(runtime_dir is None or not runtime_dir.exists(), "Private runtime directory survived")
    try:
        with (fixture / "data/printpartner.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except FileNotFoundError:
        if require_lock:
            raise RuntimeError("Rust shutdown verification failed: data lock file missing") from None


def verify(root):
    commit, node = checkout(root)
    binary = build(root, node, commit)
    with tempfile.TemporaryDirectory(prefix="pp-verify-rust-") as temporary:
        fixture = Path(temporary)
        credentials = fixture / "credentials.json"
        with (fixture / "server.stderr").open("w+") as errors:
            # Signals stay held until cleanup ends, so a stop can never skip it. A stop during
            # the proof stops pp-server at once and is raised after cleanup.
            held["active"] = True
            process = start_server(binary, root, node, commit, fixture, errors, credentials)
            runtime_dir = None
            held["on_stop"] = lambda: signal_server(process)
            try:
                if held["pending"] is not None:
                    signal_server(process)
                origin = read_origin(process, errors)
                print(f"START: pid={process.pid} origin={origin}", flush=True)
                runtime_dir = Path(json.loads(credentials.read_text())["runtime_dir"])
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
                cookie = None

                def request(path, expected=200, method="GET", body=None, headers=None, secret=False):
                    values = {"Origin": origin, **(headers or {})}
                    if cookie:
                        values["Cookie"] = cookie
                    if body is not None:
                        body = json.dumps(body).encode()
                        values["Content-Type"] = "application/json"
                    req = urllib.request.Request(origin + path, data=body, method=method, headers=values)
                    try:
                        response = opener.open(req, timeout=30)
                    except urllib.error.HTTPError as error:
                        response = error
                    with response:
                        data = response.read()
                        if "json" in response.headers.get("Content-Type", ""):
                            data = json.loads(data)
                        shown = json.dumps(data) if not isinstance(data, bytes) else f"{len(data)} bytes"
                        print(f"HTTP {method} {path if not secret else '/__desktop/bootstrap [redacted]'}"
                              f" -> {response.status}: {shown}", flush=True)
                        check(response.status == expected, "Unexpected HTTP status")
                        return data, response.headers

                shapes = json.loads(SHAPE_FIXTURE.read_text())
                unauthorized, _ = request("/health", expected=401)
                check_shape("GET /health 401", unauthorized, shapes)
                check(credentials.stat().st_mode & 0o777 == 0o600, "Credential file must be private")
                launch = json.loads(credentials.read_text())["bootstrap_url"]
                check(launch.startswith(origin + "/__desktop/bootstrap?"), "Unexpected bootstrap origin")
                _, headers = request(launch[len(origin):], expected=303, secret=True)
                cookie = headers["Set-Cookie"].split(";", 1)[0]
                credentials.unlink()
                health, _ = request("/health")
                check_shape("GET /health", health, shapes)
                check(health["ok"] is True, "Health not OK")
                state, _ = request("/__runtime")
                check_shape("GET /__runtime", state, shapes)
                check(state["compat"]["state"] == "ready", "Compatibility server not ready")
                html, _ = request("/builds", headers={"Accept": "text/html"})
                check(b'<div id="root">' in html, "React document missing")
                source, _ = request("/sources", method="POST",
                                    body={"name": "Rust verification source", "source_kind": "local"})
                saved, _ = request(f'/sources/{source["id"]}')
                check(saved["id"] == source["id"] and saved["name"] == "Rust verification source",
                      "Source did not persist")
            finally:
                cleanup_stop = None
                try:
                    ignore_signals()
                except Stop as stop:
                    # The handler installed ignored handlers before raising. Finish cleanup
                    # before propagating cancellation, preserving any earlier proof failure.
                    cleanup_stop = stop
                held["on_stop"] = None
                if cleanup_stop is None and held["pending"] is not None:
                    cleanup_stop = Stop(held["pending"])
                held["active"] = False
                failed = sys.exc_info()[0] is not None
                if failed and cleanup_stop is not None:
                    print(f"STOP: {cleanup_stop} received; proof cancelled", flush=True)
                try:
                    terminate(process)
                    credentials.unlink(missing_ok=True)
                except Exception as error:
                    if not failed:
                        raise
                    print(f"CLEANUP ERROR (original failure preserved): {error}", flush=True)
                finally:
                    process.stdout.close()
                    errors.seek(0)
                    print("SERVER STDERR:\n" + errors.read(), flush=True)
            if cleanup_stop is not None and not failed:
                raise cleanup_stop
            verify_shutdown(process, fixture, runtime_dir)
            print("STOP: exit=0; compatibility reservation removed; private sockets removed; data lock reacquired", flush=True)
    print("PASS: Rust build, gateway endpoints, persisted Source and shutdown", flush=True)


def publish_credential(source, target):
    # Write a private temporary inode, then hard-link it to the target: link() refuses to clobber
    # a file that appeared after launch started, and the target only ever names a complete file.
    # The returned handle stays open to pin this inode, so its number cannot be reused.
    fd, temporary = tempfile.mkstemp(prefix=".pp-credential-", dir=target.parent)
    handle = os.fdopen(fd, "wb")
    linked = False
    try:
        try:
            handle.write(source.read_bytes())
            handle.flush()
            os.fsync(handle.fileno())
            os.link(temporary, target)
            linked = True
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass  # A consumer already removed the private name.
    except BaseException:
        try:
            if linked:
                unpublish_credential(target, handle)
        finally:
            handle.close()
        raise
    return handle


def unpublish_credential(target, handle):
    # Move the entry into a fresh private directory first: its name is unpredictable and
    # empty, so the rename cannot overwrite anything and only this launch touches it.
    if handle is None:
        return
    try:
        try:
            vault = Path(tempfile.mkdtemp(prefix=".pp-credential-", dir=target.parent))
        except FileNotFoundError:
            # The caller removed the parent after consuming the credential, so nothing is left to remove.
            return
        entry = vault / "entry"
        try:
            os.rename(target, entry)
        except FileNotFoundError:
            vault.rmdir()
            return
        except BaseException:
            vault.rmdir()
            raise
        status = entry.lstat()
        owned = os.fstat(handle.fileno())
        if (status.st_dev, status.st_ino) == (owned.st_dev, owned.st_ino):
            entry.unlink()
            vault.rmdir()
            return
        # Foreign entries are never deleted. Only a regular file is restored, and never over
        # whatever now occupies the path; anything else stays in the vault with a warning.
        if not stat.S_ISREG(status.st_mode):
            print(f"WARNING: foreign non-file entry kept at {entry}", flush=True)
            return
        try:
            os.link(entry, target)
        except OSError as error:
            print(f"WARNING: foreign file kept at {entry} ({error})", flush=True)
            return
        entry.unlink()
        vault.rmdir()
    finally:
        handle.close()


SEED_MANIFEST = "SYNTHETIC-DATASET.json"
RUST_OWNERSHIP_MARKER = ".pp-rust-owned.json"


def check_seed(seed):
    # Only a synthetic dataset may seed a launch, so no customer data directory is copied.
    if not (seed / SEED_MANIFEST).is_file():
        raise SystemExit(f"--seed must be a synthetic dataset holding {SEED_MANIFEST}: {seed}")
    # A copied symlink would let the launch write through it, outside the disposable copy.
    for directory, names, files in os.walk(seed):
        for name in names + files:
            if (Path(directory) / name).is_symlink():
                raise SystemExit(f"--seed must not contain symlinks: {Path(directory) / name}")


def copy_seed(seed, data):
    # The copy lives in the launch's private temporary directory and is removed with it. A
    # Rust-only start refuses a non-empty unmarked directory, so that start gets the marker.
    shutil.copytree(seed, data, symlinks=True)
    if os.environ.get("PP_DESKTOP_REBUILD") in ("1", "true"):
        created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        marker = data / RUST_OWNERSHIP_MARKER
        if marker.is_symlink() or marker.exists():
            marker.unlink()
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w") as handle:
            handle.write(json.dumps({"version": 1, "owner": "rust", "created_at": created}) + "\n")


def launch(root, credentials, binary=None, seed=None):
    # Fast refusal before a long build; publish_credential() is the authoritative check.
    if credentials is not None and (credentials.exists() or credentials.is_symlink()):
        raise SystemExit(f"credential file already exists: {credentials}")
    try:
        install_stop_handlers()
        commit, node = checkout(root)
        if binary is None:
            binary = build(root, node, commit)
        else:
            # pp-server verifies its bundle against the manifest it was built with, so a
            # prebuilt server needs that manifest's commit, not the current checkout's.
            commit = json.loads((root / "rust/bundle-manifest.json").read_text())["commit"]
        serve(binary, root, node, commit, credentials, seed=seed)
    except Stop as stop:
        print(f"STOP: {stop} received before pp-server started", flush=True)


def serve(binary, root, node, commit, credentials, seed=None):
    with tempfile.TemporaryDirectory(prefix="pp-launch-rust-") as temporary:
        fixture = Path(temporary)
        if seed is not None:
            copy_seed(seed, fixture / "data")
        runtime_dir = None
        published = None
        origin = None
        with (fixture / "server.stderr").open("w+") as errors:
            # pp-server writes its bootstrap into the private fixture; launch publishes it below.
            internal = fixture / "bootstrap.json"
            held["active"] = True
            process = start_server(binary, root, node, commit, fixture, errors, internal, isolated=True)
            try:
                origin = read_origin(process, errors)
                runtime_dir = Path(json.loads(internal.read_text())["runtime_dir"])
                if held["pending"] is not None:
                    # Stopped once ready: skip publishing and stop below with full shutdown checks.
                    release_signals()
                if credentials is not None:
                    published = publish_credential(internal, credentials)
                port = urllib.parse.urlsplit(origin).port
                print(f"PP_SERVER_URL={origin} PP_SERVER_PORT={port} PP_SERVER_PID={process.pid}", flush=True)
                # Signals stay held while serving, so a stop is only recorded and cannot escape
                # between statements of the cleanup below; poll for it instead.
                while held["pending"] is None:
                    try:
                        process.wait(timeout=0.1)
                        break
                    except subprocess.TimeoutExpired:
                        pass
                if held["pending"] is not None:
                    release_signals()
                if process.returncode != 0:
                    raise RuntimeError(f"pp-server exited without a stop request (exit={process.returncode})")
                print("STOP: pp-server exited cleanly; checking shutdown", flush=True)
            except Stop as stop:
                print(f"STOP: {stop} received; stopping pp-server", flush=True)
            finally:
                try:
                    ignore_signals()
                except Stop:
                    # A stop at cleanup entry must still remove credentials and check shutdown.
                    pass
                held["active"] = False
                try:
                    # Startup may create resources before it announces an origin. Capture
                    # their test receipt before termination removes it.
                    try:
                        if runtime_dir is None and internal.exists():
                            runtime_dir = Path(json.loads(internal.read_text())["runtime_dir"])
                    finally:
                        try:
                            terminate(process)
                        finally:
                            if getattr(process, "guard", None) is not None:
                                release_guard(process.guard)
                finally:
                    try:
                        if credentials is not None:
                            unpublish_credential(credentials, published)
                    finally:
                        internal.unlink(missing_ok=True)
                    process.stdout.close()
                    errors.seek(0)
                    print("SERVER STDERR:\n" + errors.read(), flush=True)
            if origin is None:
                # A missing lock is valid before startup acquired it; any resources that
                # were created and the child exit status must still pass shutdown checks.
                verify_shutdown(process, fixture, runtime_dir, require_lock=False)
                print("STOP: exit=0; pp-server stopped before readiness", flush=True)
            else:
                verify_shutdown(process, fixture, runtime_dir)
                print("STOP: exit=0; compatibility reservation removed; private sockets removed; data lock reacquired", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", nargs="?", choices=["verify", "launch"], default="verify")
    parser.add_argument("--repo", type=Path,
                        help="checkout root (default: git top-level for this helper)")
    parser.add_argument("--credential-file", type=Path,
                        help="launch only: pp-server writes the one-use bootstrap credential here")
    parser.add_argument("--binary", type=Path,
                        help="launch only: start this prebuilt pp-server instead of building one")
    parser.add_argument("--seed", type=Path,
                        help=f"launch only: copy this synthetic dataset (it must hold {SEED_MANIFEST}) "
                             "into the temporary data directory before start")
    args = parser.parse_args()
    if args.credential_file is not None and args.mode != "launch":
        parser.error("--credential-file applies only to launch")
    if args.binary is not None and args.mode != "launch":
        parser.error("--binary applies only to launch")
    if args.binary is not None and not (args.binary.is_file() and os.access(args.binary, os.X_OK)):
        parser.error(f"--binary is not an executable file: {args.binary}")
    if args.seed is not None and args.mode != "launch":
        parser.error("--seed applies only to launch")
    if args.seed is not None:
        check_seed(args.seed)
    root = args.repo.resolve() if args.repo is not None else default_repo_root()
    if args.mode == "launch":
        # Resolve the directory only: a symlink at the final component is an existing entry to refuse.
        credentials = args.credential_file.parent.resolve() / args.credential_file.name if args.credential_file else None
        launch(root, credentials, args.binary.resolve() if args.binary else None,
               args.seed.resolve() if args.seed else None)
    else:
        # Cancelled proofs remain nonzero, after their owned processes have been cleaned up.
        install_stop_handlers()
        verify(root)


if __name__ == "__main__":
    main()
