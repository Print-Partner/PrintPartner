#!/usr/bin/env python3
"""Proof for `verify.py launch`: start, reach /health, SIGTERM, clean exit."""
import contextlib
import importlib.util
import inspect
import io
import json
import os
from pathlib import Path
import queue
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
from types import SimpleNamespace
import time
import unittest
from unittest import mock
import urllib.error
import urllib.parse
import urllib.request

HELPER = Path(__file__).resolve().with_name("verify.py")
# git rev-parse (15s), four build steps (600s each), server startup (60s), plus margin.
BUILD_TIMEOUT = 15 + 4 * 600 + 60 + 60


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def process_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                            capture_output=True, text=True, timeout=5)
    return result.returncode == 0 and any(not state.startswith("Z")
                                         for state in result.stdout.split())


def wait_until_gone(pid, timeout):
    deadline = time.monotonic() + timeout
    while process_alive(pid):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)
    return True


def write_executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def make_fixture_repo(root):
    # Just enough checkout for `verify.py launch` to pass its preflight; the build tools are stubs.
    (root / "rust/crates/pp-server").mkdir(parents=True)
    (root / "rust/crates/pp-server/Cargo.toml").write_text('[package]\nname = "pp-server"\n')
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "-c", "user.name=test", "-c", "user.email=test@example.invalid",
                    "commit", "-q", "--allow-empty", "-m", "fixture"], cwd=root, check=True)
    return root


def next_line(lines, prefix, timeout):
    deadline = time.monotonic() + timeout
    while True:
        try:
            line = lines.get(timeout=max(deadline - time.monotonic(), 0))
        except queue.Empty:
            raise AssertionError(f"timed out waiting for {prefix}") from None
        if line is None:
            raise AssertionError(f"launch exited before printing {prefix}")
        if line.startswith(prefix):
            return line


class LaunchTest(unittest.TestCase):
    def test_launch_serves_health_and_stops_on_sigterm(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with tempfile.TemporaryDirectory(prefix="pp-launch-test-") as temporary:
            credentials = Path(temporary) / "credentials.json"
            process = subprocess.Popen(
                [sys.executable, str(HELPER), "launch", "--credential-file", str(credentials)],
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            lines = queue.Queue()
            transcript = []

            def pump():
                for line in process.stdout:
                    transcript.append(line)
                    lines.put(line)
                lines.put(None)

            pump_thread = threading.Thread(target=pump, daemon=True)
            pump_thread.start()
            child_pid = None
            try:
                fields = dict(token.split("=", 1) for token in
                              next_line(lines, "PP_SERVER_URL=", BUILD_TIMEOUT).split())
                origin = fields["PP_SERVER_URL"]
                parsed = urllib.parse.urlsplit(origin)
                self.assertEqual((parsed.scheme, parsed.hostname), ("http", "127.0.0.1"))
                self.assertEqual(int(fields["PP_SERVER_PORT"]), parsed.port)
                child_pid = int(fields["PP_SERVER_PID"])
                print(f"launch printed {origin} pid={child_pid}")

                deadline = time.monotonic() + 60
                while not credentials.exists() and time.monotonic() < deadline:
                    time.sleep(0.1)
                bootstrap = json.loads(credentials.read_text())["bootstrap_url"]
                self.assertTrue(bootstrap.startswith(origin + "/__desktop/bootstrap?"))
                try:
                    opener.open(urllib.request.Request(origin + bootstrap[len(origin):],
                                                       headers={"Origin": origin}), timeout=30)
                except urllib.error.HTTPError as error:
                    self.assertEqual(error.code, 303)
                    cookie = error.headers["Set-Cookie"].split(";", 1)[0]
                    error.close()
                credentials.unlink()

                with self.assertRaises(urllib.error.HTTPError) as denied:
                    opener.open(urllib.request.Request(origin + "/health", headers={"Origin": origin}), timeout=30)
                self.assertEqual(denied.exception.code, 401)
                denied.exception.close()
                with opener.open(urllib.request.Request(origin + "/health",
                                                        headers={"Origin": origin, "Cookie": cookie}),
                                 timeout=30) as response:
                    self.assertEqual(response.status, 200)
                    self.assertIs(json.loads(response.read())["ok"], True)
                print("GET /health -> 200 {\"ok\": true}")

                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=60), 0)
                pump_thread.join(timeout=30)
                output = "".join(transcript)
                self.assertIn("STOP: SIGTERM received", output)
                self.assertIn("STOP: exit=0;", output)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child_pid, 0)
                print("SIGTERM -> exit 0; pp-server pid gone")
            finally:
                # Failure cleanup: stop the launcher gracefully so it terminates pp-server, then reap any survivor.
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=60)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)
                if child_pid is not None:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(child_pid, signal.SIGKILL)
                pump_thread.join(timeout=30)
                process.stdout.close()


def load_verify():
    spec = importlib.util.spec_from_file_location("verify", HELPER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CredentialFileTest(unittest.TestCase):
    def test_temp_unlink_failure_rolls_back_only_owned_public_link(self):
        for replace in (False, True):
            with self.subTest(foreign_replacement=replace):
                verify = load_verify()
                with tempfile.TemporaryDirectory(prefix="pp-credential-test-") as temporary:
                    source = Path(temporary) / "bootstrap.json"
                    target = Path(temporary) / "public.json"
                    source.write_text('{}')
                    unlink = os.unlink
                    def fail_private_unlink(path, *args, **kwargs):
                        if Path(path).name.startswith('.pp-credential-'):
                            if replace:
                                unlink(target)
                                target.write_text('foreign')
                            raise PermissionError('private unlink failed')
                        return unlink(path, *args, **kwargs)
                    with mock.patch.object(verify.os, 'unlink', side_effect=fail_private_unlink):
                        with self.assertRaisesRegex(PermissionError, 'private unlink failed'):
                            verify.publish_credential(source, target)
                    if replace:
                        self.assertEqual(target.read_text(), 'foreign')
                    else:
                        self.assertFalse(target.exists())

    def test_existing_symlink_is_refused_not_followed(self):
        verify = load_verify()
        with tempfile.TemporaryDirectory(prefix="pp-credential-test-") as temporary:
            temp = Path(temporary)
            source = temp / "bootstrap.json"
            source.write_text("{}")
            elsewhere = temp / "elsewhere.json"
            target = temp / "credentials.json"
            os.symlink(elsewhere, target)  # dangling: the destination does not exist
            with self.assertRaises(FileExistsError):
                verify.publish_credential(source, target)
            self.assertFalse(elsewhere.exists())
            # Exercise main's path handling too: resolving the final component would bypass refusal.
            result = subprocess.run([sys.executable, str(HELPER), "launch", "--repo", str(temp),
                                     "--credential-file", str(target)], capture_output=True, text=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("credential file already exists", result.stderr)
            self.assertTrue(target.is_symlink())
            self.assertFalse(elsewhere.exists())

    def test_removed_parent_directory_is_a_noop_on_shutdown(self):
        verify = load_verify()
        with tempfile.TemporaryDirectory(prefix="pp-credential-test-") as temporary:
            source = Path(temporary) / "bootstrap.json"
            source.write_text("{}")
            parent = Path(temporary) / "consumer"
            parent.mkdir()
            handle = verify.publish_credential(source, parent / "credentials.json")
            shutil.rmtree(parent)  # the consumer removed its directory after reading the credential
            verify.unpublish_credential(parent / "credentials.json", handle)
            self.assertFalse(parent.exists())


class StartupStopTest(unittest.TestCase):
    def test_early_stop_checks_exit_and_existing_startup_resources(self):
        for failure in (None, 'exit', 'owner', 'runtime'):
            with self.subTest(failure=failure):
                verify = load_verify()
                process = SimpleNamespace(pid=123, returncode=3 if failure == 'exit' else 0,
                                          stdout=io.StringIO())
                with tempfile.TemporaryDirectory(prefix='pp-early-stop-test-') as temporary:
                    runtime = Path(temporary) / 'runtime'
                    runtime.mkdir()
                    marker = None
                    def stop_before_origin(_process, errors):
                        nonlocal marker
                        marker = Path(errors.name).parent / 'data/.desktop-lease'
                        marker.parent.mkdir()
                        marker.touch()
                        (marker.parent.parent / "bootstrap.json").write_text(json.dumps({"runtime_dir": str(runtime)}))
                        (marker.parent.parent / "bootstrap.json").chmod(0o600)
                        (marker.parent / 'printpartner.lock').touch()
                        raise verify.Stop('SIGTERM')
                    def terminate(_process):
                        if failure != 'owner':
                            marker.unlink()
                        if failure != 'runtime':
                            runtime.rmdir()
                    output = io.StringIO()
                    with mock.patch.object(verify, 'start_server', return_value=process), \
                            mock.patch.object(verify, 'read_origin', side_effect=stop_before_origin), \
                            mock.patch.object(verify, 'terminate', side_effect=terminate), \
                            mock.patch.object(verify, 'ignore_signals'), contextlib.redirect_stdout(output):
                        if failure:
                            with self.assertRaises(RuntimeError):
                                verify.serve(Path('server'), Path('repo'), 'node', 'commit', None)
                            self.assertNotIn('STOP: exit=0', output.getvalue())
                        else:
                            verify.serve(Path('server'), Path('repo'), 'node', 'commit', None)
                            self.assertIn('pp-server stopped before readiness', output.getvalue())
                    self.assertTrue(process.stdout.closed)

    def test_stop_pending_when_startup_hits_eof_before_origin_runs_shutdown_checks(self):
        verify = load_verify()
        # A child that already exited without printing an origin: stdout is at EOF.
        child = subprocess.Popen([sys.executable, "-c", "pass"], stdout=subprocess.PIPE, text=True)
        child.wait()

        def start_leaving_owner_marker(_binary, _root, _node, _commit, fixture, _errors, *_args, **_kwargs):
            runtime = fixture / "runtime"
            runtime.mkdir()
            (fixture / "data").mkdir()
            (fixture / "data/printpartner.lock").touch()
            (fixture / "data/.desktop-lease").touch()
            (fixture / "bootstrap.json").write_text(json.dumps({"runtime_dir": str(runtime)}))
            (fixture / "bootstrap.json").chmod(0o600)
            return child

        verify.held["pending"] = "SIGTERM"
        with mock.patch.object(verify, "start_server", side_effect=start_leaving_owner_marker), \
                mock.patch.object(verify, "terminate"), mock.patch.object(verify, "ignore_signals"), \
                contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(RuntimeError) as failure:
                verify.serve(Path("server"), Path("repo"), "node", "commit", None)
        # The shutdown checks ran and found the marker the exited server left behind.
        self.assertIn("Compatibility reservation survived", str(failure.exception))

    def test_stop_during_silent_or_partial_startup_is_prompt_and_clean(self):
        with tempfile.TemporaryDirectory(prefix="pp-startup-stop-test-") as temporary:
            temp = Path(temporary)
            root = make_fixture_repo(temp / "repo")
            binary = temp / "server"
            pidfile = temp / "server.pid"
            driver = (f"import sys; from pathlib import Path; sys.path.insert(0, {str(HELPER.parent)!r}); "
                      f"import verify; verify.build = lambda *args: Path({str(binary)!r}); verify.main()")
            for partial in (False, True):
                for stop_signal in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
                    with self.subTest(partial_line=partial, signal=stop_signal.name):
                        pidfile.unlink(missing_ok=True)
                        write_executable(binary, f"#!{sys.executable}\n" +
                                         "import os, signal, sys, time\nfrom pathlib import Path\n" +
                                         "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n" +
                                         ("print('http://127.0.0.1:', end='', flush=True)\n" if partial else "") +
                                         f"Path({str(pidfile)!r}).write_text(str(os.getpid()))\n" +
                                         "time.sleep(600)\n")
                        log = temp / "launch.log"
                        with log.open("w") as stdout:
                            process = subprocess.Popen([sys.executable, "-c", driver, "launch", "--repo", str(root)],
                                                       stdout=stdout, stderr=subprocess.STDOUT, text=True)
                        child_pid = None
                        try:
                            deadline = time.monotonic() + 15
                            while not pidfile.exists() and time.monotonic() < deadline:
                                time.sleep(0.05)
                            self.assertTrue(pidfile.exists(), log.read_text())
                            child_pid = int(pidfile.read_text())
                            process.send_signal(stop_signal)
                            self.assertEqual(process.wait(timeout=5), 0, log.read_text())
                            self.assertIn("pp-server stopped before readiness", log.read_text())
                            self.assertFalse(process_alive(child_pid))
                        finally:
                            if process.poll() is None:
                                process.kill()
                                process.wait(timeout=10)
                            if child_pid is not None:
                                with contextlib.suppress(ProcessLookupError):
                                    os.kill(child_pid, signal.SIGKILL)


class PrebuiltBinaryTest(unittest.TestCase):
    def test_binary_skips_build_and_uses_manifest_commit(self):
        with tempfile.TemporaryDirectory(prefix="pp-prebuilt-test-") as temporary:
            temp = Path(temporary)
            root = make_fixture_repo(temp / "repo")
            (root / "rust/bundle-manifest.json").write_text(json.dumps({"commit": "f" * 40}))
            binary = temp / "server"
            argv_file = temp / "argv.json"
            # Exits before printing an origin, so launch stops at the startup check.
            write_executable(binary, f"#!{sys.executable}\nimport json, sys\nfrom pathlib import Path\n"
                                     f"Path({str(argv_file)!r}).write_text(json.dumps(sys.argv[1:]))\n")
            built = temp / "built"
            driver = (f"import sys; from pathlib import Path; sys.path.insert(0, {str(HELPER.parent)!r}); "
                      f"import verify; verify.build = lambda *args: Path({str(built)!r}).touch(); verify.main()")
            result = subprocess.run([sys.executable, "-c", driver, "launch", "--repo", str(root),
                                     "--binary", str(binary)], capture_output=True, text=True, timeout=60)
            output = result.stdout + result.stderr
            self.assertFalse(built.exists(), output)
            self.assertIn("Rust startup failed", output)
            argv = json.loads(argv_file.read_text())
            self.assertEqual(argv[argv.index("--commit") + 1], "f" * 40)

    def test_binary_is_launch_only(self):
        result = subprocess.run([sys.executable, str(HELPER), "verify", "--binary", sys.executable],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--binary applies only to launch", result.stderr)

    def test_binary_must_be_executable(self):
        with tempfile.NamedTemporaryFile(prefix="pp-not-executable-") as plain:
            result = subprocess.run([sys.executable, str(HELPER), "launch", "--binary", plain.name],
                                    capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--binary is not an executable file", result.stderr)


class SeedTest(unittest.TestCase):
    def launch_with_seed(self, temp, seed, env):
        root = make_fixture_repo(temp / "repo")
        (root / "rust/bundle-manifest.json").write_text(json.dumps({"commit": "f" * 40}))
        binary = temp / "server"
        seen = temp / "seen.json"
        # Records the data directory it was given, then exits before printing an origin.
        write_executable(binary, f"#!{sys.executable}\nimport json, os, sys\nfrom pathlib import Path\n"
                                 "data = Path(sys.argv[sys.argv.index('--data') + 1])\n"
                                 "marker = data / '.pp-rust-owned.json'\n"
                                 f"Path({str(seen)!r}).write_text(json.dumps({{'files': sorted(os.listdir(data)), "
                                 "'marker': json.loads(marker.read_text()) if marker.exists() else None, "
                                 "'raw': marker.read_text() if marker.exists() else None}))\n")
        result = subprocess.run([sys.executable, str(HELPER), "launch", "--repo", str(root), "--binary",
                                 str(binary), "--seed", str(seed)],
                                capture_output=True, text=True, timeout=60, env={**os.environ, **env})
        return result, json.loads(seen.read_text()) if seen.exists() else None

    def make_seed(self, temp):
        seed = temp / "seed"
        seed.mkdir()
        (seed / "SYNTHETIC-DATASET.json").write_text("{}")
        (seed / "print-partner.db").write_text("db")
        return seed

    def test_seed_is_copied_and_marked_only_for_a_rust_only_start(self):
        for flag, marked in (("1", True), ("true", True), ("0", False)):
            with self.subTest(PP_DESKTOP_REBUILD=flag), \
                    tempfile.TemporaryDirectory(prefix="pp-seed-test-") as temporary:
                temp = Path(temporary)
                seed = self.make_seed(temp)
                result, seen = self.launch_with_seed(temp, seed, {"PP_DESKTOP_REBUILD": flag})
                self.assertIsNotNone(seen, result.stdout + result.stderr)
                self.assertIn("print-partner.db", seen["files"])
                self.assertEqual(seen["marker"] is not None, marked)
                if marked:
                    self.assertRegex(seen["raw"], r'^\{"version":1,"owner":"rust","created_at":"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ"\}\n$')
                    self.assertEqual(sorted(seen["marker"]), ["created_at", "owner", "version"])
                    self.assertEqual((seen["marker"]["version"], seen["marker"]["owner"]), (1, "rust"))
                # The seed itself is never changed.
                self.assertEqual(sorted(p.name for p in seed.iterdir()),
                                 ["SYNTHETIC-DATASET.json", "print-partner.db"])

    def test_rust_only_seed_without_a_prebuilt_binary_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="pp-seed-test-") as temporary:
            temp = Path(temporary)
            seed = self.make_seed(temp)
            root = make_fixture_repo(temp / "repo")
            result = subprocess.run([sys.executable, str(HELPER), "launch", "--repo", str(root), "--seed", str(seed)],
                                    capture_output=True, text=True, timeout=60,
                                    env={**os.environ, "PP_DESKTOP_REBUILD": "1"})
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("needs --binary built with --features desktop_rebuild", result.stderr)

    def test_seed_without_synthetic_manifest_is_refused_before_start(self):
        with tempfile.TemporaryDirectory(prefix="pp-seed-test-") as temporary:
            temp = Path(temporary)
            seed = temp / "seed"
            seed.mkdir()
            (seed / "print-partner.db").write_text("db")
            result, seen = self.launch_with_seed(temp, seed, {})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--seed must be a synthetic dataset", result.stderr)
            self.assertIsNone(seen)

    def test_seed_with_a_symlink_is_refused_and_its_target_untouched(self):
        with tempfile.TemporaryDirectory(prefix="pp-seed-test-") as temporary:
            temp = Path(temporary)
            seed = self.make_seed(temp)
            outside = temp / "outside.txt"
            outside.write_text("keep")
            (seed / ".pp-rust-owned.json").symlink_to(outside)
            result, seen = self.launch_with_seed(temp, seed, {"PP_DESKTOP_REBUILD": "1"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--seed must not contain symlinks", result.stderr)
            self.assertIsNone(seen)
            self.assertEqual(outside.read_text(), "keep")

    def test_seed_is_launch_only(self):
        result = subprocess.run([sys.executable, str(HELPER), "verify", "--seed", "."],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--seed applies only to launch", result.stderr)


class StopAfterOriginTest(unittest.TestCase):
    def test_stop_after_origin_runs_shutdown_checks(self):
        self.assertIn("data lock reacquired", self.launch_stopped_after_origin(exit_status=0))

    def test_stop_after_origin_reports_a_bad_shutdown(self):
        self.assertIn("Rust exited 3", self.launch_stopped_after_origin(exit_status=3, expect_failure=True))

    def test_stop_pending_with_origin_queued_runs_shutdown_checks(self):
        self.assertIn("data lock reacquired", self.launch_stopped_after_origin(exit_status=0, queued=True))

    def test_stop_pending_with_origin_queued_reports_a_bad_shutdown(self):
        self.assertIn("Rust exited 3", self.launch_stopped_after_origin(exit_status=3, expect_failure=True,
                                                                        queued=True))

    def launch_stopped_after_origin(self, exit_status, expect_failure=False, queued=False):
        # The stop arrives as soon as the chunk holding the origin line has been read, before
        # read_origin() returns, or (queued) once the origin is printed but before read_origin()
        # reads it. Either way pp-server was ready, so the real shutdown checks must run.
        with tempfile.TemporaryDirectory(prefix="pp-origin-stop-test-") as temporary:
            temp = Path(temporary)
            root = make_fixture_repo(temp / "repo")
            binary = temp / "server"
            pidfile = temp / "server.pid"
            write_executable(binary, f"#!{sys.executable}\n" + f"""
import json, os, signal, sys, time
from pathlib import Path
data = Path(sys.argv[sys.argv.index('--data') + 1])
runtime = data / 'runtime'
runtime.mkdir(parents=True)
(data / 'printpartner.lock').touch()
marker = data / '.desktop-lease'
marker.touch()
Path(sys.argv[sys.argv.index("--test-credential-file") + 1]).write_text(json.dumps({{"runtime_dir": str(runtime)}}))
Path(sys.argv[sys.argv.index("--test-credential-file") + 1]).chmod(0o600)
def stop(*_):
    if {exit_status} == 0:
        marker.unlink()
        runtime.rmdir()
    sys.exit({exit_status})
signal.signal(signal.SIGTERM, stop)
Path({str(pidfile)!r}).write_text(str(os.getpid()))
print('http://127.0.0.1:43210', flush=True)
time.sleep(600)
""")
            driver = f"""
import os, select, signal, sys
from pathlib import Path
sys.path.insert(0, {str(HELPER.parent)!r})
import verify
verify.build = lambda *args: Path({str(binary)!r})
real_read, real_read_origin = os.read, verify.read_origin
def read_origin(process, errors):
    if {queued!r}:
        select.select([process.stdout], [], [], 10)
        os.kill(os.getpid(), signal.SIGTERM)
        return real_read_origin(process, errors)
    def read(fd, size):
        chunk = real_read(fd, size)
        if fd == process.stdout.fileno() and b'\\n' in chunk:
            os.kill(os.getpid(), signal.SIGTERM)
        return chunk
    os.read = read
    try:
        return real_read_origin(process, errors)
    finally:
        os.read = real_read
verify.read_origin = read_origin
verify.main()
"""
            child_pid = None
            try:
                result = subprocess.run([sys.executable, "-c", driver, "launch", "--repo", str(root)],
                                        capture_output=True, text=True, timeout=60)
                child_pid = int(pidfile.read_text())
                output = result.stdout + result.stderr
                self.assertNotIn("stopped before readiness", output)
                self.assertNotIn("PP_SERVER_URL=", output)
                self.assertIn("STOP: SIGTERM received; stopping pp-server", output)
                if expect_failure:
                    self.assertNotEqual(result.returncode, 0, output)
                else:
                    self.assertEqual(result.returncode, 0, output)
                self.assertFalse(process_alive(child_pid))
                when = "pending with origin queued" if queued else "after origin read"
                print(f"stop {when}, server exit {exit_status} -> launch exit {result.returncode}")
                return output
            finally:
                if child_pid is None and pidfile.exists():
                    child_pid = int(pidfile.read_text())
                if child_pid is not None:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(child_pid, signal.SIGKILL)


class StopAtEveryLineTest(unittest.TestCase):
    # Deliver a real SIGTERM as each line of serve()/verify() is reached, including the first
    # lines of their cleanup blocks, against a fake pp-server that also exits on its own.
    SERVER = """
import json, os, signal, sys, time
from pathlib import Path
data = Path(sys.argv[sys.argv.index('--data') + 1])
runtime = data / 'runtime'
runtime.mkdir(parents=True)
(data / 'printpartner.lock').touch()
marker = data / '.desktop-lease'
marker.touch()
if '--test-credential-file' in sys.argv:
    Path(sys.argv[sys.argv.index('--test-credential-file') + 1]).write_text(json.dumps({"runtime_dir": str(runtime)}))
    Path(sys.argv[sys.argv.index('--test-credential-file') + 1]).chmod(0o600)
def stop(*_):
    marker.unlink()
    runtime.rmdir()
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
Path(%r).write_text(str(os.getpid()))
print('http://127.0.0.1:43210', flush=True)
time.sleep(0.3)
stop()
"""

    def each_line(self, function, call, check):
        verify = load_verify()
        previous = {signum: signal.getsignal(signum) for signum in verify.STOP_SIGNALS}
        lines, first = inspect.getsourcelines(getattr(verify, function))
        code = getattr(verify, function).__code__
        with tempfile.TemporaryDirectory(prefix="pp-every-line-test-") as temporary:
            temp = Path(temporary)
            pidfile = temp / "server.pid"
            binary = temp / "server"
            write_executable(binary, f"#!{sys.executable}\n" + self.SERVER % str(pidfile))
            for target in range(first + 1, first + len(lines)):
                with self.subTest(function=function, line=target):
                    pidfile.unlink(missing_ok=True)
                    (temp / "credential.json").unlink(missing_ok=True)  # keep a failure from cascading
                    verify.held.update(active=False, pending=None, on_stop=None)
                    verify.install_stop_handlers()
                    fired = []

                    def trace(frame, event, _arg):
                        if frame.f_code is not code:
                            return None
                        if event == "line" and frame.f_lineno == target and not fired:
                            fired.append(target)
                            os.kill(os.getpid(), signal.SIGTERM)
                        return trace

                    raised = None
                    try:
                        with contextlib.redirect_stdout(io.StringIO()) as output:
                            sys.settrace(trace)
                            try:
                                call(verify, binary, temp)
                            except (verify.Stop, RuntimeError) as error:
                                raised = error
                            finally:
                                sys.settrace(None)
                        started = pidfile.exists()
                        if started:
                            self.assertTrue(wait_until_gone(int(pidfile.read_text()), 5), output.getvalue())
                        check(verify, temp, started, raised, output.getvalue())
                    finally:
                        for signum, handler in previous.items():
                            signal.signal(signum, handler)  # begin_stop() ignored them in this process
                        if pidfile.exists():
                            with contextlib.suppress(ProcessLookupError):
                                os.kill(int(pidfile.read_text()), signal.SIGKILL)

    def test_stop_at_any_line_of_serve_keeps_cleanup_and_shutdown_checks(self):
        def call(verify, binary, temp):
            verify.serve(binary, temp, "node", "commit", temp / "credential.json")

        def check(verify, temp, started, raised, output):
            self.assertFalse((temp / "credential.json").exists(), output)
            if isinstance(raised, verify.Stop):
                # Escaping serve() means launch reports "before pp-server started".
                self.assertFalse(started, output)
            elif isinstance(raised, RuntimeError):
                # Before the fixture installs its TERM handler, cancellation is an
                # unclean child exit and must now be reported as a failure.
                self.assertFalse(started, output)
                self.assertEqual(str(raised), f"Rust exited {-signal.SIGTERM}")
                self.assertNotIn("STOP: exit=0", output)
            else:
                self.assertIsNone(raised, output)
                if "pp-server stopped before readiness" not in output:
                    self.assertIn("data lock reacquired", output)

        self.each_line("serve", call, check)

    def test_stop_at_any_line_of_verify_reaps_server(self):
        def call(verify, binary, temp):
            verify.checkout = lambda *_: ("commit", "node")
            verify.build = lambda *_: binary
            verify.read_origin = mock.Mock(side_effect=RuntimeError("injected startup failure"))
            verify.verify(temp)

        def check(verify, temp, started, raised, output):
            self.assertIsNotNone(raised, output)
            if isinstance(raised, verify.Stop):
                self.assertFalse(started, output)

        self.each_line("verify", call, check)


class VerifyServerStopTest(unittest.TestCase):
    def test_cancelled_successful_verify_cleans_up_then_exits_nonzero(self):
        verify = load_verify()
        process = SimpleNamespace(pid=123, stdout=io.StringIO())
        def origin(_process, errors):
            fixture = Path(errors.name).parent
            marker = fixture / "data/.desktop-lease"
            marker.parent.mkdir()
            marker.touch()
            credential = fixture / "credentials.json"
            credential.write_text(json.dumps({"bootstrap_url": "http://127.0.0.1:12345/__desktop/bootstrap?token", "runtime_dir": str(fixture / "runtime")}))
            credential.chmod(0o600)
            return "http://127.0.0.1:12345"
        responses = []
        for status, body, headers in (
                (401, {}, {}), (303, {}, {"Set-Cookie": "session=test; HttpOnly"}),
                (200, {"ok": True}, {}), (200, {"compat": {"state": "ready"}}, {}),
                (200, b'<div id="root">', {"Content-Type": "text/html"}),
                (200, {"id": 1}, {}), (200, {"id": 1, "name": "Rust verification source"}, {})):
            response = mock.MagicMock()
            response.__enter__.return_value = response
            response.status = status
            response.headers = {"Content-Type": "application/json", **headers}
            response.read.return_value = body if isinstance(body, bytes) else json.dumps(body).encode()
            responses.append(response)
        opener = mock.Mock()
        opener.open.side_effect = responses
        output = io.StringIO()
        with mock.patch.object(verify, "checkout", return_value=("commit", "node")), \
                mock.patch.object(verify, "build", return_value=Path("pp-server")), \
                mock.patch.object(verify, "start_server", return_value=process), \
                mock.patch.object(verify, "read_origin", side_effect=origin), \
                mock.patch.object(verify, "check_shape"), \
                mock.patch.object(verify.urllib.request, "build_opener", return_value=opener), \
                mock.patch.object(verify, "ignore_signals", side_effect=verify.Stop("SIGTERM")), \
                mock.patch.object(verify, "terminate") as terminate, \
                mock.patch.object(verify, "verify_shutdown") as shutdown, \
                contextlib.redirect_stdout(output):
            with self.assertRaisesRegex(verify.Stop, "SIGTERM"):
                verify.verify(Path("repo"))
        terminate.assert_called_once_with(process)
        shutdown.assert_not_called()
        self.assertNotIn("PASS:", output.getvalue())
        self.assertTrue(process.stdout.closed)

    def test_stop_at_server_acquisition_and_during_cleanup_reaps_server(self):
        with tempfile.TemporaryDirectory(prefix="pp-verify-stop-test-") as temporary:
            temp = Path(temporary)
            pidfile = temp / "server.pid"
            binary = temp / "server"
            write_executable(binary, f"#!{sys.executable}\n" +
                             "import os, signal, sys, time\nfrom pathlib import Path\n" +
                             "def stop(*args):\n    time.sleep(0.3)\n    sys.exit(0)\n" +
                             "signal.signal(signal.SIGTERM, stop)\n" +
                             f"Path({str(pidfile)!r}).write_text(str(os.getpid()))\n" +
                             "time.sleep(600)\n")
            for phase in ("acquisition", "cleanup", "ignore-installation", "before-block"):
                with self.subTest(phase=phase):
                    pidfile.unlink(missing_ok=True)
                    driver = f"""
import os, signal, subprocess, sys, time
from pathlib import Path
sys.path.insert(0, {str(HELPER.parent)!r})
import verify
verify.checkout = lambda *_: ('commit', 'node')
verify.build = lambda *_: Path({str(binary)!r})
def start(*args, **kwargs):
    process = subprocess.Popen([{str(binary)!r}], stdout=subprocess.PIPE, text=True)
    while not Path({str(pidfile)!r}).exists():
        time.sleep(0.01)
    if {phase!r} == 'acquisition':
        os.kill(os.getpid(), signal.SIGTERM)
    return process
verify.start_server = start
def unread(*args):
    raise RuntimeError('injected startup failure')
verify.read_origin = unread
terminate = verify.terminate
def cleanup(process):
    if {phase!r} == 'cleanup':
        os.kill(os.getpid(), signal.SIGTERM)
    terminate(process)
verify.terminate = cleanup
verify.install_stop_handlers()
original_signal = signal.signal
injected = False
def set_handler(signum, handler):
    global injected
    result = original_signal(signum, handler)
    if {phase!r} == 'ignore-installation' and handler == signal.SIG_IGN and not injected:
        injected = True
        other = next(value for value in verify.STOP_SIGNALS if value != signum)
        os.kill(os.getpid(), other)
    return result
signal.signal = set_handler
original_mask = signal.pthread_sigmask
def set_mask(how, values):
    global injected
    if {phase!r} == 'before-block' and how == signal.SIG_BLOCK and not injected:
        injected = True
        os.kill(os.getpid(), signal.SIGTERM)
    return original_mask(how, values)
signal.pthread_sigmask = set_mask
verify.verify(Path({str(temp)!r}))
"""
                    child_pid = None
                    try:
                        result = subprocess.run([sys.executable, "-c", driver], capture_output=True,
                                                text=True, timeout=10)
                        child_pid = int(pidfile.read_text())
                        self.assertNotEqual(result.returncode, 0)
                        # Signals are held from acquisition through cleanup, so the stop is recorded
                        # (and pp-server signalled) rather than raised over the injected failure.
                        self.assertIn("injected startup failure", result.stderr)
                        if phase == "acquisition":
                            self.assertIn("STOP: SIGTERM received; proof cancelled", result.stdout)
                        self.assertNotIn("CLEANUP ERROR", result.stdout)
                        self.assertFalse(process_alive(child_pid), result.stdout + result.stderr)
                    finally:
                        if child_pid is None and pidfile.exists():
                            child_pid = int(pidfile.read_text())
                        if child_pid is not None:
                            with contextlib.suppress(ProcessLookupError):
                                os.kill(child_pid, signal.SIGKILL)


class ServerExitTest(unittest.TestCase):
    def test_clean_server_exit_runs_shutdown_checks(self):
        self.assert_server_exit(0)

    def test_failed_server_exit_remains_an_error(self):
        self.assert_server_exit(1)

    def test_stop_at_launch_cleanup_entry_removes_credential_and_checks_shutdown(self):
        verify = load_verify()
        process = SimpleNamespace(pid=123, returncode=0, stdout=io.StringIO(), wait=lambda timeout=None: 0)
        with tempfile.TemporaryDirectory(prefix="pp-launch-cleanup-test-") as temporary:
            credentials = Path(temporary) / "credential.json"
            def origin(_process, errors):
                fixture = Path(errors.name).parent
                marker = fixture / "data/.desktop-lease"
                marker.parent.mkdir()
                marker.touch()
                (fixture / "bootstrap.json").write_text(json.dumps({"runtime_dir": temporary}))
                (fixture / "bootstrap.json").chmod(0o600)
                return "http://127.0.0.1:12345"
            with mock.patch.object(verify, "start_server", return_value=process), \
                    mock.patch.object(verify, "read_origin", side_effect=origin), \
                    mock.patch.object(verify, "terminate") as terminate, \
                    mock.patch.object(verify, "verify_shutdown") as shutdown, \
                    mock.patch.object(verify, "ignore_signals", side_effect=verify.Stop("SIGTERM")), \
                    contextlib.redirect_stdout(io.StringIO()):
                verify.serve(Path("pp-server"), Path("repo"), "node", "commit", credentials)
            terminate.assert_called_once_with(process)
            shutdown.assert_called_once()
            self.assertFalse(credentials.exists())
            self.assertTrue(process.stdout.closed)

    def assert_server_exit(self, status):
        verify = load_verify()
        process = SimpleNamespace(pid=123, returncode=status, stdout=io.StringIO(), wait=lambda timeout=None: status)
        runtime = Path("/tmp/pp-test-runtime")

        def origin(_process, errors):
            marker = Path(errors.name).parent / "data/.desktop-lease"
            marker.parent.mkdir()
            marker.touch()
            (marker.parent.parent / "bootstrap.json").write_text(json.dumps({"runtime_dir": str(runtime)}))
            (marker.parent.parent / "bootstrap.json").chmod(0o600)
            return "http://127.0.0.1:12345"

        output = io.StringIO()
        with mock.patch.object(verify, "start_server", return_value=process), \
                mock.patch.object(verify, "read_origin", side_effect=origin), \
                mock.patch.object(verify, "terminate") as terminate, \
                mock.patch.object(verify, "verify_shutdown") as shutdown, \
                mock.patch.object(verify, "ignore_signals"), contextlib.redirect_stdout(output):
            if status:
                with self.assertRaisesRegex(RuntimeError, "exit=1"):
                    verify.serve(Path("pp-server"), Path("repo"), "node", "commit", None)
                shutdown.assert_not_called()
            else:
                verify.serve(Path("pp-server"), Path("repo"), "node", "commit", None)
                shutdown.assert_called_once_with(process, mock.ANY, runtime)
                self.assertIn("STOP: exit=0;", output.getvalue())
            terminate.assert_called_once_with(process)
        self.assertTrue(process.stdout.closed)


class BuildGroupTest(unittest.TestCase):
    def test_zombie_only_group_is_already_stopped(self):
        verify = load_verify()
        # A separate guardian retains an exited session leader without reaping it.
        # This reproduces a zombie-only process group independently of PID 1 behavior.
        driver = """import os, sys
pid = os.fork()
if pid == 0:
    os.setsid()
    os._exit(0)
os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT)
print(pid, flush=True)
sys.stdin.readline()
os.waitpid(pid, 0)
"""
        guardian = subprocess.Popen([sys.executable, "-c", driver], stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            pid = int(guardian.stdout.readline())
            os.killpg(pid, 0)  # The group exists even though its sole member has exited.
            self.assertFalse(process_alive(pid))
            process = SimpleNamespace(pid=pid, poll=lambda: None, stdout=None)
            started = time.monotonic()
            verify.stop_group(process, grace=0.1)
            self.assertLess(time.monotonic() - started, 2)
            os.killpg(pid, 0)  # Cleanup succeeds while the guardian still retains the zombie.
        finally:
            guardian.communicate("\n", timeout=10)
        self.assertEqual(guardian.returncode, 0)

    def test_failed_process_inspection_still_kills_live_worker(self):
        verify = load_verify()
        failures = (FileNotFoundError("ps"), subprocess.CalledProcessError(1, "ps"),
                    subprocess.TimeoutExpired("ps", 1))
        for failure in failures:
            with self.subTest(error=type(failure).__name__):
                worker = "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready', flush=True); time.sleep(600)"
                process = subprocess.Popen([sys.executable, "-c", worker], start_new_session=True,
                                           stdout=subprocess.PIPE, text=True)
                try:
                    self.assertEqual(process.stdout.readline().strip(), "ready")
                    with mock.patch.object(verify.subprocess, "check_output", side_effect=failure):
                        verify.stop_group(process, grace=0.1)
                    self.assertEqual(process.returncode, -signal.SIGKILL)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
                    process.stdout.close()

    def test_completed_build_reaps_background_children(self):
        verify = load_verify()
        with tempfile.TemporaryDirectory(prefix="pp-build-group-test-") as temporary:
            root = Path(temporary)
            for status in (0, 1):
                with self.subTest(exit_status=status):
                    pidfile = root / "child.pid"
                    command = f'sleep 600 & echo $! > "{pidfile}"; exit {status}'
                    if status:
                        with self.assertRaises(subprocess.CalledProcessError):
                            verify.run(root, "sh", "-c", command)
                    else:
                        verify.run(root, "sh", "-c", command)
                    self.assertFalse(process_alive(int(pidfile.read_text())))

    def test_stop_at_any_line_of_run_still_reaps_group(self):
        # Deliver a real SIGTERM as each line of run() is reached, including the step from the
        # leader exiting to group cleanup. Every stop must surface, and only after the
        # background worker has been reaped.
        verify = load_verify()
        previous = {signum: signal.getsignal(signum) for signum in verify.STOP_SIGNALS}
        lines, first = inspect.getsourcelines(verify.run)
        with tempfile.TemporaryDirectory(prefix="pp-build-group-test-") as temporary:
            root = Path(temporary)
            pidfile = root / "child.pid"
            for target in range(first + 1, first + len(lines)):
                with self.subTest(line=target):
                    pidfile.unlink(missing_ok=True)
                    verify.held.update(active=False, pending=None)
                    verify.install_stop_handlers()
                    fired = []

                    def trace(frame, event, _arg):
                        if frame.f_code is not verify.run.__code__:
                            return None
                        if event == "line" and frame.f_lineno == target and not fired:
                            fired.append(target)
                            os.kill(os.getpid(), signal.SIGTERM)
                        return trace

                    try:
                        with self.assertRaisesRegex(verify.Stop, "SIGTERM"), \
                                contextlib.redirect_stdout(io.StringIO()):
                            sys.settrace(trace)
                            try:
                                verify.run(root, "sh", "-c", f'sleep 600 & echo $! > "{pidfile}"; exit 0')
                            finally:
                                sys.settrace(None)
                            if not fired:
                                raise verify.Stop("SIGTERM")  # line not executed (comment, blank, ...)
                        if pidfile.exists():
                            self.assertFalse(process_alive(int(pidfile.read_text())))
                        self.assertIs(verify.held["active"], False)
                    finally:
                        for signum, handler in previous.items():
                            signal.signal(signum, handler)  # begin_stop() ignored them in this process
                        if pidfile.exists() and process_alive(int(pidfile.read_text())):
                            os.kill(int(pidfile.read_text()), signal.SIGKILL)

    def test_repeated_sigint_cannot_interrupt_group_cleanup(self):
        with tempfile.TemporaryDirectory(prefix="pp-build-group-test-") as temporary:
            root = Path(temporary)
            pidfile = root / "child.pid"
            worker = ("import os, signal, time; from pathlib import Path; "
                      "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                      f"Path({str(pidfile)!r}).write_text(str(os.getpid())); time.sleep(600)")
            driver = (f"import sys; sys.path.insert(0, {str(HELPER.parent)!r}); import verify; "
                      f"verify.run({str(root)!r}, sys.executable, '-c', {worker!r})")
            process = subprocess.Popen([sys.executable, "-c", driver],
                                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            child_pid = None
            try:
                deadline = time.monotonic() + 15
                while not pidfile.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(pidfile.exists())
                child_pid = int(pidfile.read_text())
                process.send_signal(signal.SIGINT)
                time.sleep(0.3)
                process.send_signal(signal.SIGINT)
                output, _ = process.communicate(timeout=20)
                self.assertNotEqual(process.returncode, 0, output)
                self.assertTrue(wait_until_gone(child_pid, 5), output)
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=10)
                if child_pid is not None:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(child_pid, signal.SIGKILL)


class StopDuringBuildTest(unittest.TestCase):
    def test_stop_during_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGTERM)

    def test_sigint_during_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGINT)

    def test_sighup_during_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGHUP)

    def test_sighup_during_verify_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGHUP, mode="verify")

    def test_sigterm_during_verify_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGTERM, mode="verify")

    def test_sigint_during_verify_build_leaves_no_build_children(self):
        self.assert_stop_leaves_no_children(signal.SIGINT, mode="verify")

    def assert_stop_leaves_no_children(self, stop_signal, mode="launch"):
        with tempfile.TemporaryDirectory(prefix="pp-launch-build-test-") as temporary:
            temp = Path(temporary)
            root = make_fixture_repo(temp / "repo")
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            write_executable(bin_dir / "node", "#!/bin/sh\nexit 0\n")
            # Stands in for `npm ci`: a background grandchild in the build's process tree, then a wait.
            write_executable(bin_dir / "npm", '#!/bin/sh\nsleep 600 &\necho $! > "$PP_TEST_PIDFILE.tmp"\n'
                                              'mv "$PP_TEST_PIDFILE.tmp" "$PP_TEST_PIDFILE"\nwait\n')
            pidfile = temp / "child.pid"
            env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                       PP_TEST_PIDFILE=str(pidfile))
            # Output goes to a file, not a pipe: a surviving build child must not keep a pipe open.
            log = temp / "launch.log"
            with log.open("w") as stdout:
                process = subprocess.Popen([sys.executable, str(HELPER), mode, "--repo", str(root)],
                                           env=env, stdout=stdout, stderr=subprocess.STDOUT, text=True)
            child_pid = None
            try:
                deadline = time.monotonic() + 60
                while not pidfile.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(pidfile.exists(), "fake npm never started its child")
                child_pid = int(pidfile.read_text())
                self.assertTrue(process_alive(child_pid), "build child should be running before the stop")

                process.send_signal(stop_signal)
                exit_code = process.wait(timeout=60)
                output = log.read_text()
                self.assertIn("BUILD: npm --prefix web ci", output)
                if mode == "launch":
                    self.assertEqual(exit_code, 0, output)
                    self.assertIn(f"STOP: {stop_signal.name} received before pp-server started", output)
                else:
                    self.assertNotEqual(exit_code, 0, output)
                    self.assertIn(f"Stop: {stop_signal.name}", output)
                self.assertTrue(wait_until_gone(child_pid, 5), "build child outlived the launcher")
                print(f"{stop_signal.name} during {mode} build -> exit {exit_code}; build process tree gone")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                if child_pid is not None and process_alive(child_pid):
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(child_pid, signal.SIGKILL)


class GuardStartupTest(unittest.TestCase):
    def test_sigkill_during_watcher_setup_leaves_no_descendants(self):
        for mode in ("run", "server"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                pidfile, armed = root / "worker.pid", root / "arming"
                binary = root / "server"
                write_executable(binary, f'#!/bin/sh\nsleep 600 &\necho $! > "{pidfile}"\nwait\n')
                driver = f"""import sys, time
from pathlib import Path
sys.path.insert(0, {str(HELPER.parent)!r})
import verify
original = verify.guard_group
def delayed(*args):
    time.sleep(0.3)
    Path({str(armed)!r}).touch()
    time.sleep(2)
    return original(*args)
verify.guard_group = delayed
if {mode!r} == 'run':
    verify.run({str(root)!r}, {str(binary)!r})
else:
    verify.serve(Path({str(binary)!r}), Path({str(root)!r}), 'node', 'commit', None)
"""
                with (root / "log").open("w") as log:
                    launcher = subprocess.Popen([sys.executable, "-c", driver], stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 10
                    while not armed.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(armed.exists(), "watcher setup was not reached")
                    launcher.kill()
                    launcher.wait(timeout=5)
                    time.sleep(2.2)
                    if pidfile.exists():
                        self.assertTrue(wait_until_gone(int(pidfile.read_text()), 3),
                                        "worker escaped during watcher startup")
                finally:
                    if launcher.poll() is None:
                        launcher.kill()
                        launcher.wait(timeout=5)
                    if pidfile.exists():
                        with contextlib.suppress(ProcessLookupError):
                            os.kill(int(pidfile.read_text()), signal.SIGKILL)


class ForceKillTest(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("PP_TEST_REAL_SERVER"), "requires a measured pp-server build")
    def test_sigkill_real_launcher_stops_node(self):
        binary = Path(os.environ["PP_TEST_REAL_SERVER"]).resolve()
        root = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
        with tempfile.TemporaryDirectory(prefix="pp-real-kill-") as temporary:
            log = Path(temporary) / "launch.log"
            data_path = Path(temporary) / "data-path"
            native_node = subprocess.check_output(["node", "-p", "process.execPath"], text=True).strip()
            node_bin = Path(temporary) / "bin"
            node_bin.mkdir()
            (node_bin / "node").symlink_to(native_node)
            environment = dict(os.environ, PATH=f"{node_bin}{os.pathsep}{os.environ['PATH']}")
            driver = f"""import sys
from pathlib import Path
sys.path.insert(0, {str(HELPER.parent)!r})
import verify
verify.build = lambda *_: Path({str(binary)!r})
original = verify.start_server
def start(binary, root, node, commit, fixture, *args, **kwargs):
    Path({str(data_path)!r}).write_text(str(fixture / 'data'))
    return original(binary, root, node, commit, fixture, *args, **kwargs)
verify.start_server = start
verify.main()
"""
            with log.open("w") as output:
                launcher = subprocess.Popen([sys.executable, "-c", driver, "launch", "--repo", str(root)],
                                            stdout=output, stderr=subprocess.STDOUT, env=environment)
            server_pid = node_pid = None
            try:
                deadline = time.monotonic() + 90
                while "PP_SERVER_URL=" not in log.read_text() and launcher.poll() is None:
                    self.assertLess(time.monotonic(), deadline, log.read_text())
                    time.sleep(0.05)
                fields = dict(token.split("=", 1) for line in log.read_text().splitlines()
                              if line.startswith("PP_SERVER_URL=") for token in line.split())
                server_pid = int(fields["PP_SERVER_PID"])
                data = Path(data_path.read_text())
                self.assertEqual(data.parent.stat().st_mode & 0o777, 0o700)
                self.assertEqual((data.parent / "bootstrap.json").stat().st_mode & 0o777, 0o600)
                node_pid = json.loads((data.parent / "bootstrap.json").read_text())["child_pid"]
                parent_pid = int(subprocess.check_output(["ps", "-o", "ppid=", "-p", str(node_pid)], text=True))
                self.assertEqual(parent_pid, server_pid)
                self.assertTrue(process_alive(node_pid), "Node must be running before SIGKILL")
                launcher.kill()
                self.assertEqual(launcher.wait(timeout=5), -signal.SIGKILL)
                self.assertTrue(wait_until_gone(server_pid, 10), "server outlived launcher")
                self.assertTrue(wait_until_gone(node_pid, 10), "Node outlived launcher")
            finally:
                if launcher.poll() is None:
                    launcher.kill()
                    launcher.wait(timeout=5)
                for pid in (server_pid, node_pid):
                    if pid is not None and process_alive(pid):
                        with contextlib.suppress(ProcessLookupError):
                            os.kill(pid, signal.SIGKILL)
                if data_path.exists():
                    (Path(data_path.read_text()).parent / "bootstrap.json").unlink(missing_ok=True)

    def test_sigkill_while_serving_leaves_no_server_or_node_children(self):
        node = shlex.quote(shutil.which('node'))
        server = (f'{node} -e "setInterval(() => {{}}, 1000)" &\necho $! > "$PP_TEST_PIDFILE.tmp"\nmv "$PP_TEST_PIDFILE.tmp" "$PP_TEST_PIDFILE"\n'
                  'mkdir -p "$2"\n'
                  'touch "$2/.desktop-lease"\n'
                  'umask 077\n'
                  'printf "{\\"runtime_dir\\": \\"%s\\"}" "$PP_TEST_RUNTIME" > "${10}"\n'
                  'echo "http://127.0.0.1:49152"\nwait\n')
        self.assert_sigkill_leaves_no_children(npm_body="exit 0\n", server_body=server, wait_for="PP_SERVER_URL=")

    def test_sigkill_during_build_leaves_no_build_children(self):
        node = shlex.quote(shutil.which('node'))
        npm = f'{node} -e "setInterval(() => {{}}, 1000)" &\necho $! > "$PP_TEST_PIDFILE.tmp"\nmv "$PP_TEST_PIDFILE.tmp" "$PP_TEST_PIDFILE"\nwait\n'
        self.assert_sigkill_leaves_no_children(npm_body=npm, server_body=None, wait_for=None)

    def assert_sigkill_leaves_no_children(self, npm_body, server_body, wait_for):
        with tempfile.TemporaryDirectory(prefix="pp-launch-sigkill-test-") as temporary:
            temp = Path(temporary)
            root = make_fixture_repo(temp / "repo")
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            runtime = temp / "runtime"
            runtime.mkdir()
            server = bin_dir / "pp-server"
            write_executable(bin_dir / "node", "#!/bin/sh\nexit 0\n")
            write_executable(bin_dir / "npm", "#!/bin/sh\n" + npm_body)
            write_executable(server, "#!/bin/sh\n" + (server_body or "exit 0\n"))
            write_executable(bin_dir / "cargo", "#!/bin/sh\necho '{\"reason\":\"compiler-artifact\","
                                                 "\"target\":{\"name\":\"pp-server\"},"
                                                 f"\"executable\":\"{server}\"}}'\n")
            (root / "rust/scripts").mkdir(parents=True)
            (root / "rust/scripts/build-desktop-manifest.py").write_text("")
            pidfile = temp / "child.pid"
            env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                       PP_TEST_PIDFILE=str(pidfile), PP_TEST_RUNTIME=str(runtime))
            log = temp / "launch.log"
            with log.open("w") as stdout:
                process = subprocess.Popen([sys.executable, str(HELPER), "launch", "--repo", str(root)],
                                           env=env, stdout=stdout, stderr=subprocess.STDOUT, text=True)
            child_pid = server_pid = None
            try:
                deadline = time.monotonic() + 60
                while not pidfile.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(pidfile.exists(), "background child never started")
                child_pid = int(pidfile.read_text())
                self.assertTrue(process_alive(child_pid), "background child should be running before SIGKILL")
                if wait_for is not None:
                    deadline = time.monotonic() + BUILD_TIMEOUT
                    while wait_for not in log.read_text() and time.monotonic() < deadline:
                        time.sleep(0.05)
                    line = next(line for line in log.read_text().splitlines() if line.startswith(wait_for))
                    server_pid = int(dict(token.split("=", 1) for token in line.split())["PP_SERVER_PID"])

                process.kill()
                process.wait(timeout=10)

                self.assertTrue(wait_until_gone(child_pid, 10), "background child outlived the force-killed launcher")
                if server_pid is not None:
                    self.assertTrue(wait_until_gone(server_pid, 10), "pp-server outlived the force-killed launcher")
                print(f"SIGKILL -> launcher gone; background child pid {child_pid} gone"
                      + (f"; pp-server pid {server_pid} gone" if server_pid is not None else ""))
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                for pid in (child_pid, server_pid):
                    if pid is not None and process_alive(pid):
                        with contextlib.suppress(ProcessLookupError):
                            os.kill(pid, signal.SIGKILL)


class GuardReleaseTest(unittest.TestCase):
    def test_normal_commands_reap_their_watchers(self):
        verify = load_verify()
        for _ in range(3):
            process = verify.guarded_popen([sys.executable, "-c", "pass"])
            watcher = process.guard[2]
            self.assertEqual(process.wait(timeout=5), 0)
            verify.release_guard(process.guard)
            self.assertEqual(watcher.returncode, 0)
            with self.assertRaises(ChildProcessError):
                os.waitpid(watcher.pid, os.WNOHANG)


    def _session_leader(self):
        leader = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            start_new_session=True,
        )
        return leader

    def _arm(self, verify, group):
        read_fd, write_fd = os.pipe()
        done_fd, done_write = os.pipe()
        watcher = verify.guard_group(group, read_fd, done_write)
        os.close(read_fd)
        os.close(done_write)
        return watcher, done_fd, write_fd

    def test_release_byte_leaves_the_group_alive(self):
        verify = load_verify()
        leader = self._session_leader()
        watcher = None
        try:
            watcher, done_fd, write_fd = self._arm(verify, leader.pid)
            verify.release_guard((done_fd, write_fd))
            self.assertTrue(process_alive(leader.pid))
            self.assertEqual(watcher.wait(timeout=5), 0)
            watcher = None
        finally:
            if watcher is not None and watcher.poll() is None:
                watcher.kill()
                watcher.wait(timeout=5)
            if leader.poll() is None:
                leader.kill()
                leader.wait(timeout=5)

    def test_eof_without_release_kills_the_group(self):
        verify = load_verify()
        leader = self._session_leader()
        watcher = None
        try:
            watcher, done_fd, write_fd = self._arm(verify, leader.pid)
            os.close(write_fd)
            self.assertTrue(wait_until_gone(leader.pid, 5))
            try:
                while os.read(done_fd, 1):
                    pass
            finally:
                os.close(done_fd)
            self.assertEqual(watcher.wait(timeout=5), 0)
            watcher = None
        finally:
            if watcher is not None and watcher.poll() is None:
                watcher.kill()
                watcher.wait(timeout=5)
            if leader.poll() is None:
                leader.kill()
                leader.wait(timeout=5)


if __name__ == "__main__":
    unittest.main(verbosity=2)
