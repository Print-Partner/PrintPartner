#!/usr/bin/env bash
# Build the unsigned macOS app from a completed stage. PP_TAURI names the Tauri
# CLI command (default: cargo tauri); extra arguments go to tauri build, for
# example --features desktop_rebuild. Options that move the bundle are refused.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
stage="${PP_DESKTOP_STAGE:?Set PP_DESKTOP_STAGE to a completed target-native stage}"
[[ "$(uname -s)" == "Darwin" ]] || { echo "The app bundle helper requires macOS" >&2; exit 1; }
[[ "$stage" == /* ]] || { echo "PP_DESKTOP_STAGE must be absolute" >&2; exit 1; }
export MACOSX_DEPLOYMENT_TARGET="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle"]["macOS"]["minimumSystemVersion"])' "$repo/rust/crates/pp-desktop/tauri.conf.json")"
export PP_DESKTOP_RELEASE_MANIFEST="$stage/bundle-manifest.json"
python3 "$repo/rust/scripts/assemble-macos-resources.py" --stage "$stage" --validate-stage
read -r -a tauri <<< "${PP_TAURI:-cargo tauri}"
for argument in "$@"; do
  case "$argument" in
    --target|--target=*|-t|--debug|-d|--profile|--profile=*)
      echo "desktop-bundle.sh builds the host release app; $argument changes the bundle directory" >&2; exit 1 ;;
  esac
done
cd "$repo/rust/crates/pp-desktop"
"${tauri[@]}" build --config "$stage/bundle-config.json" --no-sign --no-binary-patching --bundles app "$@"
python3 "$repo/rust/scripts/assemble-macos-resources.py" --stage "$stage"
