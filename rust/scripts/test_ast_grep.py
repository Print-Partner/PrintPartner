"""Exercise the real ast-grep ratchet, including file and crate exemptions."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


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
        (self.root / "desktop-rebuild-baseline.json").write_text(json.dumps({"version": 2, "crates": counts}))

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

    def check(self, allow_raise=False):
        self.placement_baseline()
        environment = {key: value for key, value in os.environ.items() if key != RAISE_OVERRIDE}
        if allow_raise:
            environment[RAISE_OVERRIDE] = "1"
        return subprocess.run(
            ["python3", str(self.root / "scripts" / "check-ast-grep.py")],
            text=True,
            capture_output=True,
            env=environment,
        )

    def test_cfg_macro_additions_fail(self):
        for expression in (
            'cfg!(feature="desktop_rebuild")',
            r'r#cfg!(r#feature="desktop\x5frebuild")',
            'cfg!(any(test, feature=r###"desktop_rebuild"###))',
            'cfg! { feature="desktop_rebuild" }',
            'cfg![feature="desktop_rebuild"]',
        ):
            with self.subTest(expression=expression):
                self.source("pp-source/src/lib.rs", f"fn f() {{ let x = {expression}; }}")
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("statement=1", result.stdout)

    def test_cfg_macro_baseline_migrates_once_from_main_inventory(self):
        self.source("pp-source/src/lib.rs", 'fn f() { let x=cfg!(feature="desktop_rebuild"); }')
        self.publish_main()
        # Simulate main before cfg! entered the statement inventory.
        old = {"pp-source": {"module": 0, "item": 0, "statement": 0}}
        (self.root / "desktop-rebuild-baseline.json").write_text(json.dumps(old))
        self.commit_main()
        self.placements = {"pp-source": {"module": 0, "item": 0, "statement": 1}}
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # Adding a second cfg! cannot enlarge the main-derived allowance.
        self.source("pp-source/src/lib.rs", 'fn f() { let x=cfg!(feature="desktop_rebuild");'
                    ' let y=cfg!(feature="desktop_rebuild"); }')
        self.placements["pp-source"]["statement"] = 2
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement baseline raised", result.stdout)
        # Once v2 lands, existing cfg! gates grant no further allowance.
        self.publish_main()
        self.placements["pp-source"]["statement"] = 3
        self.source("pp-source/src/lib.rs", 'fn f() { let x=cfg!(feature="desktop_rebuild");'
                    ' let y=cfg!(feature="desktop_rebuild"); let z=cfg!(feature="desktop_rebuild"); }')
        result = self.check(allow_raise=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("statement baseline raised", result.stdout)

    def test_macro_comments_and_literals_are_ignored(self):
        code = r'''fn f() { other!("cfg!(feature=desktop_rebuild)",
            r###"cfg(feature="desktop_rebuild")"###,
            /* cfg!(feature="desktop_rebuild") */
            other_feature); }'''
        self.source("pp-source/src/lib.rs", code)
        self.baseline()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("0 classified = 0 text gates", result.stdout)

    def test_docsrs_metadata_is_not_a_gate(self):
        for attr in (
            '#[cfg_attr(docsrs, doc(cfg(feature="desktop_rebuild")))]',
            '#[cfg_attr(docsrs, doc /* gap */ (cfg(feature="desktop_rebuild")))]',
            '#[cfg_attr(test, all(feature="desktop_rebuild"))]',
            '#[cfg_attr(test, cfg_attr(docsrs, doc(cfg(feature="desktop_rebuild"))))]',
        ):
            with self.subTest(attr=attr):
                self.source("pp-source/src/lib.rs", f"{attr} struct S; fn f() {{ {attr} let x=1; }}")
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("0 classified = 0 text gates", result.stdout)

    def test_opaque_macro_doc_metadata_is_conservatively_frozen(self):
        for meta, count in (
            ('cfg_attr(docsrs, doc(cfg(feature="desktop_rebuild")))', 1),
            ('#[cfg_attr(docsrs, doc(cfg(feature="desktop_rebuild")))] struct S;', 0),
        ):
            with self.subTest(meta=meta):
                self.source("pp-source/src/lib.rs", f"fn f() {{ m!({meta}); }}")
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, count, result.stdout + result.stderr)
                self.assertIn(f"{count} classified = {count} text gates", result.stdout)
                self.assertIn(f"statement={count}", result.stdout)

    def test_macro_argument_meta_counts_as_statement(self):
        for meta in ('cfg(feature="desktop_rebuild")',
                     'cfg_attr(all(), cfg(feature="desktop_rebuild"))',
                     r'r#cfg(r#feature="desktop\u{5f}rebuild")'):
            with self.subTest(meta=meta):
                self.source("pp-source/src/lib.rs",
                            'macro_rules! gate { ($m:meta) => { #[cfg_attr(all(), $m)] let x=1; } }'
                            f'fn f() {{ gate!({meta}); }}')
                self.baseline()
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("1 classified = 1 text gates", result.stdout)
                self.assertIn("statement=1", result.stdout)

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
        (self.root / "desktop-rebuild-baseline.json").unlink()
        self.commit_main()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("desktop-rebuild raise check skipped (first landing)", result.stdout)

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
