import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

from stage_dependencies import stage_locked_dependencies

SCRIPT = pathlib.Path(__file__).with_name("stage-desktop.py")
MANIFEST = pathlib.Path(__file__).with_name("build-desktop-manifest.py")
COMMIT = "a" * 40
REJECTED = (
    "supply the native pinned Node 24.21.0 ABI 137 runtime "
    "(Linux ELF or macOS Mach-O), not a dispatcher script:"
)
FLEET_DISPATCHER = pathlib.Path("/opt/agent-fleet/bin/node")


def run_stage(node, output, web, env=None):
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--web", str(web), "--node", str(node),
         "--output", str(output), "--commit", COMMIT],
        capture_output=True,
        text=True,
        env=env,
    )


class StageNodeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.web = self.root / "web"
        self.web.mkdir()
        self.output = self.root / "stage"
        self.marker = self.root / "invoked"
        self.env = os.environ.copy()
        self.env["PP_STAGE_INVOKED"] = str(self.marker)

    def tearDown(self):
        self.temporary.cleanup()

    def write_dispatcher(self, path):
        path.write_text('#!/bin/sh\n: > "$PP_STAGE_INVOKED"\nexit 0\n')
        path.chmod(0o755)

    def assert_refused(self, result):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(REJECTED, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse(self.marker.exists(), result.stderr)
        self.assertFalse(self.output.exists(), result.stderr)

    def test_shebang_dispatcher_is_refused_before_it_runs(self):
        node = self.root / "node"
        self.write_dispatcher(node)
        self.assert_refused(run_stage(node, self.output, self.web, self.env))

    def test_symlink_to_dispatcher_is_refused_before_it_runs(self):
        node = self.root / "node"
        self.write_dispatcher(node)
        link = self.root / "node-link"
        link.symlink_to(node)
        self.assert_refused(run_stage(link, self.output, self.web, self.env))

    def test_plain_text_node_is_refused_without_a_stage(self):
        node = self.root / "node"
        node.write_text("echo not a binary\n")
        node.chmod(0o755)
        self.assert_refused(run_stage(node, self.output, self.web, self.env))

    def test_empty_node_is_refused_without_a_stage(self):
        node = self.root / "node"
        node.write_bytes(b"")
        node.chmod(0o755)
        self.assert_refused(run_stage(node, self.output, self.web, self.env))

    def test_mach_o_header_is_not_rejected_as_a_dispatcher(self):
        node = self.root / "node"
        node.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 32)
        node.chmod(0o755)
        result = run_stage(node, self.output, self.web)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("dispatcher script", result.stderr)
        self.assertFalse(self.output.exists(), result.stderr)

    def test_elf_header_is_not_rejected_as_a_dispatcher(self):
        node = self.root / "node"
        node.write_bytes(b"\x7fELF" + b"\x00" * 32)
        node.chmod(0o755)
        result = run_stage(node, self.output, self.web)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("dispatcher script", result.stderr)
        self.assertFalse(self.output.exists(), result.stderr)

    @unittest.skipUnless(FLEET_DISPATCHER.is_file(), "fleet dispatcher absent")
    def test_fleet_dispatcher_is_refused_before_stage_mutation(self):
        result = run_stage(FLEET_DISPATCHER, self.output, self.web, self.env)
        self.assert_refused(result)
        self.assertIn(str(FLEET_DISPATCHER), result.stderr)


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def dangling_links(root):
    return [os.path.join(directory, name)
            for directory, dirs, files in os.walk(root)
            for name in dirs + files
            if os.path.islink(os.path.join(directory, name))
            and not os.path.exists(os.path.join(directory, name))]


class StageWorkspaceLinkTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.source = self.root / "source"
        self.stage = self.root / "stage"
        self.web = self.stage / "web"
        self.lock = {"packages": {
            "": {"version": "3.3.0"},
            "node_modules/@print-partner/contracts": {"resolved": "packages/contracts", "link": True},
            "node_modules/@print-partner/ui": {"resolved": "apps/ui", "link": True},
            "node_modules/left-pad": {"version": "1.3.0"},
        }}
        write(self.source / "packages/contracts/package.json", "{}")
        write(self.source / "apps/ui/package.json", "{}")
        write(self.source / "node_modules/left-pad/index.js", "module.exports = 1\n")
        links = self.source / "node_modules/@print-partner"
        links.mkdir(parents=True)
        (links / "contracts").symlink_to("../../packages/contracts")
        (links / "ui").symlink_to("../../apps/ui")
        self.stage_artifacts()

    def tearDown(self):
        self.temporary.cleanup()

    def stage_artifacts(self):
        version = json.dumps({"version": "3.3.0"})
        for name in ["package.json", "apps/server/package.json",
                     "packages/contracts/package.json", "packages/domain/package.json"]:
            write(self.web / name, version)
        write(self.web / "package-lock.json", json.dumps(self.lock))
        write(self.web / "apps/web/dist/index.html", "<!doctype html>")
        write(self.web / "apps/web/dist/desktop-build.json",
              json.dumps({"mode": "desktop", "version": "3.3.0", "service_worker": False}))
        write(self.web / "apps/server/dist/current/desktop-resolution.js", "")
        write(self.web / "packages/contracts/dist/current/index.js", "")
        write(self.web / "packages/domain/dist/current/index.js", "")
        self.node = self.stage / "bin/node"
        write(self.node, "#!/bin/sh\necho v24.21.0\n")
        self.node.chmod(0o755)

    def build_manifest(self):
        return subprocess.run(
            [sys.executable, str(MANIFEST), "--web", str(self.web), "--node", str(self.node),
             "--package-root", str(self.stage), "--output", str(self.root / "manifest.json"),
             "--commit", COMMIT],
            capture_output=True, text=True)

    def test_unstaged_workspace_link_is_skipped_and_manifest_builds(self):
        skipped = stage_locked_dependencies(self.source, self.web, self.lock)
        self.assertEqual(skipped, ["node_modules/@print-partner/ui"])
        self.assertFalse(os.path.lexists(self.web / "node_modules/@print-partner/ui"))
        self.assertTrue((self.web / "node_modules/@print-partner/contracts/package.json").is_file())
        self.assertTrue((self.web / "node_modules/left-pad/index.js").is_file())
        self.assertEqual(dangling_links(self.stage), [])
        result = self.build_manifest()
        self.assertEqual(result.returncode, 0, result.stderr)
        links = json.loads((self.root / "manifest.json").read_text())["dependencies"]["links"]
        self.assertEqual(links, {"web/node_modules/@print-partner/contracts": "../../packages/contracts"})

    def test_dangling_workspace_link_fails_the_manifest(self):
        stage_locked_dependencies(self.source, self.web, self.lock)
        (self.web / "node_modules/@print-partner/ui").symlink_to("../../apps/ui")
        result = self.build_manifest()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FileNotFoundError", result.stderr)


if __name__ == "__main__":
    unittest.main()
