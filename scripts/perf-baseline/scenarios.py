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
from urllib.parse import quote

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


# Thumbnails ---------------------------------------------------------------------

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
PNG_CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}
PNG_BIT_DEPTHS = {0: {1, 2, 4, 8, 16}, 2: {8, 16}, 3: {1, 2, 4, 8},
                  4: {8, 16}, 6: {8, 16}}
PNG_SIZE = (256, 256)


def source_id_for_layer(layers, source_layer):
    """The Source whose `layer_type:project_name` equals `source_layer`."""
    for layer in layers:
        project_id, name = layer.get("project_id"), layer.get("project_name")
        if project_id and name and source_layer == f"{layer.get('layer_type')}:{name}":
            return project_id
    known = [f"{layer.get('layer_type')}:{layer.get('project_name')}" for layer in layers]
    raise RuntimeError(f"no Source for {source_layer!r}; layers are {known}")


def thumbnail_url(source_id, path):
    """`GET /sources/{id}/thumbnail?path=` for one STL of that Source."""
    return f"/sources/{source_id}/thumbnail?path={quote(path, safe='')}"


def _png_dimensions(data):
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("missing PNG signature")
    offset, header, image_data, ended = len(PNG_SIGNATURE), None, [], False
    while offset < len(data):
        if offset + 12 > len(data):
            raise ValueError("truncated PNG chunk")
        length = struct.unpack(">I", data[offset:offset + 4])[0]
        kind = data[offset + 4:offset + 8]
        start, end = offset + 8, offset + 8 + length
        if end + 4 > len(data):
            raise ValueError("truncated PNG chunk")
        contents = data[start:end]
        crc = struct.unpack(">I", data[end:end + 4])[0]
        if zlib.crc32(kind + contents) & 0xFFFFFFFF != crc:
            raise ValueError("invalid PNG chunk checksum")
        if header is None:
            if kind != b"IHDR" or length != 13:
                raise ValueError("missing PNG header")
            header = struct.unpack(">IIBBBBB", contents)
        elif kind == b"IHDR":
            raise ValueError("duplicate PNG header")
        if kind == b"IDAT":
            image_data.append(contents)
        if kind == b"IEND":
            if length != 0 or end + 4 != len(data):
                raise ValueError("invalid PNG end chunk")
            ended = True
            break
        offset = end + 4
    if header is None or not image_data or not ended:
        raise ValueError("incomplete PNG")
    width, height, bit_depth, color_type, compression, filtering, interlace = header
    if width < 1 or height < 1 or width > PNG_SIZE[0] or height > PNG_SIZE[1]:
        raise ValueError(f"invalid PNG dimensions {width}x{height}")
    if (color_type not in PNG_CHANNELS or bit_depth not in PNG_BIT_DEPTHS[color_type]
            or compression != 0 or filtering != 0 or interlace != 0):
        raise ValueError("unsupported PNG format")
    decoder = zlib.decompressobj()
    try:
        pixels = decoder.decompress(b"".join(image_data)) + decoder.flush()
    except zlib.error as error:
        raise ValueError("invalid PNG image data") from error
    if not decoder.eof or decoder.unused_data or decoder.unconsumed_tail:
        raise ValueError("incomplete PNG image data")
    row_bytes = (width * PNG_CHANNELS[color_type] * bit_depth + 7) // 8
    stride = row_bytes + 1
    if len(pixels) != height * stride:
        raise ValueError("invalid PNG image data length")
    if any(pixels[row] > 4 for row in range(0, len(pixels), stride)):
        raise ValueError("invalid PNG row filter")
    return width, height


def _require_png(label, status, headers, data, expected_size=PNG_SIZE, allow_placeholder=False):
    kind = headers.get("content-type", "").split(";", 1)[0].strip()
    if status != 200 or kind != "image/png":
        raise RuntimeError(f"{label}: thumbnail answered {status} {kind}: {data[:80]!r}")
    if headers.get("x-thumbnail-placeholder") == "1" and not allow_placeholder:
        raise RuntimeError(f"{label}: thumbnail response was a placeholder")
    try:
        size = _png_dimensions(data)
    except ValueError as error:
        raise RuntimeError(f"{label}: invalid thumbnail PNG: {error}") from error
    if expected_size is not None and size != expected_size:
        raise RuntimeError(f"{label}: thumbnail dimensions were {size[0]}x{size[1]}")
    return size


def _thumbnail_pass(session, out, parts, part_key, build_key):
    started = time.perf_counter()
    responses = []
    for source_id, path in parts:
        status, headers, data, ms = session.request("GET", thumbnail_url(source_id, path))
        responses.append((source_id, path, status, headers, data, ms))
    elapsed = (time.perf_counter() - started) * 1000
    for source_id, path, status, headers, data, _ms in responses:
        _require_png(f"source {source_id} {path}", status, headers, data)
    for _source_id, _path, _status, _headers, _data, ms in responses:
        out.setdefault(part_key, []).append(ms)
    out.setdefault(build_key, []).append(elapsed)


def rust_thumbnails(session, out, build_id):
    """One `GET /sources/{id}/thumbnail` per Part of the Build. Cold is the
    first read: the cache is empty, so the server renders the STL and stores
    the PNG. Warm is the same GET, answered from that cache. Finding the
    Parts and their Sources is not timed."""
    units, _ = session.json("GET", f"/api/v2/plans/{build_id}/checkoff/units")
    listed, _ = session.json("GET", f"/api/v2/plans/{build_id}/layers")
    parts = []
    for part in units["parts"]:
        path = part["relative_path"] or part["filename"]
        parts.append((source_id_for_layer(listed["layers"], part["source_layer"]), path))
    if not parts:
        raise RuntimeError(f"build {build_id} has no parts to thumbnail")
    _thumbnail_pass(session, out, parts, "thumbnail_cold_part_ms", "thumbnail_cold_build_ms")
    for _ in range(3):
        _thumbnail_pass(session, out, parts, "thumbnail_warm_part_ms", "thumbnail_warm_build_ms")
    out.setdefault("thumbnail_parts", []).append(len(parts))


def thumbnails(session, runtime, out, build_id):
    """Part thumbnails for one Build.

    Node, the old app: cold is the placeholder, the mesh and the uploaded
    PNG (the browser's WebGL render is not timed). Warm is the cached PNG.
    Rust: cold and warm are `GET /sources/{id}/thumbnail` for that Part's
    STL. Cold renders it; warm reads the cache."""
    if runtime == "rust":
        rust_thumbnails(session, out, build_id)
        return
    status, headers, data, _ms = session.request("GET", "/parts/1/thumbnail")
    if status == 404:
        return
    _require_png("Node thumbnail route probe", status, headers, data, expected_size=(1, 1),
                 allow_placeholder=True)
    checkoff, _ = session.json("GET", f"/plans/{build_id}/checkoff")
    parts = [part["id"] for part in checkoff["parts"]]
    if not parts:
        raise RuntimeError(f"build {build_id} has no parts to thumbnail")
    png, version = thumbnail_png(), renderer_version()
    _require_png("Node uploaded thumbnail", 200, {"content-type": "image/png"}, png)
    started = time.perf_counter()
    cold_responses = []
    for part in parts:
        placeholder_status, placeholder_headers, placeholder_data, ms = session.request(
            "GET", f"/parts/{part}/thumbnail")
        if placeholder_status != 200 or placeholder_headers.get("x-thumbnail-placeholder") != "1":
            raise RuntimeError(f"part {part}: thumbnail cache was not cold")
        status, mesh_headers, _data, mesh_ms = session.request("GET", f"/parts/{part}/mesh")
        if status != 200:
            raise RuntimeError(f"part {part}: mesh answered {status}")
        body, content_type = multipart("file", "thumbnail.png", png, "image/png")
        status, _h, data, up_ms = session.request("POST", f"/parts/{part}/thumbnail", body, {
            "Content-Type": content_type, "If-Match": mesh_headers["etag"],
            "X-Thumbnail-Renderer-Version": version})
        if status != 200:
            raise RuntimeError(f"part {part}: thumbnail upload answered {status}: {data[:200]!r}")
        cold_responses.append((part, placeholder_status, placeholder_headers, placeholder_data,
                               ms + mesh_ms + up_ms))
        cold_build_ms = (time.perf_counter() - started) * 1000
    for part, status, headers, data, _ms in cold_responses:
        _require_png(f"part {part} cold placeholder", status, headers, data,
                     expected_size=(1, 1), allow_placeholder=True)
    for _part, _status, _headers, _data, ms in cold_responses:
        out.setdefault("thumbnail_cold_part_ms", []).append(ms)
    out.setdefault("thumbnail_cold_build_ms", []).append(cold_build_ms)
    for _ in range(3):
        started = time.perf_counter()
        warm_responses = []
        for part in parts:
            status, headers, data, ms = session.request("GET", f"/parts/{part}/thumbnail")
            if status != 200 or headers.get("x-thumbnail-placeholder") == "1":
                raise RuntimeError(f"part {part}: warm thumbnail answered {status}")
            warm_responses.append((part, status, headers, data, ms))
        warm_build_ms = (time.perf_counter() - started) * 1000
        for part, status, headers, data, _ms in warm_responses:
            _require_png(f"part {part} warm", status, headers, data)
            if data != png:
                raise RuntimeError(f"part {part}: warm thumbnail differs from the uploaded PNG")
        for _part, _status, _headers, _data, ms in warm_responses:
            out.setdefault("thumbnail_warm_part_ms", []).append(ms)
        out.setdefault("thumbnail_warm_build_ms", []).append(warm_build_ms)
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


# Exports ------------------------------------------------------------------------

# (sample prefix, path leaf, content-type marker, body prefix)
RUST_EXPORTS = (
    ("export_stl_pack", "exports/stl-pack", "application/zip", b"PK"),
    ("export_checklist_html", "exports/checklist", "text/html", b"<!doctype html"),
)


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


def _export_suffix(attempt):
    return "ms" if attempt == 0 else "repeat_ms"


def _record(out, key, value):
    out.setdefault(key, []).append(value)


def _export_url(build_id, leaf):
    return f"/api/v2/plans/{build_id}/{leaf}"


def _require_export(path, status, headers, data, marker, prefix):
    got = headers.get("content-type", "")
    if status != 200 or marker not in got or not data.startswith(prefix):
        raise RuntimeError(f"{path} answered {status} {got}: {data[:120]!r}")


def _rust_get(session, path, marker, prefix):
    """One GET read to the last byte. Returns (ms to headers, ms to the last
    byte), or None when the route is absent."""
    status, headers, data, header_ms, total_ms = session.exchange(
        "GET", path, headers={"Accept": "*/*"})
    if status in (404, 405):
        return None
    _require_export(path, status, headers, data, marker, prefix)
    return header_ms, total_ms


def _rust_check(session, path, marker):
    """The replacement UI's check: stop at the headers and close, without
    reading the body. Returns milliseconds, or None when the route is absent."""
    status, headers, elapsed = session.request_headers("GET", path, headers={"Accept": "*/*"})
    if status in (404, 405):
        return None
    got = headers.get("content-type", "")
    if status != 200 or marker not in got:
        raise RuntimeError(f"{path} check answered {status} {got}")
    return elapsed


def _rust_exports(session, out, build_id, repeats):
    """Times each Rust export GET from the request to the last byte, first
    and repeated. Those four samples are the rows compared with Node.

    Then times the Production tab's click, which is a second pair of GETs:
    one aborted at the headers (the core has already opened and verified the
    files) and one read to the last byte. The click samples follow the row
    GETs, so the files are already in the page cache. The cold cost of
    reaching the first byte is the headers sample of the first GET."""
    routes = tuple((name, _export_url(build_id, leaf), marker, prefix)
                   for name, leaf, marker, prefix in RUST_EXPORTS)
    for attempt in range(repeats):
        suffix = _export_suffix(attempt)
        for name, path, marker, prefix in routes:
            timed = _rust_get(session, path, marker, prefix)
            if timed is None:
                raise RuntimeError(f"{path} is not served")
            header_ms, total_ms = timed
            _record(out, f"{name}_{suffix}", total_ms)
            _record(out, f"{name}_headers_{suffix}", header_ms)
    for attempt in range(repeats):
        suffix = _export_suffix(attempt)
        for name, path, marker, prefix in routes:
            check_ms = _rust_check(session, path, marker)
            timed = _rust_get(session, path, marker, prefix)
            if check_ms is None or timed is None:
                raise RuntimeError(f"{path} is not served")
            _record(out, f"{name}_check_{suffix}", check_ms)
            _record(out, f"{name}_click_{suffix}", check_ms + timed[1])


def exports(session, runtime, out, build_id, repeats):
    """The STL pack and the HTML checklist of the 100-unit Build.

    Node starts a job, polls it every 20 ms and downloads the file. The first
    export in a run writes the file; Node names exports by content, so a
    repeat of an unchanged Build can reuse it and is recorded apart.
    Rust reads `GET /api/v2/plans/{id}/exports/stl-pack` and `.../checklist`
    to the last byte. See `_rust_exports` for the check-then-download rows."""
    if runtime == "rust":
        _rust_exports(session, out, build_id, repeats)
        return
    for attempt in range(repeats):
        suffix = _export_suffix(attempt)
        for name, path in (("export_stl_pack", "/jobs/export-stl-pack"),
                           ("export_checklist_html", "/jobs/export-checklist-html")):
            elapsed = _run_job(session, path, {"profile_id": build_id})
            if elapsed is None:
                return
            _record(out, f"{name}_{suffix}", elapsed)


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
