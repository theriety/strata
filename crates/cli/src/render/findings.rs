//! Structural-finding formatting.

use std::fmt::Write as _;
use std::io::{self, Write};

use strata_engine::{Severity, Violation, ViolationKind};

use super::{nq, wrap};

pub(super) fn findings_lines(label: &str, violations: &[Violation]) -> Vec<String> {
    let mut lines = vec![format!("{label} ({})", violations.len())];
    if violations.is_empty() {
        lines.push("  None.".to_owned());
    }
    for violation in violations {
        let location = violation
            .location
            .iter()
            .map(|path| nq(path))
            .collect::<Vec<_>>()
            .join(", ");
        let detail = if violation.kind == ViolationKind::Cycle {
            cycle_finding_text(violation)
        } else if violation
            .capacity
            .as_ref()
            .is_some_and(|capacity| capacity.path.is_some())
        {
            violation.detail.clone()
        } else if violation.capacity.is_some() {
            format!("at container ancestry {location}: {}", violation.detail)
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
        if let Some(capacity) = &violation.capacity
            && !violation.detail.contains(&format!(
                "holds {} against a cap of {}",
                capacity.measured, capacity.cap
            ))
        {
            let capacity_location = capacity
                .path
                .as_ref()
                .map_or(String::new(), |path| format!(" at `{path}`"));
            lines.extend(wrap(
                &format!(
                    "Measured {} against cap {}{capacity_location}.",
                    capacity.measured, capacity.cap
                ),
                4,
                4,
            ));
        }
        for edge in violation.break_suggestions.iter().flatten().skip(1) {
            lines.extend(wrap(
                &format!(
                    "Suggested cut: `{}` → `{}` (weight {:.4}, {})",
                    edge.source,
                    edge.target,
                    edge.weight,
                    if edge.exact { "exact" } else { "heuristic" }
                ),
                4,
                4,
            ));
        }
    }
    lines.push(String::new());
    lines
}

/// Rebuilds one cycle finding's text from its structured fields, quoting every
/// symbol name; the priced cut leads and further cuts collapse behind an
/// explicit `+N more`.
pub(super) fn cycle_finding_text(violation: &Violation) -> String {
    let members = violation
        .location
        .iter()
        .map(|name| nq(name))
        .collect::<Vec<_>>()
        .join("/");
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
    if let Some(index) = detail.find("; break ") {
        detail.truncate(index);
    }
    let method = if first.exact { "exact" } else { "heuristic" };
    let mut text = format!(
        "{members} — {detail}; break {} -> {} (w={:.1}, {method})",
        nq(&first.source),
        nq(&first.target),
        first.weight
    );
    if breaks.len() > 1 {
        let _ = write!(text, ", +{} more", breaks.len() - 1);
    }
    text
}

/// Writes a violation listing for `violations` to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn write_violation_table(violations: &[Violation], out: &mut impl Write) -> io::Result<()> {
    if violations.is_empty() {
        return writeln!(out, "no violations");
    }
    for violation in violations {
        writeln!(
            out,
            "{} [{}] {} :: {}",
            kind_tag(violation.kind),
            severity_tag(violation.severity),
            violation.location.join(", "),
            violation.detail
        )?;
    }
    Ok(())
}

/// Returns the lowercase tag of a violation kind.
fn kind_tag(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::Cycle => "cycle",
        ViolationKind::Polarity => "polarity",
        ViolationKind::Capacity => "capacity",
        ViolationKind::Visibility => "visibility",
    }
}

/// Returns the lowercase tag of a severity.
fn severity_tag(severity: Severity) -> &'static str {
    match severity {
        Severity::Violation => "violation",
        Severity::Borderline => "borderline",
    }
}
