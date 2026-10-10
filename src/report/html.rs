//! `report.html`: an askama page around the interactive views in `assets/report.js`.
//!
//! The page struct renders everything static (filters, summary cards, lists);
//! [`PageModel`] is the data the script reads, embedded as JSON.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use askama::Template;
use serde::Serialize;

use super::{covered_tiers, relation_counts, round4, verdict_of};
use crate::analysis::{Analysis, CandidateKind, RegionKey, SourceFile, TestBody};
use crate::artifacts::ArtifactDir;
use crate::ids::{ClassIdx, RegionIdx, TestIdx};
use crate::skipped::Skipped;
use crate::test_id::{TestId, Tier};
use crate::verdict::{self, LedgerContents, Verdict};

/// Longest list the summary prints before eliding the rest.
const LIST_CAP: usize = 300;

#[derive(Template)]
#[template(path = "report.html")]
struct Page<'a> {
    css: &'static str,
    script: &'static str,
    /// [`PageModel`] as JSON, safe inside a `<script>` element.
    model_json: String,
    repo: String,
    generated: &'a str,
    kinds: &'static [CandidateKind],
    verdicts: &'static [Verdict],
    reviewed: usize,
    cards: Vec<(String, String)>,
    empty: Vec<(TestIdx, &'a TestId)>,
    empty_more: usize,
    skipped: &'a [Skipped],
    skipped_more: usize,
}

/// Everything `report.js` reads. Verdicts arrive resolved; the script applies no verdict policy.
#[derive(Serialize)]
struct PageModel<'a> {
    meta: Meta<'a>,
    files: &'a [SourceFile],
    regions: &'a [RegionKey],
    classes: &'a [Vec<RegionIdx>],
    tests: Vec<PageTest<'a>>,
    /// Rank order: index = rank.
    candidates: Vec<PageCandidate<'a>>,
}

#[derive(Serialize)]
struct Meta<'a> {
    repo: String,
    near_threshold: f64,
    kinds: Vec<KindInfo>,
    /// Every verdict with its strength, for sorting.
    verdicts: Vec<VerdictInfo>,
    /// The shell command reviewers pipe verdict lines into.
    recorder: String,
    verdict_line: String,
    generated: &'a str,
}

#[derive(Serialize)]
struct KindInfo {
    kind: CandidateKind,
    other_role: &'static str,
}

#[derive(Serialize)]
struct VerdictInfo {
    verdict: Verdict,
    rank: u8,
}

#[derive(Serialize)]
struct PageTest<'a> {
    id: &'a TestId,
    binary_id: &'a str,
    name: &'a str,
    tier: Tier,
    location: Option<String>,
    ambiguous: bool,
    regions: u32,
    classes: &'a [ClassIdx],
    processes: u32,
    body: Option<&'a TestBody>,
    /// Strongest verdict over the pairs where this test is the candidate.
    verdict: Option<Verdict>,
}

#[derive(Serialize)]
struct PageCandidate<'a> {
    kind: CandidateKind,
    test: TestIdx,
    other: TestIdx,
    shared: u32,
    gap: u32,
    extra: u32,
    jaccard: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict_supersets: Option<u32>,
    same_file: bool,
    verdict: Option<PageVerdict<'a>>,
}

#[derive(Serialize)]
struct PageVerdict<'a> {
    verdict: Verdict,
    reason: &'a str,
}

pub(super) fn render(
    analysis: &Analysis,
    ledger: &LedgerContents,
    dirs: &ArtifactDir,
) -> Result<String> {
    let model = page_model(analysis, ledger, dirs.root());
    let reviewed = model
        .candidates
        .iter()
        .filter(|c| c.verdict.is_some())
        .count();
    let cards = cards(analysis, &model);
    let empty: Vec<(TestIdx, &TestId)> = analysis
        .tests
        .iter()
        .enumerate()
        .filter(|(_, t)| t.region_count == 0)
        .map(|(i, t)| (TestIdx::new(i), &t.id))
        .collect();
    let page = Page {
        css: include_str!("../../assets/report.css"),
        script: include_str!("../../assets/report.js"),
        model_json: serde_json::to_string(&model)?.replace("</", "<\\/"),
        repo: analysis.repo.display().to_string(),
        generated: &analysis.generated,
        kinds: &CandidateKind::ALL,
        verdicts: &Verdict::ALL,
        reviewed,
        cards,
        empty_more: empty.len().saturating_sub(LIST_CAP),
        empty: empty.into_iter().take(LIST_CAP).collect(),
        skipped: &analysis.skipped[..analysis.skipped.len().min(LIST_CAP)],
        skipped_more: analysis.skipped.len().saturating_sub(LIST_CAP),
    };
    Ok(page.render()?)
}

fn page_model<'a>(
    analysis: &'a Analysis,
    ledger: &'a LedgerContents,
    artifact_dir: &Path,
) -> PageModel<'a> {
    let candidates: Vec<PageCandidate> = analysis
        .candidates
        .iter()
        .map(|c| {
            let (group_size, strict_supersets) = relation_counts(c.relation);
            PageCandidate {
                kind: c.kind(),
                test: c.test,
                other: c.other,
                shared: c.shared,
                gap: c.gap,
                extra: c.extra,
                jaccard: round4(c.jaccard()),
                group_size,
                strict_supersets,
                same_file: c.same_file,
                verdict: verdict_of(analysis, ledger, c).map(|r| PageVerdict {
                    verdict: r.verdict,
                    reason: &r.reason,
                }),
            }
        })
        .collect();

    let mut strongest: BTreeMap<TestIdx, Verdict> = BTreeMap::new();
    for c in &candidates {
        if let Some(v) = &c.verdict {
            let best = strongest.entry(c.test).or_insert(v.verdict);
            if v.verdict.rank() > best.rank() {
                *best = v.verdict;
            }
        }
    }

    let tests = analysis
        .tests
        .iter()
        .enumerate()
        .map(|(i, t)| PageTest {
            id: &t.id,
            binary_id: &t.id.binary_id,
            name: &t.id.name,
            tier: t.tier(),
            location: t
                .location
                .as_ref()
                .map(|l| format!("{}:{}", l.path, l.line)),
            ambiguous: t.location.as_ref().is_some_and(|l| l.ambiguous),
            regions: t.region_count,
            classes: &t.classes,
            processes: t.processes,
            body: t.body.as_ref(),
            verdict: strongest.get(&TestIdx::new(i)).copied(),
        })
        .collect();

    PageModel {
        meta: Meta {
            repo: analysis.repo.display().to_string(),
            near_threshold: analysis.near_threshold,
            kinds: CandidateKind::ALL
                .iter()
                .map(|&kind| KindInfo {
                    kind,
                    other_role: kind.other_role(),
                })
                .collect(),
            verdicts: Verdict::ALL
                .iter()
                .map(|&verdict| VerdictInfo {
                    verdict,
                    rank: verdict.rank(),
                })
                .collect(),
            recorder: verdict::recorder_command(&analysis.repo, artifact_dir),
            verdict_line: verdict::verdict_line_example(),
            generated: &analysis.generated,
        },
        files: &analysis.files,
        regions: &analysis.regions,
        classes: &analysis.classes,
        tests,
        candidates,
    }
}

/// The overview's stat cards: (value, label).
fn cards(analysis: &Analysis, model: &PageModel) -> Vec<(String, String)> {
    let (unit, integration) = covered_tiers(&analysis.tests);
    let candidates = &model.candidates;
    let mut cards: Vec<(usize, String)> = vec![
        (analysis.tests.len(), "Tests".into()),
        (unit, "Unit (with coverage)".into()),
        (integration, "Integration (with coverage)".into()),
        (analysis.files.len(), "Files".into()),
        (analysis.classes.len(), "Coverage classes".into()),
        (analysis.regions.len(), "Regions".into()),
        (analysis.stats.covered_regions, "Covered regions".into()),
        (
            analysis.stats.excluded_test_regions,
            "Excluded test regions".into(),
        ),
    ];
    for kind in CandidateKind::ALL {
        let label = match kind {
            CandidateKind::Near => format!("Near (J ≥ {})", analysis.near_threshold),
            _ => capitalize(kind.as_str()),
        };
        cards.push((candidates.iter().filter(|c| c.kind == kind).count(), label));
    }
    cards.push((
        candidates.iter().filter(|c| c.same_file).count(),
        "Same file".into(),
    ));
    for verdict in Verdict::ALL {
        let n = candidates
            .iter()
            .filter(|c| c.verdict.as_ref().is_some_and(|v| v.verdict == verdict))
            .count();
        cards.push((n, capitalize(verdict.as_str())));
    }
    cards.push((
        candidates.iter().filter(|c| c.verdict.is_none()).count(),
        "Unreviewed".into(),
    ));
    cards.push((analysis.skipped.len(), "Skipped".into()));
    cards.push((analysis.empty_tests().count(), "Empty".into()));
    cards
        .into_iter()
        .map(|(n, label)| (thousands(n), label))
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `12345` → `12,345`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{fixture, html};
    use crate::artifacts::ArtifactDir;
    use crate::verdict::LedgerContents;

    #[test]
    fn embedded_model_survives_a_closing_script_tag_in_source() {
        let analysis = fixture::analysis();
        let page = html(
            &analysis,
            &LedgerContents::default(),
            &ArtifactDir::at("/tmp/dwd-test"),
        )
        .unwrap();
        let open = r#"<script type="application/json" id="model">"#;
        let start = page.find(open).unwrap() + open.len();
        let json = &page[start..start + page[start..].find("</script>").unwrap()];
        let model: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            model["files"][0]["source"],
            analysis.files[0].source.as_deref().unwrap()
        );
        assert_eq!(model["candidates"][0]["kind"], "subsumed");
    }
}
