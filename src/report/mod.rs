//! Reports rendered from an [`Analysis`] and the verdict ledger.
//!
//! Three adapters over one analysis: `report.md` (askama text template),
//! `candidates.json` (for agents), and `report.html` (askama page plus the
//! interactive views' script). None of them re-rank or re-filter candidates:
//! the analysis order is the review order.

mod html;
mod markdown;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::analysis::{Analysis, Candidate, Location, Relation, Test};
use crate::artifacts::ArtifactDir;
use crate::verdict::{LedgerContents, VerdictRecord};

/// Limits for the markdown report.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownOptions {
    /// Candidates listed (default 100).
    pub limit: usize,
    /// Candidates drawn in the Mermaid graph (default 40).
    pub graph_limit: usize,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            graph_limit: 40,
        }
    }
}

/// One `candidates.json` row: the candidate and its other test, by id, in rank order.
#[derive(Clone, Debug, Serialize)]
pub struct CandidateRow {
    pub rank: usize,
    pub kind: crate::analysis::CandidateKind,
    pub test: crate::test_id::TestId,
    pub tier: crate::test_id::Tier,
    pub location: Option<String>,
    pub regions: u32,
    pub other: crate::test_id::TestId,
    pub other_role: &'static str,
    pub other_tier: crate::test_id::Tier,
    pub other_location: Option<String>,
    pub other_regions: u32,
    pub shared: u32,
    pub gap: u32,
    pub extra: u32,
    pub jaccard: f64,
    /// Duplicates: every test with this coverage set, keeper included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_size: Option<u32>,
    /// Subsumed: every strict superset in the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_supersets: Option<u32>,
    pub same_file: bool,
}

pub fn candidate_rows(analysis: &Analysis) -> Vec<CandidateRow> {
    analysis
        .candidates
        .iter()
        .enumerate()
        .map(|(rank, c)| {
            let (t, o) = (analysis.test(c.test), analysis.test(c.other));
            let (group_size, strict_supersets) = relation_counts(c.relation);
            CandidateRow {
                rank,
                kind: c.kind(),
                test: t.id.clone(),
                tier: t.tier(),
                location: t.location.as_ref().map(location_string),
                regions: t.region_count,
                other: o.id.clone(),
                other_role: c.kind().other_role(),
                other_tier: o.tier(),
                other_location: o.location.as_ref().map(location_string),
                other_regions: o.region_count,
                shared: c.shared,
                gap: c.gap,
                extra: c.extra,
                jaccard: round4(c.jaccard()),
                group_size,
                strict_supersets,
                same_file: c.same_file,
            }
        })
        .collect()
}

pub fn markdown(
    analysis: &Analysis,
    verdicts: &LedgerContents,
    options: MarkdownOptions,
) -> Result<String> {
    markdown::render(analysis, verdicts, options)
}

pub fn html(analysis: &Analysis, verdicts: &LedgerContents, dirs: &ArtifactDir) -> Result<String> {
    html::render(analysis, verdicts, dirs)
}

/// Writes `report.md`, `candidates.json`, and `report.html` into `dirs`.
pub fn write_all(
    analysis: &Analysis,
    verdicts: &LedgerContents,
    dirs: &ArtifactDir,
    options: MarkdownOptions,
) -> Result<()> {
    let write = |path: std::path::PathBuf, text: String| {
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
    };
    write(dirs.report_md(), markdown(analysis, verdicts, options)?)?;
    write(
        dirs.candidates(),
        serde_json::to_string_pretty(&candidate_rows(analysis))? + "\n",
    )?;
    write(dirs.report_html(), html(analysis, verdicts, dirs)?)
}

/// `path:line`, plus ` (ambiguous)` when several functions matched.
fn location_string(location: &Location) -> String {
    let ambiguous = if location.ambiguous {
        " (ambiguous)"
    } else {
        ""
    };
    format!("{}:{}{ambiguous}", location.path, location.line)
}

/// (group size, strict supersets): the relation-specific count, for the matching kind only.
fn relation_counts(relation: Relation) -> (Option<u32>, Option<u32>) {
    match relation {
        Relation::Duplicate { group_size } => (Some(group_size), None),
        Relation::Subsumed { strict_supersets } => (None, Some(strict_supersets)),
        Relation::Near => (None, None),
    }
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// The ledger's verdict for a candidate pair.
fn verdict_of<'a>(
    analysis: &Analysis,
    verdicts: &'a LedgerContents,
    c: &Candidate,
) -> Option<&'a VerdictRecord> {
    let key = (
        analysis.test(c.test).id.clone(),
        analysis.test(c.other).id.clone(),
    );
    verdicts.verdicts.get(&key)
}

/// Tests with coverage, split by tier: (unit, integration).
fn covered_tiers(tests: &[Test]) -> (usize, usize) {
    let covered = tests.iter().filter(|t| t.region_count > 0);
    let unit = covered
        .clone()
        .filter(|t| t.tier() == crate::test_id::Tier::Unit)
        .count();
    (unit, covered.count() - unit)
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A hand-built analysis: two duplicate pairs and one subsumed pair.

    use std::path::PathBuf;

    use crate::analysis::*;
    use crate::ids::{ClassIdx, FileIdx, RegionIdx, TestIdx};
    use crate::test_id::TestId;

    fn test(binary: &str, name: &str, classes: &[u32], location: Option<(&str, u32)>) -> Test {
        Test {
            id: TestId::new(binary, name),
            manifest_dir: None,
            processes: 1,
            region_count: classes.len() as u32,
            classes: classes.iter().map(|&c| ClassIdx(c)).collect(),
            location: location.map(|(path, line)| Location {
                path: path.into(),
                line,
                ambiguous: false,
            }),
            body: None,
        }
    }

    pub fn analysis() -> Analysis {
        let region = |line| RegionKey {
            file: FileIdx(0),
            line_start: line,
            col_start: 1,
            line_end: line,
            col_end: 9,
        };
        Analysis {
            version: ANALYSIS_VERSION,
            generated: "2026-10-10T00:00:00Z".into(),
            repo: PathBuf::from("/repo"),
            near_threshold: DEFAULT_NEAR,
            files: vec![SourceFile {
                path: "src/lib.rs".into(),
                source: Some("fn a() {}\n// </script><script>alert(1)\n".into()),
            }],
            regions: vec![region(1), region(2)],
            classes: vec![vec![RegionIdx(0)], vec![RegionIdx(1)]],
            tests: vec![
                test("demo", "tests::big", &[0, 1], Some(("src/lib.rs", 10))),
                test("demo", "tests::small", &[0], Some(("src/lib.rs", 20))),
                test(
                    "demo::cli",
                    "copy_of_big",
                    &[0, 1],
                    Some(("tests/cli.rs", 3)),
                ),
                test("demo", "tests::gated", &[], None),
            ],
            candidates: vec![
                Candidate {
                    relation: Relation::Subsumed {
                        strict_supersets: 2,
                    },
                    test: TestIdx(1),
                    other: TestIdx(0),
                    shared: 1,
                    gap: 1,
                    extra: 0,
                    same_file: true,
                },
                Candidate {
                    relation: Relation::Duplicate { group_size: 2 },
                    test: TestIdx(2),
                    other: TestIdx(0),
                    shared: 2,
                    gap: 0,
                    extra: 0,
                    same_file: false,
                },
            ],
            skipped: vec![],
            stats: Stats {
                covered_regions: 2,
                covered_files: 1,
                excluded_test_regions: 0,
            },
        }
    }
}
