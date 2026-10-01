//! Per-profile report lines: baseline standing, candidate scores, and score tables.

use strata_engine::{AnalyzeResult, Candidate, CurrentStanding, ModeResult};

use super::candidate::candidate_lines;
use crate::render::{RenderOptions, TERMS, f4, sf, wrap};

pub(super) fn profile_lines(
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
