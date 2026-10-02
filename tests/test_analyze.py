"""Unit tests for analyze.py on synthetic llvm-cov exports. Run: python3 -m unittest discover tests"""

import contextlib
import gzip
import io
import importlib.util
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "skills" / "dead-weight-tests" / "scripts" / "analyze.py"
spec = importlib.util.spec_from_file_location("analyze", SCRIPT)
analyze = importlib.util.module_from_spec(spec)
sys.modules["analyze"] = analyze  # dataclasses resolve string annotations through sys.modules
spec.loader.exec_module(analyze)

LIB = "\n".join(
    ["pub fn f() {}"] * 19
    + ["#[cfg(test)]", "mod tests {", "    #[test]", "    fn a() {}", "}"]  # lines 20-24
)


class Workspace:
    """A temporary repo with source files and a directory of per-test exports."""

    def __init__(self, case: unittest.TestCase):
        self.dir = tempfile.TemporaryDirectory()
        case.addCleanup(self.dir.cleanup)
        self.root = Path(self.dir.name)
        (self.root / "src").mkdir()
        (self.root / "src" / "lib.rs").write_text(LIB)
        (self.root / "tests").mkdir()
        (self.root / "tests" / "it.rs").write_text("#[test]\nfn it() {}\n")
        self.exports = self.root / "exports"
        self.exports.mkdir()

    def test(self, name, regions, binary_id="crate"):
        """regions: (file, line) pairs, each a one-line covered region."""
        files = sorted({f for f, _ in regions})
        function = {
            "name": name,
            "count": 1,
            "filenames": files,
            "regions": [[line, 1, line, 10, 1, files.index(f), 0, 0] for f, line in regions],
        }
        export = {"data": [{"functions": [function]}], "test": {"binary_id": binary_id, "name": name, "manifest_dir": "."}}
        with gzip.open(self.exports / f"{binary_id}-{name}.json.gz".replace(":", "_"), "wt") as fh:
            json.dump(export, fh)

    def candidates(self, near=0.95):
        cov = analyze.Coverage(analyze.input_files([self.exports]), analyze.TestCode(self.root))
        found = analyze.find_candidates(cov, near)
        return cov, {(c.kind, cov.tests[c.test].name, cov.tests[c.other].name) for c in found}

    def ordered(self, near=0.95):
        """Coverage, locator, and candidates in report order (same-file pairs first)."""
        cov = analyze.Coverage(analyze.input_files([self.exports]), analyze.TestCode(self.root))
        locator = analyze.Locator(self.root)
        return cov, locator, analyze.same_file_first(cov, analyze.find_candidates(cov, near), locator)

    def two_tests_file(self):
        """src/two.rs defines test functions s1..s4; `it` lives in tests/it.rs; `ghost` is defined nowhere."""
        (self.root / "src" / "two.rs").write_text("".join(f"#[test]\nfn s{n}() {{}}\n" for n in range(1, 5)))


def prod(*lines):
    return [("src/lib.rs", line) for line in lines]


class FindCandidates(unittest.TestCase):
    def test_duplicates_keep_the_unit_test_and_subsets_name_the_smallest_witness(self):
        ws = Workspace(self)
        ws.test("it_dup", prod(1, 2), binary_id="crate::it")
        ws.test("unit_dup", prod(1, 2))
        ws.test("mid", prod(1, 2, 3))
        ws.test("big", prod(1, 2, 3, 4, 5))
        _, found = ws.candidates()
        self.assertIn(("duplicate", "it_dup", "unit_dup"), found)
        self.assertIn(("subsumed", "unit_dup", "mid"), found)
        self.assertIn(("subsumed", "mid", "big"), found)
        # A dropped duplicate is reported once, as a duplicate.
        self.assertNotIn("it_dup", {name for kind, name, _ in found if kind == "subsumed"})

    def test_near_duplicates_need_the_threshold_and_exclude_subsets(self):
        ws = Workspace(self)
        shared = list(range(100, 140))  # clear of the cfg(test) block at lines 20-24
        ws.test("left", prod(*shared, 201))
        ws.test("right", prod(*shared, 202))  # Jaccard 40/42 = 0.952
        ws.test("far", prod(*shared[:30], 203))  # Jaccard with left 30/42
        ws.test("sub", prod(*shared))  # a subset of left with Jaccard 40/41: subsumed, never near
        _, found = ws.candidates(near=0.95)
        near = {(a, b) for kind, a, b in found if kind == "near"}
        self.assertEqual(near, {("left", "right")})
        self.assertIn("sub", {name for kind, name, _ in found if kind == "subsumed"})
        _, stricter = ws.candidates(near=0.96)
        self.assertFalse(any(kind == "near" for kind, _, _ in stricter))

    def test_test_code_regions_do_not_make_tests_unique(self):
        ws = Workspace(self)
        # Each test also runs its own body: a cfg(test) line and a tests/ file line.
        ws.test("a", prod(1, 2) + [("src/lib.rs", 23)])
        ws.test("b", prod(1, 2) + [("tests/it.rs", 2)])
        cov, found = ws.candidates()
        self.assertIn(("duplicate", "b", "a"), found)
        self.assertEqual(cov.excluded_count, 2)


class SameFile(unittest.TestCase):
    def test_candidate_rows_flag_pairs_located_in_one_file(self):
        ws = Workspace(self)
        ws.two_tests_file()
        ws.test("s1", prod(1, 2))
        ws.test("s2", prod(1, 2))  # same file as its keeper s1
        ws.test("it", prod(3, 4), binary_id="crate::it")
        ws.test("s3", prod(3, 4))  # keeper of `it`, which lives in tests/it.rs
        ws.test("ghost", prod(5, 6))
        ws.test("s4", prod(5, 6))  # its keeper `ghost` has no known location
        cov, locator, found = ws.ordered()
        rows = {(r["test"], r["other"]): r["same_file"] for r in analyze.candidate_rows(cov, found, locator)}
        self.assertEqual(rows, {("crate s2", "crate s1"): True, ("crate::it it", "crate s3"): False, ("crate s4", "crate ghost"): False})

    def test_markdown_lists_same_file_pairs_before_smaller_cross_file_gaps(self):
        ws = Workspace(self)
        ws.two_tests_file()
        ws.test("it", prod(1, 2), binary_id="crate::it")
        ws.test("s3", prod(1, 2))  # cross-file duplicate: gap 0
        ws.test("s1", prod(10))
        ws.test("s2", prod(10, 11, 12))  # same-file subsumed: gap 2
        cov, locator, found = ws.ordered()
        report = analyze.render(cov, found, locator, [], 0.95, 100, 40)
        same = report.index("subsumed (same file): `crate s1`")
        cross = report.index("duplicate: `crate::it it`")
        self.assertLess(same, cross)
        self.assertIn("Same-file pairs (candidate and other in one source file, listed first): 1", report)


class LoadVerdicts(unittest.TestCase):
    def test_fix_is_kept_and_unknown_verdicts_are_skipped_with_a_warning(self):
        ws = Workspace(self)
        fix = {"test": "crate a", "other": "crate b", "verdict": "fix", "reason": "name promises a non-UTC offset"}
        unknown = {"test": "crate c", "other": "crate d", "verdict": "delete", "reason": "typo"}
        path = ws.root / "verdicts.jsonl"
        path.write_text(json.dumps(fix) + "\n" + json.dumps(unknown) + "\n")
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            loaded = analyze.load_verdicts(path)
        self.assertEqual(loaded, [fix])
        self.assertIn(f"{path}:2: skipped unknown verdict 'delete'", err.getvalue())


class Html(unittest.TestCase):
    def test_model_survives_embedding_next_to_script_tags(self):
        ws = Workspace(self)
        (ws.root / "src" / "lib.rs").write_text('pub fn f() -> &str { "</script><script>alert(1)" }\n')
        ws.test("a", prod(1))
        cov = analyze.Coverage(analyze.input_files([ws.exports]), analyze.TestCode(ws.root))
        model = analyze.build_model(cov, [], analyze.Locator(ws.root), ws.root, 0.95, ws.root / "verdicts.jsonl", [])
        out = ws.root / "report.html"
        analyze.write_html(model, out)
        payload = re.search(r'<script type="application/json" id="model">(.*?)</script>', out.read_text(), re.S).group(1)
        self.assertEqual(json.loads(payload)["files"][0]["source"], model["files"][0]["source"])

    def test_model_embeds_same_file_and_the_candidates_path(self):
        ws = Workspace(self)
        ws.two_tests_file()
        (ws.root / "src" / "two.rs").write_text('// "</script>"\n' + (ws.root / "src" / "two.rs").read_text())
        ws.test("s1", prod(1, 2))
        ws.test("s2", prod(1, 2))
        cov, locator, found = ws.ordered()
        repo = ws.root.resolve()  # macOS temp dirs sit behind a /private symlink
        model = analyze.build_model(cov, found, locator, repo, 0.95, repo / "verdicts.jsonl", [], repo / "target" / "candidates.json")
        out = ws.root / "report.html"
        analyze.write_html(model, out)
        payload = json.loads(re.search(r'<script type="application/json" id="model">(.*?)</script>', out.read_text(), re.S).group(1))
        self.assertEqual([c["same_file"] for c in payload["candidates"]], [True])
        self.assertEqual(payload["meta"]["candidates_path"], "target/candidates.json")


if __name__ == "__main__":
    unittest.main()
