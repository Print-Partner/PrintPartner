#!/bin/sh
# Check the printer-send model. faithful must pass. broken must fail.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
VERSION=1.7.4
# SHA-256 of the v1.7.4 tla2tools.jar asset. The release notes publish that
# asset's SHA-1 as bee4a54f3ee3d4afc347c3240ec2d9e93b075104.
SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
DEFAULT_JAR="$HOME/.cache/print-partner/tla2tools-$VERSION.jar"
JAR="${TLA2TOOLS_JAR:-$DEFAULT_JAR}"
MODE=${1:-faithful}
case "$MODE" in
  faithful) CFG=SendOnce.cfg ;;
  broken) CFG=SendOnceBroken.cfg ;;
  *)
    echo "usage: check.sh [faithful|broken]" >&2
    exit 2
    ;;
esac

hash_ok() {
  echo "$SHA256  $1" | sha256sum -c - >/dev/null
}

if [ -n "${TLA2TOOLS_JAR:-}" ]; then
  if [ ! -f "$JAR" ]; then
    echo "TLA2TOOLS_JAR is not a file: $JAR" >&2
    exit 1
  fi
  hash_ok "$JAR" || {
    echo "TLA2TOOLS_JAR does not match tla2tools $VERSION" >&2
    exit 1
  }
else
  if [ -f "$JAR" ] && ! hash_ok "$JAR"; then
    rm -f "$JAR"
  fi
  if [ ! -f "$JAR" ]; then
    mkdir -p "$(dirname "$JAR")"
    tmp="$JAR.tmp.$$"
    curl -fsSL -o "$tmp" "https://github.com/tlaplus/tlaplus/releases/download/v$VERSION/tla2tools.jar" || {
      rm -f "$tmp"
      exit 1
    }
    hash_ok "$tmp" || {
      rm -f "$tmp"
      echo "Downloaded tla2tools $VERSION failed the SHA-256 check" >&2
      exit 1
    }
    mv "$tmp" "$JAR"
  fi
fi

cd "$ROOT"
METADIR="${TMPDIR:-/tmp}/pp-send-once-$MODE"
rm -rf "$METADIR"
mkdir -p "$METADIR"
exec java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -workers 1 -metadir "$METADIR" -config "$CFG" SendOnce.tla
