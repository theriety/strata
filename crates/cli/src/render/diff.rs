//! Candidate diff rendering.

use std::io::{self, Write};

use strata_engine::{BlockedMirrorReason, Candidate, Move, MoveKind, SymbolKind, SymbolMove};

use super::{improvement_summary, nq};

pub(super) fn blocked_mirror_reason(reason: BlockedMirrorReason) -> &'static str {
    match reason {
        BlockedMirrorReason::AmbiguousMapping => "ambiguous mapping",
        BlockedMirrorReason::PackageBoundary => "package boundary",
        BlockedMirrorReason::NamespaceBoundary => "namespace boundary",
        BlockedMirrorReason::Capacity => "capacity",
        BlockedMirrorReason::PathCollision => "path collision",
    }
}

fn moved_file_count(entry: &Move) -> usize {
    entry.files.len().saturating_add(entry.mirrors.len())
}

/// One prose line per symbol relocation (FIX08): what moves, from which file
/// to which file, the objective delta it earned at acceptance, and how many
/// imports must be re-pointed. Full repo-relative paths — symbol moves name no
/// folder to abbreviate against.
pub(crate) fn symbol_move_line(entry: &SymbolMove) -> String {
    let kind = if entry.kind == SymbolKind::Type {
        " type"
    } else {
        ""
    };
    format!(
        " - move{kind} `{}` from {} to {} (delta {:+.4}, {} import(s) to re-point)",
        entry.symbol,
        nq(&entry.from_path),
        nq(&entry.to_path),
        entry.delta,
        entry.broken_imports
    )
}

/// Writes the narrated move list of `candidate` versus the current tree to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_diff(candidate: &Candidate, out: &mut impl Write) -> io::Result<()> {
    let no_symbols = candidate.symbol_moves.is_empty();
    if candidate.delta_narration.is_empty() && no_symbols {
        return writeln!(out, "no moves");
    }
    if candidate.delta_narration.is_empty() {
        writeln!(
            out,
            "symbol moves ({} symbol(s); {}):",
            candidate.symbol_moves.len(),
            improvement_summary(candidate.improvement)
        )?;
    } else {
        let groups = candidate.delta_narration.len();
        let files: usize = candidate.delta_narration.iter().map(moved_file_count).sum();
        writeln!(
            out,
            "moves ({groups} group(s), {files} file(s); {}):",
            improvement_summary(candidate.improvement)
        )?;
        for line in move_step_lines(&candidate.delta_narration) {
            writeln!(out, "{line}")?;
        }
    }
    // FIX08: symbol-grain relocations ride after the whole-file steps so the
    // diff face reports the full plan.
    for entry in &candidate.symbol_moves {
        writeln!(out, "{}", symbol_move_line(entry).trim_start())?;
    }
    Ok(())
}

/// Renders a candidate's moves as numbered per-file steps for `diff`.
///
/// Each group prints a header (`{kind} — {reason}`), then one numbered step per
/// moved file reading `{path} [{from} → {to}]`; step numbers run continuously
/// across the candidate so the whole change reads as one ordered plan. Lines
/// carry no trailing newline. An empty source or destination renders as `(root)`.
fn move_step_lines(moves: &[Move]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut step = 1_usize;
    for entry in moves {
        lines.push(format!("{} — {}", move_tag(entry.kind), entry.reason));
        let to = if entry.to.is_empty() {
            "(root)"
        } else {
            &entry.to
        };
        for file in &entry.files {
            let from = if file.from.is_empty() {
                "(root)"
            } else {
                &file.from
            };
            lines.push(format!("  {step}. {} [{from} → {to}]", file.path));
            step = step.saturating_add(1);
        }
        for mirror in &entry.mirrors {
            lines.push(format!(
                "  {step}. {} [{} → {}] (mirrored test for {})",
                mirror.path, mirror.from, mirror.to, mirror.source_path
            ));
            step = step.saturating_add(1);
        }
        for mirror in &entry.blocked_mirrors {
            lines.push(format!(
                "  blocked: {} [{} → {}] ({})",
                mirror.path,
                mirror.from,
                mirror.intended_to,
                blocked_mirror_reason(mirror.reason)
            ));
        }
    }
    lines
}

/// Returns the lowercase tag of a move kind.
fn move_tag(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Move => "move",
        MoveKind::Split => "split",
        MoveKind::Merge => "merge",
    }
}
