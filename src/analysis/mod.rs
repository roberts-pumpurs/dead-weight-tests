//! Candidate analysis: per-test exports in, ranked candidates out.
//!
//! [`analyze`] is the whole interface. It loads every [`crate::export_format::TestExport`],
//! drops test code, compresses regions into coverage classes, finds duplicate,
//! subsumed, and near-duplicate tests, locates each test's source, and ranks
//! the candidates in review order. The result is persisted as `analysis.json`
//! so reports re-render without reloading coverage.

mod candidates;
mod coverage;
mod locate;
mod test_code;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use self::coverage::Coverage;
use self::locate::Locator;
use self::test_code::TestCode;
use serde::{Deserialize, Serialize};

use crate::ids::{ClassIdx, FileIdx, RegionIdx, TestIdx};
use crate::skipped::Skipped;
use crate::test_id::{TestId, Tier};

/// Bump when the persisted analysis layout changes.
pub const ANALYSIS_VERSION: u32 = 1;

/// Default Jaccard threshold for near duplicates.
pub const DEFAULT_NEAR: f64 = 0.95;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Analysis {
    pub version: u32,
    /// UTC timestamp, RFC 3339, seconds precision.
    pub generated: String,
    /// Workspace root the analysis resolved sources against.
    pub repo: PathBuf,
    pub near_threshold: f64,
    /// Every file that holds a non-test region some test executed, with its
    /// source text at analysis time (`None` when unreadable).
    pub files: Vec<SourceFile>,
    /// Every non-test code region inside a function some test executed,
    /// covered or not (the report needs uncovered ones to draw nesting).
    pub regions: Vec<RegionKey>,
    /// Coverage classes: covered regions executed by exactly the same tests.
    pub classes: Vec<Vec<RegionIdx>>,
    pub tests: Vec<Test>,
    /// Candidates in review rank order: position 0 is reviewed first.
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
    pub stats: Stats,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFile {
    /// Relative to the workspace root when inside it, else absolute.
    pub path: String,
    pub source: Option<String>,
}

/// A region identity: file and span. On disk: `[file, line_start, col_start, line_end, col_end]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "RegionKeyTuple", into = "RegionKeyTuple")]
pub struct RegionKey {
    pub file: FileIdx,
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
}

type RegionKeyTuple = (FileIdx, u32, u32, u32, u32);

impl From<RegionKeyTuple> for RegionKey {
    fn from((file, line_start, col_start, line_end, col_end): RegionKeyTuple) -> Self {
        RegionKey {
            file,
            line_start,
            col_start,
            line_end,
            col_end,
        }
    }
}

impl From<RegionKey> for RegionKeyTuple {
    fn from(r: RegionKey) -> Self {
        (r.file, r.line_start, r.col_start, r.line_end, r.col_end)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Test {
    pub id: TestId,
    /// Package directory relative to the workspace root.
    pub manifest_dir: Option<String>,
    pub processes: u32,
    /// Size of the coverage set: covered non-test regions.
    pub region_count: u32,
    /// The coverage classes this test covers, ascending.
    pub classes: Vec<ClassIdx>,
    pub location: Option<Location>,
    pub body: Option<TestBody>,
}

impl Test {
    pub fn tier(&self) -> Tier {
        self.id.tier()
    }
}

/// Where a test function is defined. Best effort: found by function name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub line: u32,
    /// Several functions matched the name equally well.
    pub ambiguous: bool,
}

/// The test function's source with the attributes and doc comments above it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestBody {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Duplicate,
    Subsumed,
    Near,
}

impl CandidateKind {
    pub const ALL: [CandidateKind; 3] = [
        CandidateKind::Duplicate,
        CandidateKind::Subsumed,
        CandidateKind::Near,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CandidateKind::Duplicate => "duplicate",
            CandidateKind::Subsumed => "subsumed",
            CandidateKind::Near => "near",
        }
    }

    /// What the report calls the other test of the pair.
    pub fn other_role(self) -> &'static str {
        match self {
            CandidateKind::Duplicate => "keeper",
            CandidateKind::Subsumed => "witness",
            CandidateKind::Near => "near twin",
        }
    }
}

/// How the candidate's coverage set relates to the other test's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Relation {
    /// Same coverage set as `other`, the keeper. `group_size` counts every test
    /// with this set, keeper included.
    Duplicate { group_size: u32 },
    /// Strict subset of `other`, the smallest witness. `strict_supersets` counts
    /// every strict superset in the run.
    Subsumed { strict_supersets: u32 },
    /// Jaccard similarity at or above the threshold, neither set contains the
    /// other. `other` is the larger twin.
    Near,
}

impl Relation {
    pub fn kind(self) -> CandidateKind {
        match self {
            Relation::Duplicate { .. } => CandidateKind::Duplicate,
            Relation::Subsumed { .. } => CandidateKind::Subsumed,
            Relation::Near => CandidateKind::Near,
        }
    }
}

/// A test to consider cutting, paired with the test that covers its regions.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(flatten)]
    pub relation: Relation,
    pub test: TestIdx,
    pub other: TestIdx,
    /// Regions both tests cover.
    pub shared: u32,
    /// Regions only `other` covers.
    pub gap: u32,
    /// Regions only `test` covers (non-zero only for near duplicates).
    pub extra: u32,
    /// Both tests were located, in the same source file.
    pub same_file: bool,
}

impl Candidate {
    pub fn kind(&self) -> CandidateKind {
        self.relation.kind()
    }

    pub fn jaccard(&self) -> f64 {
        let union = self.shared + self.gap + self.extra;
        if union == 0 {
            0.0
        } else {
            f64::from(self.shared) / f64::from(union)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub covered_regions: usize,
    pub covered_files: usize,
    /// Covered test-code regions left out of every coverage set.
    pub excluded_test_regions: usize,
}

/// Runs the analysis over every `*.json.gz` / `*.json` export in `exports`.
///
/// `repo` is the workspace root for test-code detection and source lookup.
/// `skipped` is carried through to the reports.
pub fn analyze(
    exports: &Path,
    skipped: Vec<Skipped>,
    repo: &Path,
    near_threshold: f64,
) -> Result<Analysis> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let paths = export_files(exports)?;
    if paths.is_empty() {
        bail!("no coverage exports in {}", exports.display());
    }
    let coverage = Coverage::load(&paths, &TestCode::scan(&repo))?;
    let mut candidates = candidates::find(&coverage, near_threshold);
    let Coverage {
        mut tests,
        files,
        regions,
        classes,
        stats,
        ..
    } = coverage;

    let mut locator = Locator::new(&repo);
    for test in &mut tests {
        test.location = locator.locate(test.manifest_dir.as_deref(), &test.id.name);
        test.body = test.location.as_ref().and_then(|l| locator.body(l));
    }
    for c in &mut candidates {
        c.same_file = locate::same_file(
            &repo,
            tests[c.test.get()].location.as_ref(),
            tests[c.other.get()].location.as_ref(),
        );
    }
    // Same-file pairs are the cheapest to review: move them ahead within each
    // section. The sort is stable, so each section keeps its own order.
    candidates.sort_by_key(|c| (c.kind() == CandidateKind::Near, !c.same_file));

    let files = files
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(repo.join(&path)).ok();
            SourceFile { path, source }
        })
        .collect();
    Ok(Analysis {
        version: ANALYSIS_VERSION,
        generated: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        repo,
        near_threshold,
        files,
        regions,
        classes,
        tests,
        candidates,
        skipped,
        stats,
    })
}

/// The `*.json.gz` / `*.json` exports in `dir`, sorted by file name.
fn export_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if (name.ends_with(".json.gz") || name.ends_with(".json")) && name != "skipped.json" {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

impl Analysis {
    pub fn load(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let Version { version } =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if version != ANALYSIS_VERSION {
            bail!(
                "{}: analysis version {version} (this dwd reads version {ANALYSIS_VERSION}); re-run dwd analyze",
                path.display()
            );
        }
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, self)?;
        writer
            .flush()
            .with_context(|| format!("write {}", path.display()))
    }

    pub fn test(&self, idx: TestIdx) -> &Test {
        &self.tests[idx.get()]
    }

    /// Every (candidate, other) pair, as the verdict ledger keys them.
    pub fn pairs(&self) -> HashSet<(TestId, TestId)> {
        self.candidates
            .iter()
            .map(|c| (self.test(c.test).id.clone(), self.test(c.other).id.clone()))
            .collect()
    }

    /// Tests with no covered non-test regions.
    pub fn empty_tests(&self) -> impl Iterator<Item = &Test> {
        self.tests.iter().filter(|t| t.region_count == 0)
    }

    /// The `limit` files holding the most regions of `classes`, with their region counts.
    pub fn top_files(
        &self,
        classes: impl IntoIterator<Item = ClassIdx>,
        limit: usize,
    ) -> Vec<(&str, usize)> {
        let mut counts: HashMap<FileIdx, usize> = HashMap::new();
        for class in classes {
            for region in &self.classes[class.get()] {
                *counts.entry(self.regions[region.get()].file).or_default() += 1;
            }
        }
        let mut counts: Vec<(FileIdx, usize)> = counts.into_iter().collect();
        counts.sort_by_key(|&(file, count)| (std::cmp::Reverse(count), file));
        counts
            .into_iter()
            .take(limit)
            .map(|(file, count)| (self.files[file.get()].path.as_str(), count))
            .collect()
    }
}
