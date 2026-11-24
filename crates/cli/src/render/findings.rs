//! Structural-finding formatting.

use std::fmt::Write as _;
use std::io::{self, Write};

use unlace_engine::{CycleAnalysis, CycleCoverage, EdgeBreak, Severity, Violation, ViolationKind};

use super::limit::{more_line, take_worst};
use super::{RenderOptions, nq, wrap};

pub(super) fn findings_lines(
    label: &str,
    violations: &[Violation],
    options: RenderOptions,
) -> Vec<String> {
    let mut lines = vec![format!("{label} ({})", violations.len())];
    if violations.is_empty() {
        lines.push("  None.".to_owned());
    }
    let (shown, hidden) = take_worst(violations, options, |violation| {
        severity_rank(violation.severity)
    });
    for violation in shown {
        lines.extend(finding_lines(violation));
    }
    lines.extend(more_line(hidden, 2));
    lines
}

/// Formats one finding with its capacity measurement and suggested cuts.
fn finding_lines(violation: &Violation) -> Vec<String> {
    let mut lines = Vec::new();
    let location = violation
        .location
        .iter()
        .map(|path| nq(path))
        .collect::<Vec<_>>()
        .join(", ");
    let detail = if violation.kind == ViolationKind::Cycle {
        cycle_finding_text(violation)
    } else if let Some(capacity) = &violation.capacity {
        capacity.describe(&violation.location)
    } else {
        format!("at {location}: {}", violation.detail)
    };
    lines.extend(wrap(
        &format!(
            "- {} [{}] {detail}",
            kind_tag(violation.kind),
            severity_tag(violation.severity)
        ),
        2,
        4,
    ));
    for edge in violation.break_suggestions.iter().flatten().skip(1) {
        lines.extend(wrap(&format!("Also cut: {}", cut_text(edge)), 4, 4));
    }
    let further = violation
        .import_cycle
        .iter()
        .flat_map(|cycle| cycle.cut.iter().skip(1));
    for import in further {
        lines.extend(wrap(&format!("Also {}", import.describe(nq)), 4, 4));
    }
    lines
}

/// Formats one cut as `src -> dst (w=.., method[, same file])`.
fn cut_text(edge: &EdgeBreak) -> String {
    let method = if edge.exact { "exact" } else { "heuristic" };
    let scope = if edge.same_file { ", same file" } else { "" };
    format!(
        "{} -> {} (w={:.1}, {method}{scope})",
        nq(&edge.source),
        nq(&edge.target),
        edge.weight
    )
}

/// Rebuilds one cycle finding's text from its structured fields, quoting every
/// path, specifier and symbol: the first import to remove leads and further
/// imports collapse behind an explicit `+N more` (ADR-35). A saved result
/// that predates import cycles still words its symbol cut.
pub(super) fn cycle_finding_text(violation: &Violation) -> String {
    let members = violation
        .location
        .iter()
        .map(|name| nq(name))
        .collect::<Vec<_>>()
        .join("/");
    if let Some(cycle) = &violation.import_cycle {
        return format!("{members} — {}", cycle.describe(&violation.location, nq));
    }
    let size = violation.location.len();
    let mut detail = if violation.detail.is_empty() {
        format!("{size}-symbol cycle")
    } else {
        violation.detail.clone()
    };
    let Some(breaks) = &violation.break_suggestions else {
        return format!("{members} — {detail}");
    };
    let Some(first) = breaks.first() else {
        return format!("{members} — {detail}");
    };
    if let Some(index) = detail.find("; to split it, break ") {
        detail.truncate(index);
    }
    let mut text = format!(
        "{members} — {detail}; to split it, break {}",
        cut_text(first)
    );
    if breaks.len() > 1 {
        let _ = write!(text, ", +{} more", breaks.len() - 1);
    }
    text
}

/// Writes a violation listing for `violations` to `out`, worst findings first
/// and capped unless `options.full`, as a fully covered run.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
#[cfg(test)]
pub fn write_violation_table(
    violations: &[Violation],
    options: RenderOptions,
    out: &mut impl Write,
) -> io::Result<()> {
    write_violation_report(violations, &CycleCoverage::default(), options, out)
}

/// Writes the violation listing followed by the cycle coverage note when cycle
/// findings could not see every file (ADR-35), so a CI log tells zero cycles
/// apart from cycles the analysis could not see.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn write_violation_report(
    violations: &[Violation],
    coverage: &CycleCoverage,
    options: RenderOptions,
    out: &mut impl Write,
) -> io::Result<()> {
    if violations.is_empty() {
        writeln!(out, "no violations")?;
    }
    let (shown, hidden) = take_worst(violations, options, |violation| {
        severity_rank(violation.severity)
    });
    for violation in shown {
        writeln!(
            out,
            "{} [{}] {} :: {}",
            kind_tag(violation.kind),
            severity_tag(violation.severity),
            violation.location.join(", "),
            violation.detail
        )?;
    }
    if let Some(line) = more_line(hidden, 0) {
        writeln!(out, "{line}")?;
    }
    if let Some(note) = cycle_coverage_note(coverage) {
        writeln!(out, "{note}")?;
    }
    writeln!(out, "{}", super::ADVICE_DISCLAIMER)
}

/// Says when cycle findings could not see the whole file-import graph
/// (ADR-35): no cycle finding then does not mean no cycle. `None` when every
/// analysed file states its imports.
pub(super) fn cycle_coverage_note(coverage: &CycleCoverage) -> Option<String> {
    match coverage.status {
        CycleAnalysis::Complete => None,
        CycleAnalysis::Partial => Some(format!(
            "Cycle analysis is partial: {} {} no imports, so a cycle through them is shown only where the symbol graph sees one.",
            coverage.files_without_imports,
            if coverage.files_without_imports == 1 {
                "file states"
            } else {
                "files state"
            },
        )),
        CycleAnalysis::Unavailable => Some(
            "Cycle analysis is unavailable: no analysed file states its imports, so no cycle finding does not mean no cycle.".to_owned(),
        ),
    }
}

/// Ranks a severity so hard violations are shown before borderline findings.
///
/// The engine already orders each findings list by severity, so this ordering is
/// defensive for those lists. It matters for the combined `violations` table,
/// which merges shared and profile-specific findings.
fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Violation => 0,
        Severity::Borderline => 1,
    }
}

/// Returns the lowercase tag of a violation kind.
fn kind_tag(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::Cycle => "cycle",
        ViolationKind::Polarity => "polarity",
        ViolationKind::Capacity => "capacity",
        ViolationKind::Visibility => "visibility",
        ViolationKind::TestOnly => "testOnly",
    }
}

/// Returns the lowercase tag of a severity.
fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Violation => "violation",
        Severity::Borderline => "borderline",
    }
}
