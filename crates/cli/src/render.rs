//! TTY-aware rendering of an [`AnalyzeResult`] into human-readable text.
//!
//! Rendering is a pure function of the result and an output sink: the same result
//! always renders the same bytes, and `--format json` bypasses this module
//! entirely (the JSON path serializes the library value verbatim, the AD-5
//! parity guarantee). The renderers here produce plain ASCII — trees with simple
//! prefixes and aligned tables — so piping a rendered view into a file is stable
//! and diffable. Color is intentionally not emitted: a deterministic, redirect-safe
//! byte stream is worth more than terminal escapes the goldens would have to strip.

use std::io::{self, Write};

use strata_engine::{
    AnalyzeResult, Candidate, ContainerNode, Level, ModeResult, Move, MoveKind, Severity,
    Violation, ViolationKind,
};

/// The output format the `analyze` command renders in.
///
/// `Summary` is the human-readable text face; `Json` is the serialized library
/// result, the AD-5 parity face. The `violations` command carries its own table
/// face so the two surfaces never share a variant they do not both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The default human-readable analysis summary.
    Summary,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

impl Format {
    /// Resolves the effective format from an optional explicit choice and whether
    /// the sink is a terminal.
    ///
    /// With no explicit choice the format is TTY-aware: a terminal gets the
    /// human-readable `default_human` face, a pipe gets [`Format::Json`]. An
    /// explicit choice always wins.
    #[must_use]
    pub fn resolve(explicit: Option<Format>, is_terminal: bool, default_human: Format) -> Format {
        match explicit {
            Some(format) => format,
            None if is_terminal => default_human,
            None => Format::Json,
        }
    }
}

/// Renders the analysis `result` in `format` to `out`.
///
/// The JSON face is the parity-pinned serialization of the library result; the
/// summary face prints the snapshot census, the current-tree score and
/// violations, and the best candidate per mode. The table face is the violation
/// listing used by the `violations` command.
///
/// `show_suggestions` only affects the summary face: when set, each mode's best
/// candidate has its proposed structure rendered below the candidate headlines.
/// The JSON face already serializes every candidate's tree, so the flag is a
/// no-op there.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if writing to `out` fails.
pub fn render(
    result: &AnalyzeResult,
    format: Format,
    show_suggestions: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    match format {
        Format::Json => write_json(result, out),
        Format::Summary => write_summary(result, show_suggestions, out),
    }
}

/// Serializes `result` as pretty JSON to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if serialization or writing fails.
pub fn write_json(result: &AnalyzeResult, out: &mut impl Write) -> io::Result<()> {
    let json = serde_json::to_vec(result).map_err(io::Error::other)?;
    out.write_all(&json)?;
    out.write_all(b"\n")
}

/// Writes the human-readable analysis summary to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
fn write_summary(
    result: &AnalyzeResult,
    show_suggestions: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(out, "snapshot {}", result.snapshot_hash)?;
    writeln!(
        out,
        "summary: {} symbols, {} edges, {} files",
        result.summary.symbols, result.summary.edges, result.summary.files
    )?;
    writeln!(out, "current score: {:.4}", result.current.score)?;
    write_violation_table(&result.current.violations, out)?;

    if let Some(mode) = &result.modes.anchored {
        write_mode_summary("anchored", mode, show_suggestions, out)?;
    }
    if let Some(mode) = &result.modes.greenfield {
        write_mode_summary("greenfield", mode, show_suggestions, out)?;
    }
    Ok(())
}

/// Writes one mode's candidate headlines to `out`.
///
/// When `show_suggestions` is set and the mode has at least one candidate, the
/// best candidate's proposed structure (candidates are best-score-first, so the
/// best is the first) is rendered below the headlines via [`render_tree`].
fn write_mode_summary(
    name: &str,
    mode: &ModeResult,
    show_suggestions: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(out, "mode {name}: {} candidate(s)", mode.candidates.len())?;
    if !mode.solution_space_converged {
        writeln!(out, "  (fewer than k candidates; solution space converged)")?;
    }
    for candidate in &mode.candidates {
        writeln!(
            out,
            "  candidate {} score {:.4} ({} move group(s))",
            candidate.index,
            candidate.score,
            candidate.delta_narration.len()
        )?;
    }
    if show_suggestions && let Some(best) = mode.candidates.first() {
        writeln!(
            out,
            "  suggested structure (candidate {}, score {:.4}):",
            best.index, best.score
        )?;
        render_tree(&best.tree, false, None, out)?;
    }
    Ok(())
}

/// Renders one container `node` as an indented tree to `out`.
///
/// `symbols` lists each file's symbols with their derived visibility; `depth`
/// truncates the tree below the given container depth (the root is depth zero),
/// and `None` renders the full tree.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_tree(
    node: &ContainerNode,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    write_tree_node(node, 0, symbols, depth, out)
}

/// Writes one tree node at `indent` and recurses into its children.
fn write_tree_node(
    node: &ContainerNode,
    indent: u32,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    let pad = "  ".repeat(indent as usize);
    match node.production_sloc {
        Some(sloc) => writeln!(
            out,
            "{pad}{} [{}] {sloc} sloc",
            node.name,
            level_tag(node.level)
        )?,
        None => writeln!(out, "{pad}{} [{}]", node.name, level_tag(node.level))?,
    }

    if let Some(placements) = node.symbols.as_ref().filter(|_| symbols) {
        for placement in placements {
            writeln!(
                out,
                "{pad}  - {} ({})",
                placement.name,
                level_tag(placement.visibility)
            )?;
        }
    }

    if depth.is_some_and(|limit| indent + 1 > limit) {
        return Ok(());
    }
    if let Some(children) = &node.children {
        for child in children {
            write_tree_node(child, indent + 1, symbols, depth, out)?;
        }
    }
    Ok(())
}

/// Writes the narrated move list of `candidate` versus the current tree to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_diff(candidate: &Candidate, out: &mut impl Write) -> io::Result<()> {
    if candidate.delta_narration.is_empty() {
        return writeln!(out, "no moves");
    }
    for entry in &candidate.delta_narration {
        write_move(entry, out)?;
    }
    Ok(())
}

/// Writes one narrated move entry to `out`.
fn write_move(entry: &Move, out: &mut impl Write) -> io::Result<()> {
    let from = entry.from.join("/");
    let to = entry.to.join("/");
    writeln!(
        out,
        "{} {} :: {} -> {} ({})",
        move_tag(entry.kind),
        entry.symbols.join(", "),
        if from.is_empty() { "(root)" } else { &from },
        if to.is_empty() { "(root)" } else { &to },
        entry.reason
    )?;
    if let Some(subject) = &entry.follows_subject {
        writeln!(out, "    follows {subject}")?;
    }
    Ok(())
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

/// Returns the lowercase tag of a scope level.
fn level_tag(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "packageGroup",
    }
}

/// Returns the lowercase tag of a move kind.
fn move_tag(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Move => "move",
        MoveKind::Split => "split",
        MoveKind::Merge => "merge",
    }
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

#[cfg(test)]
mod tests {
    use strata_engine::{
        ContainerNode, CurrentTree, Modes, ScoreBreakdown, Summary, SymbolPlacement,
    };

    use super::*;

    /// Builds an empty-but-valid result with `violations` on its current tree.
    fn result_with(violations: Vec<Violation>) -> AnalyzeResult {
        AnalyzeResult {
            snapshot_hash: "deadbeef".to_owned(),
            summary: Summary {
                symbols: 2,
                edges: 1,
                files: 1,
                files_by_language: std::collections::BTreeMap::new(),
            },
            current: CurrentTree {
                tree: file_node("lib", 2),
                score: 1.5,
                score_breakdown: zero_breakdown(),
                violations,
            },
            modes: Modes::default(),
        }
    }

    /// Builds a result whose anchored mode carries one candidate whose tree has a
    /// `proposed` container, for exercising the `--show-suggestions` face.
    fn result_with_candidate() -> AnalyzeResult {
        let mut result = result_with(Vec::new());
        result.modes = Modes {
            anchored: Some(ModeResult {
                candidates: vec![Candidate {
                    index: 1,
                    score: 0.25,
                    score_breakdown: zero_breakdown(),
                    tree: ContainerNode {
                        name: "proposed".to_owned(),
                        level: Level::Folder,
                        children: Some(vec![file_node("lib", 1)]),
                        symbols: None,
                        production_sloc: None,
                    },
                    conditional_splits: Vec::new(),
                    delta_narration: Vec::new(),
                }],
                pairwise_distance: Vec::new(),
                solution_space_converged: true,
            }),
            greenfield: None,
        };
        result
    }

    /// Builds a zeroed score breakdown.
    fn zero_breakdown() -> ScoreBreakdown {
        ScoreBreakdown {
            cut: 0.0,
            imbalance: 0.0,
            naming: 0.0,
            path: 0.0,
            anchor: 0.0,
        }
    }

    /// Builds a file container node with `sloc` symbols.
    fn file_node(name: &str, sloc: u32) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(vec![SymbolPlacement {
                name: "thing".to_owned(),
                visibility: Level::File,
            }]),
            production_sloc: Some(sloc),
        }
    }

    /// Builds a sample cycle violation.
    fn cycle() -> Violation {
        Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["a".to_owned(), "b".to_owned()],
            detail: "dependency cycle".to_owned(),
            break_suggestions: None,
        }
    }

    #[test]
    fn should_resolve_to_json_when_piped_without_an_explicit_format() {
        assert_eq!(Format::resolve(None, false, Format::Summary), Format::Json);
    }

    #[test]
    fn should_resolve_to_the_human_face_on_a_terminal() {
        assert_eq!(
            Format::resolve(None, true, Format::Summary),
            Format::Summary
        );
    }

    #[test]
    fn should_honour_an_explicit_format_over_the_tty_default() {
        assert_eq!(
            Format::resolve(Some(Format::Json), true, Format::Summary),
            Format::Json
        );
    }

    #[test]
    fn should_render_json_byte_identically_to_the_serialized_result() {
        let result = result_with(Vec::new());
        let mut buffer = Vec::new();

        write_json(&result, &mut buffer).unwrap_or_default();

        let mut expected = serde_json::to_vec(&result).unwrap_or_default();
        expected.push(b'\n');
        assert_eq!(buffer, expected);
    }

    #[test]
    fn should_report_no_violations_when_the_list_is_empty() {
        let mut buffer = Vec::new();

        write_violation_table(&[], &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no violations\n");
    }

    #[test]
    fn should_tabulate_a_violation_with_its_kind_and_severity() {
        let mut buffer = Vec::new();

        write_violation_table(&[cycle()], &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "cycle [violation] a, b :: dependency cycle\n");
    }

    #[test]
    fn should_render_a_tree_with_symbols_and_visibility() {
        let mut buffer = Vec::new();

        render_tree(&file_node("lib", 1), true, None, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "lib [file] 1 sloc\n  - thing (file)\n");
    }

    #[test]
    fn should_truncate_a_tree_below_the_requested_depth() {
        let root = ContainerNode {
            name: "root".to_owned(),
            level: Level::Folder,
            children: Some(vec![file_node("child", 1)]),
            symbols: None,
            production_sloc: None,
        };
        let mut buffer = Vec::new();

        render_tree(&root, false, Some(0), &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "root [folder]\n");
    }

    #[test]
    fn should_render_a_summary_with_the_snapshot_and_score() {
        let result = result_with(vec![cycle()]);
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("snapshot deadbeef"));
        assert!(text.contains("current score: 1.5000"));
        assert!(text.contains("cycle [violation]"));
    }

    #[test]
    fn should_render_the_best_candidate_structure_when_suggestions_are_shown() {
        let result = result_with_candidate();
        let mut buffer = Vec::new();

        render(&result, Format::Summary, true, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("suggested structure (candidate 1"));
        assert!(text.contains("proposed"));
    }

    #[test]
    fn should_omit_the_candidate_structure_when_suggestions_are_not_shown() {
        let result = result_with_candidate();
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(!text.contains("suggested structure"));
        assert!(!text.contains("proposed"));
    }

    #[test]
    fn should_report_no_moves_for_an_empty_narration() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: Vec::new(),
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no moves\n");
    }

    #[test]
    fn should_narrate_a_move_with_its_from_and_to_paths() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Move,
                symbols: vec!["alpha".to_owned()],
                from: vec!["old".to_owned()],
                to: vec!["new".to_owned()],
                reason: "cohesion gain".to_owned(),
                follows_subject: None,
            }],
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "move alpha :: old -> new (cohesion gain)\n");
    }
}
