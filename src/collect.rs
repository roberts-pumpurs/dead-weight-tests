//! `dwd collect`: per-test region coverage for a Cargo workspace.
//!
//! Enables cargo-llvm-cov's instrumentation environment, runs nextest with
//! `dwd __runner` as the target runner, triages the profile directories, and
//! exports one [`crate::export_format::TestExport`] per passing test.
//! Instrumented builds live in `<target>/dwd`, apart from normal builds, so
//! both stay incremental.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::artifacts::ArtifactDir;
use crate::doctor;
use crate::export::{self, Env, LlvmTools};
use crate::profile;
use crate::runner::PROFILE_ROOT_ENV;
use crate::skipped::write_skipped;

#[derive(Clone, Debug)]
pub struct CollectOptions {
    /// Workspace root; commands run here.
    pub workspace: PathBuf,
    /// Command line that runs nextest, e.g. `cargo nextest run`. Split with shell rules.
    pub nextest: String,
    /// Shell command run after coverage is enabled and before the tests.
    pub before: Option<String>,
    /// Arguments appended to the nextest command.
    pub nextest_args: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectSummary {
    pub exported: usize,
    pub skipped: usize,
}

/// Wrapper variables from `cargo llvm-cov show-env` that change codegen
/// outside cargo's fingerprint.
const STAMPED_VARS: [&str; 3] = [
    "__CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS",
    "__CARGO_LLVM_COV_RUSTC_WRAPPER_CRATE_NAMES",
    "RUSTC_WRAPPER",
];

pub fn collect(options: &CollectOptions, dirs: &ArtifactDir) -> Result<CollectSummary> {
    let workspace = options.workspace.as_path();
    doctor::require_tools(workspace)?;
    for dir in [dirs.profiles(), dirs.other(), dirs.exports()] {
        remove_dir(&dir)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    }
    remove_file(&dirs.skipped())?;

    let exe = std::env::current_exe().context("locate the dwd executable")?;
    let exe = exe.to_str().context("dwd executable path is not UTF-8")?;
    if exe.contains(char::is_whitespace) {
        bail!(
            "dwd's path {exe:?} contains whitespace; cargo splits runner commands on whitespace, so install dwd elsewhere"
        );
    }
    let tools = LlvmTools::locate(workspace)?;
    let target_dir = export::target_directory(workspace)?.join("dwd");
    let coverage_env = coverage_env(workspace, &target_dir)?;
    clean_if_flags_changed(workspace, &target_dir, &coverage_env)?;

    let mut env = coverage_env.clone();
    // Processes outside a test (build scripts, setup scripts, test listing) write here.
    env.push((
        "LLVM_PROFILE_FILE".into(),
        path_string(&dirs.other().join("%p-%m.profraw")),
    ));
    env.push((PROFILE_ROOT_ENV.into(), path_string(&dirs.profiles())));
    let runner_var = format!(
        "CARGO_TARGET_{}_RUNNER",
        export::host_triple(workspace)?
            .to_uppercase()
            .replace('-', "_")
    );
    env.push((runner_var, format!("{exe} __runner")));

    if let Some(before) = &options.before {
        // bash, not sh: existing hooks may use `source` or `[[`.
        let status = run_status(Command::new("bash").args(["-c", before]), workspace, &env)?;
        if !status.success() {
            bail!("--before command failed with {status}");
        }
    }

    let words = shell_words::split(&options.nextest).context("split --nextest")?;
    let Some((program, rest)) = words.split_first() else {
        bail!("--nextest is empty")
    };
    let status = run_status(
        Command::new(program)
            .args(rest)
            .arg("--no-fail-fast")
            .args(&options.nextest_args),
        workspace,
        &env,
    )?;
    // 100 = some tests failed; triage skips them. Anything else is a broken run.
    if !matches!(status.code(), Some(0 | 100)) {
        bail!("{} failed with {status}", options.nextest);
    }

    let listed = list_tests(&words, options, workspace, &env);
    let triage = profile::triage(&dirs.profiles(), listed.as_ref())?;
    // Needs the instrumented binaries for -object.
    let bins = export::workspace_bins(workspace, &coverage_env)?;
    let mut skipped = triage.skipped;
    let failed = export::export_all(&triage.passed, &dirs.exports(), &tools, workspace, &bins)?;
    let exported = triage.passed.len() - failed.len();
    skipped.extend(failed);
    write_skipped(&dirs.skipped(), &skipped)?;
    remove_dir(&dirs.profiles())?;
    remove_dir(&dirs.other())?;
    Ok(CollectSummary {
        exported,
        skipped: skipped.len(),
    })
}

/// The tests the nextest command selects, via `nextest list`. Best effort:
/// `None` (with a warning) when the command has no `nextest run` or listing fails.
fn list_tests(
    words: &[String],
    options: &CollectOptions,
    workspace: &Path,
    env: &Env,
) -> Option<std::collections::HashSet<crate::test_id::TestId>> {
    const OMITS: &str = "skipped.json omits tests that never reached the runner";
    let Some(run) = words
        .windows(2)
        .position(|w| w[0] == "nextest" && w[1] == "run")
    else {
        eprintln!("dwd: warning: no 'nextest run' in --nextest command; {OMITS}");
        return None;
    };
    let mut list = words.to_vec();
    list[run + 1] = "list".into();
    let output = Command::new(&list[0])
        .args(&list[1..])
        .args(["--message-format", "json"])
        .args(&options.nextest_args)
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .output();
    match output {
        Ok(output) if output.status.success() => {
            profile::parse_listed(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last = stderr.trim().lines().last().unwrap_or_default().to_owned();
            eprintln!("dwd: warning: test listing failed ({last}); {OMITS}");
            None
        }
        Err(error) => {
            eprintln!("dwd: warning: test listing failed ({error}); {OMITS}");
            None
        }
    }
}

fn run_status(command: &mut Command, workspace: &Path, env: &Env) -> Result<ExitStatus> {
    command
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .with_context(|| format!("run {command:?}"))
}

/// cargo-llvm-cov's environment for instrumenting workspace crates into
/// `target_dir`, including `CARGO_TARGET_DIR` itself.
fn coverage_env(workspace: &Path, target_dir: &Path) -> Result<Vec<(String, String)>> {
    // Both dirs: `build.build-dir` (or CARGO_BUILD_BUILD_DIR) can move the
    // fingerprints and intermediate artifacts out of the target dir, and they
    // must not be shared with normal builds either.
    let isolate = ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"].map(|key| (key, target_dir));
    let show_env = |flag: &str| {
        Command::new("cargo")
            .args(["llvm-cov", "show-env", flag])
            .current_dir(workspace)
            .envs(isolate)
            .output()
    };
    let output = match show_env("--sh")? {
        output if output.status.success() => output,
        _ => show_env("--export-prefix")?,
    };
    if !output.status.success() {
        bail!(
            "cargo llvm-cov show-env failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut env = parse_show_env(&String::from_utf8(output.stdout)?)?;
    env.extend(isolate.map(|(key, dir)| (key.into(), path_string(dir))));
    Ok(env)
}

/// Parses `export KEY=VALUE` lines, unquoting values with shell rules.
fn parse_show_env(text: &str) -> Result<Vec<(String, String)>> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("export "))
        .map(|assignment| {
            let (key, value) = assignment
                .split_once('=')
                .with_context(|| format!("bad show-env line {assignment:?}"))?;
            let value = shell_words::split(value)
                .with_context(|| format!("unquote {value:?}"))?
                .concat();
            Ok((key.to_owned(), value))
        })
        .collect()
}

/// The stamp content for `env`: one `KEY=VALUE` line per [`STAMPED_VARS`] entry.
fn stamp(env: &Env) -> String {
    STAMPED_VARS
        .iter()
        .map(|key| {
            let value = env.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v);
            format!("{key}={value}\n")
        })
        .collect()
}

/// Cleans the workspace crates in `target_dir` when the wrapper flags differ
/// from the last run's, since cargo's fingerprint misses them.
fn clean_if_flags_changed(workspace: &Path, target_dir: &Path, env: &Env) -> Result<()> {
    let path = target_dir.join("dwd-flags");
    let stamp = stamp(env);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }
    // Inside the coverage env, so it cleans target_dir.
    let status = Command::new("cargo")
        .args(["llvm-cov", "clean", "--workspace"])
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .context("run cargo llvm-cov clean")?;
    if !status.success() {
        bail!("cargo llvm-cov clean --workspace failed with {status}");
    }
    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("create {}", target_dir.display()))?;
    std::fs::write(&path, stamp).with_context(|| format!("write {}", path.display()))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("remove {}", dir.display()))
        }
        _ => Ok(()),
    }
}

fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_env_values_are_unquoted() {
        let text = "export RUSTFLAGS='-C instrument-coverage --cfg=x'\nexport CARGO_LLVM_COV=1\nexport A=\"it's\"\nnoise\n";
        let env = parse_show_env(text).unwrap();
        assert_eq!(
            env,
            [
                ("RUSTFLAGS".into(), "-C instrument-coverage --cfg=x".into()),
                ("CARGO_LLVM_COV".into(), "1".into()),
                ("A".into(), "it's".into()),
            ]
        );
    }
}
