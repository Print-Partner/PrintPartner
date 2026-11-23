#!/usr/bin/env bash
set -euo pipefail

test "$(node --version)" = v24.21.0
test "$("${PP_SCHEMA_PARITY_NODE:?Parity Node must be configured}" --version)" = v22.23.3
test "$(rustc --version | cut -d ' ' -f 2)" = 1.100.0
