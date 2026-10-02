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
# passing test, and DIR/tests/skipped.json for tests that failed, were
# killed, or never reached the runner.
#
# Requires cargo-nextest, cargo-llvm-cov, the llvm-tools-preview rustup
# component, and python3. Workspace crates are cleaned and rebuilt with
# coverage. When the script exits, even after a failure, it removes those
# instrumented artifacts again, so the next normal build rebuilds workspace
# crates.
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
    -h|--help) sed -n '2,23p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
# Normal builds would reuse instrumented artifacts left behind, and their
# binaries would drop default_*.profraw files into every cwd. The clean must
# run inside the show-env environment: outside it, it cleans a different dir.
cleanup() {
  local status=$?
  trap - EXIT
  echo "collect.sh: removing coverage-instrumented workspace artifacts" >&2
  if ! cargo llvm-cov clean --workspace; then
    echo "collect.sh: 'cargo llvm-cov clean --workspace' failed; run it under 'eval \"\$(cargo llvm-cov show-env --sh)\"' before a normal build" >&2
    [[ "$status" -ne 0 ]] || status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
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

# Tests nextest killed before runner.sh started leave no profile dir; list the
# selection so export.py can report them. Listing is best effort.
listed=()
if [[ " $nextest " == *" nextest run "* ]]; then
  list_cmd=" $nextest "
  list_cmd="${list_cmd/ nextest run / nextest list }"
  # shellcheck disable=SC2086 # $list_cmd is a command line on purpose.
  if $list_cmd --message-format json "$@" >"$out/other/listed.json" 2>"$out/other/list.log"; then
    listed=(--listed "$out/other/listed.json")
  else
    echo "collect.sh: warning: test listing failed ($(tail -n 1 "$out/other/list.log")); skipped.json omits tests that never reached the runner" >&2
  fi
else
  echo "collect.sh: warning: no 'nextest run' in --nextest command; skipped.json omits tests that never reached the runner" >&2
fi

# Needs the instrumented binaries for -object, so it runs before cleanup.
# ${listed[@]+...}: bash 3.2 treats an empty array as unset under `set -u`.
python3 "$here/export.py" ${listed[@]+"${listed[@]}"} "$out/profiles" "$out/tests"
rm -rf "$out/profiles" "$out/other"
