#!/usr/bin/env bash
# Collect per-test region coverage for a Cargo workspace with cargo-nextest.
#
# Usage (from the workspace root):
#   collect.sh [--out DIR] [--nextest CMD] [--before CMD] [-- NEXTEST_ARGS...]
#
#   --out DIR      output directory (default: target/coverage/per-test)
#   --nextest CMD  command that runs nextest (default: "cargo nextest run");
#                  use it for wrappers that end in `cargo nextest run "$@"`
#   --before CMD   shell command to run after coverage is enabled and before
#                  the tests, e.g. building binaries that tests spawn from
#                  another package
#   NEXTEST_ARGS   passed to nextest, e.g. --workspace -E 'package(foo)'
#
# Output: DIR/tests/<hash>.json.gz, one pruned `llvm-cov export` JSON per
# passing test, and DIR/tests/skipped.json for failed or killed tests.
#
# Requires cargo-nextest, cargo-llvm-cov, the llvm-tools-preview rustup
# component, and python3. Workspace crates are cleaned and rebuilt with
# coverage, so the next normal build recompiles them.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="target/coverage/per-test"
nextest="cargo nextest run"
before=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --nextest) nextest="$2"; shift 2 ;;
    --before) before="$2"; shift 2 ;;
    --) shift; break ;;
    -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) break ;;
  esac
done

for tool in cargo-nextest cargo-llvm-cov python3; do
  command -v "$tool" >/dev/null || { echo "collect.sh: missing $tool" >&2; exit 1; }
done

mkdir -p "$out"
out="$(cd "$out" && pwd)"
rm -rf "$out/profiles" "$out/other" "$out/tests"
mkdir -p "$out/profiles" "$out/other"

# Instrument workspace crates only (cargo-llvm-cov's RUSTC_WRAPPER mode).
eval "$(cargo llvm-cov show-env --sh 2>/dev/null || cargo llvm-cov show-env --export-prefix)"
# The wrapper adds -C instrument-coverage outside cargo's fingerprint, so artifacts from a
# normal build would be reused uninstrumented. Remove the workspace crates' artifacts first.
cargo llvm-cov clean --workspace
# Processes outside a test (build scripts, setup scripts, test listing) write here.
export LLVM_PROFILE_FILE="$out/other/%p-%m.profraw"
export DWT_PROFILE_ROOT="$out/profiles"
host="$(rustc -vV | sed -n 's/^host: //p')"
export "CARGO_TARGET_$(printf '%s' "$host" | tr 'a-z-' 'A-Z_')_RUNNER=$here/runner.sh"

if [[ -n "$before" ]]; then
  bash -c "$before"
fi

status=0
# shellcheck disable=SC2086 # $nextest is a command line on purpose.
$nextest --no-fail-fast "$@" || status=$?
# 100 = some tests failed; export skips them. Anything else is a broken run.
if [[ "$status" -ne 0 && "$status" -ne 100 ]]; then
  exit "$status"
fi

python3 "$here/export.py" "$out/profiles" "$out/tests"
rm -rf "$out/profiles" "$out/other"
