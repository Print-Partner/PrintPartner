#!/usr/bin/env bash
# Prepare a Factory Environment instance for Print Partner Mission workers.
# Use it as the repository setup script: `bash scripts/factory-environment/setup.sh`.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

RUSTUP_VERSION=1.29.2
PREFIX="$HOME/.local/print-partner"

case "$(uname -m)" in
  x86_64)
    node_arch=x64 rust_triple=x86_64-unknown-linux-gnu
    rustup_sha=5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4
    runtime_sha=fd8e59d5a511510f6a298afb548f18c7d2b1be404d8b4a27d94fbe49f56cb2d6
    parity_sha=df450af89261115ef9f9e3830c3eeb2cc9213b63c720b1af623cb5dcbe2e02de ;;
  aarch64)
    node_arch=arm64 rust_triple=aarch64-unknown-linux-gnu
    rustup_sha=a9c168dbea81a84671c6b7a5c1795cb0eb699020110d107ddfca427bd797e697
    runtime_sha=6ad1325edbdb5649c379b75a237147a666c95d4f9ae8d340fef2d1575d289ad2
    parity_sha=a44aeb94849a299b22df10b9e622ec2f605c2183501bc40590705131de7c740f ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

# Native packages come from the development container, browsers included, so a Mission can
# capture the UI screenshot that web and desktop PRs need. Headless Chromium needs no display.
mapfile -t native_packages < <(awk '/--no-install-recommends/ { on = 1; sub(/.*--no-install-recommends/, "") }
  on { for (i = 1; i <= NF; i++) { if ($i == "&&") { on = 0; break } if ($i != "\\") print $i } }' \
  .devcontainer/Dockerfile)
(( ${#native_packages[@]} )) || { echo "No packages found in .devcontainer/Dockerfile" >&2; exit 1; }
packages=(build-essential ca-certificates curl git xz-utils "${native_packages[@]}")
if [[ $(id -u) == 0 ]]; then
  as_root=()
elif sudo -n true 2>/dev/null; then
  as_root=(sudo -n)
else
  echo "Root or passwordless sudo is required to install: ${packages[*]}" >&2
  exit 1
fi
"${as_root[@]}" apt-get update
"${as_root[@]}" env DEBIAN_FRONTEND=noninteractive \
  apt-get install -y --no-install-recommends "${packages[@]}"

node_versions="$(python3 - <<'PY'
from pathlib import Path
import re
dockerfile = Path(".devcontainer/Dockerfile").read_text()
for stage in ("runtime-node", "parity-node"):
    print(re.search(rf"^FROM node:([0-9.]+)-\S+ AS {stage}$", dockerfile, re.M)[1])
PY
)"
read -r NODE_RUNTIME NODE_PARITY <<<"$(echo "$node_versions" | tr '\n' ' ')"

download() { # url sha256 destination
  curl -fsSL --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum -c --quiet -
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$PREFIX"
for spec in "node24 $NODE_RUNTIME $runtime_sha" "node22 $NODE_PARITY $parity_sha"; do
  read -r name version sha <<<"$spec"
  if [[ ! -x "$PREFIX/$name/bin/node" || "$("$PREFIX/$name/bin/node" --version)" != "v$version" ]]; then
    archive="$work/node-$version.tar.xz"
    download "https://nodejs.org/dist/v$version/node-v$version-linux-$node_arch.tar.xz" "$sha" "$archive"
    rm -rf "${PREFIX:?}/${name:?}" && mkdir -p "$PREFIX/$name"
    tar -xJf "$archive" -C "$PREFIX/$name" --strip-components 1
  fi
done
# Existing runtime proofs invoke this path rather than resolving PATH.
"${as_root[@]}" ln -sfn "$PREFIX/node24/bin/node" /usr/bin/node
test "$(/usr/bin/node --version)" = "v$NODE_RUNTIME"

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
  download "https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/$rust_triple/rustup-init" \
    "$rustup_sha" "$work/rustup-init"
  chmod +x "$work/rustup-init"
  "$work/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal
fi
# rust-toolchain.toml names the version and components.
"$CARGO_HOME/bin/rustup" toolchain install
# Outside the checkout no toolchain file applies, so make the pinned one the default.
"$CARGO_HOME/bin/rustup" default "$("$CARGO_HOME/bin/rustup" show active-toolchain | cut -d" " -f1)"

python3 -m venv "$PREFIX/sidecar-venv"
sidecar_pins="$(python3 - <<'PY'
from pathlib import Path
import re
dockerfile = Path("slicer-sidecar/Dockerfile").read_text()
for dependency in ("flask", "waitress"):
    print(re.search(rf"\b{dependency}==[0-9.]+", dockerfile)[0])
PY
)"
read -r -a sidecar_requirements <<<"$(echo "$sidecar_pins" | tr '\n' ' ')"
"$PREFIX/sidecar-venv/bin/pip" install --quiet "${sidecar_requirements[@]}"

cat >"$PREFIX/env.sh" <<ENV
export PATH="$PREFIX/node24/bin:$CARGO_HOME/bin:\$PATH"
export CARGO_HOME="$CARGO_HOME" RUSTUP_HOME="$RUSTUP_HOME"
export PP_SCHEMA_PARITY_NODE="$PREFIX/node22/bin/node"
export PP_SIDECAR_PYTHON="$PREFIX/sidecar-venv/bin/python"
export PLAYWRIGHT_CHROMIUM_EXECUTABLE="/usr/bin/chromium"
export INSTA_UPDATE=no
ENV
for profile in "$HOME/.bashrc" "$HOME/.profile"; do
  grep -qsF "$PREFIX/env.sh" "$profile" || echo ". \"$PREFIX/env.sh\"" >>"$profile"
done
# shellcheck source=/dev/null
. "$PREFIX/env.sh"

cargo clippy --version
# post-create runs the shared Node and Rust version preflight.
bash .devcontainer/post-create.sh
echo "Print Partner environment ready. New shells load $PREFIX/env.sh."
