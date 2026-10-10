//! `dwd __runner`: the nextest target runner `dwd collect` installs.
//!
//! Gives each test process its own profile directory so coverage is attributed
//! to one test. Subprocesses the test spawns inherit `LLVM_PROFILE_FILE`, so
//! their coverage lands in the same directory. Outside a test (nextest's list
//! phase) it runs the binary unchanged.

use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::profile::{EXIT_FILE, META_FILE, META_VERSION, ProfileMeta};
use crate::test_id::TestId;

/// Environment variable naming the profile root; set by `dwd collect`.
pub const PROFILE_ROOT_ENV: &str = "DWD_PROFILE_ROOT";

/// Runs `binary` with `args` and returns the exit code to exit with.
pub fn run(binary: OsString, args: Vec<OsString>) -> Result<i32> {
    let (Some(name), Some(root)) = (
        std::env::var_os("NEXTEST_TEST_NAME"),
        std::env::var_os(PROFILE_ROOT_ENV),
    ) else {
        let error = Command::new(&binary).args(&args).exec();
        return Err(error).with_context(|| format!("exec {}", binary.to_string_lossy()));
    };
    let binary_id = std::env::var("NEXTEST_BINARY_ID").context("NEXTEST_BINARY_ID is not set")?;
    let test = TestId::new(binary_id, name.to_string_lossy());
    let dir = profile_dir(Path::new(&root), &test);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let meta = ProfileMeta {
        version: META_VERSION,
        test,
        binary: PathBuf::from(&binary),
        manifest_dir: std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default(),
    };
    std::fs::write(dir.join(META_FILE), serde_json::to_string(&meta)?)?;

    let status = Command::new(&binary)
        .args(&args)
        .env("LLVM_PROFILE_FILE", dir.join("%p-%m.profraw"))
        .status()
        .with_context(|| format!("run {}", binary.to_string_lossy()))?;
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    // A missing `exit` file means the runner itself was killed.
    std::fs::write(dir.join(EXIT_FILE), format!("{code}\n"))?;
    Ok(code)
}

/// `<root>/<first 32 hex digits of sha256("<binary_id> <name>")>`.
fn profile_dir(root: &Path, test: &TestId) -> PathBuf {
    let digest = Sha256::digest(test.to_string().as_bytes());
    let hash: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    root.join(hash)
}
