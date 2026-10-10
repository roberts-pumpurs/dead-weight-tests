use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::export_format::{CoveredFunction, EXPORT_VERSION, ExportedTest, Region, TestExport};

/// A temporary repo with source files and a directory of per-test exports.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::create_dir_all(root.join("exports")).unwrap();
        // Lines 20-24 are an inline cfg(test) module.
        let mut lib = vec!["pub fn f() {}"; 19].join("\n");
        lib.push_str("\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {}\n}");
        fs::write(root.join("src/lib.rs"), lib).unwrap();
        fs::write(root.join("tests/it.rs"), "#[test]\nfn it() {}\n").unwrap();
        Workspace { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// `regions`: (file, line) pairs, each a one-line covered region.
    fn test(&self, binary_id: &str, name: &str, regions: &[(&str, u32)]) {
        let filenames: Vec<String> = regions
            .iter()
            .map(|&(f, _)| f.to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let regions = regions
            .iter()
            .map(|&(f, line)| Region {
                line_start: line,
                col_start: 1,
                line_end: line,
                col_end: 10,
                count: 1,
                file: filenames.iter().position(|n| n == f).unwrap() as u32,
            })
            .collect();
        let export = TestExport {
            version: EXPORT_VERSION,
            test: ExportedTest {
                id: TestId::new(binary_id, name),
                binary: PathBuf::new(),
                manifest_dir: ".".into(),
                processes: 1,
            },
            functions: vec![CoveredFunction { filenames, regions }],
        };
        let file = format!("{binary_id}-{name}.json.gz").replace(':', "_");
        export
            .write_gz(&self.root().join("exports").join(file))
            .unwrap();
    }

    /// src/two.rs defines test functions s1..s4.
    fn two_tests_file(&self) {
        let text: String = (1..=4)
            .map(|n| format!("#[test]\nfn s{n}() {{}}\n"))
            .collect();
        fs::write(self.root().join("src/two.rs"), text).unwrap();
    }

    fn analyze(&self, near: f64) -> Analysis {
        analyze(&self.root().join("exports"), Vec::new(), self.root(), near).unwrap()
    }
}

fn prod(lines: impl IntoIterator<Item = u32>) -> Vec<(&'static str, u32)> {
    lines.into_iter().map(|l| ("src/lib.rs", l)).collect()
}

/// (kind, test name, other name) in rank order.
fn found(analysis: &Analysis) -> Vec<(CandidateKind, &str, &str)> {
    analysis
        .candidates
        .iter()
        .map(|c| {
            (
                c.kind(),
                analysis.test(c.test).id.name.as_str(),
                analysis.test(c.other).id.name.as_str(),
            )
        })
        .collect()
}

#[test]
fn duplicates_keep_the_unit_test_and_subsets_name_the_smallest_witness() {
    let ws = Workspace::new();
    ws.test("crate::it", "it_dup", &prod([1, 2]));
    ws.test("crate", "unit_dup", &prod([1, 2]));
    ws.test("crate", "mid", &prod([1, 2, 3]));
    ws.test("crate", "big", &prod([1, 2, 3, 4, 5]));
    let analysis = ws.analyze(DEFAULT_NEAR);
    let found = found(&analysis);
    assert!(found.contains(&(CandidateKind::Duplicate, "it_dup", "unit_dup")));
    assert!(found.contains(&(CandidateKind::Subsumed, "unit_dup", "mid")));
    assert!(found.contains(&(CandidateKind::Subsumed, "mid", "big")));
    // A dropped duplicate is reported once, as a duplicate.
    assert!(
        !found
            .iter()
            .any(|&(kind, name, _)| kind == CandidateKind::Subsumed && name == "it_dup")
    );
}

#[test]
fn near_duplicates_need_the_threshold_and_exclude_subsets() {
    let ws = Workspace::new();
    let shared = 100..140; // clear of the cfg(test) block at lines 20-24
    ws.test("crate", "left", &prod(shared.clone().chain([201])));
    ws.test("crate", "right", &prod(shared.clone().chain([202]))); // Jaccard 40/42 = 0.952
    ws.test("crate", "far", &prod(shared.clone().take(30).chain([203]))); // Jaccard with left 30/42
    ws.test("crate", "sub", &prod(shared)); // subset of left with Jaccard 40/41: subsumed, never near

    let analysis = ws.analyze(0.95);
    let found = found(&analysis);
    let near: Vec<_> = found
        .iter()
        .filter(|c| c.0 == CandidateKind::Near)
        .map(|c| (c.1, c.2))
        .collect();
    assert_eq!(near, [("left", "right")]);
    assert!(
        found
            .iter()
            .any(|&(kind, name, _)| kind == CandidateKind::Subsumed && name == "sub")
    );

    let stricter = ws.analyze(0.96);
    assert!(
        !stricter
            .candidates
            .iter()
            .any(|c| c.kind() == CandidateKind::Near)
    );
}

#[test]
fn test_code_regions_do_not_make_tests_unique() {
    let ws = Workspace::new();
    // Each test also runs its own body: a cfg(test) line and a tests/ file line.
    let mut a = prod([1, 2]);
    a.push(("src/lib.rs", 23));
    let mut b = prod([1, 2]);
    b.push(("tests/it.rs", 2));
    ws.test("crate", "a", &a);
    ws.test("crate", "b", &b);
    let analysis = ws.analyze(DEFAULT_NEAR);
    assert_eq!(found(&analysis), [(CandidateKind::Duplicate, "b", "a")]);
    assert_eq!(analysis.stats.excluded_test_regions, 2);
}

#[test]
fn same_file_needs_both_tests_located_in_one_file() {
    let ws = Workspace::new();
    ws.two_tests_file();
    ws.test("crate", "s1", &prod([1, 2]));
    ws.test("crate", "s2", &prod([1, 2])); // same file as its keeper s1
    ws.test("crate::it", "it", &prod([3, 4]));
    ws.test("crate", "s3", &prod([3, 4])); // keeper of `it`, which lives in tests/it.rs
    ws.test("crate", "ghost", &prod([5, 6]));
    ws.test("crate", "s4", &prod([5, 6])); // its keeper `ghost` has no known location
    let analysis = ws.analyze(DEFAULT_NEAR);
    let same_file: BTreeMap<_, _> = analysis
        .candidates
        .iter()
        .map(|c| {
            (
                (
                    analysis.test(c.test).id.name.as_str(),
                    analysis.test(c.other).id.name.as_str(),
                ),
                c.same_file,
            )
        })
        .collect();
    assert_eq!(
        same_file,
        BTreeMap::from([
            (("s2", "s1"), true),
            (("it", "s3"), false),
            (("s4", "ghost"), false)
        ])
    );
}

#[test]
fn same_file_pair_ranks_before_a_smaller_cross_file_gap() {
    let ws = Workspace::new();
    ws.two_tests_file();
    ws.test("crate::it", "it", &prod([1, 2]));
    ws.test("crate", "s3", &prod([1, 2])); // cross-file duplicate: gap 0
    ws.test("crate", "s1", &prod([10]));
    ws.test("crate", "s2", &prod([10, 11, 12])); // same-file subsumed: gap 2
    let analysis = ws.analyze(DEFAULT_NEAR);
    assert_eq!(
        found(&analysis),
        [
            (CandidateKind::Subsumed, "s1", "s2"),
            (CandidateKind::Duplicate, "it", "s3")
        ]
    );
    assert!(analysis.candidates[0].same_file);
}
