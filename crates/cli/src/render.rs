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
    AnalyzeResult, Candidate, ContainerNode, CurrentStanding, Level, ModeResult, Move, MoveKind,
    MoveReason, Severity, Violation, ViolationKind,
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

    let current_capacity = hard_capacity_count(&result.current.violations);
    let anchored_notice = result
        .modes
        .anchored
        .as_ref()
        .and_then(|mode| infeasible_notice(mode, current_capacity));
    let greenfield_notice = result
        .modes
        .greenfield
        .as_ref()
        .and_then(|mode| infeasible_notice(mode, current_capacity));
    // both modes carrying the identical notice would print it twice; say it
    // once, unindented, above the mode sections instead.
    let shared = anchored_notice.is_some() && anchored_notice == greenfield_notice;
    if shared && let Some(lines) = &anchored_notice {
        for line in lines {
            writeln!(out, "{line}")?;
        }
    }
    if let Some(mode) = &result.modes.anchored {
        let notice = if shared {
            None
        } else {
            anchored_notice.as_deref()
        };
        write_mode_summary("anchored", mode, notice, show_suggestions, out)?;
    }
    if let Some(mode) = &result.modes.greenfield {
        let notice = if shared {
            None
        } else {
            greenfield_notice.as_deref()
        };
        write_mode_summary("greenfield", mode, notice, show_suggestions, out)?;
    }
    Ok(())
}

/// Returns the infeasible-notice lines for a mode, unindented; `None` when the
/// mode's standing is not `Infeasible`.
///
/// The remainder figures come from the mode's own best candidate, so the two
/// modes' notices can genuinely differ — the caller compares them before
/// deciding whether to de-duplicate.
fn infeasible_notice(mode: &ModeResult, current_capacity: u32) -> Option<Vec<String>> {
    if mode.current_standing != CurrentStanding::Infeasible {
        return None;
    }
    let mut lines = Vec::new();
    match mode
        .candidates
        .first()
        .and_then(|candidate| candidate.capacity_remainder)
    {
        Some(capacity) => {
            let resolved = current_capacity.saturating_sub(capacity.remaining);
            lines.push(format!(
                "current layout violates capacity caps; best candidate resolves {resolved} of {current_capacity} capacity finding(s)"
            ));
            if capacity.file_level > 0 {
                lines.push(format!(
                    "{} file-level breach(es) exceed the file cap; only conditional splits can fix them",
                    capacity.file_level
                ));
            }
        }
        None => lines.push("current layout violates capacity caps".to_owned()),
    }
    Some(lines)
}

/// Counts the hard capacity violations of the current layout.
pub fn hard_capacity_count(violations: &[Violation]) -> u32 {
    let count = violations
        .iter()
        .filter(|violation| {
            violation.kind == ViolationKind::Capacity && violation.severity == Severity::Violation
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Writes one mode's candidate headlines to `out`.
///
/// `notice` carries the mode's infeasible-notice lines when they should print
/// inside this section; the caller withholds them (passing `None`) when both
/// modes share one notice printed above the sections.
///
/// When `show_suggestions` is set and the mode has at least one candidate, the
/// best candidate's proposed structure (candidates are best-score-first, so the
/// best is the first) is rendered below the headlines via [`render_tree`].
fn write_mode_summary(
    name: &str,
    mode: &ModeResult,
    notice: Option<&[String]>,
    show_suggestions: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(out, "mode {name}: {} candidate(s)", mode.candidates.len())?;
    if mode.solution_space_converged {
        writeln!(out, "  (fewer than k candidates; solution space converged)")?;
    }
    match mode.current_standing {
        CurrentStanding::Optimal => writeln!(
            out,
            "  current layout is already optimal; candidate 1 is the current tree"
        )?,
        CurrentStanding::Infeasible => {
            for line in notice.into_iter().flatten() {
                writeln!(out, "  {line}")?;
            }
        }
        CurrentStanding::Outscored => {}
    }
    for candidate in &mode.candidates {
        writeln!(
            out,
            "  candidate {} improvement {:+.4} (score {:.4}; {} move group(s))",
            candidate.index,
            candidate.improvement,
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
    let groups = candidate.delta_narration.len();
    let files: usize = candidate
        .delta_narration
        .iter()
        .map(|entry| entry.symbols.len())
        .sum();
    writeln!(
        out,
        "moves ({groups} group(s), {files} file(s); improvement {:+.4}):",
        candidate.improvement
    )?;
    for entry in &candidate.delta_narration {
        write_move(entry, out)?;
    }
    Ok(())
}

/// Move groups larger than this wrap: the header shows a count and the files
/// (or origins) print one per indented line instead of a single joined run.
pub(crate) const MOVE_INLINE_LIMIT: usize = 3;

/// Writes one narrated move entry to `out`.
///
/// `from`/`to` hold one complete folded folder path per element (a merge lists
/// several sources), so they join with a comma, never a path separator. Groups
/// beyond [`MOVE_INLINE_LIMIT`] files (or origins) wrap onto indented lines so
/// a large merge never renders as one unreadable line.
fn write_move(entry: &Move, out: &mut impl Write) -> io::Result<()> {
    let wrap_files = entry.symbols.len() > MOVE_INLINE_LIMIT;
    let wrap_origins = entry.from.len() > MOVE_INLINE_LIMIT;
    let files = if wrap_files {
        format!("{} file(s)", entry.symbols.len())
    } else {
        entry.symbols.join(", ")
    };
    let from = if wrap_origins {
        format!("{} folder(s)", entry.from.len())
    } else {
        entry.from.join(", ")
    };
    let to = entry.to.join(", ");
    writeln!(
        out,
        "{} {} :: {} -> {} ({})",
        move_tag(entry.kind),
        files,
        if from.is_empty() { "(root)" } else { &from },
        if to.is_empty() { "(root)" } else { &to },
        entry.reason
    )?;
    if wrap_origins {
        for origin in &entry.from {
            writeln!(out, "    from {origin}")?;
        }
    }
    if wrap_files {
        for file in &entry.symbols {
            writeln!(out, "    {file}")?;
        }
    }
    if let MoveReason::Follows { subject } = &entry.reason {
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
        CapacityRemainder, ContainerNode, CurrentTree, Modes, ScoreBreakdown, Summary,
        SymbolPlacement,
    };

    use super::*;

    /// Builds an empty-but-valid result with `violations` on its current tree.
    fn result_with(violations: Vec<Violation>) -> AnalyzeResult {
        AnalyzeResult {
            schema_version: 1,
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
                    improvement: 1.25,
                    tree: ContainerNode {
                        name: "proposed".to_owned(),
                        level: Level::Folder,
                        children: Some(vec![file_node("lib", 1)]),
                        symbols: None,
                        production_sloc: None,
                    },
                    conditional_splits: Vec::new(),
                    delta_narration: Vec::new(),
                    capacity_remainder: None,
                }],
                pairwise_distance: Vec::new(),
                solution_space_converged: true,
                current_score: 1.5,
                current_score_breakdown: zero_breakdown(),
                current_standing: CurrentStanding::Outscored,
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
            capacity: None,
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
            improvement: 0.0,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: Vec::new(),
            capacity_remainder: None,
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
            improvement: 1.25,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Move,
                symbols: vec!["alpha".to_owned()],
                from: vec!["old".to_owned()],
                to: vec!["new".to_owned()],
                reason: MoveReason::Clustering,
            }],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(
            text,
            "moves (1 group(s), 1 file(s); improvement +1.2500):\nmove alpha :: old -> new (regrouped by clustering)\n"
        );
    }

    #[test]
    fn should_print_the_optimal_notice_when_the_current_layout_wins() {
        let mut result = result_with_candidate();
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Optimal;
        }
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains("current layout is already optimal; candidate 1 is the current tree")
        );
    }

    #[test]
    fn should_print_the_infeasible_notice_with_the_resolved_capacity_count() {
        // two hard capacity findings today; the best candidate leaves one, a
        // file-level breach only a conditional split can fix.
        let capacity = |name: &str| Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: vec![name.to_owned()],
            detail: format!("{name} over cap"),
            break_suggestions: None,
            capacity: None,
        };
        let mut result = result_with_candidate();
        result.current.violations = vec![capacity("big_folder"), capacity("huge_file")];
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
            if let Some(candidate) = mode.candidates.first_mut() {
                candidate.capacity_remainder = Some(CapacityRemainder {
                    remaining: 1,
                    file_level: 1,
                });
            }
        }
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains(
            "current layout violates capacity caps; best candidate resolves 1 of 2 capacity finding(s)"
        ));
        assert!(text.contains(
            "1 file-level breach(es) exceed the file cap; only conditional splits can fix them"
        ));
    }

    #[test]
    fn should_print_the_plain_infeasible_notice_when_the_remainder_is_absent() {
        // a saved result from an older run has no capacityRemainder field;
        // the notice degrades to the plain sentence instead of inventing numbers.
        let mut result = result_with_candidate();
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
            if let Some(candidate) = mode.candidates.first_mut() {
                candidate.capacity_remainder = None;
            }
        }
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("current layout violates capacity caps\n"));
        assert!(!text.contains("resolves"));
    }

    #[test]
    fn should_print_candidate_improvement_in_the_summary() {
        let result = result_with_candidate();
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("candidate 1 improvement +1.2500 (score 0.2500; 0 move group(s))"));
    }

    #[test]
    fn should_wrap_a_large_move_group_with_one_line_per_file() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Merge,
                symbols: vec![
                    "a.ts".to_owned(),
                    "b.ts".to_owned(),
                    "c.ts".to_owned(),
                    "d.ts".to_owned(),
                ],
                from: vec!["old".to_owned()],
                to: vec!["new".to_owned()],
                reason: MoveReason::Clustering,
            }],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains("merge 4 file(s) :: old -> new (regrouped by clustering)\n"),
            "the header counts instead of joining: {text}"
        );
        for file in ["a.ts", "b.ts", "c.ts", "d.ts"] {
            assert!(text.contains(&format!("\n    {file}\n")), "{file}: {text}");
        }
    }

    #[test]
    fn should_summarize_many_origins_with_a_folder_count() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Merge,
                symbols: vec!["a.ts".to_owned()],
                from: vec![
                    "one".to_owned(),
                    "two".to_owned(),
                    "three".to_owned(),
                    "four".to_owned(),
                ],
                to: vec!["new".to_owned()],
                reason: MoveReason::Clustering,
            }],
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains("merge a.ts :: 4 folder(s) -> new (regrouped by clustering)\n"),
            "the header counts the origins: {text}"
        );
        for origin in ["one", "two", "three", "four"] {
            assert!(
                text.contains(&format!("\n    from {origin}\n")),
                "{origin}: {text}"
            );
        }
    }

    #[test]
    fn should_print_a_shared_infeasible_notice_once_when_both_modes_match() {
        let capacity = |name: &str| Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: vec![name.to_owned()],
            detail: format!("{name} over cap"),
            break_suggestions: None,
            capacity: None,
        };
        let mut result = result_with_candidate();
        result.current.violations = vec![capacity("big_folder"), capacity("huge_file")];
        let infeasible = |mode: &mut ModeResult| {
            mode.current_standing = CurrentStanding::Infeasible;
            if let Some(candidate) = mode.candidates.first_mut() {
                candidate.capacity_remainder = Some(CapacityRemainder {
                    remaining: 1,
                    file_level: 1,
                });
            }
        };
        result.modes.greenfield = result.modes.anchored.clone();
        if let Some(mode) = result.modes.anchored.as_mut() {
            infeasible(mode);
        }
        if let Some(mode) = result.modes.greenfield.as_mut() {
            infeasible(mode);
        }
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(
            text.matches("current layout violates capacity caps")
                .count(),
            1,
            "the shared notice prints once: {text}"
        );
        assert!(
            text.contains(
                "\ncurrent layout violates capacity caps; best candidate resolves 1 of 2 capacity finding(s)\n"
            ),
            "the shared notice is unindented: {text}"
        );
    }

    #[test]
    fn should_keep_per_mode_notices_when_the_remainders_differ() {
        let capacity = |name: &str| Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: vec![name.to_owned()],
            detail: format!("{name} over cap"),
            break_suggestions: None,
            capacity: None,
        };
        let mut result = result_with_candidate();
        result.current.violations = vec![capacity("big_folder"), capacity("huge_file")];
        result.modes.greenfield = result.modes.anchored.clone();
        if let Some(mode) = result.modes.anchored.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
            if let Some(candidate) = mode.candidates.first_mut() {
                candidate.capacity_remainder = Some(CapacityRemainder {
                    remaining: 1,
                    file_level: 1,
                });
            }
        }
        if let Some(mode) = result.modes.greenfield.as_mut() {
            mode.current_standing = CurrentStanding::Infeasible;
            if let Some(candidate) = mode.candidates.first_mut() {
                candidate.capacity_remainder = Some(CapacityRemainder {
                    remaining: 0,
                    file_level: 0,
                });
            }
        }
        let mut buffer = Vec::new();

        render(&result, Format::Summary, false, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains(
                "  current layout violates capacity caps; best candidate resolves 1 of 2"
            ),
            "the anchored notice stays in its section: {text}"
        );
        assert!(
            text.contains(
                "  current layout violates capacity caps; best candidate resolves 2 of 2"
            ),
            "the greenfield notice stays in its section: {text}"
        );
    }
}
