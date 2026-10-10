//! Profile directories written by `dwd __runner`, and their triage.
//!
//! Layout: `<profile root>/<hash>/` with `meta.json` (written before the test
//! runs), `exit` (written after it ends), and `*.profraw` files.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::skipped::{SkipReason, Skipped};
use crate::test_id::TestId;

/// Bump when [`ProfileMeta`] changes.
pub const META_VERSION: u32 = 1;
pub const META_FILE: &str = "meta.json";
pub const EXIT_FILE: &str = "exit";

/// What the runner knew about the test before it started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub version: u32,
    pub test: TestId,
    /// The test binary nextest ran.
    pub binary: PathBuf,
    /// `CARGO_MANIFEST_DIR` of the test's package (absolute; empty when unset).
    pub manifest_dir: String,
}

/// A test that exited 0 and wrote at least one profile.
#[derive(Clone, Debug)]
pub struct PassedProfile {
    /// The test's profile directory.
    pub dir: PathBuf,
    pub meta: ProfileMeta,
    /// Sorted `*.profraw` paths: the test process plus instrumented subprocesses.
    pub profraws: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Triage {
    pub passed: Vec<PassedProfile>,
    pub skipped: Vec<Skipped>,
}

/// Classifies every profile directory under `profile_root`. With `listed`
/// (the tests nextest selected), listed tests without a directory are
/// [`SkipReason::NeverReached`]. Directories without `meta.json` are ignored.
pub fn triage(profile_root: &Path, listed: Option<&HashSet<TestId>>) -> Result<Triage> {
    let mut triage = Triage::default();
    let mut reached = HashSet::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(profile_root)
        .with_context(|| format!("read {}", profile_root.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    dirs.sort();
    for dir in dirs {
        let meta_path = dir.join(META_FILE);
        if !meta_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("read {}", meta_path.display()))?;
        let meta: ProfileMeta = serde_json::from_str(&text)
            .with_context(|| format!("parse {}", meta_path.display()))?;
        reached.insert(meta.test.clone());
        let reason = match read_exit(&dir)? {
            None => Some(SkipReason::Killed),
            Some(0) => None,
            Some(exit) => Some(SkipReason::Failed { exit }),
        };
        if let Some(reason) = reason {
            triage.skipped.push(Skipped {
                test: meta.test,
                reason,
            });
            continue;
        }
        let mut profraws: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|e| e.path()))
            .filter(|path| {
                path.as_ref()
                    .map_or(true, |p| p.extension().is_some_and(|e| e == "profraw"))
            })
            .collect::<std::io::Result<_>>()?;
        if profraws.is_empty() {
            triage.skipped.push(Skipped {
                test: meta.test,
                reason: SkipReason::NoProfile,
            });
            continue;
        }
        profraws.sort();
        triage.passed.push(PassedProfile {
            dir,
            meta,
            profraws,
        });
    }
    if let Some(listed) = listed {
        let mut never: Vec<&TestId> = listed.difference(&reached).collect();
        never.sort();
        triage.skipped.extend(never.into_iter().map(|test| Skipped {
            test: test.clone(),
            reason: SkipReason::NeverReached,
        }));
    }
    Ok(triage)
}

fn read_exit(dir: &Path) -> Result<Option<i32>> {
    let path = dir.join(EXIT_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(
            text.trim()
                .parse()
                .with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

#[derive(Deserialize)]
struct NextestList {
    #[serde(rename = "rust-suites")]
    rust_suites: BTreeMap<String, Suite>,
}

#[derive(Deserialize)]
struct Suite {
    #[serde(rename = "binary-id")]
    binary_id: Option<String>,
    testcases: BTreeMap<String, TestCase>,
}

#[derive(Deserialize)]
struct TestCase {
    #[serde(rename = "filter-match")]
    filter_match: Option<FilterMatch>,
}

#[derive(Deserialize)]
struct FilterMatch {
    status: String,
}

/// The tests `cargo nextest list --message-format json` selected. Malformed
/// input yields `None` with a warning: the listing is a best-effort extra.
pub fn parse_listed(text: &str) -> Option<HashSet<TestId>> {
    let list: NextestList = match serde_json::from_str(text) {
        Ok(list) => list,
        Err(error) => {
            eprintln!(
                "dwd: warning: cannot read the nextest test list ({error}); skipped.json omits tests that never reached the runner"
            );
            return None;
        }
    };
    Some(
        list.rust_suites
            .into_iter()
            .flat_map(|(key, suite)| {
                let binary_id = suite.binary_id.unwrap_or(key);
                suite
                    .testcases
                    .into_iter()
                    .filter(|(_, case)| {
                        case.filter_match
                            .as_ref()
                            .is_some_and(|m| m.status == "matches")
                    })
                    .map(move |(name, _)| TestId::new(binary_id.clone(), name))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(root: &Path, binary_id: &str, name: &str, exit: Option<i32>, profraws: usize) {
        let test = TestId::new(binary_id, name);
        let dir = root.join(test.to_string().replace([' ', ':', '/'], "_"));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = ProfileMeta {
            version: META_VERSION,
            test,
            binary: "/bin/t".into(),
            manifest_dir: String::new(),
        };
        std::fs::write(dir.join(META_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        if let Some(exit) = exit {
            std::fs::write(dir.join(EXIT_FILE), format!("{exit}\n")).unwrap();
        }
        for i in 0..profraws {
            std::fs::write(dir.join(format!("{i}-x.profraw")), "").unwrap();
        }
    }

    #[test]
    fn triage_classifies_each_outcome() {
        let root = tempfile::tempdir().unwrap();
        profile(root.path(), "c", "passed", Some(0), 2);
        profile(root.path(), "c", "failed", Some(101), 1);
        profile(root.path(), "c", "killed", None, 1);
        profile(root.path(), "c", "silent", Some(0), 0);
        profile(root.path(), "c::it", "unlisted_but_ran", Some(0), 1);
        let listed: HashSet<TestId> = ["passed", "failed", "killed", "silent", "never"]
            .map(|n| TestId::new("c", n))
            .into();

        let triage = triage(root.path(), Some(&listed)).unwrap();

        let mut passed: Vec<_> = triage
            .passed
            .iter()
            .map(|p| (p.meta.test.name.as_str(), p.profraws.len()))
            .collect();
        passed.sort();
        assert_eq!(passed, [("passed", 2), ("unlisted_but_ran", 1)]);
        let mut skipped: Vec<_> = triage
            .skipped
            .iter()
            .map(|s| (s.test.name.as_str(), s.reason.clone()))
            .collect();
        skipped.sort();
        assert_eq!(
            skipped,
            [
                ("failed", SkipReason::Failed { exit: 101 }),
                ("killed", SkipReason::Killed),
                ("never", SkipReason::NeverReached),
                ("silent", SkipReason::NoProfile),
            ]
        );
    }

    #[test]
    fn listing_selects_only_matching_tests() {
        let text = r#"{"rust-suites": {
            "c": {"binary-id": "c", "testcases": {
                "a": {"filter-match": {"status": "matches"}},
                "b": {"filter-match": {"status": "mismatch", "reason": "string"}}}},
            "key": {"testcases": {"x": {"filter-match": {"status": "matches"}}}}}}"#;
        let listed = parse_listed(text).unwrap();
        assert_eq!(
            listed,
            [TestId::new("c", "a"), TestId::new("key", "x")].into()
        );
    }

    #[test]
    fn malformed_listing_is_none() {
        assert!(parse_listed("").is_none());
        assert!(parse_listed(r#"{"rust-suites": []}"#).is_none());
    }
}
