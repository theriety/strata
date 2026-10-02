//! Precondition evaluation: filters violations by class, severity, and location.

use strata_engine::result::{
    AnalyzeResult, Severity as EngineSeverity, Violation, ViolationKind as EngineViolationKind,
};

use crate::harness::Verdict;
use crate::target::{Precondition, PreconditionKind, SeverityFilter, ViolationClass};

/// Returns the deduplicated findings visible across the executed profiles.
pub(in crate::harness) fn all_findings(result: &AnalyzeResult) -> Vec<Violation> {
    let mut findings = result.current.shared_findings.clone();
    for profile in [
        result.profiles.anchored.as_ref(),
        result.profiles.greenfield.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        findings.extend(profile.current.unique_findings.iter().cloned());
    }
    let mut unique = Vec::new();
    for finding in findings {
        if !unique.contains(&finding) {
            unique.push(finding);
        }
    }
    unique
}

/// Preconditions verified against the current layout before scoring.
pub(in crate::harness) fn evaluate_preconditions(
    preconditions: &[Precondition],
    files: u32,
    violations: &[Violation],
) -> Vec<Verdict> {
    preconditions
        .iter()
        .map(|precondition| {
            let label = precondition_label(precondition);
            let (passed, observed) = match precondition.kind {
                PreconditionKind::FileCount => {
                    let min = precondition.min.unwrap_or(0);
                    let max = precondition.max.unwrap_or(u32::MAX);
                    (
                        files >= min && files <= max,
                        format!("summary.files = {files} against [{min}, {max}]"),
                    )
                }
                PreconditionKind::ViolationPresent | PreconditionKind::ViolationAbsent => {
                    let matches = matching_violations(precondition, violations);
                    let present = !matches.is_empty();
                    let expect_present = precondition.kind == PreconditionKind::ViolationPresent;
                    let classes: Vec<String> = matches
                        .iter()
                        .map(|violation| violation_class_name(violation.kind))
                        .collect();
                    let observed = if present {
                        format!("found [{}]", classes.join(", "))
                    } else {
                        "found none".to_owned()
                    };
                    (present == expect_present, observed)
                }
            };
            Verdict {
                label,
                passed,
                detail: format!("{observed}; because {}", precondition.because),
            }
        })
        .collect()
}

/// A precondition's stable label.
fn precondition_label(precondition: &Precondition) -> String {
    let filters = class_filters(precondition);
    match precondition.kind {
        PreconditionKind::FileCount => "file_count".to_owned(),
        PreconditionKind::ViolationPresent => {
            format!("violation_present({filters})")
        }
        PreconditionKind::ViolationAbsent => {
            format!("violation_absent({filters})")
        }
    }
}

/// The violation class plus optional suffix/severity a precondition filters on.
fn class_filters(precondition: &Precondition) -> String {
    let class = match precondition.violation {
        Some(ViolationClass::Cycle) => violation_class_name(EngineViolationKind::Cycle),
        Some(ViolationClass::Polarity) => violation_class_name(EngineViolationKind::Polarity),
        Some(ViolationClass::Capacity) => violation_class_name(EngineViolationKind::Capacity),
        Some(ViolationClass::Visibility) => violation_class_name(EngineViolationKind::Visibility),
        None => "any".to_owned(),
    };
    let suffix = precondition
        .location_suffix
        .as_ref()
        .map(|suffix| format!("@{suffix}"))
        .unwrap_or_default();
    let severity = precondition
        .severity
        .map(|severity| match severity {
            SeverityFilter::Violation => "::violation",
            SeverityFilter::Borderline => "::borderline",
        })
        .unwrap_or_default();
    format!("{class}{suffix}{severity}")
}

/// The DTO violation kind rendered for messages.
fn violation_class_name(kind: EngineViolationKind) -> String {
    match kind {
        EngineViolationKind::Cycle => "cycle".to_owned(),
        EngineViolationKind::Polarity => "polarity".to_owned(),
        EngineViolationKind::Capacity => "capacity".to_owned(),
        EngineViolationKind::Visibility => "visibility".to_owned(),
    }
}

/// Class equality between the target's vocabulary and the DTO's.
fn class_matches(precondition: &Precondition, violation: &Violation) -> bool {
    let Some(wanted) = precondition.violation else {
        return true;
    };
    matches!(
        (wanted, violation.kind),
        (ViolationClass::Cycle, EngineViolationKind::Cycle)
            | (ViolationClass::Polarity, EngineViolationKind::Polarity)
            | (ViolationClass::Capacity, EngineViolationKind::Capacity)
            | (ViolationClass::Visibility, EngineViolationKind::Visibility)
    )
}

/// Severity filter; absent means any severity qualifies.
fn severity_matches(precondition: &Precondition, violation: &Violation) -> bool {
    match precondition.severity {
        None => true,
        Some(SeverityFilter::Violation) => violation.severity == EngineSeverity::Violation,
        Some(SeverityFilter::Borderline) => violation.severity == EngineSeverity::Borderline,
    }
}

/// The violations matching a precondition's class, severity, and dot-segment
/// location filters.
fn matching_violations<'a>(
    precondition: &Precondition,
    violations: &'a [Violation],
) -> Vec<&'a Violation> {
    violations
        .iter()
        .filter(|violation| {
            class_matches(precondition, violation)
                && severity_matches(precondition, violation)
                && precondition
                    .location_suffix
                    .as_deref()
                    .is_none_or(|suffix| {
                        violation
                            .location
                            .iter()
                            .any(|location| location_suffix_matches(location, suffix))
                    })
        })
        .collect()
}

/// Dot-segment location matching: the location's last dot-segment equals the
/// suffix, or the location ends with `.<suffix>`.
fn location_suffix_matches(location: &str, suffix: &str) -> bool {
    location.ends_with(&format!(".{suffix}"))
        || location.rsplit('.').next().unwrap_or(location) == suffix
}

#[cfg(test)]
mod tests {
    use strata_engine::result::{
        Severity as EngineSeverity, Violation, ViolationKind as EngineViolationKind,
    };

    use crate::target::{Precondition, PreconditionKind, SeverityFilter, ViolationClass};

    use super::{location_suffix_matches, matching_violations};

    #[test]
    fn violation_suffix_matches_on_dot_segment_boundaries() {
        assert!(location_suffix_matches("a.hub", "hub"));
        assert!(location_suffix_matches("hub", "hub"));
        assert!(!location_suffix_matches("hub.sub", "hu"));
        let violations = vec![
            Violation {
                kind: EngineViolationKind::Capacity,
                severity: EngineSeverity::Borderline,
                location: vec!["src".to_owned(), "hub".to_owned()],
                detail: "over cap".to_owned(),
                break_suggestions: None,
                capacity: None,
            },
            Violation {
                kind: EngineViolationKind::Capacity,
                severity: EngineSeverity::Borderline,
                location: vec!["ingest.hub".to_owned()],
                detail: "dotted container".to_owned(),
                break_suggestions: None,
                capacity: None,
            },
        ];
        let precondition = Precondition {
            kind: PreconditionKind::ViolationAbsent,
            min: None,
            max: None,
            violation: Some(ViolationClass::Capacity),
            location_suffix: Some("hub".to_owned()),
            severity: Some(SeverityFilter::Borderline),
            because: "no borderline hub breach".to_owned(),
        };
        let matches = matching_violations(&precondition, &violations);
        assert_eq!(
            matches.len(),
            2,
            "dot-segment suffix 'hub' matches the bare segment and the dotted tail"
        );
        let precondition_narrower = Precondition {
            kind: PreconditionKind::ViolationPresent,
            severity: Some(SeverityFilter::Violation),
            ..precondition
        };
        assert!(matching_violations(&precondition_narrower, &violations).is_empty());
    }
}
