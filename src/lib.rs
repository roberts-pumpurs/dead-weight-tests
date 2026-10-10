//! dead-weight-tests: find Rust tests whose coverage another test already has.
//!
//! Pipeline: [`collect`] → per-test [`export_format::TestExport`] files →
//! [`analysis::analyze`] → [`report`] (markdown, candidates JSON, HTML) and the
//! [`verdict`] ledger for reviewed decisions. The `dwd` binary wires them up.

pub mod analysis;
pub mod artifacts;
pub mod collect;
pub mod export;
pub mod export_format;
pub mod ids;
pub mod profile;
pub mod report;
pub mod runner;
pub mod skipped;
pub mod test_id;
pub mod verdict;
