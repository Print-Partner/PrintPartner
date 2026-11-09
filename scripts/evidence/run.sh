#!/bin/sh
# Screenshot replacement-UI paths from a seeded Rust-only core. See scripts/evidence/README.md.
exec python3 "$(dirname "$0")/run.py" "$@"
