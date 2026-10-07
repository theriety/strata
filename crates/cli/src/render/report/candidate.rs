//! Candidate report lines: proposed file and symbol moves, splits, and impact.

use strata_engine::{AnalyzeResult, Candidate};

use crate::render::advice::caption_for;
use crate::render::changes::Changes;
use crate::render::limit::{more_line, take_worst};
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
    let mut moves = file_move_items(candidate, &changes);
    for entry in &candidate.symbol_moves {
        let kind = if entry.kind == strata_engine::SymbolKind::Type {
            "type"
        } else {
            "symbol"
        };
        let mut item = wrap(
            &format!(
                "- Move {kind} `{}` from `{}` → `{}`",
                entry.label(),
                entry.from_path,
                changes.after_path(&entry.to_path)
            ),
            4,
            6,
        );
        if options.verbose {
            item.push(format!(
                "      Objective delta {}; {} imports to repoint.",
                sf(entry.delta),
                entry.broken_imports
            ));
        }
        moves.items.push(MoveItem {
            lines: item,
            group: None,
            closes_group: false,
        });
    }
    // Moves are shown in the engine's plan order; they carry no ranking of their own.
    let (shown, hidden) = take_worst(&moves.items, options, |_| ());
    lines.extend(moves.render(&shown));
    lines.extend(more_line(hidden, 4));
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

/// One displayed move line: a file move, a mirrored test move, or a symbol move.
struct MoveItem {
    lines: Vec<String>,
    /// The narration group this line belongs to, when it has a shared reason.
    group: Option<usize>,
    /// Whether the group's reason caption follows this line when it is shown.
    closes_group: bool,
}

/// The move lines in plan order plus the reason caption of each narration group.
struct MoveList {
    items: Vec<MoveItem>,
    captions: Vec<Vec<String>>,
}

impl MoveList {
    /// Returns the reason caption lines of `group`.
    fn caption(&self, group: usize) -> impl Iterator<Item = String> + '_ {
        self.captions.get(group).into_iter().flatten().cloned()
    }

    /// Renders the shown items, printing each group's caption once after the
    /// group's last shown file line, even when the cap cuts the group short.
    fn render(&self, shown: &[&MoveItem]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut open = None;
        for item in shown {
            lines.extend(item.lines.iter().cloned());
            open = item.group;
            if item.closes_group
                && let Some(group) = item.group
            {
                lines.extend(self.caption(group));
                open = None;
            }
        }
        if let Some(group) = open {
            // the cap cut this group before its last file line; caption the shown part.
            lines.extend(self.caption(group));
        }
        lines
    }
}

/// One item per file move and per mirrored test file, in plan order.
fn file_move_items(candidate: &Candidate, changes: &Changes) -> MoveList {
    let mut list = MoveList {
        items: Vec::new(),
        captions: Vec::new(),
    };
    for entry in &candidate.delta_narration {
        let group = list.captions.len();
        let last = entry.files.len().saturating_sub(1);
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
        let caption = wrap(&caption_for(&reason), 6, 6);
        // an entry with no files emits its caption as a standalone item.
        if entry.files.is_empty() {
            list.items.push(MoveItem {
                lines: caption,
                group: None,
                closes_group: false,
            });
        } else {
            list.captions.push(caption);
        }
        for (index, file) in entry.files.iter().enumerate() {
            list.items.push(MoveItem {
                lines: wrap(
                    &format!(
                        "- Move file `{}` → `{}`",
                        changes.path(&file.path),
                        changes.destination(&file.path, &entry.to)
                    ),
                    4,
                    6,
                ),
                group: Some(group),
                closes_group: index == last,
            });
        }
        for mirror in &entry.mirrors {
            list.items.push(MoveItem {
                lines: wrap(
                    &format!(
                        "- Move mirrored test file `{}` → `{}`; follows `{}`",
                        changes.path(&mirror.path),
                        changes.destination(&mirror.path, &mirror.to),
                        changes.path(&mirror.source_path)
                    ),
                    4,
                    6,
                ),
                // a mirror follows its group's caption, so it never reopens the group.
                group: None,
                closes_group: false,
            });
        }
    }
    list
}
