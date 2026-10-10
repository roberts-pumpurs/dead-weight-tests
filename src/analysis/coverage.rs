//! Coverage sets compressed into region classes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

use super::test_code::{TestCode, TestSpans};
use super::{RegionKey, Stats, Test};
use crate::export_format::TestExport;
use crate::ids::{ClassIdx, FileIdx, RegionIdx, TestIdx};

/// Every export loaded: tests, interned regions, and classes of covered
/// regions executed by exactly the same tests.
pub(crate) struct Coverage {
    pub tests: Vec<Test>,
    pub files: Vec<String>,
    pub regions: Vec<RegionKey>,
    pub classes: Vec<Vec<RegionIdx>>,
    /// Per class, the tests covering it, ascending. Sparse: classes are
    /// usually covered by few tests.
    pub postings: Vec<Vec<TestIdx>>,
    pub stats: Stats,
}

/// A file name seen in some export, with its test code and, once it holds a
/// non-test region, its index in `Coverage::files`.
struct SeenFile {
    name: String,
    spans: TestSpans,
    file: Option<FileIdx>,
}

impl Coverage {
    pub(crate) fn load(paths: &[PathBuf], test_code: &TestCode) -> Result<Self> {
        let mut tests = Vec::with_capacity(paths.len());
        let mut files = Vec::new();
        let mut regions = Vec::new();
        let mut region_ids: HashMap<RegionKey, usize> = HashMap::new();
        // Per region, the tests covering it; tests load in order, so ascending.
        let mut region_postings: Vec<Vec<u32>> = Vec::new();
        let mut seen_ids: HashMap<String, usize> = HashMap::new();
        let mut seen: Vec<SeenFile> = Vec::new();
        let mut excluded: HashSet<(usize, u32, u32, u32, u32)> = HashSet::new();

        for (done, path) in paths.iter().enumerate() {
            let export = TestExport::read(path)?;
            let index = u32::try_from(tests.len()).expect("test count overflows u32");
            let mut region_count = 0u32;
            for function in &export.functions {
                let local: Vec<usize> = function
                    .filenames
                    .iter()
                    .map(|name| match seen_ids.get(name) {
                        Some(&i) => i,
                        None => {
                            seen.push(SeenFile {
                                name: name.clone(),
                                spans: test_code.lookup(name),
                                file: None,
                            });
                            seen_ids.insert(name.clone(), seen.len() - 1);
                            seen.len() - 1
                        }
                    })
                    .collect();
                for r in &function.regions {
                    let s = local[r.file as usize];
                    let entry = &mut seen[s];
                    if entry.spans.covers(r.line_start) {
                        if r.count > 0 {
                            excluded.insert((s, r.line_start, r.col_start, r.line_end, r.col_end));
                        }
                        continue;
                    }
                    let file = *entry.file.get_or_insert_with(|| {
                        files.push(entry.name.clone());
                        FileIdx::new(files.len() - 1)
                    });
                    let key = RegionKey {
                        file,
                        line_start: r.line_start,
                        col_start: r.col_start,
                        line_end: r.line_end,
                        col_end: r.col_end,
                    };
                    let rid = *region_ids.entry(key).or_insert_with(|| {
                        regions.push(key);
                        region_postings.push(Vec::new());
                        regions.len() - 1
                    });
                    let posting = &mut region_postings[rid];
                    if r.count > 0 && posting.last() != Some(&index) {
                        posting.push(index);
                        region_count += 1;
                    }
                }
            }
            tests.push(Test {
                id: export.test.id,
                manifest_dir: Some(export.test.manifest_dir),
                processes: export.test.processes,
                region_count,
                classes: Vec::new(),
                location: None,
                body: None,
            });
            if (done + 1) % 500 == 0 {
                eprintln!("loaded {}/{}", done + 1, paths.len());
            }
        }

        let mut class_ids: HashMap<&[u32], usize> = HashMap::new();
        let mut classes: Vec<Vec<RegionIdx>> = Vec::new();
        let mut postings: Vec<Vec<TestIdx>> = Vec::new();
        let mut covered_files = HashSet::new();
        let mut covered_regions = 0;
        for (rid, posting) in region_postings.iter().enumerate() {
            if posting.is_empty() {
                continue;
            }
            covered_regions += 1;
            covered_files.insert(regions[rid].file);
            let cid = *class_ids.entry(posting.as_slice()).or_insert_with(|| {
                let cid = ClassIdx::new(classes.len());
                // Classes are numbered in creation order, so each test's list stays ascending.
                for &t in posting {
                    tests[t as usize].classes.push(cid);
                }
                classes.push(Vec::new());
                postings.push(posting.iter().map(|&t| TestIdx(t)).collect());
                cid.get()
            });
            classes[cid].push(RegionIdx::new(rid));
        }

        Ok(Coverage {
            tests,
            files,
            regions,
            classes,
            postings,
            stats: Stats {
                covered_regions,
                covered_files: covered_files.len(),
                excluded_test_regions: excluded.len(),
            },
        })
    }
}
