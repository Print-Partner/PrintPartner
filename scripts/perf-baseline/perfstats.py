"""Pure helpers for the Node-versus-Rust baseline: statistics, Linux PSS of a
process tree, and the Markdown table. No network, no child processes."""

import math
import os
from pathlib import Path


def percentile(samples, pct):
    """Nearest-rank percentile: the smallest sample with at least pct% of the
    samples at or below it. Needs at least one sample."""
    if not samples:
        raise ValueError("no samples")
    if not 0 < pct <= 100:
        raise ValueError("pct must be in (0, 100]")
    ordered = sorted(samples)
    return ordered[max(0, math.ceil(pct / 100 * len(ordered)) - 1)]


def median(samples):
    if not samples:
        raise ValueError("no samples")
    ordered = sorted(samples)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def summarize(samples):
    """Median, nearest-rank p95, min, max and count. The spread is
    (p95 - median) / median in percent, the noise figure the report prints."""
    med = median(samples)
    p95 = percentile(samples, 95)
    return {
        "n": len(samples),
        "median": med,
        "p95": p95,
        "min": min(samples),
        "max": max(samples),
        "spread_pct": 0.0 if med == 0 else (p95 - med) / med * 100,
    }


def _children(proc, pid):
    """Child PIDs of pid from every thread's children file. A thread or
    process that exits while it is read is skipped."""
    found = []
    try:
        threads = os.listdir(proc / str(pid) / "task")
    except OSError:
        return found
    for tid in threads:
        try:
            text = (proc / str(pid) / "task" / tid / "children").read_text()
        except OSError:
            continue
        found.extend(int(child) for child in text.split())
    return found


def tree_pids(root_pid, proc=Path("/proc")):
    """root_pid and every descendant, breadth first, each once."""
    seen, queue = [], [root_pid]
    while queue:
        pid = queue.pop(0)
        if pid in seen:
            continue
        seen.append(pid)
        queue.extend(_children(proc, pid))
    return seen


def pss_kib(pid, proc=Path("/proc")):
    """Pss from /proc/PID/smaps_rollup in KiB, or None when the process is gone."""
    try:
        text = (proc / str(pid) / "smaps_rollup").read_text()
    except OSError:
        return None
    for line in text.splitlines():
        if line.startswith("Pss:"):
            return int(line.split()[1])
    return None


def tree_pss_bytes(root_pid, proc=Path("/proc")):
    """Summed PSS of root_pid and its descendants, and how many processes
    contributed. Processes that exit during the walk are left out."""
    total, counted = 0, 0
    for pid in tree_pids(root_pid, proc):
        kib = pss_kib(pid, proc)
        if kib is not None:
            total += kib * 1024
            counted += 1
    return total, counted


def stat_ticks(text):
    """utime + stime + cutime + cstime from one /proc/PID/stat line. The
    command name may hold spaces and parentheses, so fields are counted after
    the last ')'. cutime and cstime hold the ticks of children it reaped."""
    fields = text.rsplit(")", 1)[1].split()
    # fields[0] is the state (field 3), so utime (field 14) is fields[11].
    return sum(int(fields[index]) for index in (11, 12, 13, 14))


def tree_cpu_ticks(root_pid, proc=Path("/proc")):
    """{pid: ticks} for root_pid and its descendants still running."""
    ticks = {}
    for pid in tree_pids(root_pid, proc):
        try:
            ticks[pid] = stat_ticks((proc / str(pid) / "stat").read_text())
        except (OSError, IndexError, ValueError):
            continue
    return ticks


def cpu_delta(before, after):
    """Ticks used between two tree_cpu_ticks readings. A process present in
    both counts its difference, a new one all its ticks. A process that exits
    in the window loses its ticks, or moves them all into its parent's
    cutime, so a window with exits can be off; the harness records the
    process count at both ends. Never negative."""
    return sum(max(0, ticks - before.get(pid, 0)) for pid, ticks in after.items())


def fmt_value(value, unit):
    if value is None:
        return "-"
    if unit == "bytes":
        return f"{value / (1024 * 1024):.1f} MiB"
    if unit == "ms":
        return f"{value:.1f} ms" if value < 100 else f"{value:.0f} ms"
    if unit == "%":
        return f"{value:.2f} %"
    if unit == "load":
        return f"{value:.1f}"
    return f"{value:g} {unit}"


def ratio(node, rust):
    """Rust as a fraction of Node, as text. Below 1.00 means Rust was faster
    or smaller."""
    if node in (None, 0) or rust is None:
        return "-"
    return f"{rust / node:.2f}x"


# Below this many samples the nearest-rank p95 is the maximum.
P95_MIN_SAMPLES = 20


def table(rows):
    """Markdown table from rows of {"measure", "unit", "node": summary|None,
    "rust": summary|None, "note", "comparable"}. A row whose two sides send
    different requests has comparable False and no ratio. A p95 cell from
    fewer than P95_MIN_SAMPLES samples is the maximum and says so."""
    lines = [
        "| Measure | Node median | Node p95 | Rust median | Rust p95 | Rust/Node (median) | n (Node/Rust) | Note |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    for row in rows:
        node, rust, unit = row.get("node"), row.get("rust"), row["unit"]

        def cell(side, key):
            if not side:
                return "-"
            text = fmt_value(side[key], unit)
            return f"{text} (max)" if key == "p95" and side["n"] < P95_MIN_SAMPLES else text

        counts = f"{node['n'] if node else 0}/{rust['n'] if rust else 0}"
        compared = ratio(node and node["median"], rust and rust["median"]) if row.get("comparable", True) else "-"
        lines.append(
            f"| {row['measure']} | {cell(node, 'median')} | {cell(node, 'p95')} | "
            f"{cell(rust, 'median')} | {cell(rust, 'p95')} | {compared} | {counts} | {row.get('note', '')} |"
        )
    return "\n".join(lines) + "\n"
