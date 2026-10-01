//! Subcommand dispatch: maps parsed flags onto command arguments and exit codes.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use strata_engine::{Mode, StrataError};

use crate::args::{
    AnalyzeCli, Command, DiffCli, FormatChoice, ReportCli, TreeCli, ViolationFormatChoice,
    ViolationsCli,
};
use crate::commands;
use crate::commands::ConfigOverrides;
use crate::commands::analyze::AnalyzeArgs;
use crate::commands::diff::DiffArgs;
use crate::commands::report::ReportArgs;
use crate::commands::tree::TreeArgs;
use crate::commands::violations::{self, ViolationFormat, ViolationsArgs};
use crate::render::{Format, RenderOptions};

/// Dispatches a parsed command, returning its success exit code.
///
/// # Errors
///
/// Returns the command's [`StrataError`], which the caller maps to exit code `1`.
pub(super) fn dispatch(
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
        allow_cross_package_moves: cli.allow_cross_package_moves,
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
        if cli.verbose {
            commands::analyze::run_with_options(
                &args,
                RenderOptions { verbose: true },
                &mut buffer,
            )?;
        } else {
            commands::analyze::run(&args, &mut buffer)?;
        }
        std::fs::write(&path, &buffer).map_err(|error| StrataError::InputUnreadable {
            path,
            reason: error.to_string(),
        })?;
    } else if cli.verbose {
        commands::analyze::run_with_options(&args, RenderOptions { verbose: true }, out)?;
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
    if cli.verbose {
        commands::report::run_with_options(&args, RenderOptions { verbose: true }, out)?;
    } else {
        commands::report::run(&args, out)?;
    }
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

#[cfg(test)]
mod tests {
    use super::resolve_config_path;

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
}
