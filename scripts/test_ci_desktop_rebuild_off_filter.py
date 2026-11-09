import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("off_filter", ROOT / "scripts/ci-desktop-rebuild-off-filter.py")
off = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(off)

GATE = '#[cfg(feature = "desktop_rebuild")]'


class SourceScanTests(unittest.TestCase):
    def package(self, **files):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        for name, text in files.items():
            path = root / name.replace("__", "/").removesuffix("_rs")
            path = path.with_name(path.name + ".rs")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return root

    def test_whole_items_behind_the_feature_are_additive(self):
        enum = "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\nenum Policy {\n    Strict,\n    " + GATE + "\n    Nfc,\n}\n"
        additive = {
            "module": f"{GATE}\npub mod rebuild;\n",
            "free function with attributes": f"{GATE}\n#[must_use]\n/// Docs.\npub(crate) async fn rebuild_only() {{}}\n",
            "struct with a unique name": f"{GATE}\npub struct RebuildOnly;\n",
            "item in a mod block": f"mod inner {{\n    {GATE}\n    pub fn rebuild_only() {{}}\n}}\n",
            "file": f'#![cfg(feature = "desktop_rebuild")]\nfn f() {{ let x = 1; }}\n',
            "glob import in another module": f"{GATE}\nfn rebuild_only() {{}}\nmod tests {{\n    use super::*;\n}}\n",
            "gated variant and its arm": enum + (f"fn f(p: Policy) {{ match p {{ Policy::Strict => 1,\n"
                                                 f"    {GATE}\n    Policy::Nfc => 2, }}; }}\n"),
            "arm through Self": enum + (f"impl Policy {{\n    fn f(self) -> u8 {{\n        match self {{\n"
                                        f"            Self::Strict => 1,\n            {GATE}\n"
                                        f"            Self::Nfc => 2,\n        }}\n    }}\n}}\n"),
        }
        for label, text in additive.items():
            with self.subTest(label):
                self.assertEqual(off.off_reasons(self.package(src__lib_rs=text)), [])

    def test_code_that_can_differ_with_the_feature_off_is_sensitive(self):
        enum = "#[derive(Debug)]\nenum Policy {\n    Strict,\n    " + GATE + "\n    Nfc,\n}\n"
        sensitive = {
            "not()": '#[cfg(not(feature = "desktop_rebuild"))]\nconst V: u64 = 35;\n',
            "cfg!()": 'fn f() -> bool { cfg!(feature = "desktop_rebuild") }\n',
            "any()": '#[cfg(any(test, feature = "desktop_rebuild"))]\nfn f() {}\n',
            "other spacing": '#[cfg(feature="desktop_rebuild")]\nfn f() {}\n',
            "multi-line attribute": '#[cfg(\n    feature = "desktop_rebuild"\n)]\nfn f() {}\n',
            "cfg_attr": '#[cfg_attr(feature = "desktop_rebuild", derive(Debug))]\nstruct S;\n',
            "runtime switch": 'fn f() { std::env::var("PP_DESKTOP_REBUILD").ok(); }\n',
            "gated statement": f"fn f() {{\n    {GATE}\n    let limit = 2;\n}}\n",
            "gated block": f"fn f() {{\n    {GATE}\n    {{\n        g();\n    }}\n}}\n",
            "fn inside a function": f"fn f() {{\n    {GATE}\n    fn g() {{}}\n}}\n",
            "struct field": f"struct S {{\n    a: u8,\n    {GATE}\n    b: u8,\n}}\n",
            "list element": f"const L: &[u8] = &[\n    1,\n    {GATE}\n    Extra,\n];\n",
            "trait impl": f"{GATE}\nimpl Drop for Lock {{\n    fn drop(&mut self) {{}}\n}}\n",
            "item next to a glob import": f"use other::*;\n{GATE}\nconst LIMIT: u8 = 2;\n",
            "gated use": f"{GATE}\nuse std::fmt;\n",
            "gated use of a trait": f"{GATE}\nuse unicode_normalization::UnicodeNormalization;\n",
            "gated inherent impl block": f"{GATE}\nimpl Directory {{\n    fn create_child(&self) {{}}\n}}\n",
            # The review threads on #1632: inherent methods win over trait methods from any crate.
            "gated impl block that shadows a local trait": (f"{GATE}\nimpl S {{ fn len(&self) -> usize {{ 0 }} }}\n"
                                                            "trait L { fn len(&self) -> usize; }\n"),
            "method that could shadow": (f"impl S {{\n    {GATE}\n    fn len(&self) -> usize {{ 0 }}\n}}\n"
                                         "trait L { fn len(&self) -> usize; }\n"),
            "method with a unique name": f"impl Directory {{\n    {GATE}\n    pub fn create_child(&self) {{}}\n}}\n",
            "free function named like a prelude item": f"{GATE}\nfn drop() {{}}\n",
            "struct named like a prelude type": f"{GATE}\nstruct Result;\n",
            "macro": f"{GATE}\nmacro_rules! m {{ () => {{}} }}\n",
            "arm on an ungated variant": enum + (f"fn f(p: Policy) {{ match p {{\n    {GATE}\n    Policy::Strict => 2,\n"
                                                 "    _ => 1, }; }\n"),
            "or-pattern arm": enum + (f"fn f(p: Policy) {{ match p {{\n    {GATE}\n    Policy::Nfc | Policy::Strict => 2,\n"
                                      "    Policy::Strict => 1, }; }\n"),
            "same variant name on another enum": enum + "enum Other {\n    Nfc,\n}\n" + (
                f"fn f(p: Other) {{ match p {{\n    {GATE}\n    Other::Nfc => 2,\n    _ => 1, }}; }}\n"),
            "variant of an ordered enum": f"#[derive(PartialOrd, Ord, PartialEq, Eq)]\nenum E {{\n    A,\n    {GATE}\n    B,\n    C,\n}}\n",
            "variant of a repr enum": f"#[repr(u8)]\nenum E {{\n    A,\n    {GATE}\n    B,\n}}\n",
            "variant of a serde enum": f"#[derive(Deserialize)]\n#[serde(untagged)]\nenum E {{\n    A(u8),\n    {GATE}\n    B(u8),\n}}\n",
            "gate at the end of a file": f"fn f() {{}}\n{GATE}\n",
        }
        for label, text in sensitive.items():
            with self.subTest(label):
                self.assertTrue(off.off_reasons(self.package(src__lib_rs=text)))
        # Each std trait method from the review threads, as a gated method or impl block.
        for method in ("len", "read", "to_string", "into", "clone", "fmt"):
            for text in (f"impl Directory {{\n    {GATE}\n    pub fn {method}(&self) {{}}\n}}\n",
                         f"{GATE}\nimpl Directory {{\n    pub fn {method}(&self) {{}}\n}}\n",
                         f"{GATE}\npub fn {method}() {{}}\n"):
                with self.subTest(method=method, text=text):
                    self.assertTrue(off.off_reasons(self.package(src__lib_rs=text)))
        # A free item whose name another crate in the workspace also defines is not unique.
        self.assertTrue(off.off_reasons(self.package(src__lib_rs=f"{GATE}\npub fn rebuild_only() {{}}\n"),
                                        {"rebuild_only": 2}))
        # Tests, benches and build scripts count too.
        self.assertTrue(off.off_reasons(self.package(build_rs='fn main() { cfg!(feature = "desktop_rebuild"); }')))
        self.assertTrue(off.off_reasons(self.package(tests__t_rs='#[cfg(not(feature = "desktop_rebuild"))]\n'
                                                                  '#[test]\nfn t() {}\n')))

    def test_a_checkout_under_a_target_directory_is_still_scanned(self):
        root = self.package(target__pkg__src__lib_rs='const B: bool = cfg!(feature = "desktop_rebuild");\n')
        self.assertTrue(off.off_reasons(root / "target/pkg"))
        self.assertEqual(off.off_reasons(self.package(target__debug__build__x__out__gen_rs=
                                                      'const B: bool = cfg!(feature = "desktop_rebuild");\n')), [])


def crates(**sources):
    folder = tempfile.TemporaryDirectory()
    root = Path(folder.name)
    for name, text in sources.items():
        name = name.replace("_", "-")
        (root / name / "src").mkdir(parents=True)
        (root / name / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
        (root / name / "src/lib.rs").write_text(text)
    return folder, root


class FilterTests(unittest.TestCase):
    def test_the_filter_names_sensitive_packages_and_every_dependent(self):
        folder, root = crates(pp_a=f"{GATE}\npub mod x;\n", pp_b='const B: bool = cfg!(feature = "desktop_rebuild");\n',
                              pp_c="fn c() {}\n", pp_d="fn d() {}\n")
        self.addCleanup(folder.cleanup)
        graph = {"pp-a": set(), "pp-b": set(), "pp-c": {"pp-a"}, "pp-d": {"pp-c", "pp-b"}}
        self.assertEqual(off.off_filter(root, graph, {}, {}), "package(=pp-b) + package(=pp-d)")

    def test_the_filter_fails_instead_of_selecting_nothing_or_missing_a_member(self):
        folder, root = crates(pp_a=f"{GATE}\npub mod x;\n")
        self.addCleanup(folder.cleanup)
        with self.assertRaises(SystemExit) as raised:
            off.off_filter(root, {"pp-a": set()}, {}, {})
        self.assertIn("No package mentions", str(raised.exception))
        with self.assertRaises(SystemExit) as raised:
            off.off_filter(root, {"pp-a": set(), "pp-elsewhere": set()}, {}, {})
        self.assertIn("pp-elsewhere", str(raised.exception))


    def test_cargo_metadata_is_read_as_utf_8(self):
        # rust (windows) on #1632 decoded it with cp1252 and crashed on a non-ASCII byte.
        calls = []

        def fake_run(command, **options):
            calls.append(options)
            return subprocess.CompletedProcess(command, 0, stdout='{"name": "caf\u00e9"}', stderr="")

        original, off.subprocess.run = off.subprocess.run, fake_run
        self.addCleanup(setattr, off.subprocess, "run", original)
        self.assertEqual(off.cargo_metadata(Path("Cargo.toml")), {"name": "caf\u00e9"})
        self.assertEqual(calls[0]["encoding"], "utf-8")

    def test_a_failed_cargo_metadata_stops_with_its_error(self):
        def fake_run(command, **options):
            return subprocess.CompletedProcess(command, 101, stdout="", stderr="error: lock file needs updating")

        original, off.subprocess.run = off.subprocess.run, fake_run
        self.addCleanup(setattr, off.subprocess, "run", original)
        with self.assertRaises(SystemExit) as raised:
            off.cargo_metadata(Path("Cargo.toml"))
        self.assertIn("error: lock file needs updating", str(raised.exception))


class ExternalFeatureTests(unittest.TestCase):
    def test_only_reviewed_additive_features_pass(self):
        before = {"axum 1": ("axum", {"json"}), "serde_json 1": ("serde_json", {"std"}),
                  "hashbrown 14": ("hashbrown", {"raw"}), "hashbrown 15": ("hashbrown", set())}
        after = {**before, "axum 1": ("axum", {"json", "multipart"}), "multer 3": ("multer", {"default"})}
        self.assertEqual(off.unreviewed_features(before, after), [])
        self.assertEqual(off.unreviewed_features(before, {**after, "serde_json 1": ("serde_json", {"std", "preserve_order"})}),
                         ["serde_json/preserve_order"])
        # Each resolved version counts on its own, so one version cannot hide a feature added to another.
        self.assertEqual(off.unreviewed_features(before, {**before, "hashbrown 15": ("hashbrown", {"raw"})}),
                         ["hashbrown/raw"])
        folder, root = crates(pp_b='const B: bool = cfg!(feature = "desktop_rebuild");\n')
        self.addCleanup(folder.cleanup)
        with self.assertRaises(SystemExit) as raised:
            off.off_filter(root, {"pp-b": set()}, before, {**before, "serde_json 1": ("serde_json", {"std", "arbitrary_precision"})})
        self.assertIn("serde_json/arbitrary_precision", str(raised.exception))

    def test_resolved_features_and_graph_read_cargo_metadata(self):
        metadata = {"workspace_members": ["a 1", "b 1"],
                    "packages": [{"id": "a 1", "name": "pp-a"}, {"id": "b 1", "name": "pp-b"},
                                 {"id": "x 1", "name": "axum"}, {"id": "x 2", "name": "axum"}],
                    "resolve": {"nodes": [{"id": "a 1", "features": ["desktop_rebuild"], "deps": [{"pkg": "x 1"}]},
                                          {"id": "b 1", "features": [], "deps": [{"pkg": "a 1"}, {"pkg": "x 2"}]},
                                          {"id": "x 1", "features": ["json"], "deps": []},
                                          {"id": "x 2", "features": ["ws"], "deps": []}]}}
        self.assertEqual(off.resolved_features(metadata), {"x 1": ("axum", {"json"}), "x 2": ("axum", {"ws"})})
        self.assertEqual(off.workspace_graph(metadata), {"pp-a": set(), "pp-b": {"pp-a"}})


class RepositoryTests(unittest.TestCase):
    def test_current_split(self):
        """A package that moves between the passes shows up here for review."""
        names = off.defined_names(ROOT / "rust/crates")
        additive = sorted(name for name, folder in off.packages(ROOT / "rust/crates").items()
                          if not off.off_reasons(folder, names))
        self.assertEqual(additive, ["pp-api-types", "pp-contracts", "pp-domain", "pp-drivers", "pp-net", "pp-os",
                                    "pp-perf", "pp-services", "pp-testkit"])

    def test_the_workflow_narrows_only_the_off_pass(self):
        # Text checks: the changes job runs this file without PyYAML.
        text = (ROOT / ".github/workflows/rust-cross-os.yml").read_text()
        off_step = text.split("- name: nextest (desktop_rebuild off)\n", 1)[1].split("- name:", 1)[0]
        self.assertIn("off_filter=$(python3 -B scripts/ci-desktop-rebuild-off-filter.py)\n", off_step)
        self.assertIn('--no-tests=warn -E "($NEXTEST_FILTER) & ($off_filter)"\n', off_step)
        self.assertEqual(text.count("off_filter"), 3)
        for name in ("nextest (desktop_rebuild on)", "Archive nextest (desktop_rebuild on)"):
            step = text.split(f"- name: {name}\n", 1)[1].split("- name:", 1)[0]
            self.assertIn('-E "$NEXTEST_FILTER"', step)
        # macOS PRs skip the on pass, so they keep the whole off pass; no other job sets it.
        self.assertEqual(text.count("PP_NEXTEST_OFF_ALL:"), 1)
        macos = text.split("\n  rust-macos:\n", 1)[1].split("\n  rust-windows:\n", 1)[0]
        self.assertIn("      PP_NEXTEST_OFF_ALL: ${{ github.event_name == 'pull_request' }}\n", macos)

if __name__ == "__main__":
    unittest.main()
