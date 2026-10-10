//! End to end: `dwd` collects, analyzes, and records a verdict for the dead
//! weight planted in the `dwd-fixture` workspace member (see fixture/src/lib.rs),
//! and leaves no instrumented artifacts behind.
//!
//! Needs cargo-nextest, cargo-llvm-cov, and llvm-tools-preview: run it inside
//! `nix develop` (or direnv) with `cargo test --test fixture_e2e -- --ignored`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use dead_weight_tests::analysis::Analysis;
use dead_weight_tests::export_format::TestExport;
use dead_weight_tests::skipped::{SkipReason, read_skipped};
use dead_weight_tests::test_id::TestId;
use serde::Deserialize;

/// The part of a `candidates.json` row this test reads.
#[derive(Deserialize)]
struct CandidateRow {
    kind: String,
    test: TestId,
    other: TestId,
}

/// One end-to-end run: its own target dir and artifact dir.
///
/// The separate target dir keeps the inner cargo off the outer `cargo test`'s
/// build lock, and keeps `cargo llvm-cov clean --workspace` away from this
/// crate's own binaries.
struct Run {
    tmp: tempfile::TempDir,
}

impl Run {
    fn repo() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn target(&self) -> PathBuf {
        self.tmp.path().join("target")
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("artifacts")
    }

    fn cargo(&self, args: &[&str]) {
        let status = Command::new(env!("CARGO"))
            .args(args)
            .current_dir(Self::repo())
            .env("CARGO_TARGET_DIR", self.target())
            .status()
            .expect("run cargo");
        assert!(status.success(), "cargo {args:?} failed: {status}");
    }

    fn dwd(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dwd"));
        command
            .arg("--dir")
            .arg(self.dir())
            .arg("-C")
            .arg(Self::repo())
            .args(args)
            .env("CARGO_TARGET_DIR", self.target());
        command
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir().join(file)).unwrap()
    }
}

/// Whether `binary` links the LLVM profiler runtime, i.e. was built with coverage.
fn instrumented(binary: &Path) -> bool {
    if let Ok(output) = Command::new("nm").arg(binary).output()
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).contains("__llvm_profile_runtime");
    }
    // Without LLVM_PROFILE_FILE an instrumented binary writes default_*.profraw into its cwd.
    let cwd = tempfile::tempdir().unwrap();
    let status = Command::new(binary)
        .arg("1")
        .current_dir(cwd.path())
        .env_remove("LLVM_PROFILE_FILE")
        .status()
        .expect("run describe");
    assert!(status.success());
    std::fs::read_dir(cwd.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "profraw")
    })
}

#[test]
#[ignore = "needs cargo-nextest, cargo-llvm-cov, llvm-tools-preview (nix develop)"]
fn finds_the_planted_dead_weight() {
    let run = Run {
        tmp: tempfile::tempdir().unwrap(),
    };

    // A normal build first, as a developer's target dir would have: collect
    // must replace these uninstrumented artifacts.
    run.cargo(&["build", "--quiet", "-p", "dwd-fixture", "--all-targets"]);

    let status = run
        .dwd(&["collect", "--", "-p", "dwd-fixture"])
        .env("DWD_FIXTURE_FAIL", "1")
        .status()
        .unwrap();
    assert!(status.success(), "dwd collect failed: {status}");
    // The near pair differs by one branch out of about ten, so lower the threshold.
    let status = run.dwd(&["analyze", "--near", "0.8"]).status().unwrap();
    assert!(status.success(), "dwd analyze failed: {status}");

    let candidates: Vec<CandidateRow> = serde_json::from_str(&run.read("candidates.json")).unwrap();
    let find = |kind: &str, test: &str, other: &str| {
        candidates
            .iter()
            .find(|c| c.kind == kind && c.test.name == test && c.other.name == other)
            .unwrap_or_else(|| panic!("missing candidate {kind} {test} -> {other}"))
    };
    let duplicate = find(
        "duplicate",
        "tests::parses_another_number",
        "tests::parses_a_number",
    );
    find(
        "subsumed",
        "tests::classifies_positive",
        "tests::describes_a_positive_number",
    );
    find(
        "near",
        "tests::tallies_letters_and_digits",
        "tests::tallies_letters_and_spaces",
    );

    let skipped = read_skipped(&run.dir().join("skipped.json")).unwrap();
    assert!(
        skipped
            .iter()
            .any(|s| s.test.name == "tests::fails_when_asked"
                && s.reason == SkipReason::Failed { exit: 101 }),
        "fails_when_asked is not skipped as failed: {skipped:?}"
    );

    let analysis = Analysis::load(&run.dir().join("analysis.json")).unwrap();
    assert!(
        analysis
            .empty_tests()
            .any(|t| t.id.name == "tests::only_runs_test_code"),
        "only_runs_test_code is not listed as empty"
    );

    // The subprocess test reaches the library only through the `describe` binary.
    let lib = std::fs::read_to_string(Run::repo().join("fixture/src/lib.rs")).unwrap();
    let negative_line = lib
        .lines()
        .position(|line| line.contains("\"negative\""))
        .unwrap() as u32
        + 1;
    let cli = std::fs::read_dir(run.dir().join("exports"))
        .unwrap()
        .map(|entry| TestExport::read(&entry.unwrap().path()).unwrap())
        .find(|export| export.test.id.name == "cli_describes_a_negative_number")
        .expect("no coverage for cli_describes_a_negative_number");
    assert!(
        cli.test.processes >= 2,
        "subprocess profile missing: processes={}",
        cli.test.processes
    );
    let covers_negative = cli.functions.iter().any(|function| {
        function.regions.iter().any(|region| {
            region.count > 0
                && region.line_start == negative_line
                && function.filenames[region.file as usize] == "fixture/src/lib.rs"
        })
    });
    assert!(
        covers_negative,
        "the subprocess's coverage of the negative branch is not attributed to the test"
    );

    assert!(
        run.read("report.html").contains("id=\"model\""),
        "report.html has no model"
    );

    // A verdict for the duplicate is recorded and shows in the re-rendered report.
    let mut verdict = run
        .dwd(&["verdict"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    writeln!(
        verdict.stdin.take().unwrap(),
        r#"{{"test": "{}", "other": "{}", "verdict": "cut", "reason": "same assertion shape on the same parse path"}}"#,
        duplicate.test,
        duplicate.other
    )
    .unwrap();
    assert!(
        verdict.wait().unwrap().success(),
        "dwd verdict rejected a valid row"
    );
    assert!(
        run.read("report.md")
            .contains("- Verdict: cut. same assertion shape")
    );

    // Cargo's fingerprint ignores the coverage wrapper, so collect must remove its
    // instrumented artifacts or the next normal build reuses them.
    run.cargo(&["build", "--quiet", "-p", "dwd-fixture", "--bin", "describe"]);
    let describe = run.target().join("debug/describe");
    assert!(
        !instrumented(&describe),
        "{} is coverage-instrumented after a normal build",
        describe.display()
    );
}
