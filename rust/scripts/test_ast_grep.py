"""Exercise the real ast-grep ratchet, including file and crate exemptions."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


RUST = Path(__file__).resolve().parents[1]
RAISE_OVERRIDE = "PP_AST_GREP_ALLOW_BASELINE_RAISE"


SPEC = importlib.util.spec_from_file_location("ast_checker", RUST / "scripts/check-ast-grep.py")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class TextGateTests(unittest.TestCase):
    def test_only_cfg_attr_predicate_counts(self):
        for text, expected in (
            ('#[cfg_attr(all(feature="desktop_rebuild", unix), allow(unused))] let x=1;', 1),
            ('#[cfg_attr(test, doc="desktop_rebuild")] struct S;', 0),
            ('#[r#cfg(feature=r###"desktop_rebuild"###)] fn f() {}', 1),
        ):
            with self.subTest(text=text):
                self.assertEqual(len(CHECKER.text_gates(text.encode())), expected)

    def test_nested_cfg_attributes_and_doc_literals(self):
        for text, expected in (
            ('#[cfg_attr(all(), cfg(feature="desktop_rebuild"))]', 1),
            ('#[cfg_attr(not(any()), cfg_attr(feature="desktop_rebuild", cfg(all())))]', 1),
            ('#![cfg_attr(not(test), cfg(feature="desktop_rebuild"))]', 1),
            ('#[cfg_attr(all(), cfg_attr(test, doc="desktop_rebuild"))]', 0),
            ('#[cfg_attr(feature="desktop_rebuild", cfg(feature="desktop_rebuild"))]', 1),
        ):
            with self.subTest(text=text):
                self.assertEqual(len(CHECKER.text_gates(text.encode())), expected)

    def test_escaped_feature_values_are_decoded(self):
        for literal in (rb'"desktop\x5frebuild"', rb'"desktop\u{5f}rebuild"',
                        rb'"desktop_rebui\u{6c}d"'):
            code = b"#[cfg(feature=" + literal + b")] fn f() {}"
            self.assertEqual(CHECKER.text_gates(code), {0})

    def test_non_code_and_nested_comments_do_not_count(self):
        gate = '#[cfg(feature="desktop_rebuild")]'
        text = f'// {gate}\n/* outer /* {gate} */ end */ const S: &str = r##"{gate}"##;'
        self.assertEqual(CHECKER.text_gates(text.encode()), set())
        for prefix in ("r", "br", "cr"):
            text = f'const S = {prefix}###"quoted " text {gate}"###;'
            self.assertEqual(CHECKER.text_gates(text.encode()), set())

    def test_character_literals_and_unicode_preserve_gate_offsets(self):
        text = """const C: char = '"'; /* é */ #[cfg(feature="desktop_rebuild")] fn f() {}"""
        data = text.encode()
        self.assertEqual(CHECKER.text_gates(data), {data.index(b"#")})


class RatchetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pp-ast-grep-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        shutil.copy(RUST / "sgconfig.yml", self.root)
        shutil.copytree(RUST / "rules", self.root / "rules")
        script = self.root / "scripts" / "check-ast-grep.py"
        script.parent.mkdir()
        shutil.copy(RUST / "scripts" / "check-ast-grep.py", script)
        self.source("pp-source/src/lib.rs", "fn production() { value.unwrap(); }")
        self.git("init", "-q", "-b", "main")
        self.publish_main(unwrap=1)

    def source(self, path, text):
        file = self.root / "crates" / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def placement_baseline(self):
        counts = getattr(self, "placements", {
            p.name: dict.fromkeys(("module", "item", "statement"), 0)
            for p in (self.root / "crates").iterdir()
        })
        directory = self.root / "desktop-rebuild-baseline"
        directory.mkdir(exist_ok=True)
        for crate, values in counts.items():
            (directory / f"{crate}.json").write_text(json.dumps({crate: values}))
        (self.root / "desktop-rebuild-baseline.json").unlink(missing_ok=True)

    def baseline(self, unwrap=0, command=0):
        self.placement_baseline()
        (self.root / "ast-grep-baseline.json").write_text(json.dumps({
            "no-unwrap-expect": unwrap,
            "runtime-process-command": command,
            "no-writer-family-literal": 0,
        }))

    def git(self, *args):
        subprocess.run(
            ["git", "-c", "user.name=test", "-c", "user.email=test@example.invalid",
             "-c", "commit.gpgsign=false", *args],
            cwd=self.root, check=True, capture_output=True, text=True,
        )

    def publish_main(self, unwrap=0, command=0):
        """Commit the current tree as origin/main, the reference for the baseline rise check."""
        self.baseline(unwrap=unwrap, command=command)
        self.commit_main()

    def commit_main(self):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", "main")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def publish_shared_placements(self, counts):
        shutil.rmtree(self.root / "desktop-rebuild-baseline")
        (self.root / "desktop-rebuild-baseline.json").write_text(json.dumps(counts))
        self.commit_main()

    def check(self, allow_raise=False, *, write_placement=True, arguments=()):
        if write_placement:
            self.placement_baseline()
        environment = {key: value for key, value in os.environ.items() if key != RAISE_OVERRIDE}
        if allow_raise:
            environment[RAISE_OVERRIDE] = "1"
        return subprocess.run(
            ["python3", str(self.root / "scripts" / "check-ast-grep.py"), *arguments],
            text=True,
            capture_output=True,
            env=environment,
        )

    def test_statement_gate_addition_fails(self):
        self.source("pp-source/src/lib.rs",
                    'fn f() { #[cfg(feature = "desktop_rebuild")] let x = 1; }')
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement=1", result.stdout)

    def test_escaped_statement_gate_is_classified(self):
        self.source("pp-source/src/lib.rs",
                    r'fn f() { #[cfg(feature="desktop\x5frebuild")] let x=1; }')
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement=1", result.stdout)

    def test_statement_baseline_raise_cannot_hide_addition(self):
        self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 0}}
        self.publish_main()
        self.source("pp-source/src/lib.rs",
                    'fn f() { #[cfg(feature = "desktop_rebuild")] let x = 1; }')
        self.placements["pp-source"]["statement"] = 1
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement baseline raised", result.stdout)

    def test_module_and_item_baselines_can_grow(self):
        self.source("pp-source/src/lib.rs",
                    '#[cfg(feature = "desktop_rebuild")] mod m;'
                    '#[cfg(feature = "desktop_rebuild")] struct S;')
        self.placements = {"pp-source": {"module": 1, "item": 1, "statement": 0}}
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_statement_count_is_per_crate(self):
        self.source("pp-source/src/lib.rs",
                    'fn f() { #[cfg(feature = "desktop_rebuild")] let x = 1; }')
        self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 1}}
        self.publish_main()
        self.source("pp-source/src/lib.rs", "fn f() {}")
        self.source("pp-other/src/lib.rs",
                    'fn f() { #[cfg(feature = "desktop_rebuild")] let x = 1; }')
        self.placements = {
            "pp-source": {"module": 0, "item": 0, "statement": 0},
            "pp-other": {"module": 0, "item": 0, "statement": 1},
        }
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("pp-other: statement baseline raised", result.stdout)

    def test_ambiguous_item_forms_remain_frozen_as_statements(self):
        self.source("pp-source/src/lib.rs",
                    '#[cfg(feature="desktop_rebuild")] extern crate foo;'
                    '#[cfg(feature="desktop_rebuild")] node_case!(x);'
                    'impl S { #![cfg(feature="desktop_rebuild")] fn f() {} }'
                    'trait T { #![cfg(feature="desktop_rebuild")] fn f(); }'
                    'extern "C" { #![cfg(feature="desktop_rebuild")] fn f(); }')
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement=5", result.stdout)

    def test_nested_conditional_statement_gates_cannot_bypass_freeze(self):
        for attr in (
            '#[cfg_attr(all(), cfg(feature = "desktop_rebuild"))]',
            '#[cfg_attr(not(any()), cfg_attr(feature = "desktop_rebuild", cfg(all())))]',
            '#![cfg_attr(not(test), cfg(feature = "desktop_rebuild"))]',
        ):
            with self.subTest(attr=attr):
                self.source("pp-source/src/lib.rs", f"fn f() {{ {attr} let x=1; }}")
                self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 1}}
                self.baseline()
                result = self.check(allow_raise=True)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("statement=1", result.stdout)
                self.assertIn("statement baseline raised", result.stdout)

    def test_inner_conditional_crate_gate_is_counted_once(self):
        self.source("pp-source/src/lib.rs",
                    '#![cfg_attr(not(test), cfg(feature = "desktop_rebuild"))] fn f() {}')
        self.placements = {"pp-source": {"module": 1, "item": 0, "statement": 0}}
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("1 classified = 1 text gates", result.stdout)

    def test_ast_hit_without_text_gate_fails_coverage(self):
        self.source("pp-source/src/lib.rs", '#[cfg(feature="other_feature")] struct S;')
        self.baseline()
        rule = self.root / "rules/desktop-rebuild-item.yml"
        # Deliberately broaden one AST rule to a non-desktop feature.
        text = rule.read_text().replace("  gate:\n", "  gate:\n    kind: attribute_item\n  unused-gate:\n", 1)
        rule.write_text(text)
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("AST placement hits have no text gate", result.stderr)

    def test_macro_token_gates_count_as_statements(self):
        for code in (
            'fn f() { json!({ #[cfg(feature="desktop_rebuild")] "x": 1 }); }',
            'fn f() { vec![#[cfg(feature="desktop_rebuild")] 1]; }',
            'fn f() { select! { #[cfg(feature="desktop_rebuild")] x = f() => x }; }',
            'macro_rules! m { () => { #[cfg(feature="desktop_rebuild")] let x=1; } }',
        ):
            with self.subTest(code=code):
                self.source("pp-source/src/lib.rs", code)
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("statement=1", result.stdout)

    def test_single_rule_cannot_drop_or_double_classify_a_gate(self):
        self.source("pp-source/src/lib.rs",
                    '#[cfg(feature="desktop_rebuild")] struct S;')
        self.baseline()
        rule = self.root / "rules/desktop-rebuild-item.yml"
        original = rule.read_text()
        for replacement in ("      - kind: function_item", "      - matches: gate"):
            with self.subTest(replacement=replacement):
                # Drop item coverage, or make the module rule overlap it.
                target = rule if "kind:" in replacement else self.root / "rules/desktop-rebuild-module.yml"
                before = target.read_text()
                target.write_text(before.replace(
                    "      - matches: item" if target == rule else "      - matches: module",
                    replacement))
                result = self.check()
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("placement coverage expected one class", result.stderr)
                target.write_text(before)
        rule.write_text(original)

    def test_cfg_attr_body_is_not_a_feature_predicate(self):
        for attr in (
            '#[cfg_attr(test, doc="desktop_rebuild")]',
            '#[cfg_attr(all(), cfg_attr(test, doc="desktop_rebuild"))]',
        ):
            with self.subTest(attr=attr):
                self.source("pp-source/src/lib.rs", f"{attr} struct S;")
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("0 classified = 0 text gates", result.stdout)

    def test_placement_first_landing_is_reported(self):
        shutil.rmtree(self.root / "desktop-rebuild-baseline")
        self.commit_main()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("desktop-rebuild raise check skipped (first landing)", result.stdout)

    def test_shared_placement_migration_keeps_the_statement_cap(self):
        self.source("pp-source/src/lib.rs",
                    'fn f() { #[cfg(feature="desktop_rebuild")] let x=1; }')
        self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 1}}
        self.publish_main()
        self.publish_shared_placements(self.placements)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("layout migration", result.stdout)
        self.source("pp-source/src/lib.rs",
                    'fn f() { #[cfg(feature="desktop_rebuild")] let x=1;'
                    ' #[cfg(feature="desktop_rebuild")] let y=2; }')
        self.placements["pp-source"]["statement"] = 2
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement baseline raised over origin/main (1 -> 2)", result.stdout)

    def test_shared_placement_migration_allows_module_and_item_growth(self):
        self.publish_shared_placements({"pp-source": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)})
        self.source("pp-source/src/lib.rs",
                    'fn production() { value.unwrap(); }'
                    '#[cfg(feature="desktop_rebuild")] mod m;'
                    '#[cfg(feature="desktop_rebuild")] struct S;')
        self.placements = {"pp-source": {"module": 1, "item": 1, "statement": 0}}
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_invalid_shared_main_data_cannot_skip_the_raise_check(self):
        self.publish_shared_placements({"pp-source": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)})
        (self.root / "desktop-rebuild-baseline.json").write_text("[]")
        self.commit_main()
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("placement baseline must map crates", result.stderr)
        self.assertNotIn("raise check skipped", result.stdout)

    def test_split_main_does_not_fall_back_to_a_shared_crate_allowance(self):
        (self.root / "desktop-rebuild-baseline.json").write_text(json.dumps({
            "pp-other": {"module": 0, "item": 0, "statement": 9},
        }))
        self.commit_main()
        self.source("pp-other/src/lib.rs",
                    'fn f() { #[cfg(feature="desktop_rebuild")] let x=1; }')
        self.placements = {
            "pp-source": {"module": 0, "item": 0, "statement": 0},
            "pp-other": {"module": 0, "item": 0, "statement": 1},
        }
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("pp-other: statement baseline raised over origin/main (0 -> 1)", result.stdout)

    def test_first_placement_landing_can_initialize_statement_counts(self):
        shutil.rmtree(self.root / "desktop-rebuild-baseline")
        self.commit_main()
        self.source("pp-source/src/lib.rs",
                    'fn production() { value.unwrap(); }'
                    'fn f() { #[cfg(feature="desktop_rebuild")] let x=1; }')
        self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 1}}
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("desktop-rebuild raise check skipped (first landing)", result.stdout)

    def test_missing_zero_count_crate_file_fails(self):
        (self.root / "desktop-rebuild-baseline/pp-source.json").unlink()
        result = self.check(write_placement=False)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("counts differ from desktop-rebuild-baseline/pp-source.json", result.stdout)

    def test_baseline_for_absent_crate_fails(self):
        path = self.root / "desktop-rebuild-baseline/pp-absent.json"
        path.write_text(json.dumps({"pp-absent": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)}))
        result = self.check(write_placement=False)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("pp-absent: counts differ", result.stdout)

    def test_crate_filename_cannot_borrow_another_crates_counts(self):
        path = self.root / "desktop-rebuild-baseline/pp-source.json"
        path.rename(path.with_name("pp-other.json"))
        result = self.check(write_placement=False)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("must contain only its named crate", result.stderr)

    def test_invalid_per_crate_counts_fail_locally_and_on_main(self):
        for invalid in (True, -1, "0"):
            with self.subTest(invalid=invalid):
                path = self.root / "desktop-rebuild-baseline/pp-source.json"
                path.write_text(json.dumps({"pp-source": {"module": 0, "item": 0, "statement": invalid}}))
                result = self.check(write_placement=False)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("nonnegative module/item/statement counts", result.stderr)
                self.commit_main()
                result = self.check()
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("nonnegative module/item/statement counts", result.stderr)
                self.commit_main()

    def test_generator_splits_shared_data_and_is_idempotent(self):
        self.publish_shared_placements({"pp-source": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)})
        result = self.check(write_placement=False, arguments=("--write-placement-baseline",))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        directory = self.root / "desktop-rebuild-baseline"
        self.assertEqual([p.name for p in directory.iterdir()], ["pp-source.json"])
        self.assertFalse((self.root / "desktop-rebuild-baseline.json").exists())
        before = (directory / "pp-source.json").read_bytes()
        result = self.check(write_placement=False, arguments=("--write-placement-baseline",))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((directory / "pp-source.json").read_bytes(), before)

    def test_generator_removes_stale_crate_files(self):
        path = self.root / "desktop-rebuild-baseline/pp-absent.json"
        path.write_text(json.dumps({"pp-absent": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)}))
        result = self.check(write_placement=False, arguments=("--write-placement-baseline",))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(path.exists())

    def test_generator_cannot_legitimize_a_statement_raise(self):
        self.source("pp-source/src/lib.rs",
                    'fn production() { value.unwrap(); }'
                    'fn f() { #[cfg(feature="desktop_rebuild")] let x=1; }')
        result = self.check(allow_raise=True, write_placement=False,
                            arguments=("--write-placement-baseline",))
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement baseline raised", result.stdout)

    def test_edits_to_different_crates_merge_without_shared_baseline_conflicts(self):
        self.source("pp-source/src/lib.rs", "fn source() {}")
        self.source("pp-other/src/lib.rs", "fn other() {}")
        self.publish_main(unwrap=0)
        empty = dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)
        self.git("checkout", "-q", "-b", "crate-source")
        self.source("pp-source/src/lib.rs", '#[cfg(feature="desktop_rebuild")] struct S;')
        self.placements = {"pp-source": {**empty, "item": 1}, "pp-other": empty}
        self.placement_baseline()
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "source crate")
        self.git("checkout", "-q", "-b", "crate-other", "refs/remotes/origin/main")
        self.source("pp-other/src/lib.rs", '#[cfg(feature="desktop_rebuild")] struct S;')
        self.placements = {"pp-source": empty, "pp-other": {**empty, "item": 1}}
        self.placement_baseline()
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "other crate")
        self.git("merge", "--no-edit", "crate-source")
        result = self.check(write_placement=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_per_crate_main_lookup_works_below_the_git_root(self):
        nested = self.root / "rust"
        nested.mkdir()
        for path in self.root.iterdir():
            if path.name not in (".git", "rust"):
                shutil.move(path, nested / path.name)
        self.root = nested
        self.commit_main()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_nonbaseline_notes_are_ignored_locally_and_on_main(self):
        (self.root / "desktop-rebuild-baseline/README.md").write_text("Placement notes.\n")
        self.commit_main()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_main_layout_change_during_read_cannot_skip_the_statement_cap(self):
        counts = {"pp-source": dict.fromkeys(CHECKER.PLACEMENT_CLASSES, 0)}
        self.publish_shared_placements(counts)
        old = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        self.placement_baseline()
        self.commit_main()
        new = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        self.git("update-ref", "refs/remotes/origin/main", old)
        run = subprocess.run
        moved = False

        def advance_main(args, **kwargs):
            nonlocal moved
            result = run(args, **kwargs)
            if args[:2] == ["git", "ls-tree"] and not moved:
                moved = True
                self.git("update-ref", "refs/remotes/origin/main", new)
            return result

        with patch.object(CHECKER.subprocess, "run", side_effect=advance_main):
            baseline = CHECKER.main_placements(self.root)
        self.assertTrue(moved)
        self.assertEqual(baseline, counts)

    def test_matching_baseline_passes(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: 1 hits (baseline 1)", result.stdout)

    def test_counts_below_baseline_fail_until_baseline_is_lowered(self):
        self.source("pp-source/src/lib.rs", "fn production() {}")
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: 0 hits (baseline 1)", result.stdout)
        self.assertIn("baseline must equal hit counts", result.stdout)
        self.baseline(unwrap=0)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_baseline_raise_over_main_fails_without_override(self):
        self.publish_main(unwrap=0)
        self.baseline(unwrap=1)
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: baseline raised 0 -> 1 over origin/main", result.stdout)
        self.assertIn(f"{RAISE_OVERRIDE}=1", result.stdout)

    def test_baseline_raise_over_main_passes_with_override(self):
        self.publish_main(unwrap=0)
        self.baseline(unwrap=1)
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: baseline raised 0 -> 1 over origin/main", result.stdout)
        self.assertIn("ast-grep ratchet passed", result.stdout)

    def test_rule_absent_from_main_counts_as_zero_for_raise_check(self):
        (self.root / "ast-grep-baseline.json").write_text(json.dumps({"no-unwrap-expect": 1}))
        self.commit_main()
        self.baseline(unwrap=1, command=1)
        self.source("pp-source/tests/case.rs", 'use std::process::Command;')
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("runtime-process-command: baseline raised 0 -> 1 over origin/main", result.stdout)

    def test_first_landing_without_main_baseline_skips_raise_check(self):
        (self.root / "ast-grep-baseline.json").unlink()
        self.commit_main()
        self.baseline(unwrap=1)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("baseline raise check skipped", result.stdout)

    def test_unavailable_main_ref_fails_closed(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("origin/main is unavailable", result.stderr)

    def test_new_unwrap_fails(self):
        self.source("pp-source/src/lib.rs", "fn production() { value.unwrap(); value.unwrap(); }")
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: 2 hits (baseline 1)", result.stdout)
        self.assertIn("crates/pp-source/src/lib.rs:1: no-unwrap-expect", result.stdout)

    def test_unit_and_integration_tests_are_exempt_from_unwrap_rule(self):
        self.source("pp-source/src/lib.rs", '#[cfg(test)] mod tests { fn helper() { value.unwrap(); } }')
        self.source("pp-source/tests/case.rs", 'fn helper() { value.expect("fixture"); }')
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_new_macro_unwrap_fails(self):
        self.source("pp-source/src/lib.rs", 'fn production() { value.unwrap(); println!("{}", value.unwrap()); }')
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("no-unwrap-expect: 2 hits (baseline 1)", result.stdout)

    def test_out_of_line_unit_test_file_conventions_are_exempt(self):
        self.source("pp-source/src/lib.rs", "#[cfg(test)] mod tests;")
        self.source("pp-source/src/tests.rs", "fn helper() { value.unwrap(); }")
        self.source("pp-net/src/nested/tests/helper.rs", "fn helper() { value.unwrap(); }")
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_command_is_allowed_only_in_runtime_crates(self):
        for crate in ("pp-core", "pp-compat", "pp-desktop"):
            self.source(f"{crate}/src/lib.rs", 'use std::process::Command;')
        self.assertEqual(self.check().returncode, 0)
        self.source("pp-net/src/lib.rs", 'use std::{process::{Command as Spawn, id}};')
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("runtime-process-command: 1 hits (baseline 0)", result.stdout)

    def test_command_in_non_runtime_integration_tests_is_baselined(self):
        self.source("pp-source/tests/case.rs", 'use std::process::Command;')
        self.assertEqual(self.check().returncode, 1)
        self.baseline(unwrap=1, command=1)
        self.assertEqual(self.check().returncode, 1)
        self.assertEqual(self.check(allow_raise=True).returncode, 0)

    def test_raw_and_ordinary_aliases_share_provenance(self):
        for code in (
            'use std::process as r#p; fn f() { p::Command::new("sh"); }',
            'use std::process as p; fn f() { r#p::Command::new("sh"); }',
            'use std as r#s; fn f() { s::process::Command::new("sh"); }',
            'use std as s; fn f() { r#s::process::Command::new("sh"); }',
            'use std::process as r#p; fn f() { self::p::Command::new("sh"); }',
            'use std::process as p; fn f() { vec![crate::r#p::Command::new("sh")]; }',
            'use std::process as r#p; mod child { fn f() { super::p::Command::new("sh"); } }',
            'use std::process as r#p; fn f() { p::Command::new("sh"); p::Command::new("sh"); }',
        ):
            with self.subTest(code=code):
                self.source("pp-source/src/lib.rs", code)
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                count = 2 if code.count('new("sh")') == 2 else 1
                self.assertIn(f"runtime-process-command: {count} hits", result.stdout)

    def test_raw_alias_shadows_and_non_code_are_preserved(self):
        for code in (
            'use std::process as p; fn f() { use local as r#p; p::Command::new("sh"); }',
            'use std::process as r#p; trait T { type Command; } fn f<p:T>() { let _: p::Command; }',
            '#[cfg(any())] use std::process as r#p; use local as p; fn f(_: p::Command) {}',
            'use std::process as r#p; fn f() { q::Command::new("sh"); }',
            'fn f() { let _ = "r#p::Command"; /* r#p::Command */ }',
            'mod r#type {} fn f() { r#type::Command::new("sh"); }',
            'use std::process as r#p; fn f() { use other::r#p::{self}; p::Command::new("sh"); }',
        ):
            with self.subTest(code=code):
                self.source("pp-source/src/lib.rs", code)
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_raw_alias_reports_original_unicode_spelling_and_line(self):
        code = 'const TEXT: &str = "é";\nuse std::process as r#p;\nfn f() { r#p::Command::new("sh"); }'
        self.source("pp-source/src/lib.rs", code)
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('crates/pp-source/src/lib.rs:3: runtime-process-command: r#p::Command', result.stdout)
        self.assertEqual((self.root / "crates/pp-source/src/lib.rs").read_text(), code)

    def test_raw_aliases_retain_runtime_crate_exemptions(self):
        for crate in ("pp-core", "pp-compat", "pp-desktop"):
            self.source(f"{crate}/src/lib.rs", 'use std::process as r#p; fn f() { p::Command::new("sh"); }')
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("runtime-process-command: 0 hits", result.stdout)

    def test_invalid_rules_fail_closed(self):
        (self.root / "rules" / "broken.yml").write_text("not a rule")
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("ast-grep ratchet error:", result.stderr)

    def test_deleted_rule_fails_closed(self):
        (self.root / "rules" / "no-unwrap-expect.yml").unlink()
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("unloaded=['no-unwrap-expect']", result.stderr)

    def test_disabled_rule_fails_closed(self):
        rule = self.root / "rules" / "no-unwrap-expect.yml"
        rule.write_text(rule.read_text().replace("severity: warning", "severity: off"))
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("unloaded=['no-unwrap-expect']", result.stderr)

    def test_unconfigured_rules_fail_closed(self):
        (self.root / "empty-rules").mkdir()
        (self.root / "sgconfig.yml").write_text("ruleDirs:\n  - empty-rules\n")
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("ast-grep ratchet error:", result.stderr)

    def test_unbaselined_rule_with_no_hits_fails_closed(self):
        (self.root / "rules" / "extra.yml").write_text(
            "id: extra-rule\nlanguage: Rust\nseverity: warning\nrule:\n  pattern: nonexistent()\n"
        )
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("unbaselined=['extra-rule']", result.stderr)


if __name__ == "__main__":
    unittest.main()
