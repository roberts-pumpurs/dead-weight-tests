//! The verdict ledger: `verdicts.jsonl`, one reviewed decision per line.
//!
//! This module is the only code that knows what a valid verdict row is.
//! Writing ([`Ledger::record`]) and reading ([`Ledger::read`]) apply the same
//! row rule; recording also requires the pair to be a known candidate and
//! stamps each side's body hash from the analysis. Later lines win for the
//! same (test, other) pair. [`judge`] decides whether a verdict is stale.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::test_id::TestId;

/// A reviewer's decision for one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The candidate's assertions are weaker than or equal to the other test's.
    Cut,
    /// The candidate has one or two small distinct assertions to move into the other test.
    Merge,
    /// The candidate checks a different input class or runs where the other cannot.
    Keep,
    /// The test's name or doc comment promises a check its body never makes. Not a removal.
    Fix,
}

impl Verdict {
    pub const ALL: [Verdict; 4] = [Verdict::Cut, Verdict::Merge, Verdict::Keep, Verdict::Fix];

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Cut => "cut",
            Verdict::Merge => "merge",
            Verdict::Keep => "keep",
            Verdict::Fix => "fix",
        }
    }

    /// Strength when one test has verdicts on several pairs: cut > merge > fix > keep.
    pub fn rank(self) -> u8 {
        match self {
            Verdict::Keep => 1,
            Verdict::Fix => 2,
            Verdict::Merge => 3,
            Verdict::Cut => 4,
        }
    }
}

/// One ledger line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub test: TestId,
    pub other: TestId,
    pub verdict: Verdict,
    /// One sentence naming the concrete assertion difference. Never blank.
    pub reason: String,
    /// [`body_hash`] of the candidate's body at record time. Absent on old lines or unknown bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_hash: Option<String>,
    /// [`body_hash`] of the other test's body at record time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_hash: Option<String>,
}

pub type Pair = (TestId, TestId);

/// Current body hashes of a candidate pair, `None` where the body is unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyHashes {
    pub test: Option<String>,
    pub other: Option<String>,
}

/// A test body's hash: the first 16 hex digits of its sha256.
pub fn body_hash(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A recorded verdict checked against the current test bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged<'a> {
    /// The reviewed bodies are unchanged: the verdict stands.
    Fresh(&'a VerdictRecord),
    /// A reviewed body changed since: the pair needs review again.
    Stale(&'a VerdictRecord),
}

impl<'a> Judged<'a> {
    /// The verdict when it still stands.
    pub fn fresh(self) -> Option<&'a VerdictRecord> {
        match self {
            Judged::Fresh(r) => Some(r),
            Judged::Stale(_) => None,
        }
    }

    /// The verdict when it went stale.
    pub fn stale(self) -> Option<&'a VerdictRecord> {
        match self {
            Judged::Stale(r) => Some(r),
            Judged::Fresh(_) => None,
        }
    }
}

/// Stale when a stored hash differs from the current one. A side without a
/// stored hash (old lines) or without a current body counts as unchanged.
pub fn judge<'a>(record: &'a VerdictRecord, current: &BodyHashes) -> Judged<'a> {
    let changed = |stored: &Option<String>, now: &Option<String>| matches!((stored, now), (Some(s), Some(n)) if s != n);
    if changed(&record.test_hash, &current.test) || changed(&record.other_hash, &current.other) {
        Judged::Stale(record)
    } else {
        Judged::Fresh(record)
    }
}

/// Why a row was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum RowError {
    #[error("malformed JSON")]
    MalformedJson,
    #[error("unknown pair")]
    UnknownPair,
    #[error("unknown verdict")]
    UnknownVerdict,
    #[error("empty reason")]
    EmptyReason,
}

/// A rejected input row. `test`/`other` echo the row's ids when they were strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidRow {
    pub line: usize,
    pub test: Option<String>,
    pub other: Option<String>,
    pub error: RowError,
}

/// The ledger after resolving later-line-wins.
#[derive(Clone, Debug, Default)]
pub struct LedgerContents {
    pub verdicts: BTreeMap<Pair, VerdictRecord>,
    /// Lines that broke the row rule; they are ignored.
    pub invalid: Vec<InvalidRow>,
}

/// What `dwd verdict` prints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordSummary {
    pub recorded: usize,
    pub invalid: Vec<InvalidRow>,
    /// Expected pairs that still have no fresh verdict.
    pub missing: Vec<Pair>,
}

impl RecordSummary {
    /// 1 when any row was invalid or an expected pair is missing, else 0.
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.invalid.is_empty() || !self.missing.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the ledger. A missing file is an empty ledger.
    ///
    /// Pair membership is not checked: verdicts survive re-collection.
    pub fn read(&self) -> Result<LedgerContents> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(LedgerContents::default());
            }
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("reading {}", self.path.display()))
                );
            }
        };
        let mut contents = LedgerContents::default();
        for (line, item) in parse_lines(&text) {
            match item.and_then(|value| check_row(value, None)) {
                Ok(record) => {
                    contents
                        .verdicts
                        .insert((record.test.clone(), record.other.clone()), record);
                }
                Err(rejected) => contents.invalid.push(rejected.at(line)),
            }
        }
        Ok(contents)
    }

    /// Validates `input` (JSON lines, or one JSON array) and appends the valid
    /// rows. `known` maps each candidate pair to its current body hashes,
    /// which replace any hashes the input carries. `expect` lists pairs that
    /// must have a verdict after recording.
    pub fn record(
        &self,
        input: &str,
        known: &HashMap<Pair, BodyHashes>,
        expect: Option<&[Pair]>,
    ) -> Result<RecordSummary> {
        let mut rows = Vec::new();
        let mut invalid = Vec::new();
        for (line, item) in parse_input(input) {
            match item.and_then(|value| check_row(value, Some(known))) {
                Ok(mut record) => {
                    let hashes = &known[&(record.test.clone(), record.other.clone())];
                    record.test_hash = hashes.test.clone();
                    record.other_hash = hashes.other.clone();
                    rows.push(record);
                }
                Err(rejected) => invalid.push(rejected.at(line)),
            }
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out = String::new();
        for row in &rows {
            out.push_str(&serde_json::to_string(row)?);
            out.push('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        file.write_all(out.as_bytes())
            .with_context(|| format!("appending to {}", self.path.display()))?;

        let mut missing = Vec::new();
        if let Some(expect) = expect {
            let recorded = self.read()?.verdicts;
            let fresh = |pair: &Pair| {
                recorded.get(pair).is_some_and(|record| {
                    let current = known.get(pair).cloned().unwrap_or_default();
                    matches!(judge(record, &current), Judged::Fresh(_))
                })
            };
            for pair in expect {
                if !fresh(pair) && !missing.contains(pair) {
                    missing.push(pair.clone());
                }
            }
        }
        Ok(RecordSummary {
            recorded: rows.len(),
            invalid,
            missing,
        })
    }
}

/// A row as written by a reviewer, before the row rule. Each field keeps
/// whether it was a string so rejected rows can echo their ids.
#[derive(Deserialize)]
struct RawRow {
    #[serde(default)]
    test: Field,
    #[serde(default)]
    other: Field,
    #[serde(default)]
    verdict: Field,
    #[serde(default)]
    reason: Field,
    #[serde(default)]
    test_hash: Field,
    #[serde(default)]
    other_hash: Field,
}

/// A JSON value that is either a string or anything else (including absent).
#[derive(Default, Deserialize)]
#[serde(untagged)]
enum Field {
    Str(String),
    #[default]
    #[serde(skip)]
    Absent,
    Other(IgnoredAny),
}

impl Field {
    fn as_str(&self) -> Option<&str> {
        match self {
            Field::Str(s) => Some(s),
            Field::Absent | Field::Other(_) => None,
        }
    }
}

/// A rejection before its line number is attached.
struct Rejected {
    test: Option<String>,
    other: Option<String>,
    error: RowError,
}

impl Rejected {
    fn malformed() -> Self {
        Self {
            test: None,
            other: None,
            error: RowError::MalformedJson,
        }
    }

    fn at(self, line: usize) -> InvalidRow {
        InvalidRow {
            line,
            test: self.test,
            other: self.other,
            error: self.error,
        }
    }
}

/// The row rule. Classification of bad shapes:
/// - not a JSON object → `MalformedJson`;
/// - `test`/`other` missing, not strings, not a [`TestId`], or (when `known`
///   is given) not a known pair → `UnknownPair`;
/// - `verdict` missing, not a string, or not a [`Verdict`] → `UnknownVerdict`;
/// - `reason` missing, not a string, or blank → `EmptyReason`.
///
/// `test_hash`/`other_hash` are kept when they are strings, else dropped.
fn check_row(
    value: Value,
    known: Option<&HashMap<Pair, BodyHashes>>,
) -> Result<VerdictRecord, Rejected> {
    if !value.is_object() {
        return Err(Rejected::malformed());
    }
    let raw = RawRow::deserialize(value).map_err(|_| Rejected::malformed())?;
    let reject = |error| Rejected {
        test: raw.test.as_str().map(str::to_owned),
        other: raw.other.as_str().map(str::to_owned),
        error,
    };
    let parse_id = |field: &Field| field.as_str().and_then(|s| s.parse::<TestId>().ok());
    let (Some(test), Some(other)) = (parse_id(&raw.test), parse_id(&raw.other)) else {
        return Err(reject(RowError::UnknownPair));
    };
    let pair = (test, other);
    if known.is_some_and(|known| !known.contains_key(&pair)) {
        return Err(reject(RowError::UnknownPair));
    }
    let verdict = raw
        .verdict
        .as_str()
        .and_then(|s| Verdict::ALL.into_iter().find(|v| v.as_str() == s))
        .ok_or_else(|| reject(RowError::UnknownVerdict))?;
    let reason = match raw.reason.as_str() {
        Some(reason) if !reason.trim().is_empty() => reason.to_owned(),
        _ => return Err(reject(RowError::EmptyReason)),
    };
    let (test, other) = pair;
    Ok(VerdictRecord {
        test,
        other,
        verdict,
        reason,
        test_hash: raw.test_hash.as_str().map(str::to_owned),
        other_hash: raw.other_hash.as_str().map(str::to_owned),
    })
}

/// Parses JSON lines: 1-based physical line numbers, blank lines skipped.
fn parse_lines(text: &str) -> impl Iterator<Item = (usize, Result<Value, Rejected>)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            (
                i + 1,
                serde_json::from_str(line).map_err(|_| Rejected::malformed()),
            )
        })
}

/// Parses record input: one JSON array when the trimmed text starts with `[`
/// (items numbered from 1), else JSON lines.
fn parse_input(text: &str) -> Vec<(usize, Result<Value, Rejected>)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('[') {
        return parse_lines(text).collect();
    }
    match serde_json::from_str::<Vec<Value>>(trimmed) {
        Ok(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, item)| (i + 1, Ok(item)))
            .collect(),
        Err(_) => vec![(1, Err(Rejected::malformed()))],
    }
}

/// The verdict line format shown to reviewers, e.g. in the copied review prompt.
pub fn verdict_line_example() -> String {
    /// Same key order as a ledger line.
    #[derive(Serialize)]
    struct Example<'a> {
        test: &'a str,
        other: &'a str,
        verdict: &'a str,
        reason: &'a str,
    }
    let verdicts: Vec<&str> = Verdict::ALL.iter().map(|v| v.as_str()).collect();
    let example = Example {
        test: "<candidate id>",
        other: "<other id>",
        verdict: &verdicts.join("|"),
        reason: "<one sentence naming the concrete assertion difference>",
    };
    serde_json::to_string(&example).expect("plain strings serialize")
}

/// The shell command a reviewer pipes verdict lines into. It names the
/// workspace and the artifact dir, so it works from any directory.
pub fn recorder_command(workspace: &Path, artifact_dir: &Path) -> String {
    format!(
        "dwd -C {} --dir {} verdict",
        shell_words::quote(&workspace.to_string_lossy()),
        shell_words::quote(&artifact_dir.to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> TestId {
        TestId::new("crate", name)
    }

    fn known() -> HashMap<Pair, BodyHashes> {
        [(id("a"), id("b")), (id("c"), id("d"))]
            .into_iter()
            .map(|pair| (pair, BodyHashes::default()))
            .collect()
    }

    fn line(test: &str, other: &str, verdict: &str, reason: &str) -> String {
        serde_json::json!({"test": test, "other": other, "verdict": verdict, "reason": reason})
            .to_string()
    }

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path().join("sub/verdicts.jsonl"));
        (dir, ledger)
    }

    #[test]
    fn appends_valid_rows_with_only_four_keys_and_keeps_existing_lines() {
        let (_dir, ledger) = ledger();
        fs::create_dir_all(ledger.path().parent().unwrap()).unwrap();
        fs::write(ledger.path(), "existing\n").unwrap();
        let input = r#"{"test": "crate a", "other": "crate b", "verdict": "cut", "reason": "same assert", "extra": 1}"#;
        let summary = ledger.record(input, &known(), None).unwrap();
        assert_eq!(summary.recorded, 1);
        assert_eq!(summary.exit_code(), 0);
        let text = fs::read_to_string(ledger.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "existing");
        let row: serde_json::Map<String, Value> = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(
            row.keys().collect::<Vec<_>>(),
            ["other", "reason", "test", "verdict"]
        );
        assert_eq!(row["verdict"], "cut");
        assert_eq!(row["test"], "crate a");
    }

    #[test]
    fn reports_invalid_rows_with_line_numbers_and_keeps_valid_row() {
        let (_dir, ledger) = ledger();
        let input = [
            line("crate a", "crate b", "keep", "different input"),
            String::new(),
            "{not json".to_owned(),
            line("crate x", "crate y", "cut", "r"),
            line("crate a", "crate b", "drop", "r"),
            line("crate c", "crate d", "merge", "  "),
            r#"{"test": 1, "other": "crate d"}"#.to_owned(),
            "[1]".to_owned(),
        ]
        .join("\n");
        let summary = ledger.record(&input, &known(), None).unwrap();
        assert_eq!(summary.recorded, 1);
        assert_eq!(summary.exit_code(), 1);
        let got: Vec<(usize, Option<&str>, RowError)> = summary
            .invalid
            .iter()
            .map(|r| (r.line, r.test.as_deref(), r.error))
            .collect();
        assert_eq!(
            got,
            [
                (3, None, RowError::MalformedJson),
                (4, Some("crate x"), RowError::UnknownPair),
                (5, Some("crate a"), RowError::UnknownVerdict),
                (6, Some("crate c"), RowError::EmptyReason),
                (7, None, RowError::UnknownPair),
                (8, None, RowError::MalformedJson),
            ]
        );
        assert_eq!(summary.invalid[4].other.as_deref(), Some("crate d"));
        assert_eq!(
            fs::read_to_string(ledger.path()).unwrap().lines().count(),
            1
        );
    }

    #[test]
    fn expect_counts_existing_verdicts_and_new_input() {
        let (_dir, ledger) = ledger();
        ledger
            .record(&line("crate a", "crate b", "cut", "r"), &known(), None)
            .unwrap();
        let expect = [(id("a"), id("b")), (id("c"), id("d")), (id("c"), id("d"))];
        let summary = ledger.record("", &known(), Some(&expect)).unwrap();
        assert_eq!(summary.missing, [(id("c"), id("d"))]);
        assert_eq!(summary.exit_code(), 1);
        let summary = ledger
            .record(
                &line("crate c", "crate d", "fix", "r"),
                &known(),
                Some(&expect),
            )
            .unwrap();
        assert!(summary.missing.is_empty());
        assert_eq!(summary.exit_code(), 0);
    }

    #[test]
    fn array_input_appends_in_order() {
        let (_dir, ledger) = ledger();
        let input = format!(
            "  [{}, {}]\n",
            line("crate c", "crate d", "keep", "r1"),
            line("crate a", "crate b", "cut", "r2")
        );
        assert_eq!(ledger.record(&input, &known(), None).unwrap().recorded, 2);
        let text = fs::read_to_string(ledger.path()).unwrap();
        let tests: Vec<String> = text
            .lines()
            .map(|l| serde_json::from_str::<VerdictRecord>(l).unwrap().test.name)
            .collect();
        assert_eq!(tests, ["c", "a"]);

        let bad = ledger.record("[{", &known(), None).unwrap();
        assert_eq!(bad.invalid, [Rejected::malformed().at(1)]);
    }

    #[test]
    fn read_resolves_later_lines_and_reports_invalid_lines() {
        let (_dir, ledger) = ledger();
        assert!(ledger.read().unwrap().verdicts.is_empty());
        fs::create_dir_all(ledger.path().parent().unwrap()).unwrap();
        let text = [
            line("crate a", "crate b", "cut", "first"),
            "garbage".to_owned(),
            line("crate a", "crate b", "keep", "second"),
            line("crate gone", "crate b", "fix", "unknown pairs survive"),
        ]
        .join("\n");
        fs::write(ledger.path(), text).unwrap();
        let contents = ledger.read().unwrap();
        assert_eq!(contents.verdicts.len(), 2);
        assert_eq!(
            contents.verdicts[&(id("a"), id("b"))].verdict,
            Verdict::Keep
        );
        assert_eq!(
            contents.verdicts[&(id("gone"), id("b"))].verdict,
            Verdict::Fix
        );
        assert_eq!(contents.invalid, [Rejected::malformed().at(2)]);
    }

    #[test]
    fn record_stamps_analysis_hashes_over_reviewer_values() {
        let (_dir, ledger) = ledger();
        let mut known = known();
        let current = BodyHashes {
            test: Some(body_hash("fn a() {}")),
            other: None,
        };
        known.insert((id("a"), id("b")), current.clone());
        let input = r#"{"test": "crate a", "other": "crate b", "verdict": "cut", "reason": "r", "test_hash": "x", "other_hash": "y"}"#;
        ledger.record(input, &known, None).unwrap();
        let record = &ledger.read().unwrap().verdicts[&(id("a"), id("b"))];
        assert_eq!(record.test_hash, current.test);
        assert_eq!(record.other_hash, None);
        assert_eq!(body_hash("x").len(), 16);
    }

    #[test]
    fn judge_flags_changed_bodies_only() {
        let record = VerdictRecord {
            test: id("a"),
            other: id("b"),
            verdict: Verdict::Cut,
            reason: "r".into(),
            test_hash: Some(body_hash("old")),
            other_hash: Some(body_hash("other")),
        };
        let now = |test: Option<&str>, other: Option<&str>| BodyHashes {
            test: test.map(body_hash),
            other: other.map(body_hash),
        };
        assert_eq!(
            judge(&record, &now(Some("old"), Some("other"))),
            Judged::Fresh(&record)
        );
        assert_eq!(
            judge(&record, &now(Some("new"), Some("other"))),
            Judged::Stale(&record)
        );
        assert_eq!(
            judge(&record, &now(Some("old"), Some("new"))),
            Judged::Stale(&record)
        );
        assert_eq!(judge(&record, &now(None, None)), Judged::Fresh(&record));
        let legacy = VerdictRecord {
            test_hash: None,
            other_hash: None,
            ..record.clone()
        };
        assert_eq!(
            judge(&legacy, &now(Some("new"), Some("new"))),
            Judged::Fresh(&legacy)
        );
    }

    #[test]
    fn example_and_command() {
        assert!(verdict_line_example().contains(r#""verdict":"cut|merge|keep|fix""#));
        assert_eq!(
            recorder_command(Path::new("/repo"), Path::new("/tmp/a b")),
            "dwd -C /repo --dir '/tmp/a b' verdict"
        );
    }
}
