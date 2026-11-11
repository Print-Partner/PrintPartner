"""Crash the actual Rust filament publisher halfway through its temporary write."""
import json
import os
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    (ROOT / ".lane-logs").mkdir(exist_ok=True)
    build = subprocess.run([
        "cargo", "test", "--locked", "--manifest-path", "rust/Cargo.toml",
        "-p", "pp-storage", "--features", "desktop_rebuild", "--lib",
        "--no-run", "--message-format=json",
    ], cwd=ROOT, capture_output=True, text=True, check=True)
    binary = next(
        message["executable"] for line in build.stdout.splitlines()
        if (message := json.loads(line)).get("reason") == "compiler-artifact"
        and message.get("target", {}).get("name") == "pp_storage"
        and message.get("profile", {}).get("test") and message.get("executable")
    )
    with tempfile.TemporaryDirectory(prefix="filament-crash-", dir=ROOT / ".lane-logs") as directory:
        store = Path(directory) / "custom_filaments.json"
        previous = b'{"filaments":[{"id":"keep"}]}'
        store.write_bytes(previous)
        env = dict(os.environ, PP_FILAMENT_CRASH_TEST=directory, PP_FILAMENT_CRASH="1")
        command = [binary, "--exact", "filaments::runtime::crash_tests::crash_child"]
        crashed = subprocess.run(command, env=env, capture_output=True, check=False)
        assert crashed.returncode == 73, crashed.stderr.decode()
        assert store.read_bytes() == previous
        assert json.loads(store.read_bytes())["filaments"][0]["id"] == "keep"
        partial = list(Path(directory).glob(".custom-filaments-*"))
        assert len(partial) == 1
        assert partial[0].stat().st_size > 0
        try:
            json.loads(partial[0].read_bytes())
        except json.JSONDecodeError:
            pass
        else:
            raise AssertionError("crash did not leave an incomplete temporary JSON file")
        del env["PP_FILAMENT_CRASH"]
        subprocess.run(command, env=env, capture_output=True, check=True)
        assert json.loads(store.read_bytes()) == {"filaments": []}
    print("PASS: 1 process crash/restart publication test; exit=73, previous file readable, partial temp ignored, restart publishes complete JSON")


if __name__ == "__main__":
    main()
