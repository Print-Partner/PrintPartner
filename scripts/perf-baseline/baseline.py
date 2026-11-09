#!/usr/bin/env python3
"""Node-versus-Rust performance baseline (GRE-388).

Runs one pp-server binary built with `--features desktop_rebuild` twice over
copies of the same synthetic dataset: with PP_DESKTOP_REBUILD unset (the
gateway supervises the Node runtime) and with PP_DESKTOP_REBUILD=1 (Rust
only). Five loopback Moonraker simulators stand in for printers; nothing is
sent to a printer. Writes raw samples, metadata and a Markdown table.

    python3 scripts/perf-baseline/baseline.py \\
        --binary rust/target/release/pp-server --runs 5 --out /tmp/pp-baseline

Prerequisites: `npm --prefix web ci`, the desktop web build and
rust/bundle-manifest.json that the binary was built against (see
docs/development/performance.md).
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import perfstats
import scenarios
from runtime import ROOT, Server, SimulatedPrinter, copy_dataset, mark_rust_owned, seed_printers

RUNTIMES = ("node", "rust")
PRINTERS = 5
SEED = 388

NO_RUST = "not available in Rust"
DIFFERENT = "different requests per runtime; see the notes"


def measure(key, label, unit="ms", note="", comparable=True):
    return {"key": key, "label": label, "unit": unit, "note": note, "comparable": comparable}


# Row order of the report. `note` is printed when a side has no samples or the
# row needs a caveat; `comparable` False drops the Rust/Node ratio, because
# the two runtimes send different requests for that row.
MEASURES = [
    measure("startup_ready_ms", "Process start to origin line"),
    measure("builds_list_first_ms", "First Builds list (`GET /api/v2/plans`)"),
    measure("app_open_ms", "App open to usable Builds list"),
    measure("builds_list_warm_ms", "Builds list, warm"),
    measure("idle_pss_bytes", "Idle memory, PSS of process tree", "bytes"),
    measure("idle_cpu_pct", "Idle CPU, process tree (% of one core)", "%"),
    measure("thumbnail_cold_build_ms", "Thumbnails cold, whole Build", note=NO_RUST),
    measure("thumbnail_cold_part_ms", "Thumbnail cold, per Part", note=NO_RUST),
    measure("thumbnail_warm_build_ms", "Thumbnails warm, whole Build", note=NO_RUST),
    measure("thumbnail_warm_part_ms", "Thumbnail warm, per Part", note=NO_RUST),
    measure("plan_open_draft_ms", "Plan change: open draft (recompute)"),
    measure("plan_edit_ms", "Plan change: edit Part quantity"),
    measure("plan_autosave_ms", "Plan change: autosave", note="Node has no draft-only autosave"),
    measure("plan_accept_preview_ms", "Plan change: Accept preview", note="Node has no Accept preview"),
    measure("plan_accept_ms", "Plan change: Accept"),
    measure("plan_change_common_ms", "Plan change: open + edit + Accept"),
    measure("plan_change_total_ms", "Plan change: whole flow as each UI sends it", note=DIFFERENT,
            comparable=False),
    measure("plan_node_publishing_save_ms", "Plan change: Node one-request Save",
            note="Node only; Rust has no `/plans/:id/save`"),
    measure("checkoff_burst_ms", "Checkoff burst, write + screen refresh", note=DIFFERENT, comparable=False),
    measure("checkoff_session_ms", "Checkoff session, 15 bursts, 100 units", note=DIFFERENT, comparable=False),
    measure("export_stl_pack_ms", "STL pack export, first in run", note=NO_RUST),
    measure("export_checklist_html_ms", "HTML checklist export, first in run", note=NO_RUST),
    measure("export_stl_pack_repeat_ms", "STL pack export, repeated", note=NO_RUST),
    measure("export_checklist_html_repeat_ms", "HTML checklist export, repeated", note=NO_RUST),
    measure("today_event_visible_ms", "Printer event to status route",
            note="the Rust gateway watcher in both runtimes; 2 s poll", comparable=False),
    measure("today_refresh_ms", "Today refresh reads", note=DIFFERENT, comparable=False),
    measure("today_tasks_read_ms", "Today read `GET /api/v2/tasks`, within the refresh",
            note="Rust only; Node has no task feed"),
    measure("workload_pss_bytes", "Memory after the workload, PSS", "bytes"),
    measure("idle_processes", "Processes in the tree", "processes", comparable=False),
    measure("load_at_start", "Machine 1-minute load at run start", "load", comparable=False),
]


def sh(*args, cwd=ROOT):
    try:
        return subprocess.check_output(args, cwd=cwd, text=True, stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def metadata(binary, node, commit, args):
    cpu = None
    try:
        cpu = next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                   if line.startswith("model name"))
    except (OSError, StopIteration):
        pass
    mem_kib = None
    try:
        mem_kib = int(Path("/proc/meminfo").read_text().split()[1])
    except (OSError, ValueError, IndexError):
        pass
    return {
        "complete": False,
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "machine": {"os": platform.system(), "kernel": platform.release(), "arch": platform.machine(),
                    "cpu": cpu, "logical_cpus": os.cpu_count(), "memory_kib": mem_kib,
                    "load_avg_at_start": os.getloadavg()},
        "artifact": {"binary": str(binary), "sha256": sha256(binary), "bytes": binary.stat().st_size,
                     "features": "desktop_rebuild", "profile": "release" if "/release/" in str(binary) else "debug",
                     "bundle_commit": commit, "checkout": sh("git", "rev-parse", "HEAD"),
                     "checkout_dirty": bool(sh("git", "status", "--porcelain"))},
        "toolchain": {"rustc": sh("rustc", "--version"), "node": sh(node, "--version"),
                      "python": platform.python_version()},
        "dataset": {"generator": "web/scripts/synthetic-dataset/generate.mts", "seed": SEED,
                    "simulated_printers": PRINTERS},
        "method": {"runs_per_runtime": args.runs,
                   "order": "interleaved; odd rounds Node first, even rounds Rust first",
                   "settle_secs": args.settle_secs, "idle_secs": args.idle_secs, "plan_iterations": args.plan_iterations,
                   "list_repeats": args.list_repeats, "export_repeats": args.export_repeats,
                   "max_load": args.max_load, "quiet_timeout": args.quiet_timeout},
    }


def idle_sample(server, settle, seconds):
    """Leaves the app alone for `settle` seconds, then counts process-tree
    CPU over `seconds` and samples PSS. Returns PSS, CPU % of one core and
    the process count at both ends."""
    time.sleep(settle)
    ticks_per_second = os.sysconf("SC_CLK_TCK")
    before, started = perfstats.tree_cpu_ticks(server.process.pid), time.perf_counter()
    time.sleep(seconds)
    after = perfstats.tree_cpu_ticks(server.process.pid)
    used = perfstats.cpu_delta(before, after) / ticks_per_second
    pss, counted = perfstats.tree_pss_bytes(server.process.pid)
    return pss, used / (time.perf_counter() - started) * 100, (len(before), len(after), counted)


def wait_for_quiet(max_load, timeout):
    """Waits until the 1-minute load average is at most max_load, for at most
    timeout seconds, and returns the load the run starts at. Other work on the
    machine adds noise to both runtimes; the load is recorded per run."""
    deadline = time.monotonic() + timeout
    while os.getloadavg()[0] > max_load and time.monotonic() < deadline:
        time.sleep(5)
    return os.getloadavg()[0]


def one_run(runtime, run, seeded, work, binary, node, commit, printers, integration_ids, args):
    data = work / f"{runtime}-{run}" / "data"
    logs = args.out / "logs" / f"{runtime}-{run}"
    logs.mkdir(parents=True)
    copy_dataset(seeded, data, node)
    if runtime == "rust":
        mark_rust_owned(data)
    for printer in printers:
        printer.reset()
    out = {}
    server = Server(binary, runtime, data, logs, node, commit)
    try:
        server.start()
        out["startup_ready_ms"] = [server.ready_ms]
        session = server.login()
        _, first_ms = session.json("GET", "/api/v2/plans")
        out["builds_list_first_ms"] = [first_ms]
        out["app_open_ms"] = [server.ready_ms + first_ms]
        pss, cpu, counts = idle_sample(server, args.settle_secs, args.idle_secs)
        if len(set(counts)) != 1:
            raise RuntimeError(f"{runtime}: the process tree changed during the idle window {counts}")
        out["idle_pss_bytes"], out["idle_cpu_pct"], out["idle_processes"] = [pss], [cpu], [counts[0]]
        scenarios.builds_list(session, out, args.list_repeats)
        ids = scenarios.build_ids(session)
        scenarios.thumbnails(session, runtime, out, ids[scenarios.CHECKOFF_BUILD])
        scenarios.plan_change(session, runtime, out, ids[scenarios.PLAN_BUILD], args.plan_iterations)
        scenarios.checkoff_session(session, runtime, out, ids[scenarios.CHECKOFF_BUILD])
        scenarios.exports(session, out, ids[scenarios.CHECKOFF_BUILD], args.export_repeats)
        scenarios.today(server, session, runtime, out, printers[0], integration_ids[0])
        time.sleep(2)
        out["workload_pss_bytes"] = [perfstats.tree_pss_bytes(server.process.pid)[0]]
        out["http_retries"] = [session.retries]
        session.close()
    finally:
        server.stop()
        if args.keep:
            shutil.move(str(data), str(logs / "data"))
        shutil.rmtree(data.parent, ignore_errors=True)
    return out


def prepare(work, binary, node, commit, printers, out):
    """Generates the synthetic dataset, then lets the Node runtime create the
    five simulated printers in it (Rust has no integration-create route)."""
    master = work / "master"
    subprocess.run(["npm", "--prefix", "web", "run", "--silent", "fixture:synthetic", "--",
                    "--seed", str(SEED), str(master)], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    seeded = work / "seeded"
    copy_dataset(master, seeded, node)
    logs = out / "logs" / "seed"
    logs.mkdir(parents=True)
    server = Server(binary, "node", seeded, logs, node, commit)
    try:
        server.start()
        ids = seed_printers(server.login(), printers)
    finally:
        server.stop()
    manifest = json.loads((master / "SYNTHETIC-DATASET.json").read_text())
    return seeded, ids, manifest["digest"]


def report(samples):
    """Rows for perfstats.table, in MEASURES order. A side with no samples
    prints "-"; the row's note says why."""
    rows = []
    for row in MEASURES:
        node, rust = samples["node"].get(row["key"]), samples["rust"].get(row["key"])
        if not node and not rust:
            continue
        note = row["note"] if (row["note"] and (not node or not rust or not row["comparable"])) else ""
        rows.append({"measure": row["label"], "unit": row["unit"], "note": note,
                     "comparable": row["comparable"],
                     "node": perfstats.summarize(node) if node else None,
                     "rust": perfstats.summarize(rust) if rust else None})
    return rows


def run_order(run):
    """Odd rounds start with Node, even rounds with Rust, so drift in page
    cache, clock speed or machine load does not always favour one side."""
    return RUNTIMES if run % 2 else tuple(reversed(RUNTIMES))


def dump_results(meta, samples):
    """JSON with the metadata indented and each sample list on one line, so a
    committed baseline diffs by measure. json.loads reads it back unchanged."""
    lines = ['{"metadata": ' + json.dumps(meta, indent=1).replace("\n", "\n ") + ",", ' "samples": {']
    runtimes = list(samples)
    for index, runtime in enumerate(runtimes):
        lines.append(f"  {json.dumps(runtime)}: {{")
        keys = sorted(samples[runtime])
        for position, key in enumerate(keys):
            values = [round(value, 3) if isinstance(value, float) else value for value in samples[runtime][key]]
            comma = "," if position < len(keys) - 1 else ""
            lines.append(f"   {json.dumps(key)}: {json.dumps(values)}{comma}")
        lines.append("  }" + ("," if index < len(runtimes) - 1 else ""))
    lines.append(" }")
    lines.append("}")
    return "\n".join(lines) + "\n"


def write_results(out, meta, samples):
    (out / "samples.json").write_text(dump_results(meta, samples))
    text = perfstats.table(report(samples))
    (out / "summary.md").write_text(text)
    return text


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True,
                        help="pp-server built with --features desktop_rebuild")
    parser.add_argument("--out", type=Path, required=True, help="new directory for results")
    parser.add_argument("--runs", type=int, default=10, help="fresh starts per runtime (default 10)")
    parser.add_argument("--settle-secs", type=float, default=5,
                        help="wait after the first Builds list before the idle window (default 5)")
    parser.add_argument("--idle-secs", type=float, default=30, help="idle CPU window (default 30)")
    parser.add_argument("--plan-iterations", type=int, default=6)
    parser.add_argument("--list-repeats", type=int, default=10)
    parser.add_argument("--export-repeats", type=int, default=2)
    parser.add_argument("--node", default=shutil.which("node"), help="Node for the Node runtime")
    parser.add_argument("--max-load", type=float, default=float(os.cpu_count() or 2),
                        help="wait before each run until the 1-minute load is at most this (default: the CPU count)")
    parser.add_argument("--quiet-timeout", type=float, default=300,
                        help="longest wait for --max-load, in seconds (default 300)")
    parser.add_argument("--keep", action="store_true", help="keep each run's data copy under OUT/logs")
    args = parser.parse_args(argv)
    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if args.out.exists() and any(args.out.iterdir()):
        parser.error(f"{args.out} is not empty")
    binary = args.binary.resolve()
    commit = json.loads((ROOT / "rust/bundle-manifest.json").read_text())["commit"]
    printers = [SimulatedPrinter() for _ in range(PRINTERS)]
    args.out.mkdir(parents=True, exist_ok=True)
    meta = metadata(binary, args.node, commit, args)
    samples = {runtime: {} for runtime in RUNTIMES}
    try:
        with tempfile.TemporaryDirectory(prefix="pp-perf-baseline-") as temp:
            work = Path(temp)
            seeded, integration_ids, digest = prepare(work, binary, args.node, commit, printers, args.out)
            meta["dataset"]["digest"] = digest
            for run in range(1, args.runs + 1):
                for runtime in run_order(run):
                    load = wait_for_quiet(args.max_load, args.quiet_timeout)
                    samples[runtime].setdefault("load_at_start", []).append(load)
                    print(f"run {run}/{args.runs} {runtime} (load {load:.1f})", file=sys.stderr, flush=True)
                    result = one_run(runtime, run, seeded, work, binary, args.node, commit,
                                     printers, integration_ids, args)
                    for key, values in result.items():
                        samples[runtime].setdefault(key, []).extend(values)
            meta["complete"] = True
    finally:
        meta["simulator_requests"] = [printer.requests for printer in printers]
        meta["load_avg_at_end"] = os.getloadavg()
        for printer in printers:
            printer.close()
        # Partial results survive an aborted run; `complete` says which it is.
        print(write_results(args.out, meta, samples))


if __name__ == "__main__":
    main()
