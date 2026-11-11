"""Update the filament live-format declaration from the Rust writer version."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "rust/crates/pp-storage/src/filaments.rs"
MANIFEST = ROOT / "rust/crates/pp-gateway/ownership-manifest.json"


def main():
    version = int(re.search(r"pub const FORMAT_VERSION: u32 = (\d+);", SOURCE.read_text())[1])
    text = MANIFEST.read_text()
    parsed = json.loads(text)
    parsed["live_formats"]["custom-filaments"]["rust_writes"] = version
    # Preserve the hand-maintained operation inventory's formatting.
    updated, count = re.subn(
        r'("custom-filaments": \{\s*"node_reads": 1,\s*"rust_writes": )("none"|\d+)',
        lambda match: match[1] + str(version), text,
    )
    if count != 1 or json.loads(updated) != parsed:
        raise ValueError("Unexpected filament live-format declaration")
    MANIFEST.write_text(updated)


if __name__ == "__main__":
    main()
