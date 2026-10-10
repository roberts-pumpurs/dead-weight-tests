//! `dwd collect`: per-test region coverage for a Cargo workspace.
//!
//! Enables cargo-llvm-cov's instrumentation environment, runs nextest with
//! `dwd __runner` as the target runner, triages the profile directories, and
//! exports one [`crate::export_format::TestExport`] per passing test. On exit,
//! even after a failure or Ctrl-C, it removes the instrumented workspace
//! artifacts so the next normal build rebuilds them.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use crate::artifacts::ArtifactDir;
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

/// Set by Ctrl-C or SIGTERM. Children share our process group, so Ctrl-C
/// reaches them too; dwd survives to clean up.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn collect(options: &CollectOptions, dirs: &ArtifactDir) -> Result<CollectSummary> {
    for tool in ["cargo-nextest", "cargo-llvm-cov"] {
        if !on_path(tool) {
            bail!("missing {tool} on PATH");
        }
    }
    let workspace = options.workspace.as_path();
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
    // Export needs these after the tests; fail before a long run when they are missing.
    let tools = LlvmTools::locate(workspace)?;
    let coverage_env = coverage_env(workspace)?;
    // cargo-llvm-cov's RUSTC_WRAPPER adds -C instrument-coverage outside cargo's
    // fingerprint, so artifacts of a normal build would be reused uninstrumented.
    // The clean must run inside the coverage env: outside it, it cleans another dir.
    llvm_cov_clean(workspace, &coverage_env)?;
    let cleanup = Cleanup {
        workspace,
        env: &coverage_env,
        done: false,
    };
    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .context("install the Ctrl-C handler")?;

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
    check_interrupted()?;
    let triage = profile::triage(&dirs.profiles(), listed.as_ref())?;
    // Needs the instrumented binaries for -object, so it runs before cleanup.
    let bins = export::workspace_bins(workspace, &coverage_env)?;
    let mut skipped = triage.skipped;
    let failed = export::export_all(&triage.passed, &dirs.exports(), &tools, workspace, &bins)?;
    // An interrupt during export turns into per-test failures; report the interrupt instead.
    check_interrupted()?;
    let exported = triage.passed.len() - failed.len();
    skipped.extend(failed);
    write_skipped(&dirs.skipped(), &skipped)?;
    remove_dir(&dirs.profiles())?;
    remove_dir(&dirs.other())?;
    cleanup.finish()?;
    Ok(CollectSummary {
        exported,
        skipped: skipped.len(),
    })
}

fn check_interrupted() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!("interrupted");
    }
    Ok(())
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

/// Runs a child to completion; fails when Ctrl-C arrived meanwhile.
fn run_status(command: &mut Command, workspace: &Path, env: &Env) -> Result<ExitStatus> {
    let status = command
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .with_context(|| format!("run {command:?}"))?;
    check_interrupted()?;
    Ok(status)
}

/// cargo-llvm-cov's environment for instrumenting workspace crates.
fn coverage_env(workspace: &Path) -> Result<Vec<(String, String)>> {
    let show_env = |flag: &str| {
        Command::new("cargo")
            .args(["llvm-cov", "show-env", flag])
            .current_dir(workspace)
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
    parse_show_env(&String::from_utf8(output.stdout)?)
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

fn llvm_cov_clean(workspace: &Path, env: &Env) -> Result<()> {
    let status = Command::new("cargo")
        .args(["llvm-cov", "clean", "--workspace"])
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .context("run cargo llvm-cov clean")?;
    if !status.success() {
        bail!("cargo llvm-cov clean --workspace failed with {status}");
    }
    Ok(())
}

/// Removes the instrumented workspace artifacts when collect ends, however it
/// ends, so a normal build never reuses them. [`Cleanup::finish`] reports a
/// failed clean as an error; on early returns, `Drop` cleans and warns.
struct Cleanup<'a> {
    workspace: &'a Path,
    env: &'a Env,
    done: bool,
}

impl Cleanup<'_> {
    fn finish(mut self) -> Result<()> {
        self.done = true;
        self.clean()
    }

    fn clean(&self) -> Result<()> {
        eprintln!("dwd: removing coverage-instrumented workspace artifacts");
        llvm_cov_clean(self.workspace, self.env).context(
            "run it under `eval \"$(cargo llvm-cov show-env --sh)\"` before a normal build",
        )
    }
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if !self.done
            && let Err(error) = self.clean()
        {
            eprintln!("dwd: {error:#}");
        }
    }
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
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
