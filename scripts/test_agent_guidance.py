import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check-agent-guidance.py")
SPEC = importlib.util.spec_from_file_location("agent_guidance", SCRIPT)
GUIDANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUIDANCE)


class AgentGuidanceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write(self, name, body):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    def test_missing_document_fails(self):
        self.assertIn("missing document", GUIDANCE.check_document(self.root, "AGENTS.md")[0])

    def test_missing_title_fails(self):
        self.write("AGENTS.md", "Instructions without a heading\n")
        self.assertIn("missing title", GUIDANCE.check_document(self.root, "AGENTS.md")[0])

    def test_existing_relative_links_and_external_links_pass_without_network(self):
        self.write("docs/guide.md", "# Guide\n\n## Section\n")
        self.write("AGENTS.md", "# Guide\n[local](docs/guide.md#section)\n"
                   "[web](https://example.com/missing)\n[anchor](#rules)\n\n## Rules\n")
        self.assertEqual(GUIDANCE.check_document(self.root, "AGENTS.md"), [])

    def test_link_to_missing_heading_fails(self):
        self.write("docs/guide.md", "# Guide\n\n```sh\n# Section\n```\n")
        self.write("AGENTS.md", "# Guide\n[local](docs/guide.md#section)\n[here](#missing)\n")
        errors = GUIDANCE.check_document(self.root, "AGENTS.md")
        self.assertEqual(len(errors), 2, errors)
        self.assertTrue(all("missing heading" in error for error in errors))

    def test_heading_anchors_follow_github_rules(self):
        self.assertEqual(GUIDANCE.anchors("# Run `just ci`, then push!\n## Notes\n## Notes\n### Rust gateway ##\n"),
                         {"run-just-ci-then-push", "notes", "notes-1", "rust-gateway"})

    def test_heading_anchors_use_rendered_text(self):
        self.assertEqual(GUIDANCE.anchors("## [Setup](setup.md)\n## See [the guide][guide]\n"
                                          "## _Emphasis_ and **bold** <kbd>keys</kbd>\n"
                                          "## snake_case names\n## Run `_raw_ [x](y)`\n## [`code` link](x.md)\n"),
                         {"setup", "see-the-guide", "emphasis-and-bold-keys", "snake_case-names",
                          "run-_raw_-xy", "code-link"})

    def test_duplicate_anchors_skip_suffixes_already_used(self):
        self.assertEqual(GUIDANCE.anchors("## Notes\n## Notes\n## Notes-1\n## Notes 1\n"),
                         {"notes", "notes-1", "notes-1-1", "notes-1-2"})

    def test_fence_closes_only_on_matching_delimiter(self):
        markdown = ("````text\n~~~\n```\n# Inside\n````\n## After\n"
                    "~~~\n```\n~~~~\n## Last\n")
        self.assertEqual(GUIDANCE.anchors(markdown), {"after", "last"})

    def test_link_to_heading_with_link_text_passes(self):
        self.write("docs/setup.md", "# Setup\n")
        self.write("AGENTS.md", "# Guide\n[section](#setup)\n\n## [Setup](docs/setup.md)\n")
        self.assertEqual(GUIDANCE.check_document(self.root, "AGENTS.md"), [])

    def test_missing_link_fails(self):
        self.write("AGENTS.md", "# Guide\n[missing](docs/missing.md)\n")
        self.assertIn("missing or outside-checkout link",
                      GUIDANCE.check_document(self.root, "AGENTS.md")[0])

    def test_outside_checkout_link_fails_even_if_target_exists(self):
        self.write("AGENTS.md", "# Guide\n[outside](../)\n")
        self.assertIn("outside-checkout", GUIDANCE.check_document(self.root, "AGENTS.md")[0])

    def test_url_encoded_local_link_passes(self):
        self.write("docs/local guide.md", "# Guide\n")
        self.write("AGENTS.md", "# Guide\n[local](docs/local%20guide.md)\n")
        self.assertEqual(GUIDANCE.check_document(self.root, "AGENTS.md"), [])

    def test_skill_metadata(self):
        name = ".factory/skills/pp-readiness/SKILL.md"
        for metadata, expected in (
            ("---\nname: pp-readiness\ndescription: Run bounded QA.\n---\n", []),
            ("# No frontmatter\n", ["missing YAML frontmatter"]),
            ("---\nname: wrong\ndescription: QA\n---\n", ["name must match"]),
            ("---\nname: pp-readiness\n---\n", ["missing skill description"]),
        ):
            with self.subTest(metadata=metadata):
                self.write(name, metadata)
                errors = GUIDANCE.check_document(self.root, name)
                self.assertEqual(len(errors), len(expected))
                for error, message in zip(errors, expected):
                    self.assertIn(message, error)

    def test_ignored_guidance_fails_even_when_tracked(self):
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.write(".gitignore", "AGENTS.md\n")
        self.write("AGENTS.md", "# Shared guide\n")
        subprocess.run(["git", "-C", str(self.root), "add", "-f", "AGENTS.md"], check=True)
        self.assertEqual(GUIDANCE.check_trackability(self.root),
                         ["AGENTS.md: shared guidance must not be ignored"])
        self.write(".gitignore", "AGENTS.md\n!/AGENTS.md\n")
        self.assertEqual(GUIDANCE.check_trackability(self.root), [])

    def test_non_repository_is_an_error_not_a_pass(self):
        self.assertIn("git check-ignore failed", GUIDANCE.check_trackability(self.root)[0])

    def test_real_checkout_entrypoint(self):
        result = subprocess.run([sys.executable, str(SCRIPT)], cwd=self.root,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"{len(GUIDANCE.DOCUMENTS)} documents", result.stdout)

    def test_real_ignore_rules_allow_templates_but_not_secrets(self):
        cases = {
            "AGENTS.md": False,
            ".factory/skills/pp-readiness/SKILL.md": False,
            ".factory/skills/pp-verify/SKILL.md": False,
            ".env.example": False,
            ".env.development.example": False,
            "web/apps/web/.env.example": False,
            ".env": True,
            ".env.local": True,
            ".env.production": True,
            "web/apps/server/.env.production": True,
            "ai_secrets.json": True,
            "rust/crates/pp-core/AGENTS.md": True,
        }
        for name, ignored in cases.items():
            with self.subTest(path=name):
                result = subprocess.run(
                    ["git", "check-ignore", "--no-index", "-q", "--", name],
                    cwd=GUIDANCE.ROOT, check=False,
                )
                self.assertIn(result.returncode, (0, 1))
                self.assertEqual(result.returncode == 0, ignored)


if __name__ == "__main__":
    unittest.main()
