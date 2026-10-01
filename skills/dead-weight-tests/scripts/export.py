#!/usr/bin/env python3
"""Turn per-test LLVM profiles into per-test `llvm-cov export` JSON files.

Input: the profile root written by `runner.sh`. It has one directory per test
with `meta`, `exit`, and `*.profraw` files.
Output: one `<hash>.json.gz` per passing test. Each file is
`llvm-cov export -format=text` JSON with a top-level `test` object that names
the test. To keep files small, `data[].files` is dropped and
`data[].functions` keeps only functions the test executed, with only their
code regions (kind 0), covered or not. A region is
`[line_start, col_start, line_end, col_end, count, file_id, expanded_file_id, kind]`;
`file_id` indexes the function's `filenames`.

Failing or killed tests are listed in `skipped.json`: their coverage is partial.

Called by `collect.sh`; `analyze.py` reads the output.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

# Third-party and toolchain sources. Same intent as cargo-llvm-cov's default
# filter, except tests are kept: test code is part of what a test exercises.
IGNORE_FILENAME_REGEX = r"(^|/)(rustc/[0-9a-f]+|\.cargo/(registry|git)|\.rustup/toolchains|nix/store)/"


def llvm_tool(name: str) -> str:
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], check=True, capture_output=True, text=True).stdout.strip()
    host = next(
        line.split(": ", 1)[1]
        for line in subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout.splitlines()
        if line.startswith("host: ")
    )
    path = Path(sysroot) / "lib" / "rustlib" / host / "bin" / name
    if not path.exists():
        sys.exit(f"missing {path}; install the llvm-tools-preview rustup component")
    return str(path)


def workspace_bins(root: Path) -> list[str]:
    """Instrumented workspace binaries that tests may spawn as subprocesses."""
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    debug = Path(meta["target_directory"]) / "debug"
    names = {t["name"] for p in meta["packages"] for t in p["targets"] if "bin" in t["kind"]}
    return sorted(str(debug / n) for n in names if (debug / n).is_file())


def export_one(test_dir: Path, out_dir: Path, root: Path, bins: list[str], profdata: str, cov: str) -> tuple[str, str | None]:
    binary_id, test_name, binary, manifest_dir = (test_dir / "meta").read_text().split("\n")[:4]
    exit_file = test_dir / "exit"
    exit_code = exit_file.read_text().strip() if exit_file.exists() else "killed"
    if exit_code != "0":
        return test_dir.name, f"{binary_id} {test_name}: exit {exit_code}"
    profraws = sorted(str(p) for p in test_dir.glob("*.profraw"))
    if not profraws:
        return test_dir.name, f"{binary_id} {test_name}: no profile written"

    merged = test_dir / "merged.profdata"
    subprocess.run([profdata, "merge", "-sparse", "-o", str(merged), *profraws], check=True, capture_output=True)
    # One profile means only the test process ran; skip loading every binary.
    objects = [arg for b in bins for arg in ("-object", b)] if len(profraws) > 1 else []
    result = subprocess.run(
        [
            cov,
            "export",
            "-format=text",
            "-skip-expansions",
            f"-ignore-filename-regex={IGNORE_FILENAME_REGEX}",
            f"-instr-profile={merged}",
            binary,
            *objects,
        ],
        capture_output=True,
    )
    merged.unlink()
    if result.returncode != 0:
        return test_dir.name, f"{binary_id} {test_name}: llvm-cov failed: {result.stderr.decode()[-500:]}"

    export = json.loads(result.stdout)
    prefix = str(root) + "/"
    for data in export["data"]:
        data.pop("totals", None)
        data.pop("files", None)
        data["functions"] = [
            {
                "name": f["name"],
                "count": f["count"],
                "filenames": [name.removeprefix(prefix) for name in f["filenames"]],
                "regions": [r for r in f["regions"] if r[7] == 0],
            }
            for f in data["functions"]
            if f["count"] > 0
        ]
    export["test"] = {
        "binary_id": binary_id,
        "name": test_name,
        "binary": binary,
        "manifest_dir": manifest_dir.removeprefix(prefix) if manifest_dir != str(root) else ".",
        "processes": len(profraws),
    }
    with gzip.open(out_dir / f"{test_dir.name}.json.gz", "wt") as fh:
        json.dump(export, fh, separators=(",", ":"))
    return test_dir.name, None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("profiles", type=Path, help="profile root written by runner.sh")
    parser.add_argument("out", type=Path, help="output directory for <hash>.json.gz files")
    parser.add_argument("-j", "--jobs", type=int, default=os.cpu_count())
    args = parser.parse_args()

    root = Path.cwd().resolve()
    profdata, cov = llvm_tool("llvm-profdata"), llvm_tool("llvm-cov")
    bins = workspace_bins(root)
    test_dirs = sorted(d for d in args.profiles.iterdir() if (d / "meta").is_file())
    args.out.mkdir(parents=True, exist_ok=True)

    skipped: list[str] = []
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(export_one, d, args.out, root, bins, profdata, cov) for d in test_dirs]
        for done, future in enumerate(as_completed(futures), 1):
            _, problem = future.result()
            if problem:
                skipped.append(problem)
            if done % 200 == 0 or done == len(futures):
                print(f"exported {done}/{len(futures)}", file=sys.stderr)

    (args.out / "skipped.json").write_text(json.dumps(sorted(skipped), indent=2) + "\n")
    print(f"{len(test_dirs) - len(skipped)} tests exported to {args.out}; {len(skipped)} skipped (see skipped.json)")


if __name__ == "__main__":
    main()
