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

mod args;
mod commands;
mod dispatch;
mod render;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use strata_engine::StrataError;

use crate::args::Cli;
use crate::dispatch::dispatch;

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

    use super::clap_exit_code;
    use crate::args::{Cli, Command};

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
    fn should_parse_the_cross_package_switch_as_off_unless_passed() {
        let passed = Cli::try_parse_from(["strata", "analyze", "--allow-cross-package-moves"]);
        let absent = Cli::try_parse_from(["strata", "analyze"]);

        let switch = |cli: Result<Cli, clap::Error>| match cli.map(|cli| cli.command) {
            Ok(Command::Analyze(analyze)) => Some(analyze.allow_cross_package_moves),
            _ => None,
        };
        assert_eq!(switch(passed), Some(true));
        assert_eq!(switch(absent), Some(false));
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
