//! `dwd doctor`: environment checks for a workspace, each with a fix.
//!
//! `dwd collect` runs the [`tool_checks`] before it starts, so both commands
//! share one source of truth.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::artifacts::{ArtifactDir, workspace_root};
use crate::export::{self, LlvmTools};
use crate::verdict::Ledger;

/// The outcome of one check. Only [`Status::Fail`] fails `dwd doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

/// One check result: what was checked, what was found, and how to fix it.
#[derive(Clone, Debug)]
pub struct Check {
    pub status: Status,
    pub name: &'static str,
    pub detail: String,
    /// Set for warnings and failures.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            name,
            detail: detail.into(),
            fix: None,
        }
    }

    fn problem(
        status: Status,
        name: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            status,
            name,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match self.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        };
        write!(f, "{status:<4} {}: {}", self.name, self.detail)?;
        if let Some(fix) = &self.fix {
            write!(f, "\n     fix: {fix}")?;
        }
        Ok(())
    }
}

/// Every check for the workspace containing `start`. When the workspace does
/// not resolve, the checks that need it are reported as skipped.
pub fn run(start: &Path, dir: Option<PathBuf>) -> Vec<Check> {
    let workspace = match workspace_root(start) {
        Ok(root) => root,
        Err(err) => {
            return vec![
                Check::problem(
                    Status::Fail,
                    "workspace",
                    format!("{err:#}"),
                    "run dwd inside a Cargo workspace or pass -C <dir>",
                ),
                Check::problem(
                    Status::Fail,
                    "other checks",
                    "skipped: they need the workspace",
                    "fix the workspace check, then rerun dwd doctor",
                ),
            ];
        }
    };
    let mut checks = vec![Check::ok("workspace", workspace.display().to_string())];
    checks.extend(tool_checks(&workspace));
    checks.push(dev_shell(&workspace));
    checks.push(wrapper_scripts(&workspace));
    checks.push(artifacts(&ArtifactDir::resolve(dir, &workspace)));
    checks
}

/// The checks `dwd collect` cannot run without.
pub fn tool_checks(workspace: &Path) -> Vec<Check> {
    vec![
        cargo_tool(workspace, "nextest", "cargo-nextest"),
        cargo_tool(workspace, "llvm-cov", "cargo-llvm-cov"),
        llvm_tools(workspace),
        target_dir(workspace),
    ]
}

/// Fails with the first failing [`tool_checks`] entry, fix included.
pub fn require_tools(workspace: &Path) -> Result<()> {
    if let Some(check) = tool_checks(workspace)
        .into_iter()
        .find(|c| c.status == Status::Fail)
    {
        bail!(
            "{}: {}; fix: {}",
            check.name,
            check.detail,
            check.fix.unwrap_or_default()
        );
    }
    Ok(())
}

/// `cargo <sub> --version` for the cargo plugin `binary`.
fn cargo_tool(workspace: &Path, sub: &str, binary: &'static str) -> Check {
    let fix = format!("cargo install --locked {binary}");
    if !on_path(binary) {
        return Check::problem(Status::Fail, binary, "not on PATH", fix);
    }
    let output = Command::new("cargo")
        .args([sub, "--version"])
        .current_dir(workspace)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            Check::ok(binary, text.lines().next().unwrap_or_default().trim())
        }
        Ok(out) => Check::problem(
            Status::Fail,
            binary,
            format!(
                "`cargo {sub} --version` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            fix,
        ),
        Err(err) => Check::problem(Status::Fail, binary, format!("run cargo: {err}"), fix),
    }
}

/// `llvm-cov` and `llvm-profdata` in the sysroot the workspace selects.
fn llvm_tools(workspace: &Path) -> Check {
    const NAME: &str = "llvm-tools";
    match LlvmTools::locate(workspace) {
        Ok(tools) => Check::ok(
            NAME,
            tools
                .cov
                .parent()
                .unwrap_or(&tools.cov)
                .display()
                .to_string(),
        ),
        Err(err) => {
            let sysroot = export::sysroot(workspace).unwrap_or_default();
            let fix = if on_path("rustup") && !sysroot.starts_with("/nix/store") {
                format!(
                    "run `rustup component add llvm-tools-preview` inside {}, so a rust-toolchain.toml pin gets it",
                    workspace.display()
                )
            } else {
                "this toolchain does not come from rustup; add llvm-tools-preview to its definition (e.g. the rust-overlay or fenix toolchain in the dev shell)".to_owned()
            };
            Check::problem(Status::Fail, NAME, format!("{err:#}"), fix)
        }
    }
}

/// Fails when the workspace's target dir is a dangling symlink, which
/// `<target>/dwd` cannot be created under. Target managers such as mbx leave
/// one behind after they evict the directory the link points to.
fn target_dir(workspace: &Path) -> Check {
    const NAME: &str = "target dir";
    let target = match export::target_directory(workspace) {
        Ok(target) => target,
        Err(err) => {
            return Check::problem(
                Status::Fail,
                NAME,
                format!("{err:#}"),
                "make `cargo metadata` succeed in the workspace",
            );
        }
    };
    match dangling_link(&target) {
        None => Check::ok(NAME, target.display().to_string()),
        Some((link, missing)) => Check::problem(
            Status::Fail,
            NAME,
            format!(
                "{} is a symlink to {}, which does not exist",
                link.display(),
                missing.display()
            ),
            format!(
                "run `mkdir -p '{}'`, or set CARGO_TARGET_DIR to a real directory",
                missing.display()
            ),
        ),
    }
}

/// The symlink on `path` whose destination is missing, with that destination.
/// A path that does not exist yet is fine: cargo creates it.
fn dangling_link(path: &Path) -> Option<(PathBuf, PathBuf)> {
    // The deepest ancestor that exists without following a final symlink.
    // Every ancestor above it resolves, so only it can dangle.
    let link = path
        .ancestors()
        .find(|p| std::fs::symlink_metadata(p).is_ok())?;
    if std::fs::metadata(link).is_ok() {
        return None;
    }
    let dest = std::fs::read_link(link).ok()?;
    // A relative destination is relative to the link's directory.
    let dest = link
        .parent()
        .map_or_else(|| dest.clone(), |dir| dir.join(&dest));
    Some((link.to_owned(), dest))
}

/// How the workspace's dev shell is entered, by the file that defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevShell {
    /// `.envrc`: `direnv exec` loads it whether it uses Nix or not.
    Direnv,
    Flake,
    NixShell,
}

impl DevShell {
    /// The dev shell the workspace root defines; `.envrc` wins because it is
    /// what the developer's own shell loads.
    fn detect(workspace: &Path) -> Option<Self> {
        [
            (DevShell::Direnv, ".envrc"),
            (DevShell::Flake, "flake.nix"),
            (DevShell::NixShell, "shell.nix"),
        ]
        .into_iter()
        .find(|(_, file)| workspace.join(file).is_file())
        .map(|(shell, _)| shell)
    }

    fn file(self) -> &'static str {
        match self {
            DevShell::Direnv => ".envrc",
            DevShell::Flake => "flake.nix",
            DevShell::NixShell => "shell.nix",
        }
    }

    /// How to run one dwd command inside the shell.
    fn run_example(self) -> &'static str {
        match self {
            DevShell::Direnv => "direnv exec . dwd ...",
            DevShell::Flake => "nix develop -c dwd ...",
            DevShell::NixShell => "nix-shell --run 'dwd ...'",
        }
    }
}

/// Warns when the workspace has a dev shell and this process is outside it.
fn dev_shell(workspace: &Path) -> Check {
    const NAME: &str = "dev shell";
    let Some(shell) = DevShell::detect(workspace) else {
        return Check::ok(NAME, "none defined");
    };
    let inside = ["IN_NIX_SHELL", "DIRENV_DIR"]
        .into_iter()
        .any(|var| std::env::var_os(var).is_some());
    if inside {
        Check::ok(NAME, format!("inside ({})", shell.file()))
    } else {
        Check::problem(
            Status::Warn,
            NAME,
            format!(
                "{} defines a dev shell and dwd runs outside it",
                shell.file()
            ),
            format!("run every dwd command inside it: `{}`", shell.run_example()),
        )
    }
}

/// Warns when a nextest wrapper script lacks `target-runner = "within-wrapper"`.
fn wrapper_scripts(workspace: &Path) -> Check {
    const NAME: &str = "nextest wrapper scripts";
    let path = workspace.join(".config/nextest.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Check::ok(NAME, "no .config/nextest.toml");
    };
    match wrappers_without_runner(&text) {
        Ok(names) if names.is_empty() => Check::ok(NAME, "none without within-wrapper"),
        Ok(names) => Check::problem(
            Status::Warn,
            NAME,
            format!(
                "{}: wrapper {} lacks target-runner = \"within-wrapper\"; wrapped tests write no coverage",
                path.display(),
                names.join(", ")
            ),
            "see references/collect.md, \"Nextest wrapper scripts\"",
        ),
        Err(err) => Check::problem(
            Status::Warn,
            NAME,
            format!("{}: {err}", path.display()),
            "fix the nextest config; nextest cannot read it either",
        ),
    }
}

/// The part of a nextest config this check reads.
#[derive(Deserialize)]
struct NextestConfig {
    #[serde(default)]
    scripts: Scripts,
}

#[derive(Default, Deserialize)]
struct Scripts {
    #[serde(default)]
    wrapper: BTreeMap<String, WrapperScript>,
}

#[derive(Deserialize)]
struct WrapperScript {
    #[serde(rename = "target-runner")]
    target_runner: Option<String>,
}

/// The wrapper scripts in a nextest config whose target runner is not `within-wrapper`.
fn wrappers_without_runner(text: &str) -> Result<Vec<String>, toml::de::Error> {
    let config: NextestConfig = toml::from_str(text)?;
    Ok(config
        .scripts
        .wrapper
        .into_iter()
        .filter(|(_, w)| w.target_runner.as_deref() != Some("within-wrapper"))
        .map(|(name, _)| name)
        .collect())
}

/// The artifact dir and how many verdicts its ledger holds.
fn artifacts(dirs: &ArtifactDir) -> Check {
    const NAME: &str = "artifacts";
    let root = dirs.root().display();
    match Ledger::new(dirs.verdicts()).read() {
        Ok(contents) => Check::ok(
            NAME,
            format!("{root} ({} verdicts)", contents.verdicts.len()),
        ),
        Err(err) => Check::problem(
            Status::Warn,
            NAME,
            format!("{root}: {err:#}"),
            "inspect or move the verdict ledger",
        ),
    }
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::{dangling_link, wrappers_without_runner};

    #[test]
    fn only_a_symlink_to_a_missing_dir_dangles() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        // Not built yet: cargo creates the dir.
        assert_eq!(dangling_link(&root.join("target/dwd")), None);

        let link = root.join("target");
        symlink("store/gone", &link).unwrap();
        let missing = root.join("store/gone");
        assert_eq!(
            dangling_link(&link.join("dwd")),
            Some((link.clone(), missing.clone()))
        );

        std::fs::create_dir_all(&missing).unwrap();
        assert_eq!(dangling_link(&link.join("dwd")), None);
    }

    #[test]
    fn every_wrapper_needs_its_own_within_wrapper() {
        let config = r#"
            # target-runner = "within-wrapper" in a comment does not count
            [scripts.wrapper.sudo]
            command = "sudo"
            [scripts.wrapper.ok]
            command = "x"
            target-runner = "within-wrapper"
            [scripts.setup.db]
            command = "y"
        "#;
        assert_eq!(wrappers_without_runner(config).unwrap(), ["sudo"]);
        assert!(
            wrappers_without_runner("[profile.default]\n")
                .unwrap()
                .is_empty()
        );
    }
}
