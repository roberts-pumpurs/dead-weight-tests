//! The per-test coverage export: the contract between `dwd collect` and
//! `dwd analyze`.
//!
//! One gzipped JSON file per passing test. It keeps only the functions the test
//! executed and only their code regions (LLVM region kind 0), covered or not:
//! the report needs uncovered regions to draw nesting.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crate::test_id::TestId;

/// Bump when the export layout changes; `dwd analyze` rejects other versions.
pub const EXPORT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestExport {
    pub version: u32,
    pub test: ExportedTest,
    pub functions: Vec<CoveredFunction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedTest {
    pub id: TestId,
    /// The test binary nextest ran.
    pub binary: PathBuf,
    /// The package directory, relative to the workspace root (`.` for the root package).
    pub manifest_dir: String,
    /// Profiles the test wrote: 1 for the test process plus one per instrumented
    /// subprocess (workspace binaries the test spawned).
    pub processes: u32,
}

/// One executed function instance with its code regions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoveredFunction {
    /// Source files, relative to the workspace root when inside it, else absolute.
    pub filenames: Vec<String>,
    pub regions: Vec<Region>,
}

/// An LLVM code region. On disk: `[line_start, col_start, line_end, col_end, count, file]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RegionTuple", into = "RegionTuple")]
pub struct Region {
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
    /// Execution count; 0 means not covered by this test.
    pub count: u64,
    /// Index into the function's `filenames`.
    pub file: u32,
}

type RegionTuple = (u32, u32, u32, u32, u64, u32);

impl From<RegionTuple> for Region {
    fn from((line_start, col_start, line_end, col_end, count, file): RegionTuple) -> Self {
        Region {
            line_start,
            col_start,
            line_end,
            col_end,
            count,
            file,
        }
    }
}

impl From<Region> for RegionTuple {
    fn from(r: Region) -> Self {
        (
            r.line_start,
            r.col_start,
            r.line_end,
            r.col_end,
            r.count,
            r.file,
        )
    }
}

impl TestExport {
    /// Writes the export as gzipped JSON.
    pub fn write_gz(&self, path: &Path) -> Result<()> {
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut encoder = GzEncoder::new(BufWriter::new(file), Compression::default());
        serde_json::to_writer(&mut encoder, self)?;
        encoder.finish()?;
        Ok(())
    }

    /// Reads an export written by [`TestExport::write_gz`] (or plain JSON when the
    /// name does not end in `.gz`) and checks its version.
    pub fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let reader: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") {
            Box::new(GzDecoder::new(BufReader::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };
        let export: TestExport =
            serde_json::from_reader(reader).with_context(|| format!("parse {}", path.display()))?;
        if export.version != EXPORT_VERSION {
            bail!(
                "{}: export version {} (this dwd reads version {EXPORT_VERSION}); re-run dwd collect",
                path.display(),
                export.version
            );
        }
        Ok(export)
    }
}
