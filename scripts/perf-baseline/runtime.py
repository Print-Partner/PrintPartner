"""Starts pp-server against a disposable data directory, logs in through its
one-use bootstrap link, and serves loopback Moonraker simulators. Nothing here
contacts a real printer: every printer the harness creates points at a
simulator on 127.0.0.1."""

import http.client
import json
import os
import secrets
import shutil
import signal
import subprocess
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MARKER = ".pp-rust-owned.json"
SEED_MANIFEST = "SYNTHETIC-DATASET.json"
STATUS_FIXTURE = ROOT / "fixtures/drivers/moonraker/moonraker-success.json"
PRINTER_PREFIX = "perf-sim-"


class SimulatedPrinter:
    """One Moonraker status endpoint on loopback. `finish()` turns the print
    from printing to complete, the printer event the Today measure waits on."""

    def __init__(self):
        body = json.loads(STATUS_FIXTURE.read_text())
        self.printing = json.dumps(body).encode()
        body["result"]["status"]["print_stats"]["state"] = "complete"
        body["result"]["status"]["virtual_sdcard"]["progress"] = 1.0
        self.complete = json.dumps(body).encode()
        self.body = self.printing
        self.requests = 0
        printer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 (http.server API)
                printer.requests += 1
                if self.path.split("?")[0] == "/printer/objects/query":
                    payload, status = printer.body, 200
                else:
                    payload, status = b'{"error":"not found"}', 404
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def finish(self):
        self.body = self.complete

    def reset(self):
        self.body = self.printing

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Session:
    """Keep-alive HTTP/1.1 to one origin with the session cookie, as a
    browser tab would hold it. Every call returns (status, headers, body, ms)."""

    def __init__(self, origin, cookie=None):
        self.origin = origin
        parsed = urllib.parse.urlsplit(origin)
        self.host, self.port = parsed.hostname, parsed.port
        self.cookie = cookie
        self.conn = None
        # Requests sent again after the server closed a keep-alive connection.
        self.retries = 0

    def _connection(self):
        if self.conn is None:
            self.conn = http.client.HTTPConnection(self.host, self.port, timeout=120)
        return self.conn

    def request(self, method, path, body=None, headers=None):
        sent = {"Host": f"{self.host}:{self.port}", "Origin": self.origin}
        if self.cookie:
            sent["Cookie"] = self.cookie
        payload = None
        if body is not None:
            if isinstance(body, (bytes, bytearray)):
                payload = bytes(body)
            else:
                payload = json.dumps(body).encode()
                sent["Content-Type"] = "application/json"
        sent.update(headers or {})
        for attempt in (1, 2):
            conn = self._connection()
            started = time.perf_counter()
            try:
                conn.request(method, path, body=payload, headers=sent)
                reply = conn.getresponse()
                data = reply.read()
            except (http.client.HTTPException, ConnectionError):
                # A server may close an idle keep-alive connection; reconnect once.
                self.close()
                if attempt == 2:
                    raise
                self.retries += 1
                continue
            elapsed = (time.perf_counter() - started) * 1000
            if reply.getheader("Connection", "").lower() == "close":
                self.close()
            return reply.status, {k.lower(): v for k, v in reply.getheaders()}, data, elapsed
        raise AssertionError("unreachable")

    def raw_cookies(self, method, path, body=None):
        """Status and the name=value part of every Set-Cookie header."""
        sent = {"Host": f"{self.host}:{self.port}", "Origin": self.origin}
        if self.cookie:
            sent["Cookie"] = self.cookie
        payload = None
        if body is not None:
            payload = json.dumps(body).encode()
            sent["Content-Type"] = "application/json"
        conn = self._connection()
        conn.request(method, path, body=payload, headers=sent)
        reply = conn.getresponse()
        reply.read()
        cookies = [value.split(";", 1)[0] for value in reply.msg.get_all("Set-Cookie") or []]
        self.close()
        return reply.status, cookies

    def json(self, method, path, body=None, headers=None, expect=(200,)):
        status, _headers, data, elapsed = self.request(method, path, body, headers)
        if status not in expect:
            raise RuntimeError(f"{method} {path} answered {status}: {data[:400]!r}")
        return (json.loads(data) if data else None), elapsed

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None


def copy_dataset(seed, target, node):
    """Copies a synthetic installation with the repository's copy script, which
    rebases each Source onto the copy. Refuses anything without the synthetic
    manifest, so no customer directory is copied."""
    if not (Path(seed) / SEED_MANIFEST).is_file():
        raise SystemExit(f"{seed} is not a synthetic dataset (no {SEED_MANIFEST})")
    subprocess.run(
        [node, "--conditions=development", "--import", "tsx",
         "scripts/synthetic-dataset/copy.mts", str(seed), str(target)],
        cwd=ROOT / "web", check=True, stdout=subprocess.DEVNULL)
    # Lease files a stopped seeding run left behind belong to that run.
    for name in ("printpartner.lock", ".desktop-owner.json"):
        (Path(target) / name).unlink(missing_ok=True)
    lease = Path(target) / ".desktop-lease"
    if lease.is_dir():
        shutil.rmtree(lease)
    elif lease.exists():
        lease.unlink()


def mark_rust_owned(data_dir):
    """The ownership marker a Rust-only start needs on a non-empty directory,
    written the way scripts/verify-rust-gateway writes it for a seeded copy."""
    created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    fd = os.open(Path(data_dir) / MARKER, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(json.dumps({"version": 1, "owner": "rust", "created_at": created},
                                separators=(",", ":")) + "\n")


class Server:
    """One pp-server process. `runtime` is "node" (PP_DESKTOP_REBUILD unset:
    the gateway supervises the Node child) or "rust" (PP_DESKTOP_REBUILD=1:
    Rust only, no Node child)."""

    def __init__(self, binary, runtime, data_dir, scratch, node, commit):
        self.binary, self.runtime, self.data_dir = binary, runtime, Path(data_dir)
        self.scratch, self.node, self.commit = Path(scratch), node, commit
        self.process = None
        self.origin = None
        self.spawned_at = None
        self.ready_ms = None

    def start(self, timeout=120):
        credential = self.scratch / "credential.json"
        credential.unlink(missing_ok=True)
        env = dict(os.environ)
        env.pop("PP_DESKTOP_REBUILD", None)
        if self.runtime == "rust":
            env["PP_DESKTOP_REBUILD"] = "1"
        self.stderr = (self.scratch / "server.stderr").open("ab")
        self.spawned_at = time.perf_counter()
        self.process = subprocess.Popen(
            [str(self.binary), "--data", str(self.data_dir), "--web", str(ROOT / "web"),
             "--node", self.node, "--commit", self.commit, "--test-credential-file", str(credential)],
            cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=self.stderr, start_new_session=True)
        try:
            line = _read_line(self.process.stdout, timeout)
            self.ready_ms = (time.perf_counter() - self.spawned_at) * 1000
            if not line:
                raise RuntimeError(f"{self.runtime}: pp-server printed no origin; see {self.scratch / 'server.stderr'}")
            self.origin = line.strip()
            if urllib.parse.urlsplit(self.origin).hostname != "127.0.0.1":
                raise RuntimeError(f"{self.runtime}: origin {self.origin} is not loopback")
            threading.Thread(target=_drain, args=(self.process.stdout,), daemon=True).start()
            self.bootstrap = json.loads(credential.read_text())["bootstrap_url"]
        except BaseException:
            self.stop()
            raise
        finally:
            credential.unlink(missing_ok=True)
        return self

    def login(self):
        """Follows the one-use bootstrap link and returns a Session with the
        desktop cookie. The Rust routes also need a Rust session, so on the
        Rust-only runtime the single account is registered on the disposable
        data, as docs/development/agent-workflow.md describes; its random
        password is never printed or saved."""
        session = Session(self.origin)
        path = self.bootstrap[len(self.origin):] if self.bootstrap.startswith(self.origin) else None
        if path is None:
            raise RuntimeError("bootstrap link is not on the server's origin")
        status, cookies = session.raw_cookies("GET", path)
        if status != 303 or not cookies:
            raise RuntimeError(f"bootstrap login answered {status}")
        session.cookie = "; ".join(cookies)
        if self.runtime == "rust":
            body = {"email": "perf@example.invalid", "password": secrets.token_urlsafe(24)}
            status, cookies = session.raw_cookies("POST", "/auth/register", body)
            if status not in (200, 201) or not cookies:
                raise RuntimeError(f"registering the disposable account answered {status}")
            session.cookie = "; ".join([session.cookie, *cookies])
        return session

    def stop(self, timeout=30):
        if self.process is None:
            return
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=10)
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.stderr.close()
        self.process = None


def _read_line(stream, timeout):
    result = []
    reader = threading.Thread(target=lambda: result.append(stream.readline().decode()), daemon=True)
    reader.start()
    reader.join(timeout)
    return result[0] if result else ""


def _drain(stream):
    for _ in iter(lambda: stream.read(65536), b""):
        pass


def seed_printers(session, printers):
    """Creates one Moonraker integration per simulator and checks the list."""
    for index, printer in enumerate(printers, 1):
        session.json("POST", "/api/v1/integrations", {
            "name": f"{PRINTER_PREFIX}{index}", "type": "moonraker",
            "config": {"base_url": printer.url, "api_key": "perf-simulator"}}, expect=(201,))
    listed, _ = session.json("GET", "/api/v1/integrations")
    seeded = [item for item in listed["integrations"]
              if item["type"] == "moonraker" and item["name"].startswith(PRINTER_PREFIX)]
    if len(seeded) != len(printers):
        raise RuntimeError(f"the app lists {len(seeded)} simulated printers, expected {len(printers)}")
    return [item["id"] for item in seeded]
