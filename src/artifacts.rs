//! Where `dwd` keeps its files.
//!
//! Default: `$XDG_STATE_HOME/dwd/<workspace name>-<hash of the workspace path>/`,
//! with `~/.local/state` when `XDG_STATE_HOME` is unset. A state dir, not the
//! temp dir: the verdict ledger is reviewed work and must survive reboots and
//! temp cleaners. `--dir` or `DWD_DIR` overrides it.
//!
//! ```text
//! <dir>/profiles/        per-test LLVM profiles (transient, removed after collect)
//! <dir>/other/           profiles of build scripts and test listing (transient)
//! <dir>/exports/         one <hash>.json.gz TestExport per passing test
//! <dir>/skipped.json     tests that could not be exported
//! <dir>/analysis.json    the analysis; `dwd report` re-renders from it
//! <dir>/candidates.json  every candidate, for agents
//! <dir>/verdicts.jsonl   the verdict ledger
//! <dir>/report.md        markdown report
//! <dir>/report.html      interactive report
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct ArtifactDir {
    root: PathBuf,
}

impl ArtifactDir {
    /// Uses `dir` when given, else the default location for `workspace`.
    pub fn resolve(dir: Option<PathBuf>, workspace: &Path) -> Self {
        let root = dir.unwrap_or_else(|| Self::default_root(workspace));
        Self { root }
    }

    /// An artifact directory at an explicit path (tests, `--dir`).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn default_root(workspace: &Path) -> PathBuf {
        let canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
        let hash: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        let name = canonical
            .file_name()
            .map_or_else(|| "workspace".into(), |n| n.to_string_lossy().into_owned());
        state_home().join("dwd").join(format!("{name}-{hash}"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }
    pub fn other(&self) -> PathBuf {
        self.root.join("other")
    }
    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }
    pub fn skipped(&self) -> PathBuf {
        self.root.join("skipped.json")
    }
    pub fn analysis(&self) -> PathBuf {
        self.root.join("analysis.json")
    }
    pub fn candidates(&self) -> PathBuf {
        self.root.join("candidates.json")
    }
    pub fn verdicts(&self) -> PathBuf {
        self.root.join("verdicts.jsonl")
    }
    pub fn report_md(&self) -> PathBuf {
        self.root.join("report.md")
    }
    pub fn report_html(&self) -> PathBuf {
        self.root.join("report.html")
    }
}

/// `$XDG_STATE_HOME` when it is an absolute path (the XDG spec ignores relative
/// ones), else `~/.local/state`. Without `HOME` there is no persistent place,
/// so it falls back to the temp dir.
fn state_home() -> PathBuf {
    let var = |key| std::env::var_os(key).map(PathBuf::from);
    if let Some(dir) = var("XDG_STATE_HOME").filter(|dir| dir.is_absolute()) {
        return dir;
    }
    var("HOME").map_or_else(std::env::temp_dir, |home| home.join(".local/state"))
}

/// The root of the Cargo workspace that contains `dir`.
pub fn workspace_root(dir: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .current_dir(dir)
        .output()
        .context("run cargo locate-project")?;
    if !output.status.success() {
        bail!(
            "{} is not inside a Cargo workspace: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let manifest = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    Ok(manifest
        .parent()
        .context("manifest path has no parent")?
        .to_path_buf())
}
