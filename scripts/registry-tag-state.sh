#!/usr/bin/env bash
# Print "present" or "absent" for an image reference, e.g.
#
#   scripts/registry-tag-state.sh ghcr.io/print-partner/pp-server:1.2.3
#
# "absent" needs buildx's exact not-found error for that reference. Any other
# failure (auth, network, timeout) exits non-zero, so a release never treats an
# unreadable tag as missing and writes over it. Used by .github/workflows/release.yml.
set -euo pipefail

ref="${1:?usage: registry-tag-state.sh <image reference>}"
if err="$(docker buildx imagetools inspect "$ref" 2>&1 >/dev/null)"; then
  echo present
elif grep -Fxq "ERROR: $ref: not found" <<<"$err"; then
  echo absent
else
  printf 'registry read failed for %s:\n%s\n' "$ref" "$err" >&2
  exit 1
fi
