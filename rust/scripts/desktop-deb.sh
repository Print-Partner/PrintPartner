#!/usr/bin/env bash
# Build the Linux .deb from a completed stage and add the runtime symlinks the
# Tauri deb bundler drops. PP_TAURI names the Tauri CLI command (default:
# cargo tauri); extra arguments go to tauri build, for example
# --features desktop_rebuild. The package lands in
# ${CARGO_TARGET_DIR:-rust/target}/release/bundle/deb; options that move it
# (--target, --debug, --profile) are refused.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
stage="${PP_DESKTOP_STAGE:?Set PP_DESKTOP_STAGE to a completed Linux stage}"
[[ "$(uname -s)" == "Linux" ]] || { echo "The .deb helper requires Linux" >&2; exit 1; }
[[ "$stage" == /* ]] || { echo "PP_DESKTOP_STAGE must be absolute" >&2; exit 1; }
export PP_DESKTOP_RELEASE_MANIFEST="$stage/bundle-manifest.json"
read -r -a tauri <<< "${PP_TAURI:-cargo tauri}"
for argument in "$@"; do
  case "$argument" in
    --target|--target=*|-t|--debug|-d|--profile|--profile=*)
      echo "desktop-deb.sh builds the host release package; $argument changes the bundle directory" >&2; exit 1 ;;
  esac
done
target_dir="${CARGO_TARGET_DIR:-$repo/rust/target}"
[[ "$target_dir" == /* ]] || { echo "CARGO_TARGET_DIR must be absolute" >&2; exit 1; }
bundle="$target_dir/release/bundle/deb"
rm -f "$bundle"/*.deb
(cd "$repo/rust/crates/pp-desktop" && "${tauri[@]}" build --config "$stage/bundle-config.json" --no-sign --bundles deb "$@")
debs=("$bundle"/*.deb)
[[ "${#debs[@]}" -eq 1 && -f "${debs[0]}" ]] || { echo "Expected one .deb in $bundle" >&2; exit 1; }
python3 "$repo/rust/scripts/assemble-linux-deb.py" --stage "$stage" --deb "${debs[0]}"
