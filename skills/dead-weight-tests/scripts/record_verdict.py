#!/usr/bin/env python3
"""Validate review verdicts and append them to verdicts.jsonl.

Reads JSON lines (or one JSON array) of {"test", "other", "verdict", "reason"}
from INPUT or stdin. Appends rows whose (test, other) pair is in candidates.json,
whose verdict is cut, merge, keep, or fix, and whose reason is not empty.
Prints {"recorded", "invalid", "missing"} as JSON. Exits 1 if any row is
invalid or an --expect pair still has no verdict.
"""

import argparse
import json
import sys
from pathlib import Path

VERDICTS = ("cut", "merge", "keep", "fix")
KEYS = ("test", "other", "verdict", "reason")


def parse_items(text):
    """Return (line, item_or_None, error_or_None) for each input item."""
    stripped = text.strip()
    if stripped.startswith("["):
        try:
            items = json.loads(stripped)
        except json.JSONDecodeError:
            return [(1, None, "malformed JSON")]
        return [(i, item, None) for i, item in enumerate(items, 1)]
    out = []
    for i, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            out.append((i, json.loads(line), None))
        except json.JSONDecodeError:
            out.append((i, None, "malformed JSON"))
    return out


def validate(item, pairs):
    """Return an error string for an invalid verdict item, or None."""
    if not isinstance(item, dict):
        return "malformed JSON"
    if (item.get("test"), item.get("other")) not in pairs:
        return "unknown pair"
    if item.get("verdict") not in VERDICTS:
        return "unknown verdict"
    reason = item.get("reason")
    if not isinstance(reason, str) or not reason.strip():
        return "empty reason"
    return None


def load_pairs(path):
    """Return the set of (test, other) pairs in a candidates JSON list."""
    return {(row["test"], row["other"]) for row in json.loads(Path(path).read_text())}


def judged_pairs(path):
    """Return pairs that have a well-formed verdict line in a verdicts file."""
    done = set()
    if not path.exists():
        return done
    for line in path.read_text().splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(row, dict) and row.get("verdict") in VERDICTS:
            done.add((row.get("test"), row.get("other")))
    return done


def record(text, candidates, verdicts, expect=None):
    """Append valid items to verdicts; return (summary dict, exit code)."""
    pairs = load_pairs(candidates)
    rows, invalid = [], []
    for line, item, error in parse_items(text):
        error = error or validate(item, pairs)
        if error:
            get = item.get if isinstance(item, dict) else (lambda _k: None)
            test, other = get("test"), get("other")
            invalid.append({
                "line": line,
                "test": test if isinstance(test, str) else None,
                "other": other if isinstance(other, str) else None,
                "error": error,
            })
        else:
            rows.append({k: item[k] for k in KEYS})
    verdicts.parent.mkdir(parents=True, exist_ok=True)
    with verdicts.open("a") as f:
        for row in rows:
            f.write(json.dumps(row) + "\n")
    missing = []
    if expect:
        done = judged_pairs(verdicts)
        for row in json.loads(Path(expect).read_text()):
            pair = (row["test"], row["other"])
            if pair not in done and list(pair) not in missing:
                missing.append(list(pair))
    summary = {"recorded": len(rows), "invalid": invalid, "missing": missing}
    return summary, 1 if invalid or missing else 0


def main(argv=None):
    """CLI entry point."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("input", nargs="?", default="-", help="JSON lines or a JSON array (default: stdin)")
    parser.add_argument("--candidates", type=Path, default=Path("target/coverage/per-test/candidates.json"),
                        help="candidates.json from analyze.py (default: target/coverage/per-test/candidates.json)")
    parser.add_argument("--verdicts", type=Path, help="verdicts file to append to (default: verdicts.jsonl next to --candidates)")
    parser.add_argument("--expect", type=Path, help="JSON list of candidate rows that must all have verdicts")
    args = parser.parse_args(argv)
    verdicts = args.verdicts or args.candidates.parent / "verdicts.jsonl"
    text = sys.stdin.read() if args.input == "-" else Path(args.input).read_text()
    summary, code = record(text, args.candidates, verdicts, args.expect)
    print(json.dumps(summary))
    return code


if __name__ == "__main__":
    sys.exit(main())
