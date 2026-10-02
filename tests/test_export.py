"""Unit tests for export.py's reconciliation of nextest's test list with runner.sh profiles."""

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "skills" / "dead-weight-tests" / "scripts" / "export.py"
spec = importlib.util.spec_from_file_location("export", SCRIPT)
export = importlib.util.module_from_spec(spec)
spec.loader.exec_module(export)


def nextest_list(suites):
    """`cargo nextest list --message-format json` output; suites maps binary id to {test: matches}."""
    return json.dumps(
        {
            "rust-suites": {
                binary_id: {
                    "binary-id": binary_id,
                    "testcases": {
                        name: {"ignored": False, "filter-match": {"status": "matches"} if matches else {"status": "mismatch", "reason": "string"}}
                        for name, matches in tests.items()
                    },
                }
                for binary_id, tests in suites.items()
            }
        }
    )


class NeverReached(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.profiles = Path(tmp.name)

    def profile(self, binary_id, name):
        """A profile dir as runner.sh writes it."""
        d = self.profiles / f"{binary_id}-{name}".replace("::", "_")
        d.mkdir()
        (d / "meta").write_text(f"{binary_id}\n{name}\n/target/debug/deps/bin\n/ws\n")
        return d

    def reconcile(self, text):
        """never_reached for the list `text` against the profile dirs, as export.py's main does."""
        dirs = sorted(d for d in self.profiles.iterdir() if (d / "meta").is_file())
        with contextlib.redirect_stderr(io.StringIO()):
            return export.never_reached(export.listed_tests(text), dirs)

    def test_reports_only_listed_tests_without_a_profile(self):
        self.profile("crate", "tests::ran")
        self.profile("crate::it", "unlisted_but_ran")
        text = nextest_list(
            {
                "crate": {"tests::ran": True, "tests::killed_in_wrapper": True, "tests::filtered_out": False},
                "crate::it": {"never_started": True},
            }
        )
        self.assertEqual(
            self.reconcile(text),
            [
                "crate tests::killed_in_wrapper: never reached the runner",
                "crate::it never_started: never reached the runner",
            ],
        )

    def test_malformed_or_empty_list_reports_nothing(self):
        self.profile("crate", "tests::ran")
        for text in ["", "not json", "[]", "null", '{"rust-suites": {"crate": {"testcases": []}}}', '{"rust-suites": {"crate": {"testcases": {"t": 1}}}}']:
            with self.subTest(text=text):
                self.assertEqual(self.reconcile(text), [])


if __name__ == "__main__":
    unittest.main()
