//! `dwd`: the dead-weight-tests command line.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use dead_weight_tests::analysis::{self, Analysis, CoverageLoss, DEFAULT_NEAR};
use dead_weight_tests::artifacts::{ArtifactDir, workspace_root};
use dead_weight_tests::collect::{self, CollectOptions};
use dead_weight_tests::doctor;
use dead_weight_tests::report::{self, MarkdownOptions, Review};
use dead_weight_tests::runner;
use dead_weight_tests::skipped::read_skipped;
use dead_weight_tests::test_id::TestId;
use dead_weight_tests::verdict::{Ledger, Pair, RecordSummary};

/// Hidden subcommand nextest calls as the target runner.
const RUNNER_SUBCOMMAND: &str = "__runner";

/// Find tests whose coverage another test already has.
#[derive(Parser)]
#[command(name = "dwd", version, about)]
struct Cli {
    /// Artifact directory. Default: $XDG_STATE_HOME/dwd/<workspace>-<hash>
    /// (without XDG_STATE_HOME: ~/.local/state/dwd/<workspace>-<hash>).
    #[arg(long, global = true, env = "DWD_DIR")]
    dir: Option<PathBuf>,

    /// Any directory inside the Cargo workspace. Default: the current directory.
    #[arg(short = 'C', long, global = true)]
    workspace: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// Every subcommand. Only `doctor` runs without a resolved workspace: it
/// reports a missing one as a failed check.
#[derive(Subcommand)]
enum Command {
    /// Check the tools and setup `dwd collect` needs; print one line per check with its fix.
    Doctor,
    #[command(flatten)]
    Workspace(WorkspaceCommand),
}

/// Subcommands that need the Cargo workspace and its artifact directory.
#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Run the tests with coverage and export one coverage set per passing test.
    Collect {
        /// Command that runs nextest; it must pass its arguments to `cargo nextest run`.
        #[arg(long, default_value = "cargo nextest run")]
        nextest: String,
        /// Shell command run after coverage is enabled and before the tests,
        /// e.g. building binaries that tests spawn from another package.
        #[arg(long)]
        before: Option<String>,
        /// Arguments for nextest, after `--`, e.g. `-- --workspace -E 'package(foo)'`.
        #[arg(last = true)]
        nextest_args: Vec<String>,
    },
    /// Find candidates in the collected coverage and write every report.
    Analyze {
        /// Jaccard threshold for near duplicates.
        #[arg(long, default_value_t = DEFAULT_NEAR)]
        near: f64,
        #[command(flatten)]
        markdown: MarkdownArgs,
    },
    /// Re-render the reports from the stored analysis and the current verdicts.
    Report {
        #[command(flatten)]
        markdown: MarkdownArgs,
    },
    /// Validate verdict lines (JSON lines or a JSON array) and append them to the ledger.
    Verdict {
        /// Input file. Default: stdin.
        input: Option<PathBuf>,
        /// Inline JSON array of `{"test": …, "other": …}` rows (extra fields
        /// ignored) that must all have a verdict afterwards.
        #[arg(long)]
        expect: Option<String>,
    },
    /// Print candidates without a current verdict (none, or a changed test body), one per line, in review order.
    Pending {
        /// Most candidates printed.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print the path of an artifact.
    Path {
        #[arg(value_enum, default_value_t = Artifact::Dir)]
        artifact: Artifact,
    },
}

#[derive(clap::Args)]
struct MarkdownArgs {
    /// Candidates listed in report.md.
    #[arg(long, default_value_t = 100)]
    limit: usize,
    /// Candidates drawn in report.md's Mermaid graph.
    #[arg(long, default_value_t = 40)]
    graph_limit: usize,
}

impl From<&MarkdownArgs> for MarkdownOptions {
    fn from(args: &MarkdownArgs) -> Self {
        MarkdownOptions {
            limit: args.limit,
            graph_limit: args.graph_limit,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Artifact {
    Dir,
    Report,
    Markdown,
    Candidates,
    Verdicts,
    Analysis,
}

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    // The runner path skips clap and workspace discovery: it runs once per test.
    if args.get(1).is_some_and(|a| a == RUNNER_SUBCOMMAND) {
        let mut rest = args.split_off(2).into_iter();
        let Some(binary) = rest.next() else {
            eprintln!("dwd {RUNNER_SUBCOMMAND}: missing test binary");
            return ExitCode::from(2);
        };
        return exit_with(runner::run(binary, rest.collect()));
    }
    exit_with(run(Cli::parse()))
}

fn exit_with(result: Result<i32>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("dwd: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let command = match cli.command {
        Command::Workspace(command) => command,
        Command::Doctor => {
            let checks = doctor::run(
                &cwd.join(cli.workspace.unwrap_or_default()),
                cli.dir.map(|dir| cwd.join(dir)),
            );
            for check in &checks {
                println!("{check}");
            }
            let failed = checks.iter().any(|c| c.status == doctor::Status::Fail);
            return Ok(i32::from(failed));
        }
    };
    let workspace = workspace_root(cli.workspace.as_deref().unwrap_or(&cwd))?;
    // Children run in other directories (workspace root, package dirs), so a
    // relative --dir must resolve against the caller's directory once, here.
    let dirs = ArtifactDir::resolve(cli.dir.map(|dir| cwd.join(dir)), &workspace);

    match command {
        WorkspaceCommand::Collect {
            nextest,
            before,
            nextest_args,
        } => {
            let options = CollectOptions {
                workspace,
                nextest,
                before,
                nextest_args,
            };
            let summary = collect::collect(&options, &dirs)?;
            eprintln!(
                "dwd: {} tests exported, {} skipped, in {}",
                summary.exported,
                summary.skipped,
                dirs.root().display()
            );
            println!("{}", dirs.root().display());
            Ok(0)
        }
        WorkspaceCommand::Analyze { near, markdown } => {
            if !(0.0..=1.0).contains(&near) {
                bail!("--near must be between 0 and 1, got {near}");
            }
            let exports = dirs.exports();
            if !exports.is_dir() {
                bail!(
                    "no coverage in {}; run dwd collect first",
                    exports.display()
                );
            }
            let analysis =
                analysis::analyze(&exports, read_skipped(&dirs.skipped())?, &workspace, near)?;
            analysis.save(&dirs.analysis())?;
            render(&analysis, &dirs, (&markdown).into())?;
            Ok(0)
        }
        WorkspaceCommand::Report { markdown } => {
            let analysis = load_analysis(&dirs)?;
            render(&analysis, &dirs, (&markdown).into())?;
            Ok(0)
        }
        WorkspaceCommand::Verdict { input, expect } => {
            let analysis = load_analysis(&dirs)?;
            let text = match input {
                Some(path) => std::fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?,
                None => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
            };
            let expect = expect.map(|json| read_expect(&json)).transpose()?;
            let ledger = Ledger::new(dirs.verdicts());
            let summary = ledger.record(&text, &analysis.pairs(), expect.as_deref())?;
            let contents = ledger.read()?;
            let review = Review::of(&analysis, &contents);
            let output = VerdictOutput {
                summary: &summary,
                pending: review.pending,
                coverage_loss: review.loss,
            };
            println!("{}", serde_json::to_string(&output)?);
            report::write_all(&analysis, &contents, &dirs, MarkdownOptions::default())?;
            Ok(summary.exit_code())
        }
        WorkspaceCommand::Pending { limit } => {
            let analysis = load_analysis(&dirs)?;
            let contents = Ledger::new(dirs.verdicts()).read()?;
            let rows = report::pending_rows(&analysis, &contents);
            if rows.is_empty() {
                eprintln!(
                    "dwd: all {} candidates have current verdicts",
                    analysis.candidates.len()
                );
                return Ok(0);
            }
            let same_file = rows.iter().filter(|r| r.row.same_file).count();
            let stale = rows.iter().filter(|r| r.previous.is_some()).count();
            let shown = rows.len().min(limit);
            eprintln!(
                "dwd: {} of {} candidates pending ({same_file} same-file, {stale} stale); showing {shown}",
                rows.len(),
                analysis.candidates.len()
            );
            for row in rows.iter().take(limit) {
                println!("{}", serde_json::to_string(row)?);
            }
            Ok(0)
        }
        WorkspaceCommand::Path { artifact } => {
            let path = match artifact {
                Artifact::Dir => dirs.root().to_path_buf(),
                Artifact::Report => dirs.report_html(),
                Artifact::Markdown => dirs.report_md(),
                Artifact::Candidates => dirs.candidates(),
                Artifact::Verdicts => dirs.verdicts(),
                Artifact::Analysis => dirs.analysis(),
            };
            println!("{}", path.display());
            Ok(0)
        }
    }
}

fn load_analysis(dirs: &ArtifactDir) -> Result<Analysis> {
    let path = dirs.analysis();
    if !path.is_file() {
        bail!("no analysis at {}; run dwd analyze first", path.display());
    }
    Analysis::load(&path)
}

fn render(analysis: &Analysis, dirs: &ArtifactDir, markdown: MarkdownOptions) -> Result<()> {
    let ledger = Ledger::new(dirs.verdicts());
    let contents = ledger.read()?;
    for row in &contents.invalid {
        eprintln!(
            "dwd: {}:{}: ignored verdict line ({})",
            ledger.path().display(),
            row.line,
            row.error
        );
    }
    report::write_all(analysis, &contents, dirs, markdown)?;
    eprintln!(
        "dwd: wrote {}, {}, {}",
        dirs.report_md().display(),
        dirs.candidates().display(),
        dirs.report_html().display()
    );
    print!(
        "{}",
        report::summary(analysis, &contents, &dirs.report_html())
    );
    Ok(())
}

/// What `dwd verdict` prints: the record summary plus review status.
#[derive(Serialize)]
struct VerdictOutput<'a> {
    #[serde(flatten)]
    summary: &'a RecordSummary,
    /// Candidates with no verdict or a stale one.
    pending: usize,
    /// Coverage the cut and merge verdicts lose. Never changes the exit code.
    coverage_loss: CoverageLoss,
}

/// An `--expect` row: any candidate row; only the pair matters.
#[derive(Deserialize)]
struct ExpectRow {
    test: TestId,
    other: TestId,
}

/// Parses the inline `--expect` JSON array.
fn read_expect(json: &str) -> Result<Vec<Pair>> {
    let rows: Vec<ExpectRow> = serde_json::from_str(json).context("parse --expect")?;
    Ok(rows.into_iter().map(|r| (r.test, r.other)).collect())
}
