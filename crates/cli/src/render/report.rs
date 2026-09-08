//! Shared ordered report content for the terminal and Markdown faces.

use std::io::{self, Write};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use strata_engine::{AnalyzeResult, Candidate, CurrentStanding, ModeResult, Violation};

use super::changes::Changes;
use super::{
    RenderOptions, TERMS, advice_with_options, caption_for, effective_parameter_lines, f4, nq,
    present_modes, severity_tag, sf, wrap,
};

/// A heading and its already-wrapped, format-independent body.
struct Section {
    title: String,
    lines: Vec<String>,
}

/// Presentation content is selected once before applying either output syntax.
pub(super) struct Report {
    summary: Vec<String>,
    sections: Vec<Section>,
}

impl Report {
    pub(super) fn build(result: &AnalyzeResult, project: &str, options: RenderOptions) -> Self {
        let summary = vec![
            format!("Strata report — {project}"),
            format!(
                "{} files · {} symbols · {} edges",
                result.summary.files, result.summary.symbols, result.summary.edges
            ),
            format!("Snapshot `{}`", result.snapshot_hash),
        ];
        let mut findings = findings_lines("Shared findings", &result.current.shared_findings);
        for (name, profile) in present_modes(result) {
            findings.extend(findings_lines(
                &format!("{name} profile-specific findings"),
                &profile.current.unique_findings,
            ));
        }
        let mut candidates = wrap(
            "Lower scores are better. Compare scores only within one parameter profile of one project. A candidate is a proposed layout; inclusion does not make a move Recommended. Partial plans are not rescored.",
            0,
            0,
        );
        if present_modes(result).is_empty() {
            candidates.push("No candidates were produced.".to_owned());
        }
        for (name, profile) in present_modes(result) {
            candidates.extend(profile_lines(result, name, profile, options));
        }
        let mut advice = wrap(
            "Supporting profiles selected the destination; qualified profiles also passed the evidence thresholds; absent profiles did not select the move; conflicts name alternative destinations. Advice consolidates the best candidate from each executed profile.",
            0,
            0,
        );
        advice.extend(advice_with_options(result, options));
        let mut sections = vec![
            Section {
                title: "Structural findings".to_owned(),
                lines: findings,
            },
            Section {
                title: "Candidate layouts".to_owned(),
                lines: candidates,
            },
            Section {
                title: "Advice".to_owned(),
                lines: advice,
            },
        ];
        if options.verbose {
            let mut lines = Vec::new();
            for (name, profile) in present_modes(result) {
                lines.extend(effective_parameter_lines(name, &profile.parameters));
                // retain structured rule values as well as the readable numeric summary.
                if let Ok(parameters) = serde_json::to_string_pretty(&profile.parameters) {
                    lines.push("Complete saved profile parameters:".to_owned());
                    lines.extend(parameters.lines().map(str::to_owned));
                }
            }
            sections.push(Section {
                title: "Effective configuration".to_owned(),
                lines,
            });
        }
        Self { summary, sections }
    }

    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = self.summary.clone();
        for section in &self.sections {
            lines.push(String::new());
            lines.push(section.title.clone());
            lines.extend(section.lines.clone());
        }
        fit_lines(lines)
    }

    pub(super) fn write_text(&self, out: &mut impl Write) -> io::Result<()> {
        for line in self.lines() {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    pub(super) fn markdown(&self) -> String {
        // Fenced bodies preserve action-list wrapping and Unicode tree geometry.
        let mut blocks = vec![format!(
            "# {}",
            self.summary.first().map_or("Strata report", String::as_str)
        )];
        blocks = fit_lines(blocks);
        blocks.push(fit_lines(self.summary.iter().skip(1).cloned().collect()).join("\n"));
        for section in &self.sections {
            blocks.push(format!("## {}", section.title));
            let body = fit_lines(section.lines.clone()).join("\n");
            let fence = "`".repeat(longest_backtick_run(&body).max(2) + 1);
            blocks.push(format!("{fence}text\n{body}\n{fence}"));
        }
        format!("{}\n", blocks.join("\n\n"))
    }
}

/// Avoids terminating a Markdown fence when a source name itself contains backticks.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// Splits overflowing tokens losslessly; continuation indentation is presentation only.
fn fit_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .flat_map(|line| {
            if line.width() <= 100 {
                return vec![line];
            }
            let mut remaining = line.as_str();
            let mut fitted = Vec::new();
            while remaining.width() > 100 {
                let mut width = 0;
                let end = remaining
                    .char_indices()
                    .find_map(|(offset, character)| {
                        width += character.width().unwrap_or(0);
                        (width > 100).then_some(offset)
                    })
                    .unwrap_or(remaining.len());
                fitted.push(remaining[..end].to_owned());
                remaining = &remaining[end..];
            }
            fitted.push(remaining.to_owned());
            fitted
        })
        .collect()
}

fn findings_lines(label: &str, violations: &[Violation]) -> Vec<String> {
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
        let detail = if violation.kind == strata_engine::ViolationKind::Cycle {
            super::cycle_finding_text(violation)
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
                super::kind_tag(violation.kind),
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

fn profile_lines(
    result: &AnalyzeResult,
    name: &str,
    profile: &ModeResult,
    options: RenderOptions,
) -> Vec<String> {
    let mut lines = vec![
        String::new(),
        format!("{name} — {} candidate(s)", profile.candidates.len()),
        format!("Baseline score: {}", f4(profile.current.score)),
    ];
    if profile.solution_space_converged {
        lines.push(
            "Fewer than the requested candidates survived; the solution space converged."
                .to_owned(),
        );
    }
    match profile.current.standing {
        CurrentStanding::Optimal => {
            lines.push("Current layout is already optimal under this profile's search.".to_owned());
        }
        CurrentStanding::Infeasible => lines.push(format!(
            "Current layout violates {} capacity cap(s).",
            profile.current.capacity_breaks
        )),
        CurrentStanding::Outscored => {}
    }
    if profile.candidates.is_empty() {
        lines.push("No candidates were produced.".to_owned());
    }
    for candidate in &profile.candidates {
        lines.push(String::new());
        lines.push(format!("{name} — candidate {}", candidate.index));
        lines.extend(wrap(
            &format!(
                "Score: {} → {}; improvement {}",
                f4(profile.current.score),
                f4(candidate.score),
                sf(candidate.improvement)
            ),
            2,
            2,
        ));
        if let Some(capacity) = candidate.capacity_remainder {
            lines.push(format!(
                "  Capacity remaining: {} ({} at file level).",
                capacity.remaining, capacity.file_level
            ));
        }
        lines.extend(candidate_lines(result, candidate, options));
        if options.verbose {
            lines.extend(score_lines(profile, candidate));
        }
    }
    lines
}

fn candidate_lines(
    result: &AnalyzeResult,
    candidate: &Candidate,
    options: RenderOptions,
) -> Vec<String> {
    let changes = Changes::build(result, candidate);
    let mut lines = vec!["  Proposed moves".to_owned()];
    if changes.is_empty() {
        lines.push("  No moves versus the current layout.".to_owned());
    }
    lines.extend(file_move_lines(candidate, &changes));
    for entry in &candidate.symbol_moves {
        let kind = if entry.kind == strata_engine::SymbolKind::Type {
            "type"
        } else {
            "symbol"
        };
        lines.extend(wrap(
            &format!(
                "- Move {kind} `{}` from `{}` → `{}`",
                entry.symbol,
                entry.from_path,
                changes.after_path(&entry.to_path)
            ),
            4,
            6,
        ));
        if options.verbose {
            lines.push(format!(
                "      Objective delta {}; {} imports to repoint.",
                sf(entry.delta),
                entry.broken_imports
            ));
        }
    }
    for split in &candidate.conditional_splits {
        lines.extend(wrap(
            &format!(
                "Conditional split: {} → {} files. Requires breaking:",
                split
                    .scc
                    .iter()
                    .map(|name| nq(name))
                    .collect::<Vec<_>>()
                    .join(", "),
                split.resulting_files
            ),
            2,
            4,
        ));
        for edge in &split.preconditions {
            lines.extend(wrap(
                &format!(
                    "`{}` → `{}` (weight {:.4}, {})",
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
    lines.extend(changes.impact_lines());
    lines.extend(changes.tree_lines());
    lines
}

fn file_move_lines(candidate: &Candidate, changes: &Changes) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in &candidate.delta_narration {
        for file in &entry.files {
            lines.extend(wrap(
                &format!(
                    "- Move file `{}` → `{}`",
                    changes.path(&file.path),
                    changes.destination(&file.path, &entry.to)
                ),
                4,
                6,
            ));
        }
        let reason = match &entry.reason {
            strata_engine::MoveReason::PulledBy { partner, weight } => {
                strata_engine::MoveReason::PulledBy {
                    partner: changes.path(partner),
                    weight: *weight,
                }
            }
            strata_engine::MoveReason::RelievesOverCap {
                container,
                count,
                cap,
            } => strata_engine::MoveReason::RelievesOverCap {
                container: changes.path(container),
                count: *count,
                cap: *cap,
            },
            strata_engine::MoveReason::Follows { subject } => strata_engine::MoveReason::Follows {
                subject: changes.path(subject),
            },
            strata_engine::MoveReason::Clustering => strata_engine::MoveReason::Clustering,
            strata_engine::MoveReason::NamingCohesion { cohesion } => {
                strata_engine::MoveReason::NamingCohesion {
                    cohesion: *cohesion,
                }
            }
        };
        lines.extend(wrap(&caption_for(&reason), 6, 6));
        for mirror in &entry.mirrors {
            lines.extend(wrap(
                &format!(
                    "- Move mirrored test file `{}` → `{}`; follows `{}`",
                    changes.path(&mirror.path),
                    changes.destination(&mirror.path, &mirror.to),
                    changes.path(&mirror.source_path)
                ),
                4,
                6,
            ));
        }
        for blocked in &entry.blocked_mirrors {
            lines.extend(wrap(
                &format!(
                    "Test mirror `{}` stays in `{}`; cannot follow to `{}`: {}",
                    changes.path(&blocked.path),
                    changes.path(&blocked.from),
                    changes.path(&blocked.intended_to),
                    super::blocked_mirror_reason(blocked.reason)
                ),
                4,
                6,
            ));
        }
    }
    lines
}

fn score_lines(profile: &ModeResult, candidate: &Candidate) -> Vec<String> {
    let mut lines = vec![
        "  Score components (delta = candidate minus baseline)".to_owned(),
        format!(
            "  {:<20} {:>12} {:>12} {:>12}",
            "Term", "Baseline", "Candidate", "Delta"
        ),
    ];
    for (name, term) in TERMS {
        let before = term(&profile.current.score_breakdown);
        let after = term(&candidate.score_breakdown);
        lines.push(format!(
            "  {name:<20} {:>12} {:>12} {:>12}",
            f4(before),
            f4(after),
            sf(after - before)
        ));
    }
    for (name, before, after) in [
        (
            "capacity",
            profile.current.score_breakdown.capacity,
            candidate.score_breakdown.capacity,
        ),
        ("score", profile.current.score, candidate.score),
    ] {
        lines.push(format!(
            "  {name:<20} {:>12} {:>12} {:>12}",
            f4(before),
            f4(after),
            sf(after - before)
        ));
    }
    lines.extend(wrap("Terms: cut measures dependency separation; imbalance measures uneven sizes; naming measures name fit; path measures relocation distance; anchor credits existing placement; dependency-only penalizes moving toward dependencies alone; companion-separation penalizes separating named signature companions; capacity penalizes remaining cap breaches.", 2, 2));
    lines.extend(wrap(
        "Columns round independently to four decimals; totals use unrounded values.",
        2,
        2,
    ));
    lines
}
