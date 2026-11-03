import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("operation-inventory.py")
SPEC = importlib.util.spec_from_file_location("operation_inventory", SCRIPT)
INVENTORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INVENTORY)

ARCHITECTURE = """# Architecture

## Operation families and write ownership

| Family | Owner and collaborators |
| --- | --- |
| Admin, legal, logs, diagnostics, health | storage |
| Printers, queue, discovery, Bambu | drivers |

## Next section

| Family | Not a family row |
"""


def route(method, path, operation, owner="compat"):
    return {"method": method, "path": path, "owner": owner, "operation": operation,
            "effect": "observation", "contract": "current", "websocket": False,
            "effect_reason": "test"}


OPERATIONS = {
    "routes": [
        route("GET", "/health", "GET /health current"),
        route("HEAD", "/health", "GET /health current"),
        route("POST", "/printer-send-queue", "POST /printer-send-queue current"),
        route("GET", "/printers", "GET /printers current"),
    ],
    "denied": [{"method": "OPTIONS", "path": "*", "reason": "No CORS"}],
    "gateway_owned": [{"method": "POST", "path": "/auth/logout", "reason": "Local"}],
}
MANIFEST = {
    "schema": "pp-ownership-manifest-v2",
    "operations": [{"operation": "printer.send-queue", "owner": "node",
                    "participants": ["http.POST /printer-send-queue", "worker.queue"]}],
    "resources": {
        "reviewed": [{"operation": "tool.list_printers", "resources": []}],
        "unresolved": ["http:GET /health current", "http:GET /printers current",
                       "printer.send-queue", "mcp.status"],
    },
    "live_formats": {"sqlite-schema": {"node_reads": 34, "rust_writes": 35},
                     "covers": {"node_reads": 1, "rust_writes": "none"}},
}


def roster(**overrides):
    document = {"schema": "pp-inventory-roster-v1", "active_phase": "P1",
                "owners": {"route:GET /health current": {
                    "family": "Admin, legal, logs, diagnostics, health", "ticket": "TEST-1"}},
                "exceptions": []}
    document.update(overrides)
    return document


class InventoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write(INVENTORY.OPERATIONS, OPERATIONS)
        self.write(INVENTORY.MANIFEST, MANIFEST)
        self.write(INVENTORY.ROSTER, roster())
        (self.root / INVENTORY.ARCHITECTURE).write_text(ARCHITECTURE)

    def write(self, relative, document):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(document))

    def run_main(self, *args):
        stderr = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
            code = INVENTORY.main(["--root", str(self.root), *args])
        return code, stderr.getvalue()

    def report(self):
        self.assertEqual(self.run_main()[0], 0)
        return (self.root / INVENTORY.REPORT).read_text()

    def test_families_come_only_from_the_family_table(self):
        self.assertEqual(INVENTORY.architecture_families(ARCHITECTURE),
                         ["Admin, legal, logs, diagnostics, health",
                          "Printers, queue, discovery, Bambu"])

    def test_every_entry_gets_exactly_one_class(self):
        entries = INVENTORY.build_entries(OPERATIONS, MANIFEST)
        families = INVENTORY.architecture_families(ARCHITECTURE)
        INVENTORY.classify(entries, INVENTORY.validate_roster(roster(), families))
        classes = {entry_id: entry["class"] for entry_id, entry in entries.items()}
        self.assertEqual(classes, {
            "route:GET /health current": "rust_owner",
            "route:POST /printer-send-queue current": "uncategorized",
            "route:GET /printers current": "uncategorized",
            "denied:OPTIONS *": "delete_with_node",
            "gateway:POST /auth/logout": "gateway_kept",
            "operation:printer.send-queue": "uncategorized",
            "participant:mcp.status": "uncategorized",
            "participant:tool.list_printers": "uncategorized",
            "format:sqlite-schema": "uncategorized",
            "format:covers": "uncategorized",
        })

    def test_rollup_quotes_the_required_counts(self):
        report = self.report()
        self.assertIn("- Uncategorized entries: 7\n", report)
        self.assertIn("- Kept entries without a Rust owner: 0\n", report)
        self.assertIn("- Routes migrated to Rust: 0 of 4 (4 unmigrated)\n", report)
        self.assertIn("- Journey route misses: not measured", report)
        self.assertIn("Active phase: **P1**", report)
        self.assertIn("| sqlite-schema | 34 | 35 | Rust writes a newer version", report)
        self.assertIn("| covers | 1 | none | Node only", report)
        self.assertIn("| POST /printer-send-queue current | 1 | 0 | printer.send-queue | "
                      "via operation |", report)

    def test_only_a_rust_owner_in_the_inputs_counts_as_migrated(self):
        operations = json.loads(json.dumps(OPERATIONS))
        operations["routes"][3]["owner"] = "rust"
        manifest = json.loads(json.dumps(MANIFEST))
        manifest["operations"][0]["owner"] = "rust"
        self.write(INVENTORY.OPERATIONS, operations)
        self.write(INVENTORY.MANIFEST, manifest)
        self.assertIn("- Routes migrated to Rust: 2 of 4 (2 unmigrated)\n", self.report())

    def test_deferred_entry_without_family_is_kept_without_owner(self):
        self.write(INVENTORY.ROSTER, roster(exceptions=[
            {"entry": "format:covers", "class": "deferred", "ticket": "TEST-2",
             "expires": "20000101", "owner_ok": "owner, 2026-11-03", "reason": "later"}]))
        code, stderr = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("exception for format:covers has expired", stderr)
        report = (self.root / INVENTORY.REPORT).read_text()
        self.assertIn("- Kept entries without a Rust owner: 1\n", report)
        self.assertIn("| format:covers | Deferred, owner OK | TEST-2 | 2000-01-01 |", report)

    def test_stale_roster_entries_are_listed_not_fatal(self):
        owners = roster()["owners"] | {"route:GET /gone current": {
            "family": "Admin, legal, logs, diagnostics, health", "ticket": "TEST-3"}}
        self.write(INVENTORY.ROSTER, roster(owners=owners))
        report = self.report()
        self.assertIn("- Roster entries that match no inventory entry: 1\n", report)
        self.assertIn("- `route:GET /gone current`", report)

    def test_invalid_rosters_are_refused(self):
        family = "Admin, legal, logs, diagnostics, health"
        cases = {
            "unknown family": roster(owners={"route:GET /health current": {
                "family": "Nope", "ticket": "TEST-1"}}),
            "bad ticket": roster(owners={"route:GET /health current": {
                "family": family, "ticket": "1"}}),
            "two classes": roster(exceptions=[
                {"entry": "route:GET /health current", "class": "delete_with_node",
                 "ticket": "TEST-4", "expires": "2027-01-01", "reason": "dup"}]),
            "no expiry": roster(exceptions=[
                {"entry": "format:covers", "class": "delete_with_node", "ticket": "TEST-4",
                 "reason": "x"}]),
            "deferred without owner OK": roster(exceptions=[
                {"entry": "format:covers", "class": "deferred", "ticket": "TEST-4",
                 "expires": "2027-01-01", "reason": "x"}]),
            "fixed class": roster(owners={"denied:OPTIONS *": {
                "family": family, "ticket": "TEST-1"}}),
            "no phase": roster(active_phase=""),
            "owner is not an object": roster(owners={"route:GET /health current": "TEST-1"}),
        }
        for name, document in cases.items():
            with self.subTest(name):
                self.write(INVENTORY.ROSTER, document)
                code, stderr = self.run_main()
                self.assertEqual(code, 2)
                self.assertIn("invalid input", stderr)

    def test_check_fails_only_when_the_report_is_stale(self):
        self.assertEqual(self.run_main("--check")[0], 1)
        first = self.report()
        self.assertEqual(self.report(), first)
        self.assertEqual(self.run_main("--check")[0], 0)
        self.write(INVENTORY.ROSTER, roster(active_phase="P2"))
        code, stderr = self.run_main("--check")
        self.assertEqual(code, 1)
        self.assertIn("is stale", stderr)
        self.assertEqual((self.root / INVENTORY.REPORT).read_text(), first)


if __name__ == "__main__":
    unittest.main()
