//! Reports rendered from an [`Analysis`] and the verdict ledger.
//!
//! Three adapters over one analysis: `report.md` (askama text template),
//! `candidates.json` (for agents), and `report.html` (askama page plus the
//! interactive views' script). None of them re-rank or re-filter candidates:
//! the analysis order is the review order.

mod html;
mod markdown;

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::analysis::{Analysis, Candidate, CandidateKind, CoverageLoss, Location, Relation, Test};
use crate::artifacts::ArtifactDir;
use crate::ids::TestIdx;
use crate::verdict::{Judged, LedgerContents, Verdict, judge};

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

/// The ledger's verdict for a candidate pair, judged against the current bodies.
fn verdict_of<'a>(
    analysis: &Analysis,
    verdicts: &'a LedgerContents,
    c: &Candidate,
) -> Option<Judged<'a>> {
    verdicts
        .verdicts
        .get(&analysis.pair(c))
        .map(|record| judge(record, &analysis.body_hashes(c)))
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

/// Strongest fresh verdict per test over the pairs where it is the candidate (see [`Verdict::rank`]).
pub fn strongest_verdicts(
    analysis: &Analysis,
    ledger: &LedgerContents,
) -> BTreeMap<TestIdx, Verdict> {
    let mut strongest: BTreeMap<TestIdx, Verdict> = BTreeMap::new();
    for c in &analysis.candidates {
        if let Some(record) = verdict_of(analysis, ledger, c).and_then(Judged::fresh) {
            let best = strongest.entry(c.test).or_insert(record.verdict);
            if record.verdict.rank() > best.rank() {
                *best = record.verdict;
            }
        }
    }
    strongest
}

/// Tests a reviewer removes: any fresh pair verdict is cut or merge.
pub fn removed_tests(analysis: &Analysis, ledger: &LedgerContents) -> HashSet<TestIdx> {
    strongest_verdicts(analysis, ledger)
        .into_iter()
        .filter(|(_, v)| matches!(v, Verdict::Cut | Verdict::Merge))
        .map(|(t, _)| t)
        .collect()
}

/// Review progress over the candidates. Stale verdicts count as pending.
#[derive(Clone, Debug)]
pub struct Review {
    /// Candidates with a fresh verdict.
    pub reviewed: usize,
    /// Candidates whose verdict went stale (included in `pending`).
    pub stale: usize,
    /// Candidates without a fresh verdict.
    pub pending: usize,
    /// What the fresh cut and merge verdicts lose.
    pub loss: CoverageLoss,
}

impl Review {
    pub fn of(analysis: &Analysis, ledger: &LedgerContents) -> Self {
        let (mut reviewed, mut stale) = (0, 0);
        for c in &analysis.candidates {
            match verdict_of(analysis, ledger, c) {
                Some(Judged::Fresh(_)) => reviewed += 1,
                Some(Judged::Stale(_)) => stale += 1,
                None => {}
            }
        }
        Review {
            reviewed,
            stale,
            pending: analysis.candidates.len() - reviewed,
            loss: analysis.coverage_loss(&removed_tests(analysis, ledger)),
        }
    }

    /// `Cuts and merges remove N tests and lose X regions`, or `None` without cuts or merges.
    pub fn loss_line(&self) -> Option<String> {
        (self.loss.removed > 0).then(|| {
            let s = |n: usize| if n == 1 { "" } else { "s" };
            let (tests, regions) = (self.loss.removed, self.loss.lost_regions);
            format!(
                "Cuts and merges remove {tests} test{} and lose {regions} region{}",
                s(tests),
                s(regions)
            )
        })
    }

    /// The top `limit` files of the loss as `` `path` n ``, comma-separated.
    pub fn loss_files(&self, limit: usize) -> String {
        let parts: Vec<String> = self
            .loss
            .files
            .iter()
            .take(limit)
            .map(|f| format!("`{}` {}", f.path, f.regions))
            .collect();
        parts.join(", ")
    }
}

/// A `dwd pending` row: the candidate row plus the stale verdict it replaces.
#[derive(Clone, Debug, Serialize)]
pub struct PendingRow {
    #[serde(flatten)]
    pub row: CandidateRow,
    /// The verdict recorded before a reviewed body changed. Absent when never reviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
}

/// A stale verdict as shown to the next reviewer.
#[derive(Clone, Debug, Serialize)]
pub struct Previous {
    pub verdict: Verdict,
    pub reason: String,
}

/// Candidates without a fresh verdict, in rank order.
pub fn pending_rows(analysis: &Analysis, ledger: &LedgerContents) -> Vec<PendingRow> {
    candidate_rows(analysis)
        .into_iter()
        .zip(&analysis.candidates)
        .filter_map(|(row, c)| match verdict_of(analysis, ledger, c) {
            Some(Judged::Fresh(_)) => None,
            Some(Judged::Stale(r)) => Some(PendingRow {
                row,
                previous: Some(Previous {
                    verdict: r.verdict,
                    reason: r.reason.clone(),
                }),
            }),
            None => Some(PendingRow {
                row,
                previous: None,
            }),
        })
        .collect()
}

/// The plain-text status `dwd analyze` and `dwd report` print.
pub fn summary(analysis: &Analysis, ledger: &LedgerContents, report_html: &Path) -> String {
    let (unit, integration) = covered_tiers(&analysis.tests);
    let count = |kind| {
        analysis
            .candidates
            .iter()
            .filter(|c| c.kind() == kind)
            .count()
    };
    let review = Review::of(analysis, ledger);
    let mut out = format!(
        "tests: {} with coverage, {} skipped\n",
        unit + integration,
        analysis.skipped.len()
    );
    let kinds: Vec<String> = CandidateKind::ALL
        .iter()
        .map(|&k| format!("{} {}", count(k), k.as_str()))
        .collect();
    out += &format!(
        "candidates: {} ({}), {} same-file\n",
        analysis.candidates.len(),
        kinds.join(", "),
        analysis.candidates.iter().filter(|c| c.same_file).count()
    );
    let stale = if review.stale > 0 {
        format!(" ({} stale: a reviewed test body changed)", review.stale)
    } else {
        String::new()
    };
    out += &format!(
        "reviewed: {}, pending: {}{stale}\n",
        review.reviewed, review.pending
    );
    if let Some(line) = review.loss_line() {
        let files = review.loss_files(3);
        out += &line;
        if !files.is_empty() {
            out += &format!(" (top files: {files})");
        }
        out.push('\n');
    }
    out += &format!("report: {}\n", report_html.display());
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::TestBody;
    use crate::test_id::TestId;
    use crate::verdict::{VerdictRecord, body_hash};

    /// The fixture with `copy_of_big` (candidate 2's test) now at `body`, and a
    /// ledger holding one cut on that pair stamped with `stamped` (`None`: a legacy line).
    fn reviewed_with(body: &str, stamped: Option<&str>) -> (Analysis, LedgerContents) {
        let mut analysis = fixture::analysis();
        analysis.tests[2].body = Some(TestBody {
            path: "tests/cli.rs".into(),
            start: 3,
            end: 5,
            source: body.into(),
            hash: body_hash(body),
        });
        let record = VerdictRecord {
            test: TestId::new("demo::cli", "copy_of_big"),
            other: TestId::new("demo", "tests::big"),
            verdict: Verdict::Cut,
            reason: "same assertions".into(),
            test_hash: stamped.map(body_hash),
            other_hash: None,
        };
        let mut ledger = LedgerContents::default();
        ledger
            .verdicts
            .insert((record.test.clone(), record.other.clone()), record);
        (analysis, ledger)
    }

    #[test]
    fn changed_candidate_body_makes_the_verdict_pending_with_previous() {
        let (analysis, ledger) = reviewed_with(
            "fn copy_of_big() { assert!(x) }",
            Some("fn copy_of_big() {}"),
        );
        let rows = pending_rows(&analysis, &ledger);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].previous.is_none());
        let json = serde_json::to_value(&rows[1]).unwrap();
        assert_eq!(json["test"], "demo::cli copy_of_big");
        assert_eq!(
            json["previous"],
            serde_json::json!({"verdict": "cut", "reason": "same assertions"})
        );
        assert!(
            serde_json::to_value(&rows[0])
                .unwrap()
                .get("previous")
                .is_none()
        );
        let review = Review::of(&analysis, &ledger);
        assert_eq!((review.reviewed, review.stale, review.pending), (0, 1, 2));
        assert!(
            summary(&analysis, &ledger, Path::new("r.html"))
                .contains("reviewed: 0, pending: 2 (1 stale: a reviewed test body changed)")
        );
        let md = markdown(&analysis, &ledger, MarkdownOptions::default()).unwrap();
        assert!(md.contains("- Verdict: cut (stale: test body changed). same assertions"));
    }

    #[test]
    fn unchanged_body_stays_reviewed() {
        let body = "fn copy_of_big() {}";
        let (analysis, ledger) = reviewed_with(body, Some(body));
        assert_eq!(pending_rows(&analysis, &ledger).len(), 1);
        let review = Review::of(&analysis, &ledger);
        assert_eq!((review.reviewed, review.stale, review.pending), (1, 0, 1));
    }

    #[test]
    fn legacy_line_without_hashes_stays_reviewed() {
        let (analysis, ledger) = reviewed_with("fn copy_of_big() { changed() }", None);
        assert_eq!(pending_rows(&analysis, &ledger).len(), 1);
        assert_eq!(Review::of(&analysis, &ledger).reviewed, 1);
    }

    #[test]
    fn stale_cut_is_not_a_removal() {
        let body = "fn copy_of_big() {}";
        let (analysis, fresh) = reviewed_with(body, Some(body));
        assert_eq!(
            removed_tests(&analysis, &fresh),
            HashSet::from([TestIdx(2)])
        );
        let (analysis, stale) = reviewed_with("fn copy_of_big() { assert!(x) }", Some(body));
        assert!(removed_tests(&analysis, &stale).is_empty());
        assert_eq!(Review::of(&analysis, &stale).loss.removed, 0);
    }
}
