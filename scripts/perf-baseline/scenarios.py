"""The measured operations. Each function drives one logged-in Session the way
the UI that runtime serves drives it, and appends raw samples (milliseconds)
to `out[name]`. An operation a runtime has no route for records nothing for
that runtime; the report marks it "not available".

Node runtime: the old web app (web/apps/web). Rust-only runtime: the
replacement UI's generated client routes (pp-api client_contract.rs).
Where both serve the same path, the request is the same on both."""

import json
import re
import struct
import time
import uuid
import zlib
from concurrent.futures import ThreadPoolExecutor

from runtime import ROOT, Session

CHECKOFF_BUILD = "Synthetic Build Anchor 100u15p"
PLAN_BUILD = "Synthetic Build Anchor 47u6p"
BURSTS = 15
TODAY_POLL_S = 0.05
TODAY_TIMEOUT_S = 30


def renderer_version():
    text = (ROOT / "web/packages/contracts/src/accepted-media.ts").read_text()
    return re.search(r'THUMBNAIL_RENDERER_VERSION = "([^"]+)"', text)[1]


def thumbnail_png(size=256):
    """A valid size x size RGBA PNG standing in for the browser's WebGL render
    (stlThumbnail.ts renders 256 x 256). Its pixels vary so it compresses
    like a render, not like a flat colour."""
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            raw += bytes(((x * 7 + y) & 255, (y * 3) & 255, (x ^ y) & 255, 255))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 6)) + chunk(b"IEND", b""))


def multipart(field, filename, payload, content_type):
    boundary = uuid.uuid4().hex
    body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n"
            f"Content-Type: {content_type}\r\n\r\n").encode() + payload + f"\r\n--{boundary}--\r\n".encode()
    return body, f"multipart/form-data; boundary={boundary}"


def key():
    return {"Idempotency-Key": uuid.uuid4().hex}


def build_ids(session):
    listed, _ = session.json("GET", "/api/v2/plans")
    return {profile["name"]: profile["id"] for profile in listed["profiles"]}


def bursts(units, count=BURSTS):
    """Splits units into `count` consecutive bursts whose sizes differ by at
    most one, as an evening's Checkoff session clears a few beds at a time."""
    if count <= 0 or len(units) < count:
        raise ValueError("need at least one unit per burst")
    return [units[len(units) * i // count: len(units) * (i + 1) // count] for i in range(count)]


# Builds list -----------------------------------------------------------------

def builds_list(session, out, repeats):
    for _ in range(repeats):
        _, ms = session.json("GET", "/api/v2/plans")
        out.setdefault("builds_list_warm_ms", []).append(ms)


# Thumbnails (Node only) --------------------------------------------------------

def thumbnails(session, runtime, out, build_id):
    """The old app's Part thumbnails for one Build. Cold: the server has no
    cached PNG, so each Part answers the placeholder, the browser fetches
    the mesh, renders it and uploads the PNG (the render itself is browser
    work and not timed). Warm: each Part answers its cached PNG."""
    status, _h, _d, _ms = session.request("GET", "/parts/1/thumbnail")
    if runtime == "rust" or status == 404:
        return
    checkoff, _ = session.json("GET", f"/plans/{build_id}/checkoff")
    parts = [part["id"] for part in checkoff["parts"]]
    png, version = thumbnail_png(), renderer_version()
    started = time.perf_counter()
    for part in parts:
        status, headers, _data, ms = session.request("GET", f"/parts/{part}/thumbnail")
        if headers.get("x-thumbnail-placeholder") != "1":
            raise RuntimeError(f"part {part}: thumbnail cache was not cold")
        status, headers, _data, mesh_ms = session.request("GET", f"/parts/{part}/mesh")
        if status != 200:
            raise RuntimeError(f"part {part}: mesh answered {status}")
        body, content_type = multipart("file", "thumbnail.png", png, "image/png")
        status, _h, data, up_ms = session.request("POST", f"/parts/{part}/thumbnail", body, {
            "Content-Type": content_type, "If-Match": headers["etag"],
            "X-Thumbnail-Renderer-Version": version})
        if status != 200:
            raise RuntimeError(f"part {part}: thumbnail upload answered {status}: {data[:200]!r}")
        out.setdefault("thumbnail_cold_part_ms", []).append(ms + mesh_ms + up_ms)
    out.setdefault("thumbnail_cold_build_ms", []).append((time.perf_counter() - started) * 1000)
    for _ in range(3):
        started = time.perf_counter()
        for part in parts:
            status, headers, _data, ms = session.request("GET", f"/parts/{part}/thumbnail")
            if status != 200 or headers.get("x-thumbnail-placeholder") == "1":
                raise RuntimeError(f"part {part}: warm thumbnail answered {status}")
            out.setdefault("thumbnail_warm_part_ms", []).append(ms)
        out.setdefault("thumbnail_warm_build_ms", []).append((time.perf_counter() - started) * 1000)
    out.setdefault("thumbnail_parts", []).append(len(parts))


# Plan change -----------------------------------------------------------------

def _target(part):
    return {"part_key": part["part_key"], "relative_path": part["relative_path"],
            "source_layer": part["source_layer"]}


def _identity(draft):
    return {name: draft[name] for name in ("draft_id", "state", "lifecycle_version", "snapshot_digest", "base")}


def plan_change(session, runtime, out, build_id, iterations):
    """Open a draft, edit one Part's quantity, autosave and Accept preview
    (Rust only: Node has neither route), then Accept.
    Each iteration flips the quantity override between 2 and none, so the Plan
    returns to its start every second iteration."""
    base = f"/api/v2/plans/{build_id}"
    for iteration in range(iterations):
        value = 2 if iteration % 2 == 0 else None
        started = time.perf_counter()
        workspace, open_ms = session.json("POST", f"{base}/drafts/recompute", {"apply_manifest": False},
                                          headers=key())
        draft, part = workspace["draft"], workspace["parts"][0]
        edited, edit_ms = session.json("PATCH", f"{base}/drafts/{draft['draft_id']}/parts", {
            "expected_snapshot_digest": draft["snapshot_digest"],
            "decisions": [{"kind": "set_quantity_override", "draft_part_ids": [part["draft_part_id"]],
                           "value": value}]})
        draft = edited["draft"]
        out.setdefault("plan_open_draft_ms", []).append(open_ms)
        out.setdefault("plan_edit_ms", []).append(edit_ms)
        if runtime == "rust":
            saved, save_ms = session.json("POST", f"{base}/drafts/save", {
                "expected_base": draft["base"], "expected_draft": _identity(draft),
                "remap_checkoff_links": True,
                "decisions": [{"kind": "set_quantity_override", "target": _target(part), "value": value}]},
                headers=key())
            draft = saved["workspace"]["draft"]
            out.setdefault("plan_autosave_ms", []).append(save_ms)
            _, preview_ms = session.json("GET", f"{base}/drafts/{draft['draft_id']}/accept-preview")
            out.setdefault("plan_accept_preview_ms", []).append(preview_ms)
        _, apply_ms = session.json("POST", f"{base}/drafts/{draft['draft_id']}/apply", {
            "expected_base": draft["base"], "expected_snapshot_digest": draft["snapshot_digest"],
            "expected_lifecycle_version": draft["lifecycle_version"], "remap_checkoff_links": True},
            headers=key())
        out.setdefault("plan_accept_ms", []).append(apply_ms)
        # The steps both runtimes serve, comparable one to one.
        out.setdefault("plan_change_common_ms", []).append(open_ms + edit_ms + apply_ms)
        out.setdefault("plan_change_total_ms", []).append((time.perf_counter() - started) * 1000)
    if runtime == "node":
        node_publishing_save(session, out, build_id, iterations)


def node_publishing_save(session, out, build_id, iterations):
    """Node's one-request Save (POST /plans/:id/save), which the old app sends
    for an ordinary quantity edit: draft, reconcile and publish together."""
    for iteration in range(iterations):
        value = 2 if iteration % 2 == 0 else None
        workspace, _ = session.json("POST", f"/api/v2/plans/{build_id}/drafts/recompute",
                                    {"apply_manifest": False}, headers=key())
        draft, part = workspace["draft"], workspace["parts"][0]
        session.json("POST", f"/api/v2/plans/{build_id}/drafts/{draft['draft_id']}/abandon",
                     {"expected_lifecycle_version": draft["lifecycle_version"]}, headers=key())
        _, ms = session.json("POST", f"/plans/{build_id}/save", {
            "expected_base": draft["base"], "expected_draft": None, "remap_checkoff_links": True,
            "decisions": [{"kind": "set_quantity_override", "target": _target(part), "value": value}]},
            headers=key())
        out.setdefault("plan_node_publishing_save_ms", []).append(ms)


# Checkoff session --------------------------------------------------------------

def checkoff_session(session, runtime, out, build_id):
    """Build 22's evening: 15 bursts that check off all 100 units of the
    100-unit Build. Rust: one burst request, then the Checkoff read the
    screen refreshes. Node (old app): one PATCH per unit, then its refresh."""
    started = time.perf_counter()
    if runtime == "rust":
        read = f"/api/v2/plans/{build_id}/checkoff/units"
        units_body, _ = session.json("GET", read)
        units = [unit["token"] for part in units_body["parts"] for unit in part["units"]]
        for group in bursts(units):
            burst_started = time.perf_counter()
            session.json("POST", f"/api/v2/plans/{build_id}/checkoff/bursts", {
                "idempotency_key": uuid.uuid4().hex, "expected": units_body["basis"],
                "units": [{"token": token, "printed": True} for token in group]})
            units_body, _ = session.json("GET", read)
            out.setdefault("checkoff_burst_ms", []).append((time.perf_counter() - burst_started) * 1000)
        done = sum(part["printed_count"] for part in units_body["parts"])
    else:
        read = f"/plans/{build_id}/checkoff"
        body, _ = session.json("GET", read)
        units = [(part["id"], index) for part in body["parts"] for index in range(len(part["print_units"]))]
        for group in bursts(units):
            burst_started = time.perf_counter()
            for part_id, index in group:
                session.json("PATCH", f"/parts/{part_id}/progress", {"unit_index": index, "completed": True})
            body, _ = session.json("GET", read)
            out.setdefault("checkoff_burst_ms", []).append((time.perf_counter() - burst_started) * 1000)
        done = sum(part["printed_count"] for part in body["parts"])
    if done != len(units):
        raise RuntimeError(f"checkoff session printed {done} of {len(units)} units")
    out.setdefault("checkoff_session_ms", []).append((time.perf_counter() - started) * 1000)
    out.setdefault("checkoff_units", []).append(len(units))


# Exports (Node only) -----------------------------------------------------------

def _run_job(session, path, body):
    """Starts an export job, polls it every 20 ms as JobContext polls, then
    downloads its file. Returns milliseconds, or None when the runtime has
    no such route."""
    started = time.perf_counter()
    status, _h, data, _ms = session.request("POST", path, body)
    if status in (404, 405):
        return None
    if status != 200:
        raise RuntimeError(f"{path} answered {status}: {data[:200]!r}")
    job_id = json.loads(data)["job_id"]
    while True:
        job, _ = session.json("GET", f"/jobs/{job_id}")
        if job["status"] == "done":
            break
        if job["status"] not in ("queued", "running", "pending"):
            raise RuntimeError(f"{path} job {job_id} {job['status']}: {job.get('error')}")
        time.sleep(0.02)
    download = (job.get("result") or {}).get("download_url")
    if not download:
        raise RuntimeError(f"{path} job {job_id} finished without a download_url")
    status, _h, data, _ms = session.request("GET", download)
    if status != 200 or not data:
        raise RuntimeError(f"{download} answered {status}")
    return (time.perf_counter() - started) * 1000


def exports(session, out, build_id, repeats):
    """The STL pack and the HTML checklist of the 100-unit Build. The first
    export in a run writes the file; Node names exports by content, so a
    repeat of an unchanged Build can reuse it and is recorded apart."""
    for attempt in range(repeats):
        suffix = "ms" if attempt == 0 else "repeat_ms"
        for name, path in (("export_stl_pack", "/jobs/export-stl-pack"),
                           ("export_checklist_html", "/jobs/export-checklist-html")):
            elapsed = _run_job(session, path, {"profile_id": build_id})
            if elapsed is None:
                return
            out.setdefault(f"{name}_{suffix}", []).append(elapsed)


# Today ---------------------------------------------------------------------

TODAY_READS = {
    # The replacement UI's Today screen (web/apps/ui/src/today/TodayScreen.tsx).
    "rust": ["/api/v2/tasks", "/api/v2/printer-checkoff", "/api/v2/printers"],
    # The old app has no Today; All Production (GlobalProductionPage.tsx) reads these.
    "node": ["/printer-checkoff", "/printer-checkoff/unattributed"],
}


def today(server, session, runtime, out, printer, integration_id):
    """A simulated printer finishes its print. Records how long until the
    status route reports it, then the Today (Rust) or All Production (Node)
    reads fired together, as the screen refreshes them."""
    path = f"/api/v1/integrations/{integration_id}/status"
    state, _ = session.json("GET", path)
    if state["state"] != "printing":
        raise RuntimeError(f"simulated printer starts {state['state']}, not printing")
    printer.finish()
    started = time.perf_counter()
    while True:
        state, _ = session.json("GET", path)
        if state["state"] == "complete":
            break
        if time.perf_counter() - started > TODAY_TIMEOUT_S:
            raise RuntimeError("printer event never reached the status route")
        time.sleep(TODAY_POLL_S)
    out.setdefault("today_event_visible_ms", []).append((time.perf_counter() - started) * 1000)
    sessions = [Session(session.origin, session.cookie) for _ in TODAY_READS[runtime]]
    with ThreadPoolExecutor(len(sessions)) as pool:
        for _ in range(5):
            started = time.perf_counter()
            replies = list(pool.map(lambda pair: pair[0].json("GET", pair[1]),
                                    zip(sessions, TODAY_READS[runtime])))
            out.setdefault("today_refresh_ms", []).append((time.perf_counter() - started) * 1000)
            for path, (_body, ms) in zip(TODAY_READS[runtime], replies):
                if path == "/api/v2/tasks":
                    out.setdefault("today_tasks_read_ms", []).append(ms)
    for extra in sessions:
        extra.close()
