#!/usr/bin/env bash
# Nextest target runner installed by collect.sh.
#
# Gives each test process its own LLVM profile directory so coverage can be
# attributed to one test. Child processes the test spawns inherit
# LLVM_PROFILE_FILE, so their coverage lands in the same directory.
# The list phase has no NEXTEST_TEST_NAME and runs the binary unchanged.
set -uo pipefail

if [[ -z "${NEXTEST_TEST_NAME:-}" || -z "${DWT_PROFILE_ROOT:-}" ]]; then
  exec "$@"
fi

id="${NEXTEST_BINARY_ID} ${NEXTEST_TEST_NAME}"
hash="$(printf '%s' "$id" | { sha256sum 2>/dev/null || shasum -a 256; } | cut -c1-32)"
dir="${DWT_PROFILE_ROOT}/${hash}"
mkdir -p "$dir"
# One field per line: test names never contain newlines.
printf '%s\n' "$NEXTEST_BINARY_ID" "$NEXTEST_TEST_NAME" "$1" "${CARGO_MANIFEST_DIR:-}" >"$dir/meta"

export LLVM_PROFILE_FILE="${dir}/%p-%m.profraw"
"$@"
status=$?
# A missing `exit` file means the runner itself was killed (timeout).
printf '%s\n' "$status" >"$dir/exit"
exit "$status"
