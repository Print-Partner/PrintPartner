import hashlib
import http.client
import json
import os
from pathlib import Path
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[2]
COMMIT = json.loads((ROOT / "rust/bundle-manifest.json").read_text())["commit"]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    fixture = Path(tempfile.mkdtemp(prefix="pp-workflow-m0-"))
    credentials = fixture / "credentials.json"
    process = subprocess.Popen([
        str(ROOT / "rust/target/debug/pp-server"), "--data", str(fixture / "data"),
        "--web", str(ROOT / "web"), "--node", "/usr/bin/node", "--commit", COMMIT,
        "--test-credential-file", str(credentials),
    ], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    opener = urllib.request.build_opener(NoRedirect)
    cookie = None
    origin = None
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
        launch = json.loads(credentials.read_text())["bootstrap_url"]
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
        source, _ = request("/sources", "POST", {"name": "Isolated desktop source", "source_kind": "local"})
        boundary = "pp-m0-local-upload"
        stl = (ROOT / "rust/tests/fixtures/part.stl").read_bytes()
        filename = "cube café (sample).stl"
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="files"; filename="{filename}"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode() + stl +
                f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="relative_paths"\r\n\r\n{json.dumps([filename])}\r\n--{boundary}--\r\n'.encode())
        request(f'/sources/{source["id"]}/upload-files', "POST", body, {"Content-Type": f"multipart/form-data; boundary={boundary}"})
        encoded_filename = quote(filename, safe="")
        mesh, _ = request(f'/sources/{source["id"]}/stl/{encoded_filename}/mesh')
        assert mesh == stl, "encoded artifact did not reach the real Node file route"
        rejected_paths = ["%2e%2e", "%2F", "%5C", "%00", "%252e%252e", "%zz"]
        for bad in rejected_paths:
            request(f'/sources/{source["id"]}/stl/{bad}/mesh', allowed=(400,))
        created, _ = request("/plans", "POST", {"name": "Protected desktop workflow"})
        build = created["id"]
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
        deadline = time.monotonic() + 20
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
        result = {"proof_class": "headless_unsigned", "case": "source_plan_autosave_retry_checkoff_export", "origin": origin,
                  "fixture": str(fixture), "source_id": source["id"], "build_id": build, "part_id": accepted_part["id"],
                  "receipt": saved["receipt"], "revision_count_before": before, "revision_count_after": revision_count(),
                  "old_child_pid": old_pid, "new_child_pid": state["compat"]["pid"], "printed_count": progress["printed_count"],
                  "export_sha256": hashlib.sha256(export).hexdigest(), "export_bytes": len(export), "checkoff_reload": isinstance(checkoff, dict), "effectful_get_fetch_metadata": True, "document_csp": document_headers["Content-Security-Policy"], "encoded_artifact_filename": filename,
                  "encoded_artifact_sha256": hashlib.sha256(mesh).hexdigest(), "ambiguous_paths_denied": len(rejected_paths)}
        (fixture / "receipt.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=17)
        credentials.unlink(missing_ok=True)
        assert not (fixture / "data/.desktop-owner.json").exists(), "owner marker survived shutdown"


if __name__ == "__main__":
    main()
