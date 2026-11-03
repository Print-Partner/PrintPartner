"""Offline checks for the cloud setup; the disposable Linux run is separate."""

from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
SETUP = ROOT / "scripts/factory-environment/setup.sh"


class EnvironmentSetupTests(unittest.TestCase):
    def test_script_parses(self):
        result = subprocess.run(["bash", "-n", str(SETUP)], capture_output=True,
                                text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_runtime_and_parity_versions_match_ci(self):
        setup = SETUP.read_text()
        ci = (ROOT / "scripts/local-ci.py").read_text()
        dockerfile = (ROOT / ".devcontainer/Dockerfile").read_text()
        self.assertIn('Path(".devcontainer/Dockerfile").read_text()', setup)
        self.assertNotRegex(setup, re.compile(r"^NODE_(?:RUNTIME|PARITY)=[0-9.]+", re.M))
        for stage in ("runtime-node", "parity-node"):
            version = re.search(rf"^FROM node:([0-9.]+)-\S+ AS {stage}$", dockerfile, re.M)[1]
            self.assertIn(f"v{version}", ci)
        self.assertIn('"$CARGO_HOME/bin/rustup" toolchain install', setup)
        self.assertNotIn("toolchain install stable", setup)
        self.assertIn('rustup" default "$(', setup)
        self.assertIn('ln -sfn "$PREFIX/node24/bin/node" /usr/bin/node', setup)
        self.assertNotIn("PP_TOOL_PREFIX", setup)

    def test_downloads_have_architecture_specific_checksums(self):
        setup = SETUP.read_text()
        hashes = re.findall(r"(?:rustup|runtime|parity)_sha=([0-9a-f]+)", setup)
        self.assertEqual(len(hashes), 6)
        self.assertEqual(len(set(hashes)), 6)
        self.assertTrue(all(len(value) == 64 for value in hashes))
        self.assertIn("sha256sum -c --quiet -", setup)

    def test_sidecar_pins_come_from_product_image(self):
        setup = SETUP.read_text()
        product = (ROOT / "slicer-sidecar/Dockerfile").read_text()
        self.assertIn('Path("slicer-sidecar/Dockerfile").read_text()', setup)
        for dependency in ("flask", "waitress"):
            pin = re.search(rf"{dependency}==[0-9.]+", product)[0]
            self.assertNotIn(pin, setup)
        self.assertIn('"${sidecar_requirements[@]}"', setup)

    def test_smoke_uses_installed_sidecar_interpreter(self):
        setup = SETUP.read_text()
        smoke = (ROOT / ".devcontainer/smoke.sh").read_text()
        self.assertIn('export PP_SIDECAR_PYTHON="$PREFIX/sidecar-venv/bin/python"', setup)
        self.assertIn('"${PP_SIDECAR_PYTHON:-/opt/sidecar-venv/bin/python}" -B -m unittest', smoke)

    def test_qa_skills_defer_to_the_shared_workflow(self):
        workflow = (ROOT / "docs/development/agent-workflow.md").read_text()
        self.assertIn("control-print-partner.mjs launch", workflow)
        self.assertIn("verify.py launch", workflow)
        shared = "../../../docs/development/agent-workflow.md#"
        for name, sections in (("pp-mission-qa", ("choose-the-runtime", "web-ui", "rust-gateway", "report-and-clean-up")),
                               ("pp-verify", ("web-ui", "dialogs-and-other-client-only-state", "rust-gateway",
                                              "desktop-app", "check-and-attach-evidence"))):
            skill = (ROOT / f".factory/skills/{name}/SKILL.md").read_text()
            for section in sections:
                self.assertIn(shared + section, skill)
            # A second copy of the launch recipe would drift from the shared one.
            self.assertNotIn("control-print-partner.mjs", skill)
            self.assertNotIn("verify.py launch", skill)
            self.assertNotIn("$QA_ROOT", skill)

    def test_native_packages_come_from_devcontainer(self):
        setup = SETUP.read_text()
        self.assertNotIn("libgtk-3-dev", setup)
        script = setup[setup.index("mapfile -t native_packages"):setup.index("(( ${#native_packages")]
        result = subprocess.run(["bash", "-c", script + 'printf "%s\\n" "${native_packages[@]}"'],
                                cwd=ROOT, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        packages = result.stdout.split()
        self.assertIn("libwebkit2gtk-4.1-dev", packages)
        self.assertIn("python3-venv", packages)
        self.assertIn("chromium", packages)
        self.assertNotIn("\\", packages)

    def test_browser_is_configured_for_ui_evidence(self):
        setup = SETUP.read_text()
        self.assertIn('export PLAYWRIGHT_CHROMIUM_EXECUTABLE="/usr/bin/chromium"', setup)
        self.assertNotIn("grep -vx chromium", setup)

    def test_python_is_installed_before_version_extraction(self):
        setup = SETUP.read_text()
        self.assertLess(setup.index("apt-get install"), setup.index("node_versions="))

    def test_setup_builds_packages_without_starting_services(self):
        setup = SETUP.read_text()
        self.assertIn("bash .devcontainer/post-create.sh", setup)
        self.assertNotIn("npm --prefix web", setup)
        self.assertNotRegex(setup, r"systemctl|docker run|run dev(?:\s|$)")


if __name__ == "__main__":
    unittest.main()
