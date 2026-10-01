#!/usr/bin/env python3
"""Find dead-weight test candidates from per-test coverage.

Input: per-test `llvm-cov export` JSON files (`.json` or `.json.gz`), or
directories of them, as written by `collect.sh`. A file without a
top-level `test` object is named after its file stem.

A region is an LLVM code region: (file, line_start, col_start, line_end,
col_end). A test covers a region when its count is non-zero in any executed
function instance, including subprocesses the test spawned.

Test code is excluded: every test executes its own body, so counting test code
would make every coverage set unique. Test code is any file under a `tests/` or
`benches/` directory, files named `tests.rs`, `test.rs`, `*_tests.rs`, or
`*_test.rs`, files of `#[cfg(test)] mod x;` modules, and inline
`#[cfg(test)] mod x { ... }` blocks.

Candidate kinds:
  duplicate  same coverage set as another test (the keeper)
  subsumed   coverage set is a strict subset of another test (the witness)
  near       Jaccard similarity >= --near with another test, neither a subset

Outputs: a markdown report, optional candidates JSON, and optional
self-contained HTML report (`--html`) that embeds the model from
`report_template.html`. Verdicts from `verdicts.jsonl` are shown when present.

Candidates are suspects, not verdicts: coverage shows what a test executes,
not what it asserts. Stdlib only.
"""

from __future__ import annotations

import argparse
import datetime
import gzip
import json
import os
import re
import sys
from array import array
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent.parent


@dataclass
class Location:
    path: str
    line: int
    ambiguous: bool

    def __str__(self) -> str:
        return f"{self.path}:{self.line}" + (" (ambiguous)" if self.ambiguous else "")


@dataclass
class Test:
    id: str
    binary_id: str
    name: str
    manifest_dir: str | None
    processes: int
    weight: int = 0
    classes: list[int] = field(default_factory=list)

    @property
    def tier(self) -> str:
        # Nextest binary IDs: `crate` (lib unit tests), `crate::bin/name`
        # (bin unit tests), `crate::target` (integration test targets).
        if "::" not in self.binary_id or "::bin/" in self.binary_id:
            return "unit"
        return "integration"


@dataclass
class Candidate:
    kind: str  # "duplicate" | "subsumed" | "near"
    test: int  # the test to consider cutting
    other: int  # keeper, tightest witness, or larger near twin
    shared: int  # regions both cover
    gap: int  # regions only `other` covers
    extra: int  # regions only `test` covers
    supersets: int  # subsumed: strict supersets; duplicate: group size - 1; near: 0

    @property
    def jaccard(self) -> float:
        return self.shared / (self.shared + self.gap + self.extra)


def input_files(paths: list[Path]) -> list[Path]:
    files: list[Path] = []
    for path in paths:
        if path.is_dir():
            files += sorted(p for p in path.iterdir() if p.name.endswith((".json", ".json.gz")) and p.name != "skipped.json")
        else:
            files.append(path)
    return files


def load(path: Path) -> dict:
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt") as fh:
        return json.load(fh)


def bits(mask: int):
    while mask:
        low = mask & -mask
        yield low.bit_length() - 1
        mask ^= low


CFG_TEST_MOD_RE = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*([;{])")
FN_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)", re.M)
SKIP_DIRS = {"target", "node_modules", ".git", ".claude"}


def block_end(text: str, start: int) -> int:
    """Index of the `}` matching the `{` at `start`. Skips comments, strings, and brace char literals."""
    depth, i, n = 0, start, len(text)
    while i < n:
        ch = text[i]
        if text.startswith("//", i):
            i = text.find("\n", i)
            if i < 0:
                return n
            continue
        if text.startswith("/*", i):
            j = text.find("*/", i + 2)
            i = n if j < 0 else j + 2
            continue
        if ch == '"':
            i += 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        elif ch == "'" and i + 2 < n and text[i + 2] == "'":
            i += 2
        elif ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return n


def rust_files(base: Path, skip_nested_packages: bool = False):
    for dirpath, dirnames, filenames in os.walk(base):
        here = Path(dirpath)
        dirnames[:] = [
            d for d in dirnames if d not in SKIP_DIRS and not (skip_nested_packages and (here / d / "Cargo.toml").exists())
        ]
        for fname in filenames:
            if fname.endswith(".rs"):
                yield here / fname


class TestCode:
    """Which source lines are test code. Built by scanning the repo's `.rs` files once."""

    def __init__(self, repo: Path):
        self.repo = repo
        self.whole: set[str] = set()
        self.ranges: dict[str, list[tuple[int, int]]] = {}
        for path in rust_files(repo):
            self._scan(path)

    def _scan(self, path: Path) -> None:
        rel = str(path.relative_to(self.repo))
        parts = path.relative_to(self.repo).parts
        stem = path.stem
        if "tests" in parts[:-1] or "benches" in parts[:-1] or stem in ("tests", "test") or stem.endswith(("_tests", "_test")):
            self.whole.add(rel)
            return
        try:
            text = path.read_text()
        except (OSError, UnicodeDecodeError):
            return
        # `mod x;` in lib.rs/main.rs/mod.rs resolves next to the file; elsewhere under a dir named after the file.
        mod_dir = path.parent if stem in ("lib", "main", "mod") else path.parent / stem
        for m in CFG_TEST_MOD_RE.finditer(text):
            name, kind = m.group(1), m.group(2)
            if kind == ";":
                for target in (mod_dir / f"{name}.rs", mod_dir / name / "mod.rs"):
                    self.whole.add(str(target.relative_to(self.repo)))
            else:
                first = text.count("\n", 0, m.start()) + 1
                last = text.count("\n", 0, block_end(text, m.end() - 1)) + 1
                self.ranges.setdefault(rel, []).append((first, last))

    def lookup(self, filename: str) -> tuple[bool, list[tuple[int, int]]]:
        """(whole file is test code, inline test line ranges)."""
        rel = filename.removeprefix(str(self.repo) + "/")
        return rel in self.whole, self.ranges.get(rel, [])


class Coverage:
    """Coverage sets compressed into region classes.

    `regions` holds every non-test code region inside a function some test
    executed, covered or not (the page needs uncovered ones to draw nesting).
    Covered regions executed by exactly the same tests form one class. Subset
    checks run on classes, which are far fewer than regions. Each class keeps a
    bitset of the tests that cover it.
    """

    def __init__(self, files: list[Path], test_code: TestCode):
        self.tests: list[Test] = []
        self.file_names: list[str] = []
        self.regions: list[tuple[int, int, int, int, int]] = []
        file_ids: dict[str, int] = {}
        region_ids: dict[tuple[int, int, int, int, int], int] = {}
        postings: dict[int, array] = {}
        excluded: set[tuple[str, int, int, int, int]] = set()
        # Test-code lookups are per file name; cache them across tests.
        file_info: dict[str, tuple[bool, list[tuple[int, int]]]] = {}

        for done, path in enumerate(files, 1):
            export = load(path)
            meta = export.get("test") or {}
            binary_id = meta.get("binary_id", "unknown")
            name = meta.get("name", path.name.split(".")[0])
            index = len(self.tests)
            seen: set[int] = set()
            for data in export["data"]:
                for function in data.get("functions", []):
                    names = function["filenames"]
                    for l1, c1, l2, c2, count, file_id, _expanded, kind, *_ in function["regions"]:
                        if kind != 0:
                            continue
                        filename = names[file_id]
                        info = file_info.get(filename)
                        if info is None:
                            info = file_info[filename] = test_code.lookup(filename)
                        whole, ranges = info
                        if whole or any(lo <= l1 <= hi for lo, hi in ranges):
                            if count:
                                excluded.add((filename, l1, c1, l2, c2))
                            continue
                        fid = file_ids.get(filename)
                        if fid is None:
                            fid = file_ids[filename] = len(self.file_names)
                            self.file_names.append(filename)
                        key = (fid, l1, c1, l2, c2)
                        rid = region_ids.get(key)
                        if rid is None:
                            rid = region_ids[key] = len(self.regions)
                            self.regions.append(key)
                        if count and rid not in seen:
                            seen.add(rid)
                            postings.setdefault(rid, array("I")).append(index)
            self.tests.append(
                Test(
                    id=f"{binary_id} {name}",
                    binary_id=binary_id,
                    name=name,
                    manifest_dir=meta.get("manifest_dir"),
                    processes=meta.get("processes", 1),
                    weight=len(seen),
                )
            )
            if done % 500 == 0:
                print(f"loaded {done}/{len(files)}", file=sys.stderr)

        self.covered_count = len(postings)
        self.covered_file_count = len({self.regions[rid][0] for rid in postings})
        self.excluded_count = len(excluded)
        nbytes = (len(self.tests) + 7) // 8
        class_ids: dict[bytes, int] = {}
        self.class_regions: list[list[int]] = []
        self.class_files: list[Counter] = []
        self.class_bits: list[int] = []
        self.class_pop: list[int] = []
        for rid in sorted(postings):
            posting = postings[rid]
            key = posting.tobytes()
            cid = class_ids.get(key)
            if cid is None:
                cid = class_ids[key] = len(self.class_regions)
                self.class_regions.append([])
                self.class_files.append(Counter())
                bitmap = bytearray(nbytes)
                for t in posting:
                    bitmap[t >> 3] |= 1 << (t & 7)
                self.class_bits.append(int.from_bytes(bitmap, "little"))
                self.class_pop.append(len(posting))
                for t in posting:
                    self.tests[t].classes.append(cid)
            self.class_regions[cid].append(rid)
            self.class_files[cid][self.regions[rid][0]] += 1

    def weight(self, classes) -> int:
        return sum(len(self.class_regions[c]) for c in classes)

    def supersets(self, index: int) -> list[int]:
        """Tests whose coverage set contains this test's set (excluding itself)."""
        own = 1 << index
        mask = (1 << len(self.tests)) - 1
        for cid in sorted(self.tests[index].classes, key=self.class_pop.__getitem__):
            mask &= self.class_bits[cid]
            if mask == own:
                return []
        return list(bits(mask ^ own))

    def top_files(self, classes, limit: int = 3) -> list[tuple[str, int]]:
        counts: Counter = Counter()
        for cid in classes:
            counts.update(self.class_files[cid])
        return [(self.file_names[fid], c) for fid, c in counts.most_common(limit)]


def find_candidates(cov: Coverage, near_threshold: float) -> list[Candidate]:
    tests = cov.tests
    candidates: list[Candidate] = []

    groups: dict[tuple[int, ...], list[int]] = {}
    for i, t in enumerate(tests):
        if t.weight:
            groups.setdefault(tuple(sorted(t.classes)), []).append(i)
    dropped: set[int] = set()
    for members in groups.values():
        if len(members) < 2:
            continue
        # Keep a unit test when one exists: it fails faster and closer to the cause.
        keeper = min(members, key=lambda i: (tests[i].tier != "unit", tests[i].id))
        for i in members:
            if i != keeper:
                dropped.add(i)
                candidates.append(Candidate("duplicate", i, keeper, tests[i].weight, 0, 0, len(members) - 1))

    # A dropped duplicate is listed once, as a duplicate. Witnesses are keepers
    # or unique tests, so the report never points at a test it also flags as a duplicate.
    for i, t in enumerate(tests):
        if not t.weight or i in dropped:
            continue
        strict = [j for j in cov.supersets(i) if tests[j].weight > t.weight and j not in dropped]
        if strict:
            witness = min(strict, key=lambda j: (tests[j].weight, tests[j].tier != t.tier, tests[j].id))
            candidates.append(Candidate("subsumed", i, witness, t.weight, tests[witness].weight - t.weight, 0, len(strict)))

    candidates.sort(key=lambda c: (c.gap / tests[c.other].weight, c.kind != "duplicate", tests[c.test].id))
    candidates += sorted(near_pairs(cov, near_threshold, dropped), key=lambda c: (-c.jaccard, tests[c.test].id))
    return candidates


def near_pairs(cov: Coverage, threshold: float, dropped: set[int]) -> list[Candidate]:
    """Pairs with Jaccard >= threshold where neither set contains the other.

    Prefix filter: if J(A, B) >= t then B shares at least t * |A| regions with
    A, so B must cover one of A's rarest classes whose weights sum past
    (1 - t) * |A|. Only tests in that union are compared exactly.
    """
    tests = cov.tests
    sets = [set(t.classes) for t in tests]
    weights = [len(r) for r in cov.class_regions]
    out: list[Candidate] = []
    for a, t in enumerate(tests):
        if not t.weight or a in dropped:
            continue
        budget = (1 - threshold) * t.weight
        acc, mask = 0, 0
        for cid in sorted(t.classes, key=cov.class_pop.__getitem__):
            mask |= cov.class_bits[cid]
            acc += weights[cid]
            if acc > budget:
                break
        mask >>= a + 1  # each pair once: only b > a
        for offset in bits(mask):
            b = a + 1 + offset
            wb = tests[b].weight
            if b in dropped or not threshold * t.weight <= wb <= t.weight / threshold:
                continue
            small, large = (sets[a], sets[b]) if len(sets[a]) <= len(sets[b]) else (sets[b], sets[a])
            shared = sum(weights[c] for c in small if c in large)
            if shared == min(t.weight, wb):
                continue  # subset or duplicate: reported above
            if shared / (t.weight + wb - shared) < threshold:
                continue
            cand, other = (a, b) if (t.weight, t.id) <= (wb, tests[b].id) else (b, a)
            out.append(Candidate("near", cand, other, shared, tests[other].weight - shared, tests[cand].weight - shared, 0))
    return out


class Locator:
    """Best-effort `fn <name>` lookup for a test inside its package sources."""

    def __init__(self, repo: Path):
        self.repo = repo
        self.index: dict[str, dict[str, list[tuple[str, int]]]] = {}
        self.cache: dict[str, Location | None] = {}

    def _scan(self, manifest_dir: str) -> dict[str, list[tuple[str, int]]]:
        found: dict[str, list[tuple[str, int]]] = {}
        # Nested packages own their own tests.
        for path in rust_files(self.repo / manifest_dir, skip_nested_packages=True):
            try:
                text = path.read_text()
            except (OSError, UnicodeDecodeError):
                continue
            rel = str(path.relative_to(self.repo))
            for m in FN_RE.finditer(text):
                found.setdefault(m.group(1), []).append((rel, text.count("\n", 0, m.start()) + 1))
        return found

    def locate(self, test: Test) -> Location | None:
        if test.id in self.cache:
            return self.cache[test.id]
        location = None
        if test.manifest_dir is not None:
            if test.manifest_dir not in self.index:
                self.index[test.manifest_dir] = self._scan(test.manifest_dir)
            *modules, fn = test.name.split("::")
            options = self.index[test.manifest_dir].get(fn, [])
            if options:

                def score(option: tuple[str, int]) -> int:
                    parts = set(Path(option[0]).with_suffix("").parts)
                    return sum(m in parts for m in modules)

                best = max(score(o) for o in options)
                top = [o for o in options if score(o) == best]
                location = Location(top[0][0], top[0][1], len(top) > 1)
        self.cache[test.id] = location
        return location

    def describe(self, test: Test) -> str:
        location = self.locate(test)
        return str(location) if location else "location unknown (macro-generated?)"

    def body(self, test: Test) -> dict | None:
        """The test function's source, with the attributes and doc comments above it."""
        location = self.locate(test)
        if location is None:
            return None
        try:
            text = (self.repo / location.path).read_text()
        except (OSError, UnicodeDecodeError):
            return None
        lines = text.split("\n")
        start = location.line
        while start > 1 and lines[start - 2].lstrip().startswith(("#[", "///", "//")):
            start -= 1
        fn_offset = sum(len(line) + 1 for line in lines[: location.line - 1])
        brace = text.find("{", fn_offset)
        if brace < 0:
            return None
        end = text.count("\n", 0, block_end(text, brace)) + 1
        return {"path": location.path, "start": start, "end": end, "source": "\n".join(lines[start - 1 : end])}


def short(test: Test) -> str:
    return "::".join(test.name.split("::")[-2:])


def mermaid(cov: Coverage, candidates: list[Candidate], limit: int) -> str:
    tests = cov.tests
    nodes: dict[int, str] = {}

    def node(i: int) -> str:
        if i not in nodes:
            nodes[i] = f"t{len(nodes)}"
        return nodes[i]

    edges = []
    for c in candidates[:limit]:
        a, b = node(c.test), node(c.other)
        if c.kind == "duplicate":
            edges.append(f"  {a} ===|same| {b}")
        elif c.kind == "subsumed":
            edges.append(f"  {a} -->|+{c.gap}| {b}")
        else:
            edges.append(f"  {a} -.-|{c.jaccard:.2f}| {b}")
    lines = ["graph LR"]
    for i, nid in nodes.items():
        t = tests[i]
        label = f"{t.binary_id}<br/>{short(t)}<br/>{t.tier}, {t.weight} regions".replace('"', "'")
        lines.append(f'  {nid}["{label}"]')
    return "\n".join(lines + edges)


def fmt_files(files: list[tuple[str, int]]) -> str:
    return ", ".join(f"`{name}` {count}" for name, count in files) or "none"


def render(cov: Coverage, candidates: list[Candidate], locator: Locator, skipped: list[str], near: float, limit: int, graph_limit: int) -> str:
    tests = cov.tests
    covered = [t for t in tests if t.weight]
    empty = [t for t in tests if not t.weight]
    tiers = Counter(t.tier for t in covered)
    kinds = Counter(c.kind for c in candidates)
    pairs = Counter(f"{tests[c.test].tier} ⊂ {tests[c.other].tier}" for c in candidates if c.kind == "subsumed")

    out = [
        "# Dead-weight test candidates",
        "",
        f"- Tests with coverage: {len(covered)} ({tiers['unit']} unit, {tiers['integration']} integration)",
        f"- Covered regions: {cov.covered_count} in {cov.covered_file_count} files ({cov.excluded_count} test-code regions excluded)",
        f"- Skipped (failed or killed, partial coverage): {len(skipped)}",
        f"- No covered non-test regions: {len(empty)}",
        f"- Duplicate candidates: {kinds['duplicate']}",
        f"- Subsumed candidates: {kinds['subsumed']}" + (f" ({', '.join(f'{k}: {v}' for k, v in sorted(pairs.items()))})" if pairs else ""),
        f"- Near duplicates (Jaccard >= {near}): {kinds['near']}",
        "",
        "Candidates are suspects. Coverage shows what a test executes, not what it asserts.",
        "Read the candidate and the other test before cutting.",
        "",
    ]
    if candidates:
        shown = min(graph_limit, len(candidates))
        out += [
            f"## Containment graph (top {shown})",
            "",
            "`A --> B`: A's regions are a strict subset of B's; the label is B's extra regions. `===`: same regions. `-.-`: near duplicates, labelled with Jaccard.",
            "",
            "```mermaid",
            mermaid(cov, candidates, graph_limit),
            "```",
            "",
        ]

    out += [f"## Candidates (top {min(limit, len(candidates))} of {len(candidates)}: duplicates and subsumed by smallest gap, then near by Jaccard)", ""]
    for n, c in enumerate(candidates[:limit], 1):
        t, o = tests[c.test], tests[c.other]
        own = set(t.classes)
        out += [
            f"### {n}. {c.kind}: `{t.id}`",
            "",
            f"- Tier: {t.tier}. Location: {locator.describe(t)}",
            f"- Covers {t.weight} regions. Top files: {fmt_files(cov.top_files(own))}",
        ]
        if c.kind == "duplicate":
            out += [
                f"- Keeper: `{o.id}` ({o.tier}, {locator.describe(o)})",
                f"- Reason: executes exactly the same {t.weight} regions as the keeper. Group size: {c.supersets + 1}.",
            ]
        elif c.kind == "subsumed":
            out += [
                f"- Witness: `{o.id}` ({o.tier}, {locator.describe(o)}) covers {o.weight} regions",
                f"- Reason: all {t.weight} regions of this test are also executed by the witness, which executes {c.gap} more. Gap files: {fmt_files(cov.top_files(set(o.classes) - own))}",
                f"- Strict supersets in this run: {c.supersets}",
            ]
        else:
            out += [
                f"- Near twin: `{o.id}` ({o.tier}, {locator.describe(o)}) covers {o.weight} regions",
                f"- Reason: Jaccard {c.jaccard:.3f}. They share {c.shared} regions; this test adds {c.extra} (in {fmt_files(cov.top_files(own - set(o.classes)))}), the twin adds {c.gap}.",
            ]
        out.append("")
    if empty:
        out += ["## Tests with no covered non-test regions", "", "Either an env gate skipped the test early, or it exercises only test code (helpers, fixtures). Check them by hand.", ""]
        out += [f"- `{t.id}`" for t in empty[:limit]] + [""]
    if skipped:
        out += ["## Skipped tests", ""] + [f"- {s}" for s in skipped[:limit]] + [""]
    return "\n".join(out)


def candidate_rows(cov: Coverage, candidates: list[Candidate], locator: Locator) -> list[dict]:
    tests = cov.tests
    return [
        {
            "kind": c.kind,
            "test": tests[c.test].id,
            "tier": tests[c.test].tier,
            "location": locator.describe(tests[c.test]),
            "regions": tests[c.test].weight,
            "other": tests[c.other].id,
            "other_tier": tests[c.other].tier,
            "other_location": locator.describe(tests[c.other]),
            "other_regions": tests[c.other].weight,
            "shared": c.shared,
            "gap": c.gap,
            "extra": c.extra,
            "jaccard": round(c.jaccard, 4),
            "supersets": c.supersets,
        }
        for c in candidates
    ]


def load_verdicts(path: Path) -> list[dict]:
    """verdicts.jsonl lines: {"test", "other", "verdict", "reason"}. Later lines win per (test, other)."""
    if not path.is_file():
        return []
    latest: dict[tuple[str, str], dict] = {}
    for n, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
            latest[(row["test"], row["other"])] = {k: row[k] for k in ("test", "other", "verdict", "reason")}
        except (json.JSONDecodeError, KeyError) as error:
            print(f"{path}:{n}: skipped malformed verdict ({error})", file=sys.stderr)
    return list(latest.values())


def build_model(cov: Coverage, candidates: list[Candidate], locator: Locator, repo: Path, near: float, verdicts_path: Path, skipped: list[str]) -> dict:
    tests = cov.tests
    sources = []
    for name in cov.file_names:
        path = Path(name) if Path(name).is_absolute() else repo / name
        try:
            sources.append({"path": name, "source": path.read_text()})
        except (OSError, UnicodeDecodeError):
            sources.append({"path": name, "source": None})
    try:
        verdicts_rel = str(verdicts_path.resolve().relative_to(repo))
    except ValueError:
        verdicts_rel = str(verdicts_path.resolve())
    try:
        skill_rel = str((SKILL_DIR / "SKILL.md").relative_to(repo))
    except ValueError:
        skill_rel = str(SKILL_DIR / "SKILL.md")
    model_tests = []
    for t in tests:
        location = locator.locate(t)
        model_tests.append(
            {
                "id": t.id,
                "binary_id": t.binary_id,
                "name": t.name,
                "tier": t.tier,
                "location": f"{location.path}:{location.line}" if location else None,
                "ambiguous": bool(location and location.ambiguous),
                "regions": t.weight,
                "classes": t.classes,
                "processes": t.processes,
                "body": locator.body(t),
            }
        )
    return {
        "version": 1,
        "meta": {
            "generated": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "repo": str(repo),
            "near_threshold": near,
            "verdicts_path": verdicts_rel,
            "skill_path": skill_rel,
            "excluded_test_regions": cov.excluded_count,
            "skipped": skipped,
        },
        "files": sources,
        "regions": [list(r) for r in cov.regions],
        "classes": cov.class_regions,
        "tests": model_tests,
        "candidates": [
            {
                "kind": c.kind,
                "test": c.test,
                "other": c.other,
                "shared": c.shared,
                "gap": c.gap,
                "extra": c.extra,
                "jaccard": round(c.jaccard, 4),
                "supersets": c.supersets,
            }
            for c in candidates
        ],
        "empty": [i for i, t in enumerate(tests) if not t.weight],
        "verdicts": load_verdicts(verdicts_path),
    }


def write_html(model: dict, path: Path) -> None:
    template = (Path(__file__).resolve().parent / "report_template.html").read_text()
    # `</` inside the JSON would close the script tag early.
    payload = json.dumps(model, separators=(",", ":")).replace("</", "<\\/")
    path.write_text(template.replace("__MODEL_JSON__", payload))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("inputs", nargs="+", type=Path, help="per-test llvm-cov export JSON files or directories")
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="workspace root for source lookup (default: cwd)")
    parser.add_argument("--out", type=Path, help="write the markdown report here instead of stdout")
    parser.add_argument("--json", type=Path, help="also write every candidate as JSON")
    parser.add_argument("--html", type=Path, help="also write the interactive HTML report")
    parser.add_argument("--verdicts", type=Path, help="verdicts.jsonl to show (default: verdicts.jsonl next to the first input directory)")
    parser.add_argument("--near", type=float, default=0.95, help="Jaccard threshold for near duplicates (default 0.95)")
    parser.add_argument("--limit", type=int, default=100, help="candidates listed in markdown (default 100)")
    parser.add_argument("--graph-limit", type=int, default=40, help="candidates drawn in the markdown graph (default 40)")
    args = parser.parse_args()

    files = input_files(args.inputs)
    if not files:
        sys.exit("no coverage files found")
    skipped: list[str] = []
    for path in args.inputs:
        if (path / "skipped.json").is_file():
            skipped += json.loads((path / "skipped.json").read_text())

    repo = args.repo.resolve()
    cov = Coverage(files, TestCode(repo))
    candidates = find_candidates(cov, args.near)
    locator = Locator(repo)
    report = render(cov, candidates, locator, skipped, args.near, args.limit, args.graph_limit)
    if args.out:
        args.out.write_text(report + "\n")
        print(f"wrote {args.out}", file=sys.stderr)
    else:
        print(report)
    if args.json:
        args.json.write_text(json.dumps(candidate_rows(cov, candidates, locator), indent=2) + "\n")
        print(f"wrote {args.json}", file=sys.stderr)
    if args.html:
        first_dir = next((p for p in args.inputs if p.is_dir()), args.inputs[0].parent)
        verdicts = args.verdicts or first_dir.resolve().parent / "verdicts.jsonl"
        write_html(build_model(cov, candidates, locator, repo, args.near, verdicts, skipped), args.html)
        print(f"wrote {args.html}", file=sys.stderr)


if __name__ == "__main__":
    main()
