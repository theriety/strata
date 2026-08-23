//! Strata's command-line entry point.
//!
//! The binary is a thin shell over `strata-engine`: each subcommand parses its
//! flags (clap derive, mirroring the reference flag tables), calls the same
//! public library functions an embedder would, and renders the typed result.
//!
//! Exit codes are fixed (AD-8): `0` on success, `1` for any [`StrataError`]
//! (printed with the error's remedy code), and `2` reserved exclusively for a
//! `violations --fail-on` match. A borderline capacity finding never gates, so it
//! never produces a `2`.

mod commands;
mod render;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use strata_engine::{Mode, StrataError};

use crate::commands::ConfigOverrides;
use crate::commands::analyze::AnalyzeArgs;
use crate::commands::diff::DiffArgs;
use crate::commands::report::ReportArgs;
use crate::commands::tree::TreeArgs;
use crate::commands::violations::{self, ViolationFormat, ViolationsArgs};
use crate::render::Format;

/// The `strata` read-only module-decomposition tool.
#[derive(Debug, Parser)]
#[command(name = "strata", version, about, long_about = None)]
struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// The five `strata` subcommands.
#[derive(Debug, Subcommand)]
enum Command {
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
enum FormatChoice {
    /// The human-readable analysis summary.
    Summary,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

/// The restructuring-mode choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModeChoice {
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
struct AnalyzeCli {
    /// Repository root to analyze.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Configuration file (defaults to `<root>/strata.toml`; built-in defaults
    /// apply when absent).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Restructuring mode.
    #[arg(long)]
    mode: Option<ModeChoice>,
    /// Candidates per mode.
    #[arg(short = 'k', long)]
    candidates: Option<u32>,
    /// Where to write the result (stdout when absent).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Output format (TTY-aware when absent: summary on a terminal, json piped).
    #[arg(long)]
    format: Option<FormatChoice>,
    /// Deterministic seed.
    #[arg(long)]
    seed: Option<u64>,
    /// Parallelism for parsing and shattering.
    #[arg(long)]
    jobs: Option<u32>,
}

/// `strata tree` flags.
#[derive(Debug, clap::Args)]
struct TreeCli {
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    input: PathBuf,
    /// Mode to draw a candidate from.
    #[arg(long)]
    mode: Option<String>,
    /// 1-based candidate index; omit to list available candidates.
    #[arg(long)]
    candidate: Option<usize>,
    /// Print the current (as-is) structure instead of a candidate.
    #[arg(long)]
    current: bool,
    /// List each file's symbols with derived visibility.
    #[arg(long)]
    symbols: bool,
    /// Truncate the tree at container depth n.
    #[arg(long)]
    depth: Option<u32>,
}

/// `strata diff` flags.
#[derive(Debug, clap::Args)]
struct DiffCli {
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    input: PathBuf,
    /// The first structure reference (`current` or `mode/index`).
    left: String,
    /// The second structure reference (`current` or `mode/index`).
    right: String,
}

/// `strata violations` flags.
#[derive(Debug, clap::Args)]
struct ViolationsCli {
    /// Repository root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Configuration file (defaults to `<root>/strata.toml`; built-in defaults
    /// apply when absent).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Parallelism for parsing.
    #[arg(long)]
    jobs: Option<u32>,
    /// Comma-separated `kind[:severity]` selectors that trigger exit code 2.
    #[arg(long)]
    fail_on: Option<String>,
    /// Output format.
    #[arg(long, default_value = "table")]
    format: ViolationFormatChoice,
}

/// The violations output-format choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ViolationFormatChoice {
    /// The default table face.
    Table,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

/// `strata report` flags.
#[derive(Debug, clap::Args)]
struct ReportCli {
    /// `AnalyzeResult` JSON from a previous `analyze`.
    #[arg(long)]
    input: PathBuf,
    /// Markdown file to write (stdout when absent).
    #[arg(long)]
    output: Option<PathBuf>,
}

/// Parses the arguments, dispatches the subcommand, and maps the result to an
/// exit code: `0` success, `1` any error (including a clap usage error), `2` a
/// `violations --fail-on` match.
///
/// Clap's own parse failures are intercepted here so a malformed flag, unknown
/// subcommand, or bad value yields exit `1` — never the default `2` — keeping `2`
/// exclusively for a gating `violations --fail-on` match. A `--help` or
/// `--version` request still prints to stdout and exits `0`.
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return exit_for_clap_error(&error),
    };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    match dispatch(cli.command, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(error) => {
            report_error(&error, &mut stderr);
            ExitCode::from(1)
        }
    }
}

/// Renders a clap parse `error` and maps it to a process exit code.
///
/// A `--help`/`--version` request is not a failure: clap stores its rendered
/// text in the error, so it is printed to stdout and exits `0`. Every genuine
/// usage error is printed to stderr and exits `1`, so exit code `2` stays
/// reserved for a gating `violations --fail-on` match.
fn exit_for_clap_error(error: &clap::Error) -> ExitCode {
    let _ = error.print();
    ExitCode::from(clap_exit_code(error.kind()))
}

/// Maps a clap [`ErrorKind`] to its process exit code.
///
/// A `--help`/`--version` request (or the bare-invocation help) is a success and
/// returns `0`; every other parse failure is a usage error and returns `1`. The
/// value `2` is never produced here, keeping it reserved for a gating
/// `violations --fail-on` match.
///
/// [`ErrorKind`]: clap::error::ErrorKind
fn clap_exit_code(kind: clap::error::ErrorKind) -> u8 {
    use clap::error::ErrorKind;

    match kind {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
        _ => 1,
    }
}

/// Dispatches a parsed command, returning its success exit code.
///
/// # Errors
///
/// Returns the command's [`StrataError`], which the caller maps to exit code `1`.
fn dispatch(
    command: Command,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, StrataError> {
    match command {
        Command::Analyze(args) => run_analyze(args, out, err),
        Command::Tree(args) => run_tree(args, out),
        Command::Diff(args) => run_diff(args, out),
        Command::Violations(args) => run_violations(args, out, err),
        Command::Report(args) => run_report(args, out),
    }
}

/// Resolves the effective config path: an explicit `--config` wins (warning to
/// `err` when the file is missing), otherwise the root's own `strata.toml`.
fn resolve_config_path(
    explicit: Option<PathBuf>,
    root: &std::path::Path,
    err: &mut impl Write,
) -> PathBuf {
    match explicit {
        Some(path) => {
            if !path.exists() {
                let _ = writeln!(
                    err,
                    "warning: config {} not found; using built-in defaults",
                    path.display()
                );
            }
            path
        }
        None => root.join("strata.toml"),
    }
}

/// Runs `analyze`, honoring `--output` (a file) over the provided sink.
fn run_analyze(
    cli: AnalyzeCli,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, StrataError> {
    let format = Format::resolve(
        cli.format.map(format_from_choice),
        out_is_terminal(),
        Format::Summary,
    );
    let overrides = ConfigOverrides {
        seed: cli.seed,
        candidates: cli.candidates,
        jobs: cli.jobs,
        mode: cli.mode.map(Mode::from),
    };
    let config = resolve_config_path(cli.config, &cli.root, err);
    let args = AnalyzeArgs {
        root: cli.root,
        config,
        overrides,
        format,
    };

    if let Some(path) = cli.output {
        let mut buffer = Vec::new();
        commands::analyze::run(&args, &mut buffer)?;
        std::fs::write(&path, &buffer).map_err(|error| StrataError::InputUnreadable {
            path,
            reason: error.to_string(),
        })?;
    } else {
        commands::analyze::run(&args, out)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `tree`.
fn run_tree(cli: TreeCli, out: &mut impl Write) -> Result<ExitCode, StrataError> {
    let args = TreeArgs {
        input: cli.input,
        mode: cli.mode,
        candidate: cli.candidate,
        current: cli.current,
        symbols: cli.symbols,
        depth: cli.depth,
    };
    commands::tree::run(&args, out)?;
    Ok(ExitCode::SUCCESS)
}

/// Runs `diff`.
fn run_diff(cli: DiffCli, out: &mut impl Write) -> Result<ExitCode, StrataError> {
    let args = DiffArgs {
        input: cli.input,
        left: cli.left,
        right: cli.right,
    };
    commands::diff::run(&args, out)?;
    Ok(ExitCode::SUCCESS)
}

/// Runs `violations`, mapping a gating match to exit code `2`.
fn run_violations(
    cli: ViolationsCli,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, StrataError> {
    let fail_on = match cli.fail_on {
        Some(value) => violations::parse_fail_on(&value)?,
        None => Vec::new(),
    };
    let config = resolve_config_path(cli.config, &cli.root, err);
    let args = ViolationsArgs {
        root: cli.root,
        config,
        jobs: cli.jobs,
        fail_on,
        format: match cli.format {
            ViolationFormatChoice::Table => ViolationFormat::Table,
            ViolationFormatChoice::Json => ViolationFormat::Json,
        },
    };
    match violations::run(&args, out)? {
        violations::Outcome::Clean => Ok(ExitCode::SUCCESS),
        violations::Outcome::Gated => Ok(ExitCode::from(2)),
    }
}

/// Runs `report`.
fn run_report(cli: ReportCli, out: &mut impl Write) -> Result<ExitCode, StrataError> {
    let args = ReportArgs {
        input: cli.input,
        output: cli.output,
    };
    commands::report::run(&args, out)?;
    Ok(ExitCode::SUCCESS)
}

/// Maps a `FormatChoice` to the renderer's [`Format`].
fn format_from_choice(choice: FormatChoice) -> Format {
    match choice {
        FormatChoice::Summary => Format::Summary,
        FormatChoice::Json => Format::Json,
    }
}

/// Returns whether the process stdout is a terminal.
fn out_is_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Writes a one-line error report with its stable code and remedy hint to `err`.
fn report_error(error: &StrataError, err: &mut impl Write) {
    let _ = writeln!(err, "error[{}]: {error}", error.code());
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let _ = writeln!(err, "  caused by: {cause}");
        source = cause.source();
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, clap_exit_code, resolve_config_path};

    /// Parses `argv` and returns the resulting clap error kind.
    ///
    /// A parse that unexpectedly succeeds yields [`ErrorKind::InvalidValue`] so the
    /// asserting test fails on a real, non-gating code rather than on a panic.
    ///
    /// [`ErrorKind::InvalidValue`]: clap::error::ErrorKind::InvalidValue
    fn parse_error_kind(argv: &[&str]) -> clap::error::ErrorKind {
        match Cli::try_parse_from(argv) {
            Ok(_) => clap::error::ErrorKind::InvalidValue,
            Err(error) => error.kind(),
        }
    }

    #[test]
    fn should_map_an_unknown_flag_to_exit_code_one() {
        let kind = parse_error_kind(&["strata", "--bogus-flag"]);

        assert_eq!(clap_exit_code(kind), 1);
    }

    #[test]
    fn should_map_an_unknown_subcommand_to_exit_code_one() {
        let kind = parse_error_kind(&["strata", "nonexistent-subcommand"]);

        assert_eq!(clap_exit_code(kind), 1);
    }

    #[test]
    fn should_map_a_bad_value_to_exit_code_one() {
        let kind = parse_error_kind(&["strata", "analyze", "--candidates", "notanumber"]);

        assert_eq!(clap_exit_code(kind), 1);
    }

    #[test]
    fn should_never_map_a_usage_error_to_the_gating_exit_code_two() {
        for argv in [
            vec!["strata", "--bogus-flag"],
            vec!["strata", "nonexistent-subcommand"],
            vec!["strata", "analyze", "--candidates", "notanumber"],
            vec!["strata", "tree"],
        ] {
            let kind = parse_error_kind(&argv);

            assert_ne!(clap_exit_code(kind), 2, "argv {argv:?} must not gate");
        }
    }

    #[test]
    fn should_parse_analyze_without_an_explicit_config() {
        let cli = Cli::try_parse_from(["strata", "analyze"]);

        assert!(cli.is_ok(), "--config is optional");
    }

    #[test]
    fn should_default_the_config_to_the_root_strata_toml() {
        let mut err = Vec::new();

        let path = resolve_config_path(None, std::path::Path::new("/some/root"), &mut err);

        assert_eq!(path, std::path::PathBuf::from("/some/root/strata.toml"));
        assert!(err.is_empty(), "no warning for the implicit default");
    }

    #[test]
    fn should_warn_when_an_explicit_config_is_missing() {
        let mut err = Vec::new();
        let missing = std::path::PathBuf::from("/nonexistent/strata.toml");

        let path = resolve_config_path(Some(missing.clone()), std::path::Path::new("."), &mut err);

        assert_eq!(path, missing, "the explicit path still wins");
        let text = String::from_utf8(err).unwrap_or_default();
        assert!(text.contains(
            "warning: config /nonexistent/strata.toml not found; using built-in defaults"
        ));
    }

    #[test]
    fn should_map_help_to_exit_code_zero() {
        let kind = parse_error_kind(&["strata", "--help"]);

        assert_eq!(clap_exit_code(kind), 0);
    }

    #[test]
    fn should_map_version_to_exit_code_zero() {
        let kind = parse_error_kind(&["strata", "--version"]);

        assert_eq!(clap_exit_code(kind), 0);
    }

    #[test]
    fn should_map_a_bare_invocation_help_to_exit_code_zero() {
        let kind = parse_error_kind(&["strata"]);

        assert_eq!(clap_exit_code(kind), 0);
    }
}
