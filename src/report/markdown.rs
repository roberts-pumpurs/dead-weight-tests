//! `report.md`: summary, Mermaid containment graph, and candidates in rank order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::Result;
use askama::Template;

use super::{MarkdownOptions, covered_tiers, location_string, relation_counts, verdict_of};
use crate::analysis::{Analysis, CandidateKind, Test};
use crate::ids::{ClassIdx, TestIdx};
use crate::skipped::Skipped;
use crate::test_id::TestId;
use crate::verdict::VerdictRecord;

#[derive(Template)]
#[template(path = "report.md")]
struct Page<'a> {
    covered: usize,
    unit: usize,
    integration: usize,
    covered_regions: usize,
    covered_files: usize,
    excluded: usize,
    skipped: &'a [Skipped],
    shown_skipped: &'a [Skipped],
    empty_count: usize,
    shown_empty: Vec<&'a TestId>,
    duplicates: usize,
    subsumed: usize,
    /// e.g. ` (unit ⊂ integration: 2)`, empty when there are no subsumed candidates.
    subsumed_tiers: String,
    near: usize,
    near_threshold: f64,
    same_file: usize,
    graph_shown: usize,
    mermaid: String,
    total: usize,
    candidates: Vec<Entry<'a>>,
}

/// One listed candidate.
struct Entry<'a> {
    n: usize,
    kind: &'static str,
    same_file: bool,
    test: &'a TestId,
    tier: &'static str,
    location: String,
    regions: u32,
    top_files: String,
    other: &'a TestId,
    other_tier: &'static str,
    other_location: String,
    other_regions: u32,
    shared: u32,
    gap: u32,
    extra: u32,
    jaccard: String,
    group_size: Option<u32>,
    strict_supersets: Option<u32>,
    /// Files of the regions only the other test covers (subsumed) or only this test covers (near).
    diff_files: String,
    verdict: Option<&'a VerdictRecord>,
}

pub(super) fn render(
    analysis: &Analysis,
    verdicts: &crate::verdict::LedgerContents,
    options: MarkdownOptions,
) -> Result<String> {
    let candidates = &analysis.candidates;
    let (unit, integration) = covered_tiers(&analysis.tests);
    let empty: Vec<&TestId> = analysis.empty_tests().map(|t| &t.id).collect();
    let count = |kind| candidates.iter().filter(|c| c.kind() == kind).count();

    let mut tier_pairs: BTreeMap<String, usize> = BTreeMap::new();
    for c in candidates
        .iter()
        .filter(|c| c.kind() == CandidateKind::Subsumed)
    {
        let key = format!(
            "{} ⊂ {}",
            analysis.test(c.test).tier(),
            analysis.test(c.other).tier()
        );
        *tier_pairs.entry(key).or_default() += 1;
    }
    let subsumed_tiers = if tier_pairs.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = tier_pairs
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        format!(" ({})", parts.join(", "))
    };

    let entries = candidates
        .iter()
        .take(options.limit)
        .enumerate()
        .map(|(rank, c)| {
            let (t, o) = (analysis.test(c.test), analysis.test(c.other));
            let own: HashSet<ClassIdx> = t.classes.iter().copied().collect();
            let theirs: HashSet<ClassIdx> = o.classes.iter().copied().collect();
            let diff_files = match c.kind() {
                CandidateKind::Duplicate => String::new(),
                CandidateKind::Subsumed => fmt_files(analysis, theirs.difference(&own).copied()),
                CandidateKind::Near => fmt_files(analysis, own.difference(&theirs).copied()),
            };
            let (group_size, strict_supersets) = relation_counts(c.relation);
            Entry {
                n: rank + 1,
                kind: c.kind().as_str(),
                same_file: c.same_file,
                test: &t.id,
                tier: t.tier().as_str(),
                location: describe(t),
                regions: t.region_count,
                top_files: fmt_files(analysis, t.classes.iter().copied()),
                other: &o.id,
                other_tier: o.tier().as_str(),
                other_location: describe(o),
                other_regions: o.region_count,
                shared: c.shared,
                gap: c.gap,
                extra: c.extra,
                jaccard: format!("{:.3}", c.jaccard()),
                group_size,
                strict_supersets,
                diff_files,
                verdict: verdict_of(analysis, verdicts, c),
            }
        })
        .collect();

    let page = Page {
        covered: unit + integration,
        unit,
        integration,
        covered_regions: analysis.stats.covered_regions,
        covered_files: analysis.stats.covered_files,
        excluded: analysis.stats.excluded_test_regions,
        skipped: &analysis.skipped,
        shown_skipped: &analysis.skipped[..analysis.skipped.len().min(options.limit)],
        empty_count: empty.len(),
        shown_empty: empty.into_iter().take(options.limit).collect(),
        duplicates: count(CandidateKind::Duplicate),
        subsumed: count(CandidateKind::Subsumed),
        subsumed_tiers,
        near: count(CandidateKind::Near),
        near_threshold: analysis.near_threshold,
        same_file: candidates.iter().filter(|c| c.same_file).count(),
        graph_shown: options.graph_limit.min(candidates.len()),
        mermaid: mermaid(analysis, options.graph_limit),
        total: candidates.len(),
        candidates: entries,
    };
    Ok(page.render()? + "\n")
}

fn describe(test: &Test) -> String {
    test.location.as_ref().map_or_else(
        || "location unknown (macro-generated?)".into(),
        location_string,
    )
}

fn fmt_files(analysis: &Analysis, classes: impl IntoIterator<Item = ClassIdx>) -> String {
    let files = analysis.top_files(classes, 3);
    if files.is_empty() {
        return "none".into();
    }
    let parts: Vec<String> = files
        .iter()
        .map(|(name, count)| format!("`{name}` {count}"))
        .collect();
    parts.join(", ")
}

/// The Mermaid containment graph over the first `limit` candidates.
fn mermaid(analysis: &Analysis, limit: usize) -> String {
    let mut nodes: Vec<TestIdx> = Vec::new();
    let mut ids: HashMap<TestIdx, usize> = HashMap::new();
    let mut node = |t: TestIdx| {
        *ids.entry(t).or_insert_with(|| {
            nodes.push(t);
            nodes.len() - 1
        })
    };
    let mut edges = String::new();
    for c in analysis.candidates.iter().take(limit) {
        let (a, b) = (node(c.test), node(c.other));
        // Writing to a String cannot fail.
        let _ = match c.kind() {
            CandidateKind::Duplicate => writeln!(edges, "  t{a} ===|same| t{b}"),
            CandidateKind::Subsumed => writeln!(edges, "  t{a} -->|+{}| t{b}", c.gap),
            CandidateKind::Near => writeln!(edges, "  t{a} -.-|{:.2}| t{b}", c.jaccard()),
        };
    }
    let mut out = String::from("graph LR\n");
    for (i, &t) in nodes.iter().enumerate() {
        let t = analysis.test(t);
        let label = format!(
            "{}<br/>{}<br/>{}, {} regions",
            t.id.binary_id,
            t.id.short_name(),
            t.tier(),
            t.region_count
        );
        let _ = writeln!(out, "  t{i}[\"{}\"]", label.replace('"', "'"));
    }
    out.push_str(&edges);
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::super::{MarkdownOptions, fixture, markdown};
    use crate::test_id::TestId;
    use crate::verdict::{LedgerContents, Verdict, VerdictRecord};

    #[test]
    fn lists_candidates_in_rank_order_and_labels_same_file_pairs() {
        let md = markdown(
            &fixture::analysis(),
            &LedgerContents::default(),
            MarkdownOptions::default(),
        )
        .unwrap();
        let first = md
            .find("### 1. subsumed (same file): `demo tests::small`")
            .expect(&md);
        let second = md
            .find("### 2. duplicate: `demo::cli copy_of_big`")
            .expect(&md);
        assert!(first < second);
        assert!(md.contains(
            "- Same-file pairs (candidate and other in one source file, listed first): 1"
        ));
        assert!(md.contains("Strict supersets in this run: 2"));
        assert!(md.contains("Group size: 2."));
        assert!(md.contains("- `demo tests::gated`"));
    }

    #[test]
    fn shows_a_resolved_verdict_on_its_candidate() {
        let record = VerdictRecord {
            test: TestId::new("demo::cli", "copy_of_big"),
            other: TestId::new("demo", "tests::big"),
            verdict: Verdict::Cut,
            reason: "asserts nothing the keeper does not".into(),
        };
        let mut ledger = LedgerContents::default();
        ledger
            .verdicts
            .insert((record.test.clone(), record.other.clone()), record);
        let md = markdown(&fixture::analysis(), &ledger, MarkdownOptions::default()).unwrap();
        let second = md.find("### 2.").unwrap();
        assert!(md[second..].contains("- Verdict: cut. asserts nothing the keeper does not"));
        assert!(!md[..second].contains("- Verdict:"));
    }
}
