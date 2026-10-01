//! Candidate report lines: proposed file and symbol moves, splits, and impact.

use strata_engine::{AnalyzeResult, Candidate};

use crate::render::advice::caption_for;
use crate::render::changes::Changes;
use crate::render::diff::blocked_mirror_reason;
use crate::render::{RenderOptions, nq, sf, wrap};

pub(super) fn candidate_lines(
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
                    blocked_mirror_reason(blocked.reason)
                ),
                4,
                6,
            ));
        }
    }
    lines
}
