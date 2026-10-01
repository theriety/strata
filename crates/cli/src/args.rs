//! Clap argument types for the `strata` subcommands.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use strata_engine::Mode;

/// The `strata` read-only module-decomposition tool.
#[derive(Debug, Parser)]
#[command(name = "strata", version, about, long_about = None)]
pub(super) struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub(super) command: Command,
}

/// The five `strata` subcommands.
#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Run the full decomposition pipeline and emit violations plus candidates.
    Analyze(AnalyzeCli),
    /// Print the file tree of a candidate or the current structure.
    Tree(TreeCli),
    /// Compare two structures as a narrated move list.
    Diff(DiffCli),
    /// Report structural violations of the current codebase (the CI gate).
    Violations(ViolationsCli),
    /// Render a saved analysis as a Markdown report.
    Report(ReportCli),
}

/// The output-format choice shared by `analyze`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum FormatChoice {
    /// The human-readable analysis summary.
    Summary,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

/// The parameter-profile selector exposed through the compatibility `--mode` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum ModeChoice {
    /// Stay close to the current layout.
    Anchored,
    /// Propose an unbiased ideal.
    Greenfield,
    /// Produce both results.
    Both,
}

impl From<ModeChoice> for Mode {
    fn from(choice: ModeChoice) -> Self {
        match choice {
            ModeChoice::Anchored => Mode::Anchored,
            ModeChoice::Greenfield => Mode::Greenfield,
            ModeChoice::Both => Mode::Both,
        }
    }
}

/// `strata analyze` flags.
#[derive(Debug, clap::Args)]
pub(super) struct AnalyzeCli {
    /// Include numerical evidence, score components, and effective parameters.
    #[arg(long)]
    pub(super) verbose: bool,
    /// Repository root to analyze.
    #[arg(long, default_value = ".")]
    pub(super) root: PathBuf,
    /// Configuration file (defaults to `<root>/strata.toml`; built-in defaults
    /// apply when absent).
    #[arg(long)]
    pub(super) config: Option<PathBuf>,
    /// Parameter profile(s) to execute.
    #[arg(long)]
    pub(super) mode: Option<ModeChoice>,
    /// Candidates per selected parameter profile.
    #[arg(short = 'k', long)]
    pub(super) candidates: Option<u32>,
    /// Where to write the result (stdout when absent).
    #[arg(long)]
    pub(super) output: Option<PathBuf>,
    /// Output format (defaults to summary, including when piped or redirected).
    #[arg(long)]
    pub(super) format: Option<FormatChoice>,
    /// Deterministic seed.
    #[arg(long)]
    pub(super) seed: Option<u64>,
    /// Parallelism for parsing and shattering.
    #[arg(long)]
    pub(super) jobs: Option<u32>,
    /// Let relocations cross package boundaries in every profile, overriding
    /// each profile's `allow-cross-package-moves`.
    #[arg(long)]
    pub(super) allow_cross_package_moves: bool,
}

/// `strata tree` flags.
#[derive(Debug, clap::Args)]
pub(super) struct TreeCli {
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    pub(super) input: PathBuf,
    /// Mode to draw a candidate from.
    #[arg(long)]
    pub(super) mode: Option<String>,
    /// 1-based candidate index; omit to list available candidates.
    #[arg(long)]
    pub(super) candidate: Option<usize>,
    /// Print the current (as-is) structure instead of a candidate.
    #[arg(long)]
    pub(super) current: bool,
    /// List each file's symbols with derived visibility.
    #[arg(long)]
    pub(super) symbols: bool,
    /// Truncate the tree at container depth n.
    #[arg(long)]
    pub(super) depth: Option<u32>,
}

/// `strata diff` flags.
#[derive(Debug, clap::Args)]
pub(super) struct DiffCli {
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    pub(super) input: PathBuf,
    /// The first structure reference (`current` or `mode/index`).
    pub(super) left: String,
    /// The second structure reference (`current` or `mode/index`).
    pub(super) right: String,
}

/// `strata violations` flags.
#[derive(Debug, clap::Args)]
pub(super) struct ViolationsCli {
    /// Repository root.
    #[arg(long, default_value = ".")]
    pub(super) root: PathBuf,
    /// Configuration file (defaults to `<root>/strata.toml`; built-in defaults
    /// apply when absent).
    #[arg(long)]
    pub(super) config: Option<PathBuf>,
    /// Parallelism for parsing.
    #[arg(long)]
    pub(super) jobs: Option<u32>,
    /// Comma-separated `kind[:severity]` selectors that trigger exit code 2.
    #[arg(long)]
    pub(super) fail_on: Option<String>,
    /// Output format.
    #[arg(long, default_value = "table")]
    pub(super) format: ViolationFormatChoice,
}

/// The violations output-format choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum ViolationFormatChoice {
    /// The default table face.
    Table,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

/// `strata report` flags.
#[derive(Debug, clap::Args)]
pub(super) struct ReportCli {
    /// Include numerical evidence, score components, and effective parameters.
    #[arg(long)]
    pub(super) verbose: bool,
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    pub(super) input: PathBuf,
    /// Markdown file to write (stdout when absent).
    #[arg(long)]
    pub(super) output: Option<PathBuf>,
}
