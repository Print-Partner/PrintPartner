#!/usr/bin/env bash
# Build the Linux .deb from a completed stage and add the runtime symlinks the
# Tauri deb bundler drops. PP_TAURI names the Tauri CLI command (default:
# cargo tauri); extra arguments go to tauri build, for example
# --features desktop_rebuild.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
stage="${PP_DESKTOP_STAGE:?Set PP_DESKTOP_STAGE to a completed Linux stage}"
[[ "$(uname -s)" == "Linux" ]] || { echo "The .deb helper requires Linux" >&2; exit 1; }
[[ "$stage" == /* ]] || { echo "PP_DESKTOP_STAGE must be absolute" >&2; exit 1; }
export PP_DESKTOP_RELEASE_MANIFEST="$stage/bundle-manifest.json"
read -r -a tauri <<< "${PP_TAURI:-cargo tauri}"
bundle="$repo/rust/target/release/bundle/deb"
rm -f "$bundle"/*.deb
(cd "$repo/rust/crates/pp-desktop" && "${tauri[@]}" build --config "$stage/bundle-config.json" --no-sign --bundles deb "$@")
debs=("$bundle"/*.deb)
[[ "${#debs[@]}" -eq 1 && -f "${debs[0]}" ]] || { echo "Expected one .deb in $bundle" >&2; exit 1; }
python3 "$repo/rust/scripts/assemble-linux-deb.py" --stage "$stage" --deb "${debs[0]}"
