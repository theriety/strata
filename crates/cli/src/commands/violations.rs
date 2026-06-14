//! `strata violations`: the CI gate over the current codebase.
//!
//! The command snapshots the root, derives the current tree's structural
//! violations (cycles, polarity breaches, over-exports) via the engine, augments
//! them with capacity findings counted against the configured caps, and reports
//! them. `--fail-on <classes>` raises the exit decision to a gating match when a
//! hard violation of a listed class is present; capacity findings within ±10% of
//! a cap are reported as `borderline` and never gate.

use std::io::Write;
use std::path::PathBuf;

use strata_engine::{
    AnalyzeConfig, ContainerNode, Level, Severity, StrataError, Violation, ViolationKind, analyze,
    snapshot_from_root,
};

use crate::commands::{BORDERLINE_CAPACITY_MARGIN, ConfigOverrides, resolve_config};
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
    let mut result = analyze(&snapshot, &config)?;

    let mut violations = std::mem::take(&mut result.current.violations);
    violations.extend(capacity_violations(&result.current.tree, &config));
    result.current.violations.clone_from(&violations);

    match args.format {
        ViolationFormat::Table => {
            write_violation_table(&violations, out).map_err(|error| write_error(&error))?;
        }
        ViolationFormat::Json => write_json(&result, out).map_err(|error| write_error(&error))?,
    }

    Ok(gate(&violations, &args.fail_on))
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

/// Derives capacity violations from the current tree against the configured caps.
///
/// A file over its production-SLOC cap and an interior container over its
/// member-count cap each yield a finding; a finding within ±10% of its cap is
/// `borderline` and never gates. Each container is checked against the cap of its
/// own level.
fn capacity_violations(tree: &ContainerNode, config: &AnalyzeConfig) -> Vec<Violation> {
    let mut findings = Vec::new();
    walk_capacity(
        tree,
        std::slice::from_ref(&tree.name),
        config,
        &mut findings,
    );
    findings
}

/// Recursively checks `node` and its descendants against their level caps.
fn walk_capacity(
    node: &ContainerNode,
    path: &[String],
    config: &AnalyzeConfig,
    findings: &mut Vec<Violation>,
) {
    let (measure, cap) = match node.level {
        Level::File => (node.production_sloc.unwrap_or(0), config.capacity.file),
        Level::Folder => (child_count(node), config.capacity.folder),
        Level::Domain => (child_count(node), config.capacity.domain),
        Level::Package => (child_count(node), config.capacity.package),
        Level::PackageGroup => (child_count(node), config.capacity.package_group),
    };

    if let Some(finding) = capacity_finding(node, path, measure, cap) {
        findings.push(finding);
    }

    if let Some(children) = &node.children {
        for child in children {
            let mut child_path = path.to_vec();
            child_path.push(child.name.clone());
            walk_capacity(child, &child_path, config, findings);
        }
    }
}

/// Returns the child count of an interior container.
fn child_count(node: &ContainerNode) -> u32 {
    node.children.as_ref().map_or(0, |children| {
        u32::try_from(children.len()).unwrap_or(u32::MAX)
    })
}

/// Builds a capacity finding if `measure` is at or over the borderline band of
/// `cap`, classifying borderline (within ±10%) versus a hard breach.
fn capacity_finding(
    node: &ContainerNode,
    path: &[String],
    measure: u32,
    cap: u32,
) -> Option<Violation> {
    let cap_f = f64::from(cap);
    let measure_f = f64::from(measure);
    let lower = cap_f * (1.0 - BORDERLINE_CAPACITY_MARGIN);

    // below the borderline band entirely: not a finding.
    if measure_f < lower {
        return None;
    }
    // a hard breach is strictly over the cap; the band around the cap is borderline.
    let upper = cap_f * (1.0 + BORDERLINE_CAPACITY_MARGIN);
    let severity = if measure_f > upper {
        Severity::Violation
    } else {
        Severity::Borderline
    };

    Some(Violation {
        kind: ViolationKind::Capacity,
        severity,
        location: path.to_vec(),
        detail: format!(
            "{} `{}` holds {measure} against a cap of {cap}",
            level_word(node.level),
            node.name
        ),
        break_suggestions: None,
    })
}

/// Returns the noun for a container level used in a capacity message.
fn level_word(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "package group",
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

    /// Builds a file node with `sloc` production SLOC.
    fn file(name: &str, sloc: u32) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(Vec::new()),
            production_sloc: Some(sloc),
        }
    }

    /// Builds a folder node holding `children`.
    fn folder(name: &str, children: Vec<ContainerNode>) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::Folder,
            children: Some(children),
            symbols: None,
            production_sloc: None,
        }
    }

    /// Builds a config with the file cap set to `cap`.
    fn config_with_file_cap(cap: u32) -> AnalyzeConfig {
        let mut config = AnalyzeConfig::default();
        config.capacity.file = cap;
        config
    }

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
        };
        let fail_on = parse_fail_on("capacity:borderline").unwrap_or_default();

        assert_eq!(gate(&[borderline], &fail_on), Outcome::Clean);
    }

    #[test]
    fn should_report_a_file_over_its_cap_as_a_hard_violation() {
        let tree = file("big", 100);

        let findings = capacity_violations(&tree, &config_with_file_cap(10));

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings.first().map(|f| f.severity),
            Some(Severity::Violation)
        );
    }

    #[test]
    fn should_report_a_file_within_the_band_as_borderline() {
        // cap 100, file at 105 sits inside the +10% band -> borderline.
        let tree = file("near", 105);

        let findings = capacity_violations(&tree, &config_with_file_cap(100));

        assert_eq!(
            findings.first().map(|f| f.severity),
            Some(Severity::Borderline)
        );
    }

    #[test]
    fn should_not_report_a_file_well_under_its_cap() {
        let tree = file("small", 10);

        let findings = capacity_violations(&tree, &config_with_file_cap(100));

        assert!(findings.is_empty());
    }

    #[test]
    fn should_count_folder_members_against_the_folder_cap() {
        let children = (0..20).map(|i| file(&format!("f{i}"), 1)).collect();
        let tree = folder("dir", children);
        let mut config = AnalyzeConfig::default();
        config.capacity.folder = 5;

        let findings = capacity_violations(&tree, &config);

        assert!(findings.iter().any(|f| f.kind == ViolationKind::Capacity
            && f.severity == Severity::Violation
            && f.location == vec!["dir".to_owned()]));
    }

    #[test]
    fn should_gate_only_on_a_hard_violation_of_a_listed_class() {
        let hard = Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: Vec::new(),
            detail: String::new(),
            break_suggestions: None,
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
        };
        let fail_on = vec![FailOn {
            kind: ViolationKind::Capacity,
            severity: None,
        }];

        assert_eq!(gate(&[hard], &fail_on), Outcome::Clean);
    }
}
