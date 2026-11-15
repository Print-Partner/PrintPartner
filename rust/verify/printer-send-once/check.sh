#!/bin/sh
# Check the printer-send model. faithful must pass. broken must fail.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
VERSION=1.7.4
JAR="${TLA2TOOLS_JAR:-$HOME/.cache/print-partner/tla2tools-$VERSION.jar}"
MODE=${1:-faithful}
case "$MODE" in
  faithful) CFG=SendOnce.cfg ;;
  broken) CFG=SendOnceBroken.cfg ;;
  *)
    echo "usage: check.sh [faithful|broken]" >&2
    exit 2
    ;;
esac
if [ ! -f "$JAR" ]; then
  mkdir -p "$(dirname "$JAR")"
  curl -fsSL -o "$JAR" "https://github.com/tlaplus/tlaplus/releases/download/v$VERSION/tla2tools.jar"
fi
cd "$ROOT"
METADIR="${TMPDIR:-/tmp}/pp-send-once-$MODE"
rm -rf "$METADIR"
mkdir -p "$METADIR"
exec java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -workers 1 -metadir "$METADIR" -config "$CFG" SendOnce.tla
