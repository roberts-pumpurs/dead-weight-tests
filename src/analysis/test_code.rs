//! Which source lines are test code, plus the source scanning helpers the
//! locator shares.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

const SKIP_DIRS: [&str; 4] = ["target", "node_modules", ".git", ".claude"];

static CFG_TEST_MOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*([;{])",
    )
    .unwrap()
});

/// Index of the `}` matching the `{` at `start`, or `text.len()` when unmatched.
/// Skips comments, strings, and brace char literals.
pub(crate) fn block_end(text: &[u8], start: usize) -> usize {
    let n = text.len();
    let mut depth = 0i32;
    let mut i = start;
    while i < n {
        let rest = &text[i..];
        if rest.starts_with(b"//") {
            match rest.iter().position(|&b| b == b'\n') {
                Some(p) => i += p,
                None => return n,
            }
            continue;
        }
        if rest.starts_with(b"/*") {
            i = match text[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(p) => i + 2 + p + 2,
                None => n,
            };
            continue;
        }
        match text[i] {
            b'"' => {
                i += 1;
                while i < n && text[i] != b'"' {
                    i += if text[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if i + 2 < n && text[i + 2] == b'\'' => i += 2,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    n
}

/// 1-based line number of byte `offset`.
pub(crate) fn line_of(text: &[u8], offset: usize) -> u32 {
    let newlines = text[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(newlines + 1).expect("line number overflows u32")
}

/// Every `.rs` file under `base`, sorted, skipping build and tool directories
/// and, when asked, nested Cargo packages.
pub(crate) fn rust_files(base: &Path, skip_nested_packages: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                let skip = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| SKIP_DIRS.contains(&name))
                    || (skip_nested_packages && path.join("Cargo.toml").exists());
                if !skip {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The test code inside one file.
#[derive(Clone, Debug, Default)]
pub(crate) struct TestSpans {
    whole: bool,
    /// Inline `#[cfg(test)] mod x { … }` blocks as inclusive line ranges.
    lines: Vec<(u32, u32)>,
}

impl TestSpans {
    pub(crate) fn covers(&self, line: u32) -> bool {
        self.whole || self.lines.iter().any(|&(lo, hi)| lo <= line && line <= hi)
    }
}

/// Test code across the workspace, built by scanning its `.rs` files once.
///
/// Test code is any file under a `tests/` or `benches/` directory, files named
/// `tests.rs`, `test.rs`, `*_tests.rs`, or `*_test.rs`, files of
/// `#[cfg(test)] mod x;` modules, and inline `#[cfg(test)] mod x { … }` blocks.
pub(crate) struct TestCode {
    repo: PathBuf,
    whole: HashSet<PathBuf>,
    ranges: HashMap<PathBuf, Vec<(u32, u32)>>,
}

impl TestCode {
    pub(crate) fn scan(repo: &Path) -> Self {
        let mut code = TestCode {
            repo: repo.to_path_buf(),
            whole: HashSet::new(),
            ranges: HashMap::new(),
        };
        for path in rust_files(repo, false) {
            let rel = path
                .strip_prefix(repo)
                .expect("rust_files stays under its base")
                .to_path_buf();
            code.scan_file(&path, rel);
        }
        code
    }

    fn scan_file(&mut self, path: &Path, rel: PathBuf) {
        let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let in_test_dir = rel
            .parent()
            .is_some_and(|dir| dir.iter().any(|c| c == "tests" || c == "benches"));
        if in_test_dir
            || matches!(stem, "tests" | "test")
            || stem.ends_with("_tests")
            || stem.ends_with("_test")
        {
            self.whole.insert(rel);
            return;
        }
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        // `mod x;` in lib.rs/main.rs/mod.rs resolves next to the file; elsewhere under a dir named after the file.
        let parent = rel.parent().unwrap_or(Path::new(""));
        let mod_dir = if matches!(stem, "lib" | "main" | "mod") {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        for caps in CFG_TEST_MOD.captures_iter(&text) {
            let name = &caps[1];
            let delimiter = caps.get(2).expect("group 2 always matches");
            if delimiter.as_str() == ";" {
                self.whole.insert(mod_dir.join(format!("{name}.rs")));
                self.whole.insert(mod_dir.join(name).join("mod.rs"));
            } else {
                let bytes = text.as_bytes();
                let first = line_of(bytes, caps.get(0).expect("whole match").start());
                let last = line_of(bytes, block_end(bytes, delimiter.start()));
                self.ranges
                    .entry(rel.clone())
                    .or_default()
                    .push((first, last));
            }
        }
    }

    /// The test code of an export file name (relative to the repo or absolute).
    pub(crate) fn lookup(&self, filename: &str) -> TestSpans {
        let path = Path::new(filename);
        let rel = path.strip_prefix(&self.repo).unwrap_or(path);
        TestSpans {
            whole: self.whole.contains(rel),
            lines: self.ranges.get(rel).cloned().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block_end;

    #[test]
    fn block_end_ignores_braces_in_comments_strings_and_chars() {
        let text = br#"{ // }
    /* } */ let s = "}\"}"; let c = '}'; { } }"#;
        assert_eq!(block_end(text, 0), text.len() - 1);
    }
}
