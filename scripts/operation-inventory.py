"""Generate the route, operation and format inventory report.

The report measures how much of the Node runtime has a Rust owner. It reads only
generated inputs and the roster, and never edits them:

- rust/crates/pp-gateway/operations.json (routes, denied, gateway_owned)
- rust/crates/pp-gateway/ownership-manifest.json (operations, resource sets,
  live_formats)
- the "Operation families and write ownership" table in docs/architecture.md
- docs/plan/inventory-roster.json (entry -> family and ticket, exceptions)
- docs/parity/journey-route-misses.json, when present (proxy attempts the
  Rust-only gateway journey counted; written by that test with PP_JOURNEY_REPORT=1)

    python3 scripts/operation-inventory.py           # rewrite the report
    python3 scripts/operation-inventory.py --check   # exit 1 if it is stale

Report mode: no CI step runs --check.
"""
import argparse
import datetime
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OPERATIONS = "rust/crates/pp-gateway/operations.json"
MANIFEST = "rust/crates/pp-gateway/ownership-manifest.json"
ARCHITECTURE = "docs/architecture.md"
ROSTER = "docs/plan/inventory-roster.json"
REPORT = "docs/parity/operation-inventory.md"
JOURNEY = "docs/parity/journey-route-misses.json"
FAMILY_HEADING = "## Operation families and write ownership"
ROSTER_SCHEMA = "pp-inventory-roster-v1"
TICKET = re.compile(r"^[A-Z]+-[0-9]+$")

RUST_OWNER = "rust_owner"
DELETE = "delete_with_node"
GATEWAY = "gateway_kept"
DEFERRED = "deferred"
UNCATEGORIZED = "uncategorized"
CLASS_LABELS = {
    RUST_OWNER: "Rust owner + ticket",
    DELETE: "Delete with Node / stays denied",
    GATEWAY: "Gateway-owned, kept",
    DEFERRED: "Deferred, owner OK",
    UNCATEGORIZED: "Uncategorized",
}
CLASSES = tuple(CLASS_LABELS)
KINDS = ("route", "denied", "gateway", "operation", "participant", "format")
EXCEPTION_CLASSES = (DEFERRED, DELETE)


class RosterError(ValueError):
    pass


def architecture_families(text):
    """Return the family names from the architecture family table, in order."""
    families = []
    inside = False
    for line in text.splitlines():
        if line.startswith("## "):
            inside = line.strip() == FAMILY_HEADING
            continue
        if inside and line.startswith("|"):
            cell = line.strip("|").split("|")[0].strip()
            if cell and cell != "Family" and not set(cell) <= set("-: "):
                families.append(cell)
    if not families:
        raise RosterError(f"{ARCHITECTURE} has no rows under {FAMILY_HEADING!r}")
    return families


def is_rust(owner):
    return owner == "rust"


def build_entries(operations, manifest):
    """Return inventory entries keyed by id, before roster classification."""
    op_owner = {op["operation"]: op["owner"] for op in manifest["operations"]}
    member_of = {}
    for op in manifest["operations"]:
        for participant in op["participants"]:
            member_of[participant] = op["operation"]
    resources = manifest["resources"]
    status = {item["operation"]: "reviewed" for item in resources["reviewed"]}
    status.update({name: "unresolved" for name in resources["unresolved"]})

    entries = {}

    def add(kind, name, **fields):
        entries[f"{kind}:{name}"] = {"id": f"{kind}:{name}", "kind": kind, "name": name, **fields}

    def operation_of(route):
        # Fastify answers HEAD with the GET handler, so HEAD inherits the GET claim
        # (rust/crates/pp-gateway/src/ownership/manifest.rs).
        own = member_of.get(f"http.{route['method']} {route['path']}")
        if own is None and route["method"] == "HEAD":
            own = member_of.get(f"http.GET {route['path']}")
        return own

    groups = {}
    for route in operations["routes"]:
        groups.setdefault(route["operation"], []).append(route)
    for name, routes in groups.items():
        ops = sorted({operation_of(r) for r in routes if operation_of(r) is not None})
        migrated = sum(1 for r in routes if is_rust(op_owner.get(operation_of(r), r["owner"])))
        add("route", name, routes=len(routes), migrated=migrated, node_operation=", ".join(ops),
            resources=status.get(f"http:{name}", "via operation" if ops else "none"))
    for route in operations["denied"]:
        add("denied", f"{route['method']} {route['path']}", reason=route["reason"])
    for route in operations["gateway_owned"]:
        add("gateway", f"{route['method']} {route['path']}", reason=route["reason"])
    for op in manifest["operations"]:
        add("operation", op["operation"], owner=op["owner"],
            participants=len(op["participants"]), resources=status.get(op["operation"], "none"))
    for name in sorted(status):
        if name.startswith("http:") or name in op_owner:
            continue
        add("participant", name, node_operation=member_of.get(name, ""), resources=status[name])
    for name, fmt in manifest["live_formats"].items():
        add("format", name, node_reads=fmt["node_reads"], rust_writes=fmt["rust_writes"])
    return entries


def check_date(value, where):
    try:
        return datetime.date.fromisoformat(value)
    except (TypeError, ValueError):
        raise RosterError(f"{where}: expires must be YYYY-MM-DD, got {value!r}") from None


def check_ticket(value, where):
    if not isinstance(value, str) or not TICKET.match(value):
        raise RosterError(f"{where}: ticket must be a Linear ID such as ABC-123, got {value!r}")


def validate_roster(roster, families):
    """Return {entry id: assignment} or raise RosterError."""
    if roster.get("schema") != ROSTER_SCHEMA:
        raise RosterError(f"roster schema must be {ROSTER_SCHEMA!r}")
    if not isinstance(roster.get("active_phase"), str) or not roster["active_phase"]:
        raise RosterError("roster must name active_phase")
    assignments = {}
    for entry_id, owner in sorted(roster.get("owners", {}).items()):
        where = f"owners[{entry_id!r}]"
        check_ticket(owner.get("ticket"), where)
        if owner.get("family") not in families:
            raise RosterError(f"{where}: family {owner.get('family')!r} is not in the "
                              "architecture family table")
        assignments[entry_id] = {"class": RUST_OWNER, "family": owner["family"],
                                 "ticket": owner["ticket"]}
    for index, exception in enumerate(roster.get("exceptions", [])):
        entry_id = exception.get("entry")
        where = f"exceptions[{index}] ({entry_id!r})"
        if exception.get("class") not in EXCEPTION_CLASSES:
            raise RosterError(f"{where}: class must be one of {EXCEPTION_CLASSES}")
        check_ticket(exception.get("ticket"), where)
        expires = check_date(exception.get("expires"), where).isoformat()
        if not exception.get("reason"):
            raise RosterError(f"{where}: reason is required")
        if exception["class"] == DEFERRED and not exception.get("owner_ok"):
            raise RosterError(f"{where}: a deferred entry needs owner_ok")
        family = exception.get("family", "")
        if family and family not in families:
            raise RosterError(f"{where}: family {family!r} is not in the architecture family table")
        if entry_id in assignments:
            raise RosterError(f"{where}: entry already has a class; each entry gets exactly one")
        assignments[entry_id] = {"class": exception["class"], "family": family,
                                 "ticket": exception["ticket"], "expires": expires,
                                 "owner_ok": exception.get("owner_ok", ""),
                                 "reason": exception["reason"]}
    for entry_id in assignments:
        if entry_id.startswith(("denied:", "gateway:")):
            raise RosterError(f"{entry_id!r} has a fixed class; remove it from the roster")
    return assignments


def classify(entries, assignments):
    """Give each entry exactly one class. Return roster ids with no entry."""
    for entry in entries.values():
        if entry["kind"] == "denied":
            entry.update({"class": DELETE, "family": "", "ticket": ""})
        elif entry["kind"] == "gateway":
            entry.update({"class": GATEWAY, "family": "pp-gateway", "ticket": ""})
        else:
            entry.update(assignments.get(entry["id"],
                                         {"class": UNCATEGORIZED, "family": "", "ticket": ""}))
    return sorted(set(assignments) - set(entries))


def format_status(entry):
    node, rust = entry["node_reads"], entry["rust_writes"]
    if rust == "none":
        return "Node only; Rust writes nothing"
    if node == rust:
        return "in step"
    if isinstance(node, int) and isinstance(rust, int) and rust > node:
        return "Rust writes a newer version than Node reads"
    return "Rust writes an older version than Node reads"


def journey_line(journey):
    """The rollup line for the journey report, or 'not measured' without one."""
    if journey is None:
        return f"- Journey route misses: not measured (no {JOURNEY})"
    runs = journey.get("runs") if isinstance(journey, dict) else None
    if not runs or not isinstance(runs, list):
        raise RosterError(f"{JOURNEY} must list runs")
    for run in runs:
        attempts = run.get("proxy_attempts") if isinstance(run, dict) else None
        # type() rather than isinstance(): JSON true is a bool, which isinstance counts as int.
        if type(attempts) is not int or attempts < 0 \
                or not isinstance(run.get("PP_DESKTOP_REBUILD"), str):
            raise RosterError(f"{JOURNEY}: each run needs PP_DESKTOP_REBUILD and a "
                              f"non-negative proxy_attempts, got {run!r}")
    total = sum(run["proxy_attempts"] for run in runs)
    values = ", ".join(f"`{run['PP_DESKTOP_REBUILD']}`" for run in runs)
    return (f"- Journey route misses: {total} proxy attempts in the Rust-only gateway journey "
            f"with PP_DESKTOP_REBUILD={values} (from `{JOURNEY}`)")


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def cell(value):
    return str(value).replace("|", "\\|") if value not in ("", None) else "-"


def table(headers, rows):
    lines = ["| " + " | ".join(headers) + " |", "|" + "---|" * len(headers)]
    lines += ["| " + " | ".join(cell(value) for value in row) + " |" for row in rows]
    return lines


def summarize(entries):
    by_kind = {kind: [e for e in entries.values() if e["kind"] == kind] for kind in KINDS}
    kept = [e for e in entries.values() if e["class"] in (RUST_OWNER, GATEWAY, DEFERRED)]
    return {
        "by_kind": by_kind,
        "uncategorized": sum(e["class"] == UNCATEGORIZED for e in entries.values()),
        "kept_without_owner": sum(not e["family"] for e in kept),
        "routes": sum(e["routes"] for e in by_kind["route"]),
        "migrated": sum(e["migrated"] for e in by_kind["route"]),
    }


def render(entries, stale, roster, digests, journey=None):
    s = summarize(entries)
    by_kind = s["by_kind"]
    out = [
        "# Operation inventory",
        "",
        "Generated by `python3 scripts/operation-inventory.py`. Do not edit by hand;",
        "change the roster or regenerate the inputs, then rerun the script.",
        "[Operation inventory](../development/operation-inventory.md) explains each field.",
        "",
        f"Active phase: **{roster['active_phase']}**",
        "",
        "## Rollup",
        "",
        f"- Uncategorized entries: {s['uncategorized']}",
        f"- Kept entries without a Rust owner: {s['kept_without_owner']}",
        f"- Routes migrated to Rust: {s['migrated']} of {s['routes']}"
        f" ({s['routes'] - s['migrated']} unmigrated)",
        journey_line(journey),
        f"- Roster entries that match no inventory entry: {len(stale)}",
        "",
    ]
    rows = [[kind, len(by_kind[kind])]
            + [sum(e["class"] == c for e in by_kind[kind]) for c in CLASSES] for kind in KINDS]
    rows.append(["total", len(entries)]
                + [sum(e["class"] == c for e in entries.values()) for c in CLASSES])
    out += table(["Kind", "Entries"] + [CLASS_LABELS[c] for c in CLASSES], rows) + [""]

    out += ["## Live formats", ""]
    out += table(["Format", "node_reads", "rust_writes", "Status", "Class", "Ticket"],
                 [[e["name"], e["node_reads"], e["rust_writes"], format_status(e),
                   CLASS_LABELS[e["class"]], e["ticket"]] for e in by_kind["format"]]) + [""]

    exceptions = sorted((e for e in entries.values() if "expires" in e),
                        key=lambda e: (e["expires"], e["id"]))
    out += ["## Exceptions", ""]
    out += table(["Entry", "Class", "Ticket", "Expires", "Owner OK", "Reason"],
                 [[e["id"], CLASS_LABELS[e["class"]], e["ticket"], e["expires"], e["owner_ok"],
                   e["reason"]] for e in exceptions]) if exceptions else ["None."]
    out += [""]
    if stale:
        out += ["## Roster entries with no inventory entry", ""]
        out += [f"- `{entry_id}`" for entry_id in stale] + [""]

    def ordered(kind):
        return sorted(by_kind[kind], key=lambda e: e["name"])

    out += ["## Route groups", "",
            "A route counts as migrated only when its owner in the generated inputs is Rust.", ""]
    out += table(["Group", "Routes", "Migrated", "Node operation", "Resources", "Class",
                  "Family", "Ticket"],
                 [[e["name"], e["routes"], e["migrated"], e["node_operation"], e["resources"],
                   CLASS_LABELS[e["class"]], e["family"], e["ticket"]]
                  for e in ordered("route")]) + [""]
    out += ["## Node operations", ""]
    out += table(["Operation", "Owner", "Participants", "Resources", "Class", "Family", "Ticket"],
                 [[e["name"], e["owner"], e["participants"], e["resources"],
                   CLASS_LABELS[e["class"]], e["family"], e["ticket"]]
                  for e in ordered("operation")]) + [""]
    out += ["## Non-HTTP participants", ""]
    out += table(["Participant", "Node operation", "Resources", "Class", "Family", "Ticket"],
                 [[e["name"], e["node_operation"], e["resources"], CLASS_LABELS[e["class"]],
                   e["family"], e["ticket"]] for e in ordered("participant")]) + [""]
    out += ["## Gateway-owned and denied routes", ""]
    out += table(["Route", "Class", "Reason"],
                 [[e["name"], CLASS_LABELS[e["class"]], e["reason"]]
                  for e in ordered("gateway") + ordered("denied")]) + [""]
    out += ["## Inputs", ""]
    out += table(["Input", "SHA-256"], sorted(digests.items())) + [""]
    return "\n".join(out)


def unique_keys(pairs):
    """object_pairs_hook that refuses duplicate keys instead of keeping the last one."""
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise RosterError(f"roster repeats the key {key!r}")
        seen[key] = value
    return seen


def generate(root):
    texts = {path: (root / path).read_text() for path in (OPERATIONS, MANIFEST, ARCHITECTURE, ROSTER)}
    families = architecture_families(texts[ARCHITECTURE])
    roster = json.loads(texts[ROSTER], object_pairs_hook=unique_keys)
    entries = build_entries(json.loads(texts[OPERATIONS]), json.loads(texts[MANIFEST]))
    stale = classify(entries, validate_roster(roster, families))
    digests = {path: digest(text) for path, text in texts.items() if path != ARCHITECTURE}
    digests[f"{ARCHITECTURE} (family table)"] = digest("\n".join(families))
    journey = None
    if (root / JOURNEY).is_file():
        text = (root / JOURNEY).read_text()
        journey = json.loads(text, object_pairs_hook=unique_keys)
        digests[JOURNEY] = digest(text)
    expired = sorted(e["id"] for e in entries.values()
                     if "expires" in e and e["expires"] < datetime.date.today().isoformat())
    return render(entries, stale, roster, digests, journey), expired


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--check", action="store_true",
                        help="exit 1 when the committed report is stale; write nothing")
    args = parser.parse_args(argv)
    try:
        report, expired = generate(args.root)
    except (RosterError, KeyError, TypeError, AttributeError, json.JSONDecodeError) as error:
        print(f"operation-inventory: invalid input: {error}", file=sys.stderr)
        return 2
    for entry_id in expired:
        print(f"operation-inventory: warning: exception for {entry_id} has expired",
              file=sys.stderr)
    target = args.root / REPORT
    if args.check:
        if not target.exists() or target.read_text() != report:
            print(f"operation-inventory: {REPORT} is stale; run "
                  "python3 scripts/operation-inventory.py", file=sys.stderr)
            return 1
        print(f"operation-inventory: {REPORT} is current")
        return 0
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(report)
    print(f"operation-inventory: wrote {REPORT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
