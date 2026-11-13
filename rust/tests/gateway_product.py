import hashlib
import http.client
import fcntl
import json
import os
from pathlib import Path
import select
import signal
import socket
import struct
import random
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
COMMIT = json.loads((ROOT / "rust/bundle-manifest.json").read_text())["commit"]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def without_log_rows(stderr):
    """pp-server's stderr minus its structured tracing rows: JSON objects with a target (GRE-1457)."""
    kept = []
    for line in stderr.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            row = None
        if not (isinstance(row, dict) and "target" in row):
            kept.append(line)
    return "\n".join(kept)


def main():
    fixture = Path(tempfile.mkdtemp(prefix="pp-gateway-product-"))
    credentials = fixture / "credentials.json"
    process = subprocess.Popen([
        str(ROOT / "rust/target/debug/pp-server"), "--data", str(fixture / "data"),
        "--web", str(ROOT / "web"), "--node", "/usr/bin/node", "--commit", COMMIT,
        "--test-credential-file", str(credentials),
    ], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    opener = urllib.request.build_opener(NoRedirect)
    cookie = None
    origin = None
    expected_exit = 0
    def request(path, method="GET", body=None, headers=None, allowed=(200, 201, 202, 204)):
        values = {"Origin": origin}
        if cookie:
            values["Cookie"] = cookie
        if isinstance(body, dict):
            body = json.dumps(body).encode()
            values["Content-Type"] = "application/json"
        values.update(headers or {})
        req = urllib.request.Request(origin + path, data=body, method=method, headers=values)
        try:
            response = opener.open(req, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        data = response.read()
        assert response.status in allowed, (method, path, response.status, data[:300])
        if "json" in response.headers.get("Content-Type", ""):
            data = json.loads(data)
        return data, response.headers
    def revision_count():
        with sqlite3.connect(f"file:{fixture}/data/print-partner.db?mode=ro", uri=True) as db:
            return db.execute("SELECT count(*) FROM plan_revisions WHERE profile_id = ?", [build]).fetchone()[0]
    try:
        assert select.select([process.stdout], [], [], 40)[0], "runtime readiness timed out"
        origin = process.stdout.readline().strip()
        assert origin.startswith("http://127.0.0.1:"), "runtime did not emit a clean origin"
        assert credentials.stat().st_mode & 0o777 == 0o600
        launch_receipt = json.loads(credentials.read_text())
        launch = launch_receipt["bootstrap_url"]
        _, headers = request(launch.removeprefix(origin), allowed=(303,))
        cookie = headers["Set-Cookie"].split(";", 1)[0]
        html, document_headers = request("/builds", headers={"Accept": "text/html"})
        # Feature-off (Node-backed) runtime: the legacy document policy, unchanged by GRE-1117.
        # A Rust-only runtime sends rust/crates/pp-gateway/src/replacement-csp.txt instead;
        # `cargo nextest run -p pp-gateway -E 'test(csp)'` checks that one.
        assert document_headers["Content-Security-Policy"] == "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
        assert not any(directive in document_headers["Content-Security-Policy"] for directive in ("script-src", "style-src", "connect-src", "nonce-"))
        assert b'name="pp-style-nonce"' not in html, "feature-off document gained the replacement style nonce"
        assert b'<div id="root">' in html
        provider_receipts = {}
        for path, key in [("/printers", "printers"), ("/settings/api-keys", "keys"), ("/api/v1/integrations", "integrations")]:
            value, _ = request(path)
            assert isinstance(value[key], list), (path, value)
            provider_receipts[path] = {"status": 200, "field": key, "count": len(value[key])}
        unavailable, _ = request("/settings/external-access", "PUT", {"mode": "lan"}, allowed=(501,))
        assert unavailable["code"] == "desktop_feature_unavailable"
        unknown, _ = request("/not-a-registered-api", allowed=(404,))
        assert unknown["detail"] == "Operation not registered"
        invalid, _ = request("/printers", "POST", {}, allowed=(400,))
        assert isinstance(invalid.get("detail"), str)
        source, _ = request("/sources", "POST", {"name": "Isolated desktop source", "source_kind": "local"})
        boundary = "pp-m0-local-upload"
        stl = (ROOT / "rust/tests/fixtures/part.stl").read_bytes()
        if "--streams" in sys.argv:
            rng = random.Random(42017)
            triangles = 300000
            stl = b"gateway slow consumer fixture".ljust(80, b"\0") + struct.pack("<I", triangles)
            stl += b"".join(struct.pack("<12fH", *[rng.uniform(0, 100) for _ in range(12)], 0) for _ in range(triangles))
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="files"; filename="pièce test.stl"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode() + stl +
                f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="relative_paths"\r\n\r\n["pièce test.stl"]\r\n--{boundary}--\r\n'.encode())
        request(f'/sources/{source["id"]}/upload-files', "POST", body, {"Content-Type": f"multipart/form-data; boundary={boundary}"})
        encoded_path = f'/sources/{source["id"]}/stl/pi%c3%a8ce%20test.stl/mesh'
        original_uri = encoded_path.replace('/sources/', '/%73ources/') + '?proof=%2f'
        mesh, mesh_headers = request(original_uri)
        assert mesh == stl
        assert mesh_headers["Content-Type"].startswith("model/stl")
        preview, preview_headers = request(encoded_path.rsplit('/', 1)[0] + '/preview')
        assert preview.startswith(bytes.fromhex('89504e470d0a1a0a'))
        assert preview_headers["Content-Type"].startswith("image/png")
        rejected_targets = []
        for fragment in ['%2fescape', '%5cescape', '%252fescape', '%2e%2e', '.%2E', '%00', '%ZZ', '%ff', '%c0%af']:
            path = f'/sources/{source["id"]}/stl/{fragment}/mesh'
            rejected, _ = request(path, allowed=(400,))
            assert rejected["detail"] == "Invalid request target"
            rejected_targets.append(path)
        created, _ = request("/plans", "POST", {"name": "Protected desktop workflow"})
        build = created["id"]
        aliases = {}
        for path in [f"/plans/{build}", f"/api/v1/plans/{build}", f"/api/v2/plans/{build}"]:
            value, _ = request(path)
            assert value["id"] == build
            aliases[path] = sorted(value.keys())

        request(f"/plans/{build}/layers/base", "PUT", {"project_id": source["id"]})
        workspace, _ = request(f"/plans/{build}/drafts/recompute", "POST", {"apply_manifest": True}, {"Idempotency-Key": "m0-prepare"})
        draft = workspace["draft"]
        receipt, _ = request(f'/plans/{build}/drafts/{draft["draft_id"]}/apply', "POST", {
            "expected_snapshot_digest": draft["snapshot_digest"], "expected_lifecycle_version": draft["lifecycle_version"], "expected_base": draft["base"]
        }, {"Idempotency-Key": "m0-publish"})
        part = workspace["parts"][0]
        save = {"expected_base": {"revision_id": receipt["revision_id"], "plan_version": receipt["plan_version"]}, "expected_draft": None,
                "remap_checkoff_links": True, "decisions": [{"kind": "set_quantity_override", "value": 2, "target": {
                    "part_key": part["part_key"], "relative_path": part["relative_path"], "source_layer": part["source_layer"]}}]}
        before = revision_count()
        authority = origin.removeprefix("http://")
        lost = http.client.HTTPConnection(authority, timeout=20)
        lost.request("POST", f"/plans/{build}/save", json.dumps(save), {"Cookie": cookie, "Origin": origin,
                     "Content-Type": "application/json", "Idempotency-Key": "m0-lost-response"})
        deadline = time.monotonic() + 15
        while revision_count() == before and time.monotonic() < deadline:
            time.sleep(.05)
        assert revision_count() == before + 1, "save did not commit once"
        lost.close()
        state, _ = request("/__runtime")
        old_pid = state["compat"]["pid"]
        os.kill(old_pid, signal.SIGKILL)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            state, _ = request("/__runtime")
            if state["compat"].get("state") == "ready" and state["compat"].get("pid") != old_pid:
                break
            time.sleep(.1)
        assert state["compat"].get("pid") != old_pid and state["compat"]["state"] == "ready"
        saved, _ = request(f"/plans/{build}/save", "POST", save, {"Idempotency-Key": "m0-lost-response"})
        assert revision_count() == before + 1, "retry published an extra revision"
        replayed, _ = request(f"/plans/{build}/save", "POST", save, {"Idempotency-Key": "m0-lost-response"})
        assert replayed["receipt"] == saved["receipt"]
        parts, _ = request(f"/plans/{build}/parts")
        accepted_part = parts["parts"][0]
        assert accepted_part["quantity_effective"] == 2
        progress, _ = request(f'/parts/{accepted_part["id"]}/progress', "PATCH", {"unit_index": 0, "completed": True})
        assert progress["printed_count"] == 1
        checkoff, _ = request(f"/plans/{build}/checkoff")
        for site, expected in ((None, 403), ("same-site", 403), ("same-origin", 200)):
            browser_get = http.client.HTTPConnection(authority, timeout=10)
            values = {"Cookie": cookie}
            if site:
                values["Sec-Fetch-Site"] = site
            browser_get.request("GET", f"/printer-checkoff?profile_id={build}", headers=values)
            response = browser_get.getresponse()
            response.read()
            assert response.status == expected, (site, response.status)
            browser_get.close()
        job, _ = request("/jobs/export-stl-pack", "POST", {"profile_id": build})
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            snapshot, _ = request(f'/jobs/{job["job_id"]}')
            if snapshot["status"] in ("done", "error", "cancelled"):
                break
            time.sleep(.1)
        assert snapshot["status"] == "done", snapshot
        download = snapshot["result"]["download_url"]
        export, headers = request(download)
        assert export[:2] == b"PK"
        assert "attachment" in headers["Content-Disposition"]
        assert headers["Content-Type"].startswith("application/zip")
        ws = subprocess.run(["/usr/bin/node", str(ROOT / "rust/tests/gateway_ws.mjs")],
                            input=json.dumps({"origin": origin, "cookie": cookie, "job_id": job["job_id"]}),
                            text=True, capture_output=True, timeout=30)
        assert ws.returncode == 0, ws.stderr
        websocket = json.loads(ws.stdout)
        streams = None
        def hold_websocket():
            live = subprocess.Popen(["/usr/bin/node", str(ROOT / "rust/tests/gateway_ws.mjs")], stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            live.stdin.write(json.dumps({"origin": origin, "cookie": cookie, "mode": "hold"}))
            live.stdin.close()
            assert select.select([live.stdout], [], [], 10)[0]
            assert json.loads(live.stdout.readline()) == {"ready": True}
            return live
        if "--streams" in sys.argv:
            assert len(export) > 12_000_000, len(export)
            child_pid = state["compat"]["pid"]
            def rss(pid):
                fields = Path(f"/proc/{pid}/status").read_text().splitlines()
                return int(next(line for line in fields if line.startswith("VmRSS:")).split()[1]) * 1024
            def slow_download():
                connection = http.client.HTTPConnection(authority, timeout=10)
                connection.connect()
                connection.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
                connection.request("GET", download, headers={"Cookie": cookie, "Origin": origin})
                response = connection.getresponse()
                assert response.status == 200
                assert response.read(4096).startswith(b"PK")
                return connection, response
            before_rss = {"rust": rss(process.pid), "node": rss(child_pid)}
            slow, response = slow_download()
            samples = []
            for _ in range(20):
                samples.append({"rust": rss(process.pid), "node": rss(child_pid)})
                time.sleep(.1)
            active, _ = request("/__runtime")
            assert active["gateway"]["active_requests"] >= 1, active
            peak_growth = {name: max(sample[name] for sample in samples) - before_rss[name] for name in before_rss}
            assert peak_growth["rust"] < 8 * 1024 * 1024, peak_growth
            response.close()
            slow.close()
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                traffic, _ = request("/__runtime")
                if traffic["gateway"] == {"active_requests": 0, "active_upgrades": 0}:
                    break
                time.sleep(.05)
            assert traffic["gateway"] == {"active_requests": 0, "active_upgrades": 0}, traffic
            downloaded, _ = request(download)
            assert hashlib.sha256(downloaded).digest() == hashlib.sha256(export).digest()
            assert revision_count() == before + 1
            live_ws = hold_websocket()
            slow, response = slow_download()
            active, _ = request("/__runtime")
            assert active["gateway"]["active_requests"] >= 2 and active["gateway"]["active_upgrades"] == 1, active
            runtime_dir = Path(launch_receipt["runtime_dir"])
            started = time.monotonic()
            expected_exit = 1
            process.terminate()
            time.sleep(.2)
            denied_during_drain, _ = request("/plans", "POST", {"name": "must not be created during drain"}, allowed=(503,))
            process.wait(timeout=17)
            elapsed = time.monotonic() - started
            shutdown_stderr = process.stderr.read()
            shutdown_receipt = json.loads(process.stdout.read())
            assert process.returncode == expected_exit, (process.returncode, shutdown_stderr, shutdown_receipt)
            assert without_log_rows(shutdown_stderr).strip() == "Error: Desktop shutdown incomplete", shutdown_stderr
            assert shutdown_receipt == {
                "proof_class": "headless_unsigned", "tracked_idle": True, "gateway_idle": False,
                "compat_exit": "graceful", "compat_reaped": True, "storage_released": True,
                "gateway_stopped": True, "marker_removed": True, "runtime_removed": True,
                "errors": ["gateway_drain_timed_out"],
            }, shutdown_receipt
            assert 5 <= elapsed < 17, elapsed
            assert not Path(f"/proc/{child_pid}").exists(), "Node child was not reaped"
            assert not runtime_dir.exists(), "runtime directory survived shutdown"
            with (fixture / "data/printpartner.lock").open("rb") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(lock, fcntl.LOCK_UN)
            assert live_ws.wait(timeout=3) == 0, live_ws.stderr.read()
            ws_closed = json.loads(live_ws.stdout.readline())
            assert ws_closed["closed"] is True
            response.close()
            slow.close()
            streams = {"export_bytes": len(export), "slow_read_bytes": 4096, "rss_before_bytes": before_rss,
                       "rss_peak_growth_bytes": peak_growth, "cancelled_readers_joined": traffic["gateway"],
                       "download_replay_sha256_equal": True, "revision_count_unchanged": True,
                       "shutdown_accepted": active["gateway"], "shutdown_seconds": elapsed,
                       "shutdown_exit": process.returncode, "shutdown_receipt": shutdown_receipt,
                       "websocket_close": ws_closed,
                       "drain_write_status": 503, "node_reaped": True,
                       "runtime_removed": True, "storage_lock_reacquired": True}
        else:
            live_ws = hold_websocket()
            runtime_dir = Path(launch_receipt["runtime_dir"])
            child_pid = state["compat"]["pid"]
            request("/auth/logout", "POST", allowed=(200,))
            assert live_ws.wait(timeout=3) == 0, live_ws.stderr.read()
            assert json.loads(live_ws.stdout.readline())["closed"] is True
            process.wait(timeout=17)
            assert process.returncode == 0, process.stderr.read()
            assert not Path(f"/proc/{child_pid}").exists(), "Node child was not reaped"
            assert not (fixture / "data/.desktop-lease").exists(), "compatibility reservation survived shutdown"
            assert not runtime_dir.exists(), "runtime directory survived shutdown"
            with (fixture / "data/printpartner.lock").open("rb") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(lock, fcntl.LOCK_UN)
            websocket["logout_closed_active_upgrade"] = True
            websocket["logout_runtime_exit"] = 0
            websocket["logout_node_reaped"] = True
            websocket["logout_marker_removed"] = True
            websocket["logout_runtime_removed"] = True
            websocket["logout_storage_lock_reacquired"] = True
        result = {"proof_class": "headless_unsigned", "case": "gateway_product_coverage", "origin": origin,
                  "websocket": websocket, "streams": streams, "providers": provider_receipts, "aliases": aliases, "encoded_source": {"original_uri": original_uri, "canonical_target": encoded_path.replace("%c3%a8", "%C3%A8") + "?proof=%2f", "decoded_path": "pièce test.stl", "mesh_sha256": hashlib.sha256(mesh).hexdigest(), "preview_sha256": hashlib.sha256(preview).hexdigest(), "rejected_targets": rejected_targets},
                  "fixture": str(fixture), "source_id": source["id"], "build_id": build, "part_id": accepted_part["id"],
                  "receipt": saved["receipt"], "revision_count_before": before, "revision_count_after": revision_count(),
                  "old_child_pid": old_pid, "new_child_pid": state["compat"]["pid"], "printed_count": progress["printed_count"],
                  "export_sha256": hashlib.sha256(export).hexdigest(), "export_bytes": len(export), "checkoff_reload": isinstance(checkoff, dict), "effectful_get_fetch_metadata": True, "document_csp": document_headers["Content-Security-Policy"]}
        (fixture / "receipt.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=17)
        assert process.returncode == expected_exit, process.stderr.read()
        credentials.unlink(missing_ok=True)
        assert not (fixture / "data/.desktop-lease").exists(), "compatibility reservation survived shutdown"


if __name__ == "__main__":
    main()
