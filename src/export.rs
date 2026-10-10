//! LLVM export: merged profiles of one passing test to a [`crate::export_format::TestExport`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Deserialize;

use crate::export_format::{CoveredFunction, EXPORT_VERSION, ExportedTest, Region, TestExport};
use crate::profile::PassedProfile;
use crate::skipped::{SkipReason, Skipped};

/// Third-party and toolchain sources. Same intent as cargo-llvm-cov's default
/// filter, except tests are kept: test code is part of what a test exercises.
const IGNORE_FILENAME_REGEX: &str =
    r"(^|/)(rustc/[0-9a-f]+|\.cargo/(registry|git)|\.rustup/toolchains|nix/store)/";

/// Environment variables passed to child processes.
pub type Env = [(String, String)];

/// The LLVM tools of the active Rust toolchain.
#[derive(Clone, Debug)]
pub struct LlvmTools {
    pub profdata: PathBuf,
    pub cov: PathBuf,
}

impl LlvmTools {
    /// Finds `llvm-profdata` and `llvm-cov` in the sysroot of the toolchain
    /// `workspace` selects (it may pin one with rust-toolchain.toml).
    pub fn locate(workspace: &Path) -> Result<Self> {
        let sysroot = command_stdout(
            Command::new("rustc")
                .args(["--print", "sysroot"])
                .current_dir(workspace),
        )?;
        let bin = Path::new(sysroot.trim())
            .join("lib/rustlib")
            .join(host_triple(workspace)?)
            .join("bin");
        let tool = |name: &str| {
            let path = bin.join(name);
            if !path.is_file() {
                bail!(
                    "missing {}; install it with `rustup component add llvm-tools-preview`",
                    path.display()
                );
            }
            Ok(path)
        };
        Ok(Self {
            profdata: tool("llvm-profdata")?,
            cov: tool("llvm-cov")?,
        })
    }
}

/// The host target triple from `rustc -vV`, for the toolchain `workspace` selects.
pub fn host_triple(workspace: &Path) -> Result<String> {
    let text = command_stdout(Command::new("rustc").arg("-vV").current_dir(workspace))?;
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("`rustc -vV` printed no host")
}

fn command_stdout(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("run {command:?}"))?;
    if !output.status.success() {
        bail!(
            "{command:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[derive(Deserialize)]
struct Metadata {
    target_directory: PathBuf,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

/// Built workspace binaries that tests may spawn as subprocesses. Run with the
/// coverage `env`, whose target directory differs from a normal build's.
pub fn workspace_bins(workspace: &Path, env: &Env) -> Result<Vec<PathBuf>> {
    let text = command_stdout(
        Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(workspace)
            .envs(env.iter().map(|(k, v)| (k, v))),
    )?;
    let metadata: Metadata = serde_json::from_str(&text).context("parse cargo metadata")?;
    let debug = metadata.target_directory.join("debug");
    let mut bins: Vec<PathBuf> = metadata
        .packages
        .iter()
        .flat_map(|p| &p.targets)
        .filter(|t| t.kind.iter().any(|k| k == "bin"))
        .map(|t| debug.join(&t.name))
        .filter(|path| path.is_file())
        .collect();
    bins.sort();
    bins.dedup();
    Ok(bins)
}

#[derive(Deserialize)]
struct CovExport {
    data: Vec<CovData>,
}

#[derive(Deserialize)]
struct CovData {
    functions: Vec<CovFunction>,
}

#[derive(Deserialize)]
struct CovFunction {
    count: u64,
    filenames: Vec<String>,
    /// `[line_start, col_start, line_end, col_end, count, file_id, expanded_file_id, kind]`.
    regions: Vec<Vec<u64>>,
}

/// Exports one passing test: merges its profiles and keeps the code regions of
/// the functions it executed. File paths are made relative to `workspace`.
pub fn export_one(
    passed: &PassedProfile,
    tools: &LlvmTools,
    workspace: &Path,
    bins: &[PathBuf],
) -> Result<TestExport, SkipReason> {
    let failed = |message: String| SkipReason::ExportFailed { message };
    let merged = passed.dir.join("merged.profdata");
    let output = Command::new(&tools.profdata)
        .args(["merge", "-sparse", "-o"])
        .arg(&merged)
        .args(&passed.profraws)
        .output()
        .map_err(|e| failed(format!("run llvm-profdata: {e}")))?;
    if !output.status.success() {
        return Err(failed(format!(
            "llvm-profdata failed: {}",
            stderr_tail(&output.stderr)
        )));
    }
    let mut cov = Command::new(&tools.cov);
    cov.args(["export", "-format=text", "-skip-expansions"])
        .arg(format!("-ignore-filename-regex={IGNORE_FILENAME_REGEX}"))
        .arg(format!("-instr-profile={}", merged.display()))
        .arg(&passed.meta.binary);
    // One profile means only the test process ran; skip loading every binary.
    if passed.profraws.len() > 1 {
        for bin in bins {
            cov.arg("-object").arg(bin);
        }
    }
    let output = cov.output();
    // Best effort: the profile directory is removed after collect anyway.
    let _ = std::fs::remove_file(&merged);
    let output = output.map_err(|e| failed(format!("run llvm-cov: {e}")))?;
    if !output.status.success() {
        return Err(failed(format!(
            "llvm-cov failed: {}",
            stderr_tail(&output.stderr)
        )));
    }
    let export: CovExport = serde_json::from_slice(&output.stdout)
        .map_err(|e| failed(format!("parse llvm-cov export: {e}")))?;

    let strip = |path: &str| -> String {
        Path::new(path)
            .strip_prefix(workspace)
            .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
    };
    let functions = export
        .data
        .into_iter()
        .flat_map(|d| d.functions)
        .filter(|f| f.count > 0)
        .map(|f| CoveredFunction {
            filenames: f.filenames.iter().map(|name| strip(name)).collect(),
            regions: f
                .regions
                .iter()
                .filter(|r| r.get(7) == Some(&0))
                .map(|r| region(r))
                .collect(),
        })
        .collect();
    let manifest_dir = match strip(&passed.meta.manifest_dir) {
        dir if dir.is_empty() => ".".to_owned(),
        dir => dir,
    };
    Ok(TestExport {
        version: EXPORT_VERSION,
        test: ExportedTest {
            id: passed.meta.test.clone(),
            binary: passed.meta.binary.clone(),
            manifest_dir,
            processes: passed.profraws.len() as u32,
        },
        functions,
    })
}

fn region(r: &[u64]) -> Region {
    let n = |i: usize| r[i] as u32;
    Region {
        line_start: n(0),
        col_start: n(1),
        line_end: n(2),
        col_end: n(3),
        count: r[4],
        file: n(5),
    }
}

/// The last ~500 characters of a tool's stderr.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let start = text.char_indices().rev().nth(499).map_or(0, |(i, _)| i);
    text[start..].to_owned()
}

/// Exports every passed profile in parallel to `out/<profile dir name>.json.gz`
/// and returns the tests that failed to export.
pub fn export_all(
    passed: &[PassedProfile],
    out: &Path,
    tools: &LlvmTools,
    workspace: &Path,
    bins: &[PathBuf],
) -> Result<Vec<Skipped>> {
    let done = AtomicUsize::new(0);
    let total = passed.len();
    let results: Vec<Result<Option<Skipped>>> = passed
        .par_iter()
        .map(|p| {
            let result = match export_one(p, tools, workspace, bins) {
                Ok(export) => {
                    let name = p.dir.file_name().context("profile dir has no name")?;
                    export.write_gz(&out.join(format!("{}.json.gz", name.to_string_lossy())))?;
                    None
                }
                Err(reason) => Some(Skipped {
                    test: p.meta.test.clone(),
                    reason,
                }),
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(200) || n == total {
                eprintln!("dwd: exported {n}/{total}");
            }
            Ok(result)
        })
        .collect();
    results.into_iter().filter_map(Result::transpose).collect()
}
