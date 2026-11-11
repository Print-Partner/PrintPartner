"""Keep the A8 byte comparison strict outside the enumerated link repairs."""

import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "verify_a8_record_move", Path(__file__).with_name("verify-a8-record-move.py"))
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class RecordMoveTests(unittest.TestCase):
    path = "docs/records/d19-publication-client.md"
    original = b"# Receipt\n\nSee [events](domain-events.md).\n"
    repaired = b"# Receipt\n\nSee [events](../domain-events.md).\n"

    def test_link_target_edit_passes(self):
        VERIFY.verify_bytes(self.path, self.original, self.repaired)

    def test_non_link_edits_fail(self):
        for changed in (
            self.repaired.replace(b"Receipt", b"Changed"),
            self.repaired.replace(b"See", b"Read"),
            self.repaired.replace(b"[events]", b"[other label]"),
            self.repaired.replace(b"See ", b"See  "),
            self.repaired + b"\n",
            self.repaired.replace(b"\n", b"\r\n"),
        ):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                VERIFY.verify_bytes(self.path, self.original, changed)

    def test_listed_text_rewrite_passes_and_others_fail(self):
        path = "docs/records/d9-build-graph.md"
        original = b"# Receipt\n\nGRE-604 D10b, described below.\n"
        repaired = (b"# Receipt\n\nGRE-604 D10b, described in "
                    b"[D10b manifest observation receipt](d10b-manifest-observation.md).\n")
        VERIFY.verify_bytes(path, original, repaired)
        with self.assertRaises(ValueError):
            VERIFY.verify_bytes(path, original, repaired.replace(b"described in", b"covered in"))
        with self.assertRaises(ValueError):
            VERIFY.verify_bytes(path, b"# Receipt\n", b"# Receipt\n")

    def test_unlisted_target_edit_fails(self):
        with self.assertRaises(ValueError):
            VERIFY.verify_bytes(self.path, self.original,
                                self.repaired.replace(b"../domain-events.md", b"../storage.md"))

    def test_same_text_outside_target_cannot_change(self):
        suffix = b"\ndomain-events.md\n"
        with self.assertRaises(ValueError):
            VERIFY.verify_bytes(self.path, self.original + suffix,
                                self.repaired + b"\n../domain-events.md\n")

    def test_rewrite_is_scoped_to_file(self):
        with self.assertRaises(ValueError):
            VERIFY.verify_bytes("docs/records/d10a-manifest-model.md",
                                self.original, self.repaired)

    def test_unchanged_record_passes(self):
        VERIFY.verify_bytes("docs/records/d10a-manifest-model.md",
                            self.original, self.original)

    def test_actual_records_match_parent_with_repairs(self):
        _, sections = VERIFY.parent_sections()
        for _, slug, _, _, section in sections:
            path = f"docs/records/{slug}.md"
            with self.subTest(path=path):
                VERIFY.verify_bytes(path, section[1:], (VERIFY.ROOT / path).read_bytes())


if __name__ == "__main__":
    unittest.main()
