"""Tests for record_verdict.py, run as a subprocess. Run: python3 -m unittest discover tests"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "skills" / "dead-weight-tests" / "scripts" / "record_verdict.py"
CANDIDATES = [{"test": "a", "other": "b"}, {"test": "c", "other": "d"}]


def row(test="a", other="b", verdict="cut", reason="same assertions"):
    """One verdict line."""
    return json.dumps({"test": test, "other": other, "verdict": verdict, "reason": reason})


class RecordVerdictTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.candidates = self.dir / "candidates.json"
        self.candidates.write_text(json.dumps(CANDIDATES))
        self.verdicts = self.dir / "verdicts.jsonl"

    def run_script(self, stdin, *extra):
        """Run the recorder; return (exit code, parsed stdout)."""
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "--candidates", str(self.candidates), *extra],
            input=stdin, capture_output=True, text=True, check=False,
        )
        return proc.returncode, json.loads(proc.stdout)

    def lines(self):
        """Parsed lines of the verdicts file."""
        return [json.loads(l) for l in self.verdicts.read_text().splitlines()]

    def test_valid_rows_appended_with_four_keys(self):
        self.verdicts.write_text(row("c", "d", "keep", "old") + "\n")
        noisy = json.dumps({"test": "a", "other": "b", "verdict": "fix", "reason": "name lies", "extra": 1})
        code, out = self.run_script(noisy + "\n")
        self.assertEqual(code, 0)
        self.assertEqual(out, {"recorded": 1, "invalid": [], "missing": []})
        self.assertEqual(self.lines()[0]["reason"], "old")
        self.assertEqual(self.lines()[1], {"test": "a", "other": "b", "verdict": "fix", "reason": "name lies"})

    def test_invalid_rows_reported_and_valid_rows_kept(self):
        text = "\n".join([
            row(),
            row("a", "zzz"),
            row(verdict="delete"),
            row(reason="  "),
            "{not json",
        ])
        code, out = self.run_script(text)
        self.assertEqual(code, 1)
        self.assertEqual(out["recorded"], 1)
        self.assertEqual(
            [(e["line"], e["error"]) for e in out["invalid"]],
            [(2, "unknown pair"), (3, "unknown verdict"), (4, "empty reason"), (5, "malformed JSON")],
        )
        self.assertEqual(out["invalid"][0]["other"], "zzz")
        self.assertIsNone(out["invalid"][3]["test"])
        self.assertEqual(len(self.lines()), 1)

    def test_expect_uses_existing_file_and_new_input(self):
        self.verdicts.write_text(row("c", "d", "keep", "boundary") + "\n")
        expect = self.dir / "expect.json"
        expect.write_text(json.dumps(CANDIDATES))
        code, out = self.run_script("", "--expect", str(expect))
        self.assertEqual((code, out["missing"]), (1, [["a", "b"]]))
        code, out = self.run_script(row(verdict="merge") + "\n", "--expect", str(expect))
        self.assertEqual((code, out["missing"]), (0, []))

    def test_array_input_and_duplicate_pairs_appended_in_order(self):
        items = [json.loads(row(verdict="keep")), json.loads(row(verdict="cut"))]
        code, out = self.run_script(json.dumps(items))
        self.assertEqual((code, out["recorded"]), (0, 2))
        self.assertEqual([l["verdict"] for l in self.lines()], ["keep", "cut"])

    def test_explicit_verdicts_path_and_input_file(self):
        target = self.dir / "sub" / "v.jsonl"
        source = self.dir / "in.jsonl"
        source.write_text(row())
        code, _ = self.run_script("", "--verdicts", str(target), str(source))
        self.assertEqual(code, 0)
        self.assertTrue(target.exists())
        self.assertFalse(self.verdicts.exists())


if __name__ == "__main__":
    unittest.main()
