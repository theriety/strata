//! Candidate report lines: proposed file and symbol moves, splits, and impact.

use unlace_engine::{AnalyzeResult, Candidate, TermShift};

use crate::render::advice::caption_for;
use crate::render::changes::{Changes, destination};
use crate::render::limit::{more_line, take_worst};
use crate::render::{RenderOptions, TERMS, Term, nq, sf, wrap};

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
    let mut moves = file_move_items(candidate);
    for entry in &candidate.symbol_moves {
        let kind = if entry.kind == unlace_engine::SymbolKind::Type {
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
            item.extend(entry.terms.as_ref().into_iter().flat_map(pays_lines));
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

/// The smallest term change a `pays:` line names; smaller shifts are float
/// dust.
const MIN_PAID_TERM: f64 = 1e-6;

/// The verbose `pays:` lines of one symbol move: every term whose change
/// across the acceptance reaches [`MIN_PAID_TERM`], largest first, wrapped
/// like the neighbouring report lines with continuation lines aligned under
/// the first term; empty when no term changes.
fn pays_lines(shift: &TermShift) -> Vec<String> {
    let deltas = shift.deltas();
    let capacity: Term = |breakdown| breakdown.capacity;
    let mut paid: Vec<(&str, f64)> = TERMS
        .iter()
        .copied()
        .chain([("capacity", capacity)])
        .map(|(name, term)| (name, term(&deltas)))
        .filter(|(_, change)| change.abs() >= MIN_PAID_TERM)
        .collect();
    // larger |Δ| first; equal magnitudes fall back to the term name.
    paid.sort_by(|left, right| {
        right
            .1
            .abs()
            .total_cmp(&left.1.abs())
            .then_with(|| left.0.cmp(right.0))
    });
    let terms: Vec<String> = paid
        .iter()
        .map(|(name, change)| format!("{name} {}", term_change(*change)))
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    // "      pays: " is 12 columns, so continuations start under the first term.
    wrap(&format!("pays: {}", terms.join(", ")), 6, 12)
}

/// A signed term change at up to six decimals, trailing zeros trimmed, so a
/// micro-shift still reads as non-zero.
fn term_change(change: f64) -> String {
    let text = format!("{change:+.6}");
    let trimmed = text.trim_end_matches('0');
    trimmed
        .strip_suffix('.')
        .map_or_else(|| trimmed.to_owned(), |whole| format!("{whole}.0"))
}

/// One item per file move and per mirrored test file, in plan order.
fn file_move_items(candidate: &Candidate) -> MoveList {
    let mut list = MoveList {
        items: Vec::new(),
        captions: Vec::new(),
    };
    for entry in &candidate.delta_narration {
        let group = list.captions.len();
        let last = entry.files.len().saturating_sub(1);
        let caption = wrap(&caption_for(&entry.reason), 6, 6);
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
                        file.path,
                        destination(&file.path, &entry.to)
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
                        mirror.path,
                        destination(&mirror.path, &mirror.to),
                        mirror.source_path
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

#[cfg(test)]
mod tests {
    use unlace_engine::ScoreBreakdown;

    use super::*;

    fn breakdown(cut: f64, entry_inflow: f64, naming: f64) -> ScoreBreakdown {
        ScoreBreakdown {
            cut,
            imbalance: 0.0,
            naming,
            path: 0.0,
            anchor: 0.0,
            capacity: 0.0,
            dependency_only: 0.0,
            companion_separation: 0.0,
            entry_inflow,
            unit_outflow: 0.0,
        }
    }

    #[test]
    fn should_name_paying_terms_largest_first_and_drop_float_dust() {
        let shift = TermShift {
            before: breakdown(0.5, 0.0, -0.1),
            after: breakdown(0.488, 0.05, -0.100_000_1),
        };

        let lines = pays_lines(&shift);

        assert_eq!(lines, ["      pays: entry-inflow +0.05, cut -0.012"]);
    }

    #[test]
    fn should_print_no_pays_line_when_every_term_is_unchanged() {
        let shift = TermShift {
            before: breakdown(0.5, 0.0, 0.0),
            after: breakdown(0.5, 0.0, 0.0),
        };

        assert!(pays_lines(&shift).is_empty());
    }

    #[test]
    fn should_wrap_a_long_pays_line_under_the_first_term() {
        let shift = TermShift {
            before: breakdown(0.0, 0.0, 0.0),
            after: ScoreBreakdown {
                imbalance: 0.4,
                path: 0.3,
                anchor: 0.2,
                capacity: 0.1,
                dependency_only: 0.05,
                companion_separation: 0.04,
                unit_outflow: 0.03,
                ..breakdown(0.9, 0.8, 0.7)
            },
        };

        let lines = pays_lines(&shift);

        assert!(lines.len() >= 2, "expected a wrap, got {lines:?}");
        for (index, line) in lines.iter().enumerate() {
            assert!(line.chars().count() <= 99, "line too long: {line:?}");
            let indent = if index == 0 {
                "      pays: "
            } else {
                "            "
            };
            let rest = line.strip_prefix(indent);
            assert!(
                rest.is_some_and(|rest| !rest.starts_with(' ')),
                "misaligned: {line:?}"
            );
        }
        let joined = lines
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.starts_with("pays: cut +0.9, entry-inflow +0.8, naming +0.7"));
        assert!(joined.ends_with("unit-outflow +0.03"));
    }
}
