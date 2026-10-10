//! Tests that `dwd collect` could not export, with a typed reason.
//!
//! Their coverage is partial or missing, so the analysis leaves them out and
//! the report lists them.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::test_id::TestId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Skipped {
    pub test: TestId,
    #[serde(flatten)]
    pub reason: SkipReason,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SkipReason {
    /// The test process exited non-zero (128 + signal when a signal ended it).
    Failed { exit: i32 },
    /// The runner was killed before the test finished, e.g. a nextest timeout.
    Killed,
    /// The test ran but wrote no profile.
    NoProfile,
    /// Nextest selected the test but it never reached the runner, e.g. killed
    /// inside a wrapper script.
    NeverReached,
    /// llvm-profdata or llvm-cov failed for this test.
    ExportFailed { message: String },
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkipReason::Failed { exit } => write!(f, "exit {exit}"),
            SkipReason::Killed => f.write_str("killed"),
            SkipReason::NoProfile => f.write_str("no profile written"),
            SkipReason::NeverReached => f.write_str("never reached the runner"),
            SkipReason::ExportFailed { message } => write!(f, "export failed: {message}"),
        }
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.test, self.reason)
    }
}

/// Reads `skipped.json`; a missing file means nothing was skipped.
pub fn read_skipped(path: &Path) -> Result<Vec<Skipped>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Writes `skipped.json`, sorted for stable diffs.
pub fn write_skipped(path: &Path, skipped: &[Skipped]) -> Result<()> {
    let mut sorted = skipped.to_vec();
    sorted.sort();
    std::fs::write(path, serde_json::to_string_pretty(&sorted)? + "\n")
        .with_context(|| format!("write {}", path.display()))
}
