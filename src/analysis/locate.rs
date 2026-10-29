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
    ///
    /// Parametrized test macros (rstest, test-case) turn `fn name` into a
    /// module `name` holding one function per case, so a test `m::name::case_1`
    /// has no `fn case_1` in the source. When the last segment matches no
    /// function, the segment above it is tried once: every case then points at
    /// the parametrized function.
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
        let options = match index.get(function) {
            Some(options) => options,
            None => index.get(segments.pop()?)?,
        };
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
        let start = attributes_start(&lines, line);
        let fn_offset: usize = lines[..line - 1].iter().map(|l| l.len() + 1).sum();
        let brace = fn_offset + text[fn_offset..].find('{')?;
        let end = line_of(text.as_bytes(), block_end(text.as_bytes(), brace));
        let source = lines[start - 1..end as usize].join("\n");
        Some(TestBody {
            path: location.path.clone(),
            start: start as u32,
            end,
            hash: crate::verdict::body_hash(&source),
            source,
        })
    }
}

/// The 1-based first line of the attributes and comments directly above
/// `line`. A multi-line attribute, such as an rstest `#[case(\n  1,\n)]`,
/// counts from its `#[` line, so case inputs are part of the body.
fn attributes_start(lines: &[&str], line: usize) -> usize {
    let mut start = line;
    while start > 1 {
        let above = lines[start - 2].trim();
        if above.starts_with("#[") || above.starts_with("//") {
            start -= 1;
        } else if above.ends_with(']') {
            // The last line of a multi-line attribute: its `#[` line is above,
            // before any blank line.
            let opening = (1..start - 1)
                .rev()
                .take_while(|&n| !lines[n - 1].trim().is_empty())
                .find(|&n| lines[n - 1].trim_start().starts_with("#["));
            match opening {
                Some(n) => start = n,
                None => break,
            }
        } else {
            break;
        }
    }
    start
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parametrized_cases_point_at_their_function() {
        let repo = tempfile::tempdir().unwrap();
        let src = repo.path().join("pkg/src");
        fs::create_dir_all(&src).unwrap();
        fs::write(repo.path().join("pkg/Cargo.toml"), "").unwrap();
        let lib = "#[cfg(test)]\nmod tests {\n    #[rstest]\n    fn sums(#[case] n: u8) {}\n\n    #[test]\n    fn plain() {}\n}\n";
        fs::write(src.join("lib.rs"), lib).unwrap();
        let mut locator = Locator::new(repo.path());

        let at = |l: Option<Location>| l.map(|l| (l.path, l.line));
        let lib_rs = |line| Some(("pkg/src/lib.rs".to_owned(), line));
        assert_eq!(
            at(locator.locate(Some("pkg"), "tests::sums::case_1")),
            lib_rs(4)
        );
        assert_eq!(at(locator.locate(Some("pkg"), "tests::plain")), lib_rs(7));
        // Only one level up: a case module whose function is also gone stays unlocated.
        assert_eq!(at(locator.locate(Some("pkg"), "tests::gone::case_1")), None);
    }

    #[test]
    fn body_includes_multiline_case_attributes() {
        let lines = [
            "fn helper() {}",
            "",
            "/// Doc.",
            "#[rstest]",
            "#[case(",
            "    1,",
            ")]",
            "fn sums(#[case] n: u8) {}",
        ];
        assert_eq!(attributes_start(&lines, 8), 3);
        // A blank line ends the attributes even after a line ending in `]`.
        assert_eq!(
            attributes_start(&["let a = [1];", "x]", "", "fn f() {}"], 4),
            4
        );
    }
}
