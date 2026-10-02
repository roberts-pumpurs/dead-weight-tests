#!/usr/bin/env python3
"""End-to-end check: collect.sh and analyze.py find the dead weight planted in tests/fixture.

Run from tests/fixture after:
  ../../skills/dead-weight-tests/scripts/collect.sh -- --workspace
  python3 ../../skills/dead-weight-tests/scripts/analyze.py target/coverage/per-test/tests \
    --json target/coverage/per-test/candidates.json --html target/coverage/per-test/report.html

It then runs a normal `cargo build` and checks the `describe` binary carries no
coverage instrumentation left over from the collection.
"""

import glob
import gzip
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

out = Path("target/coverage/per-test")
candidates = json.loads((out / "candidates.json").read_text())
pairs = {(c["kind"], c["test"].split(" ", 1)[1], c["other"].split(" ", 1)[1]) for c in candidates}
failures = []

for expected in [
    ("duplicate", "tests::parses_another_number", "tests::parses_a_number"),
    ("subsumed", "tests::classifies_positive", "tests::describes_a_positive_number"),
]:
    if expected not in pairs:
        failures.append(f"missing candidate {expected}")

# The subprocess test reaches the library only through the `describe` binary.
negative_line = next(n for n, line in enumerate(Path("src/lib.rs").read_text().splitlines(), 1) if '"negative"' in line)
cli = None
for path in glob.glob(str(out / "tests" / "*.json.gz")):
    export = json.load(gzip.open(path))
    if export["test"]["name"] == "cli_describes_a_negative_number":
        cli = export
if cli is None:
    failures.append("no coverage for cli_describes_a_negative_number")
else:
    if cli["test"]["processes"] < 2:
        failures.append(f"subprocess profile missing: processes={cli['test']['processes']}")
    covered = {
        (f["filenames"][r[5]], r[0])
        for d in cli["data"]
        for f in d["functions"]
        for r in f["regions"]
        if r[4] > 0
    }
    if ("src/lib.rs", negative_line) not in covered:
        failures.append("subprocess coverage of src/lib.rs negative branch not attributed to the test")

if "__MODEL_JSON__" in (out / "report.html").read_text():
    failures.append("report.html still contains the model placeholder")


def instrumented(binary: Path) -> bool:
    """Whether `binary` links the LLVM profiler runtime, i.e. was built with coverage."""
    if shutil.which("nm"):
        result = subprocess.run(["nm", str(binary)], capture_output=True, text=True)
        if result.returncode == 0:
            return "__llvm_profile_runtime" in result.stdout
    # Without LLVM_PROFILE_FILE an instrumented binary writes default_*.profraw into its cwd.
    env = {k: v for k, v in os.environ.items() if k != "LLVM_PROFILE_FILE"}
    with tempfile.TemporaryDirectory() as cwd:
        subprocess.run([str(binary.resolve()), "1"], cwd=cwd, env=env, capture_output=True, check=True)
        return bool(glob.glob(os.path.join(cwd, "*.profraw")))


# collect.sh must remove its instrumented artifacts: cargo's fingerprint ignores
# the coverage wrapper, so the next normal build would reuse them.
subprocess.run(["cargo", "build", "--quiet", "--bin", "describe"], check=True)
describe = Path("target/debug/describe")
if instrumented(describe):
    failures.append(f"{describe} is coverage-instrumented after a normal build; collect.sh left instrumented artifacts")

if failures:
    print("\n".join(failures), file=sys.stderr)
    sys.exit(1)
print(f"fixture check passed: {len(candidates)} candidates")
