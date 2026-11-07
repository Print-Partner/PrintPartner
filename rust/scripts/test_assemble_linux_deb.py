import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "assemble_linux_deb", Path(__file__).with_name("assemble-linux-deb.py"))
assembler = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assembler)

LINK = "web/node_modules/@print-partner/server"
TARGET = "../../apps/server"


def write(path, text, mode=0o644):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(mode)


class AssembleLinuxDebTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.stage = self.root / "stage"
        write(self.stage / "web/apps/server/index.js", "server")
        write(self.stage / "bin/node", "node", 0o755)
        write(self.stage / "release.json", "{}")
        (self.stage / LINK).parent.mkdir(parents=True)
        (self.stage / LINK).symlink_to(TARGET)
        manifest = {"dependencies": {"links": {LINK: TARGET}}}
        (self.stage / "bundle-manifest.json").write_text(json.dumps(manifest))
        self.write_config({str(self.stage / name): "desktop-runtime/" + name
                           for name in ("web", "bin", "release.json")})

    def write_config(self, resources):
        config = {"bundle": {"active": True, "resources": resources}}
        (self.stage / "bundle-config.json").write_text(json.dumps(config))

    def deb(self, edit=None):
        """Build a .deb the way tauri does: the stage's runtime without its symlinks."""
        package = self.root / "package"
        runtime = package / "usr/lib/Print Partner/desktop-runtime"
        write(package / "DEBIAN/control", "Package: print-partner\nVersion: 1\n"
              "Architecture: all\nMaintainer: test\nDescription: test\n")
        write(package / "usr/bin/pp-desktop", "app", 0o755)
        for relative in ("web/apps/server/index.js", "bin/node", "release.json"):
            source = self.stage / relative
            write(runtime / relative, source.read_text(), source.stat().st_mode & 0o777)
        if edit:
            edit(runtime)
        deb = self.root / "print-partner.deb"
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(package), str(deb)],
                       check=True, stdout=subprocess.DEVNULL)
        return deb

    def listing(self, deb):
        return subprocess.run(["dpkg-deb", "-c", str(deb)], check=True, capture_output=True,
                              text=True).stdout

    def test_restores_the_dropped_link(self):
        deb = self.deb()
        self.assertNotIn("@print-partner", self.listing(deb))
        result = assembler.assemble(self.stage, deb)
        self.assertEqual(result["restored_links"], [LINK])
        self.assertIn(f"desktop-runtime/{LINK} -> {TARGET}", self.listing(deb))

    def test_complete_package_is_rebuilt_unchanged(self):
        def add_link(runtime):
            (runtime / LINK).parent.mkdir(parents=True)
            (runtime / LINK).symlink_to(TARGET)

        deb = self.deb(add_link)
        self.assertEqual(assembler.assemble(self.stage, deb)["restored_links"], [])

    def test_changed_file_is_refused(self):
        deb = self.deb(lambda runtime: (runtime / "release.json").write_text("[]"))
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, deb)

    def test_extra_file_is_refused(self):
        deb = self.deb(lambda runtime: (runtime / "web/extra.js").write_text("x"))
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, deb)

    def test_missing_file_is_refused(self):
        deb = self.deb(lambda runtime: os.remove(runtime / "bin/node"))
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, deb)

    def test_restored_directory_keeps_the_stage_mode_under_a_strict_umask(self):
        deb = self.deb()
        previous = os.umask(0o077)
        try:
            assembler.assemble(self.stage, deb)
        finally:
            os.umask(previous)
        self.assertIn("drwxr-xr-x root/root", next(
            line for line in self.listing(deb).splitlines()
            if line.endswith("node_modules/@print-partner/")))

    def test_changed_directory_mode_is_refused(self):
        deb = self.deb(lambda runtime: (runtime / "web/apps").chmod(0o700))
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, deb)

    def test_resource_mapping_outside_the_runtime_is_refused(self):
        self.write_config({str(self.stage / "web"): "elsewhere/web"})
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, self.deb())

    def test_lost_executable_bit_is_refused(self):
        deb = self.deb(lambda runtime: (runtime / "bin/node").chmod(0o644))
        with self.assertRaises(SystemExit):
            assembler.assemble(self.stage, deb)


if __name__ == "__main__":
    unittest.main()
