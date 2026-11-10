"""Exercise family-index validation with disposable repositories."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "check_family_index", Path(__file__).with_name("check-family-index.py"))
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class FamilyIndexTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "docs").mkdir()
        (self.root / "src").mkdir()
        (self.root / "src/module.rs").touch()
        (self.root / "src/routes.rs").touch()
        (self.root / "docs/guide.md").write_text("# Guide\n")
        (self.root / "docs/architecture.md").write_text(
            "## Operation families and write ownership\n\n"
            "| Family | Owner |\n| --- | --- |\n| Builds | storage |\n")
        self.row = ("| Builds | [module](../src/module.rs) | "
                    "[route](../src/routes.rs) | `package(pp-storage)` | "
                    "[guide](guide.md) | Rust |\n")
        self.write(self.row)

    def write(self, row):
        (self.root / "docs/families.md").write_text(
            "# Families\n\n| Family | Module | Route | Filter | Doc | Check |\n"
            "| --- | --- | --- | --- | --- | --- |\n" + row)

    def test_existing_paths(self):
        self.assertEqual(CHECKER.check(self.root), [])

    def test_missing_module_route_or_doc(self):
        for name in ("src/module.rs", "src/routes.rs", "docs/guide.md"):
            with self.subTest(name=name):
                path = self.root / name
                original = path.read_text()
                path.unlink()
                self.assertTrue(any("Missing" in e for e in CHECKER.check(self.root)))
                path.write_text(original)

    def test_missing_extra_and_duplicate_family(self):
        for row in ("", self.row + self.row,
                    self.row + self.row.replace("Builds", "Unknown")):
            with self.subTest(row=row):
                self.write(row)
                self.assertTrue(CHECKER.check(self.root))

    def test_path_must_be_linked(self):
        self.write(self.row.replace("[module](../src/module.rs)", "`src/missing.rs`"))
        self.assertTrue(CHECKER.check(self.root))

    def test_outside_checkout(self):
        self.write(self.row.replace("../src/module.rs", "../../"))
        self.assertTrue(CHECKER.check(self.root))

    def test_deferred_family(self):
        self.write("| Builds | None; deferred | None; deferred | None; no coverage | "
                   "[guide](guide.md) | Index |\n")
        self.assertEqual(CHECKER.check(self.root), [])

    def test_missing_index(self):
        (self.root / "docs/families.md").unlink()
        self.assertTrue(CHECKER.check(self.root))

    def test_malformed_row(self):
        self.write("| Builds | too few cells |\n")
        self.assertTrue(CHECKER.check(self.root))


if __name__ == "__main__":
    unittest.main()
