//! Best-effort `fn <name>` lookup for a test inside its package sources.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::test_code::{block_end, line_of, rust_files};
use super::{Location, TestBody};

static FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)")
        .unwrap()
});

/// Function definitions by name: `(path relative to the repo, line)`.
type FnIndex = HashMap<String, Vec<(String, u32)>>;

pub(crate) struct Locator<'a> {
    repo: &'a Path,
    /// Per package directory, indexed on first use.
    index: HashMap<String, FnIndex>,
}

impl<'a> Locator<'a> {
    pub(crate) fn new(repo: &'a Path) -> Self {
        Locator {
            repo,
            index: HashMap::new(),
        }
    }

    fn scan(repo: &Path, manifest_dir: &str) -> FnIndex {
        let mut found = FnIndex::new();
        // Nested packages own their own tests.
        for path in rust_files(&repo.join(manifest_dir), true) {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(repo) else {
                continue;
            };
            let rel = rel.to_string_lossy();
            for caps in FN.captures_iter(&text) {
                let line = line_of(text.as_bytes(), caps.get(0).expect("whole match").start());
                found
                    .entry(caps[1].to_owned())
                    .or_default()
                    .push((rel.to_string(), line));
            }
        }
        found
    }

    /// Finds the test's function, preferring files whose path names more of
    /// the test's module segments.
    pub(crate) fn locate(
        &mut self,
        manifest_dir: Option<&str>,
        test_name: &str,
    ) -> Option<Location> {
        let manifest_dir = manifest_dir?;
        let repo = self.repo;
        let index = self
            .index
            .entry(manifest_dir.to_owned())
            .or_insert_with(|| Self::scan(repo, manifest_dir));
        let mut segments: Vec<&str> = test_name.split("::").collect();
        let function = segments.pop().expect("split yields at least one segment");
        let options = index.get(function)?;
        let score = |path: &str| {
            let stem = Path::new(path).with_extension("");
            let parts: HashSet<&str> = stem.iter().filter_map(|p| p.to_str()).collect();
            segments.iter().filter(|m| parts.contains(*m)).count()
        };
        let scores: Vec<usize> = options.iter().map(|(path, _)| score(path)).collect();
        let best = *scores.iter().max()?;
        let mut top = options
            .iter()
            .zip(&scores)
            .filter(|&(_, &s)| s == best)
            .map(|(o, _)| o);
        let (path, line) = top.next().expect("the best score belongs to some option");
        Some(Location {
            path: path.clone(),
            line: *line,
            ambiguous: top.next().is_some(),
        })
    }

    /// The function's source with the attributes and comments directly above it.
    pub(crate) fn body(&self, location: &Location) -> Option<TestBody> {
        let text = fs::read_to_string(self.repo.join(&location.path)).ok()?;
        let lines: Vec<&str> = text.split('\n').collect();
        let line = location.line as usize;
        let mut start = line;
        while start > 1 && {
            let above = lines[start - 2].trim_start();
            above.starts_with("#[") || above.starts_with("//")
        } {
            start -= 1;
        }
        let fn_offset: usize = lines[..line - 1].iter().map(|l| l.len() + 1).sum();
        let brace = fn_offset + text[fn_offset..].find('{')?;
        let end = line_of(text.as_bytes(), block_end(text.as_bytes(), brace));
        Some(TestBody {
            path: location.path.clone(),
            start: start as u32,
            end,
            source: lines[start - 1..end as usize].join("\n"),
        })
    }
}

/// Both tests are located and their source files resolve to the same path.
pub(crate) fn same_file(repo: &Path, a: Option<&Location>, b: Option<&Location>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    if a.path == b.path {
        return true;
    }
    match (
        repo.join(&a.path).canonicalize(),
        repo.join(&b.path).canonicalize(),
    ) {
        (Ok(pa), Ok(pb)) => pa == pb,
        _ => false,
    }
}
