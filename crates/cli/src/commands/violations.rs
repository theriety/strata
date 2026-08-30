//! `strata violations`: the CI gate over the current codebase.
//!
//! The command snapshots the root, derives the current tree's violations
//! (cycles, polarity breaches, over-exports, and capacity findings against the
//! configured caps) via the engine, and reports them. `--fail-on <classes>`
//! raises the exit decision to a gating match when a hard violation of a listed
//! class is present; capacity findings within ±10% of a cap are reported as
//! `borderline` and never gate.

use std::io::Write;
use std::path::PathBuf;

use strata_engine::{Severity, StrataError, Violation, ViolationKind, analyze, snapshot_from_root};

use crate::commands::{ConfigOverrides, findings, resolve_config};
use crate::render::{write_json, write_violation_table};

/// The output format of the violations face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationFormat {
    /// The default table face.
    Table,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

/// One parsed `--fail-on` selector: a violation class, optionally narrowed to a
/// single severity.
///
/// A bare `kind` selector (severity `None`) gates on any hard violation of that
/// class. A `kind:severity` selector narrows the match to that severity. Because
/// a borderline finding never gates CI (the ±10% capacity band), a
/// `kind:borderline` selector matches nothing — it parses, but no finding can
/// satisfy it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailOn {
    /// The violation class this selector gates on.
    pub kind: ViolationKind,
    /// The severity to narrow to; `None` accepts any gating severity.
    pub severity: Option<Severity>,
}

/// The parsed inputs of a `violations` run.
#[derive(Debug)]
pub struct ViolationsArgs {
    /// The repository root to analyze.
    pub root: PathBuf,
    /// The config file path (defaults apply when it is absent).
    pub config: PathBuf,
    /// The parallelism hint (`--jobs`).
    pub jobs: Option<u32>,
    /// The violation selectors that gate (`--fail-on`).
    pub fail_on: Vec<FailOn>,
    /// The output format.
    pub format: ViolationFormat,
}

/// The gating outcome of a `violations` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No listed class matched; exit `0`.
    Clean,
    /// A listed class matched a hard violation; exit `2`.
    Gated,
}

/// Parses a comma-separated `--fail-on` value into gating selectors.
///
/// Each token is a class name (`cycle`, `polarity`, `capacity`, `visibility`)
/// optionally followed by `:severity` (`violation` or `borderline`) to narrow the
/// match. A bare class gates on any hard violation of that class.
///
/// # Errors
///
/// Returns [`StrataError::ConfigInvalid`] when a class or severity name is
/// unknown.
pub fn parse_fail_on(value: &str) -> Result<Vec<FailOn>, StrataError> {
    let mut selectors = Vec::new();
    for token in value.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let (class, severity_token) = match token.split_once(':') {
            Some((class, severity)) => (class, Some(severity)),
            None => (token, None),
        };
        let kind = parse_kind(class)?;
        let severity = severity_token.map(parse_severity).transpose()?;
        let selector = FailOn { kind, severity };
        if !selectors.contains(&selector) {
            selectors.push(selector);
        }
    }
    Ok(selectors)
}

/// Parses a `--fail-on` class token into a [`ViolationKind`].
///
/// # Errors
///
/// Returns [`StrataError::ConfigInvalid`] when the class name is unknown.
fn parse_kind(class: &str) -> Result<ViolationKind, StrataError> {
    match class {
        "cycle" => Ok(ViolationKind::Cycle),
        "polarity" => Ok(ViolationKind::Polarity),
        "capacity" => Ok(ViolationKind::Capacity),
        "visibility" => Ok(ViolationKind::Visibility),
        other => Err(StrataError::ConfigInvalid {
            key: Some("--fail-on".to_owned()),
            reason: format!(
                "unknown violation class `{other}`; expected cycle, polarity, capacity, or visibility"
            ),
        }),
    }
}

/// Parses a `--fail-on` severity token into a [`Severity`].
///
/// # Errors
///
/// Returns [`StrataError::ConfigInvalid`] when the severity name is unknown.
fn parse_severity(severity: &str) -> Result<Severity, StrataError> {
    match severity {
        "violation" => Ok(Severity::Violation),
        "borderline" => Ok(Severity::Borderline),
        other => Err(StrataError::ConfigInvalid {
            key: Some("--fail-on".to_owned()),
            reason: format!("unknown severity `{other}`; expected violation or borderline"),
        }),
    }
}

/// Runs `violations`, writing the report to `out` and returning the gate outcome.
///
/// # Errors
///
/// Returns any [`StrataError`] from config loading, snapshotting, or analysis.
pub fn run(args: &ViolationsArgs, out: &mut impl Write) -> Result<Outcome, StrataError> {
    let overrides = ConfigOverrides {
        jobs: args.jobs,
        ..ConfigOverrides::default()
    };
    let config = resolve_config(&args.config, overrides)?;
    let snapshot = snapshot_from_root(&args.root, &config)?;
    let result = analyze(&snapshot, &config)?;
    let findings = findings(&result);

    match args.format {
        ViolationFormat::Table => {
            write_violation_table(&findings, out).map_err(|error| write_error(&error))?;
        }
        ViolationFormat::Json => write_json(&result, out).map_err(|error| write_error(&error))?,
    }

    Ok(gate(&findings, &args.fail_on))
}

/// Decides the gate outcome: a hard (non-borderline) violation matching a listed
/// selector gates; borderline findings never do.
///
/// A finding gates when its class matches a selector and either the selector
/// names no severity or names the finding's severity — but only ever for a hard
/// [`Severity::Violation`], so a borderline finding can never gate regardless of
/// the selector.
fn gate(violations: &[Violation], fail_on: &[FailOn]) -> Outcome {
    let gated = violations
        .iter()
        .any(|violation| fail_on.iter().any(|selector| matches(*selector, violation)));
    if gated {
        Outcome::Gated
    } else {
        Outcome::Clean
    }
}

/// Returns whether a hard violation satisfies a gating `selector`.
///
/// A borderline finding never matches (it never gates CI); a hard violation
/// matches when its class equals the selector's and the selector either names no
/// severity or names [`Severity::Violation`].
fn matches(selector: FailOn, violation: &Violation) -> bool {
    if violation.severity != Severity::Violation || selector.kind != violation.kind {
        return false;
    }
    match selector.severity {
        None => true,
        Some(severity) => severity == violation.severity,
    }
}

/// Wraps a rendering I/O failure into a [`StrataError`].
fn write_error(error: &std::io::Error) -> StrataError {
    StrataError::InputUnreadable {
        path: PathBuf::from("<stdout>"),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_a_comma_separated_fail_on_list() {
        let selectors = parse_fail_on("cycle, capacity").unwrap_or_default();

        assert!(selectors.contains(&FailOn {
            kind: ViolationKind::Cycle,
            severity: None,
        }));
        assert!(selectors.contains(&FailOn {
            kind: ViolationKind::Capacity,
            severity: None,
        }));
    }

    #[test]
    fn should_parse_a_kind_with_a_severity_qualifier() {
        let selectors = parse_fail_on("capacity:violation").unwrap_or_default();

        assert_eq!(
            selectors,
            vec![FailOn {
                kind: ViolationKind::Capacity,
                severity: Some(Severity::Violation),
            }]
        );
    }

    #[test]
    fn should_parse_a_borderline_severity_qualifier() {
        let selectors = parse_fail_on("capacity:borderline").unwrap_or_default();

        assert_eq!(
            selectors,
            vec![FailOn {
                kind: ViolationKind::Capacity,
                severity: Some(Severity::Borderline),
            }]
        );
    }

    #[test]
    fn should_reject_an_unknown_fail_on_class() {
        assert!(matches!(
            parse_fail_on("bogus"),
            Err(StrataError::ConfigInvalid { .. })
        ));
    }

    #[test]
    fn should_reject_an_unknown_severity_qualifier() {
        assert!(matches!(
            parse_fail_on("capacity:bogus"),
            Err(StrataError::ConfigInvalid { .. })
        ));
    }

    #[test]
    fn should_gate_a_severity_qualified_selector_on_a_matching_hard_violation() {
        let hard = Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let fail_on = parse_fail_on("capacity:violation").unwrap_or_default();

        assert_eq!(gate(&[hard], &fail_on), Outcome::Gated);
    }

    #[test]
    fn should_never_gate_a_borderline_qualified_selector() {
        // a borderline finding never gates, so `capacity:borderline` matches nothing.
        let borderline = Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Borderline,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let fail_on = parse_fail_on("capacity:borderline").unwrap_or_default();

        assert_eq!(gate(&[borderline], &fail_on), Outcome::Clean);
    }

    #[test]
    fn should_gate_only_on_a_hard_violation_of_a_listed_class() {
        let hard = Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let fail_on = vec![FailOn {
            kind: ViolationKind::Capacity,
            severity: None,
        }];

        assert_eq!(gate(&[hard], &fail_on), Outcome::Gated);
    }

    #[test]
    fn should_not_gate_on_a_borderline_finding() {
        let borderline = Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Borderline,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let fail_on = vec![FailOn {
            kind: ViolationKind::Capacity,
            severity: None,
        }];

        assert_eq!(gate(&[borderline], &fail_on), Outcome::Clean);
    }

    #[test]
    fn should_not_gate_a_class_outside_the_fail_on_set() {
        let hard = Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let fail_on = vec![FailOn {
            kind: ViolationKind::Capacity,
            severity: None,
        }];

        assert_eq!(gate(&[hard], &fail_on), Outcome::Clean);
    }
}
