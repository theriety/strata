//! `strata report`: a saved analysis rendered as a Markdown report.
//!
//! The command reads an [`AnalyzeResult`] JSON and renders the full narrative —
//! the snapshot census, the current-tree violations and score, then each mode's
//! candidates with their score breakdowns, conditional splits (and the
//! preconditions that make them legal), and delta narration. The report is plain
//! Markdown so it drops straight into a PR or a docs site.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use strata_engine::{
    AnalyzeResult, Candidate, CurrentStanding, ModeResult, ScoreBreakdown, StrataError, Violation,
};

use crate::commands::read_result;
use crate::render::effective_parameter_lines;

/// The parsed inputs of a `report` run.
#[derive(Debug)]
pub struct ReportArgs {
    /// The saved `AnalyzeResult` JSON to read.
    pub input: PathBuf,
    /// The Markdown file to write; absent writes to the provided sink.
    pub output: Option<PathBuf>,
}

/// Runs `report`, writing the Markdown to `output` (if set) or to `out`.
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when the input cannot be read or the
/// output cannot be written.
pub fn run(args: &ReportArgs, out: &mut impl Write) -> Result<(), StrataError> {
    let result = read_result(&args.input)?;
    let markdown = render_markdown(&result);

    match &args.output {
        Some(path) => std::fs::write(path, markdown.as_bytes()).map_err(|error| {
            StrataError::InputUnreadable {
                path: path.clone(),
                reason: error.to_string(),
            }
        }),
        None => out
            .write_all(markdown.as_bytes())
            .map_err(|error| StrataError::InputUnreadable {
                path: PathBuf::from("<stdout>"),
                reason: error.to_string(),
            }),
    }
}

/// Renders the full Markdown report of `result`.
fn render_markdown(result: &AnalyzeResult) -> String {
    let mut markdown = String::new();
    let _ = writeln!(markdown, "# Strata report\n");
    let _ = writeln!(markdown, "Snapshot `{}`.\n", result.snapshot_hash);
    let _ = writeln!(
        markdown,
        "- {} symbols, {} edges, {} files\n",
        result.summary.symbols, result.summary.edges, result.summary.files
    );

    let _ = writeln!(markdown, "## Shared findings\n");
    write_violations(&mut markdown, &result.current.shared_findings);

    if let Some(mode) = &result.profiles.anchored {
        write_mode(&mut markdown, "Anchored", mode);
    }
    if let Some(mode) = &result.profiles.greenfield {
        write_mode(&mut markdown, "Greenfield", mode);
    }
    markdown
}

/// Writes the violation listing section.
fn write_violations(markdown: &mut String, violations: &[Violation]) {
    let _ = writeln!(markdown, "### Violations\n");
    if violations.is_empty() {
        let _ = writeln!(markdown, "None.\n");
        return;
    }
    for violation in violations {
        let _ = writeln!(
            markdown,
            "- **{:?}** ({:?}) at {}: {}",
            violation.kind,
            violation.severity,
            violation.location.join(", "),
            violation.detail
        );
    }
    let _ = writeln!(markdown);
}

/// Writes one mode's candidate sections.
fn write_mode(markdown: &mut String, name: &str, mode: &ModeResult) {
    let _ = writeln!(markdown, "## {name} parameter profile\n");
    let _ = writeln!(markdown, "### Effective parameters\n");
    let _ = writeln!(markdown, "```text");
    for line in effective_parameter_lines(&name.to_ascii_lowercase(), &mode.parameters) {
        let _ = writeln!(markdown, "{}", line.trim_start());
    }
    let _ = writeln!(markdown, "```\n");
    let _ = writeln!(markdown, "Current score `{:.4}`.\n", mode.current.score);
    write_breakdown(markdown, &mode.current.score_breakdown);
    let _ = writeln!(markdown, "### Profile-specific findings\n");
    write_violations(markdown, &mode.current.unique_findings);
    let _ = writeln!(markdown, "### Candidates\n");
    if mode.solution_space_converged {
        let _ = writeln!(
            markdown,
            "_Fewer than the requested candidates survived; the solution space converged._\n"
        );
    }
    match mode.current.standing {
        CurrentStanding::Optimal => {
            let _ = writeln!(
                markdown,
                "_Current layout is already optimal; candidate 1 is the current tree._\n"
            );
        }
        CurrentStanding::Infeasible => match mode
            .candidates
            .first()
            .and_then(|candidate| candidate.capacity_remainder)
        {
            Some(capacity) => {
                let current_capacity = mode.current.capacity_breaks;
                let resolved = current_capacity.saturating_sub(capacity.remaining);
                let _ = writeln!(
                    markdown,
                    "_Current layout violates capacity caps; best candidate resolves {resolved} of {current_capacity} capacity finding(s)._\n"
                );
                if capacity.file_level > 0 {
                    let _ = writeln!(
                        markdown,
                        "_{} file-level breach(es) exceed the file cap; only conditional splits can fix them._\n",
                        capacity.file_level
                    );
                }
            }
            None => {
                let _ = writeln!(markdown, "_Current layout violates capacity caps._\n");
            }
        },
        CurrentStanding::Outscored => {}
    }
    for candidate in &mode.candidates {
        write_candidate(markdown, candidate);
    }
}

/// Writes one candidate's score, splits, and narration.
fn write_candidate(markdown: &mut String, candidate: &Candidate) {
    let _ = writeln!(
        markdown,
        "### Candidate {} (improvement `{:+.4}`, score `{:.4}`)\n",
        candidate.index, candidate.improvement, candidate.score
    );
    write_breakdown(markdown, &candidate.score_breakdown);

    if !candidate.conditional_splits.is_empty() {
        let _ = writeln!(markdown, "**Conditional splits**\n");
        for split in &candidate.conditional_splits {
            let _ = writeln!(
                markdown,
                "- SCC `{}` -> {} files once {} edge(s) are broken",
                split.scc.join(", "),
                split.resulting_files,
                split.preconditions.len()
            );
        }
        let _ = writeln!(markdown);
    }

    if candidate.delta_narration.is_empty() {
        let _ = writeln!(markdown, "No moves versus the current layout.\n");
        return;
    }
    // the same numbered per-file steps the `diff` face prints, fenced so the
    // Markdown renders them monospaced without re-wrapping the arrows.
    let _ = writeln!(markdown, "**Moves**\n");
    let _ = writeln!(markdown, "```");
    for line in crate::render::move_step_lines(&candidate.delta_narration) {
        let _ = writeln!(markdown, "{line}");
    }
    let _ = writeln!(markdown, "```\n");
}

/// Writes a per-term score breakdown bullet list.
fn write_breakdown(markdown: &mut String, breakdown: &ScoreBreakdown) {
    let _ = writeln!(
        markdown,
        "- cut `{:.4}`, imbalance `{:.4}`, naming `{:.4}`, path `{:.4}`, anchor `{:.4}`, dependency-only `{:.4}`\n",
        breakdown.cut,
        breakdown.imbalance,
        breakdown.naming,
        breakdown.path,
        breakdown.anchor,
        breakdown.dependency_only
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use strata_engine::{
        ConditionalSplit, ContainerNode, CurrentTree, EdgeBreak, FileMove, Level, Modes, Move,
        MoveKind, MoveReason, ProfileConfig, ProfileCurrent, Severity, Summary, ViolationKind,
    };

    use super::*;

    /// Builds a result with one anchored candidate and a cycle violation.
    fn sample() -> AnalyzeResult {
        AnalyzeResult {
            schema_version: strata_engine::RESULT_SCHEMA_VERSION,
            snapshot_hash: "abc123".to_owned(),
            summary: Summary {
                symbols: 3,
                edges: 2,
                files: 1,
                files_by_language: BTreeMap::new(),
            },
            current: CurrentTree {
                tree: file("lib"),
                shared_findings: vec![Violation {
                    kind: ViolationKind::Cycle,
                    severity: Severity::Violation,
                    location: vec!["a".to_owned()],
                    detail: "cycle".to_owned(),
                    break_suggestions: None,
                    capacity: None,
                }],
            },
            profiles: Modes {
                anchored: Some(ModeResult {
                    parameters: ProfileConfig::default(),
                    current: ProfileCurrent {
                        score: 2.0,
                        score_breakdown: zero(),
                        unique_findings: Vec::new(),
                        standing: CurrentStanding::Outscored,
                        capacity_breaks: 0,
                    },
                    candidates: vec![candidate()],
                    pairwise_distance: vec![vec![0.0]],
                    // one candidate against a default k of three: the space converged,
                    // so the report prints the fewer-than-requested notice.
                    solution_space_converged: true,
                }),
                greenfield: None,
            },
        }
    }

    /// Builds a candidate with a split and a move.
    fn candidate() -> Candidate {
        Candidate {
            index: 1,
            score: 1.0,
            score_breakdown: zero(),
            improvement: 1.0,
            tree: file("lib"),
            conditional_splits: vec![ConditionalSplit {
                scc: vec!["x".to_owned(), "y".to_owned()],
                preconditions: vec![EdgeBreak {
                    source: "x".to_owned(),
                    target: "y".to_owned(),
                    weight: 1.0,
                    exact: true,
                }],
                resulting_files: 2,
            }],
            delta_narration: vec![Move {
                kind: MoveKind::Move,
                files: vec![FileMove {
                    path: "x".to_owned(),
                    from: "old".to_owned(),
                }],
                to: "new".to_owned(),
                reason: MoveReason::Clustering,
            }],
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        }
    }

    /// Builds a file node.
    fn file(name: &str) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(Vec::new()),
            production_sloc: Some(0),
        }
    }

    /// Builds a zeroed breakdown.
    fn zero() -> ScoreBreakdown {
        ScoreBreakdown {
            cut: 0.0,
            imbalance: 0.0,
            naming: 0.0,
            path: 0.0,
            anchor: 0.0,
            dependency_only: 0.0,
            capacity: 0.0,
        }
    }

    #[test]
    fn should_render_the_full_report_sections() {
        let markdown = render_markdown(&sample());

        assert!(markdown.contains("# Strata report"));
        assert!(markdown.contains("Snapshot `abc123`"));
        assert!(markdown.contains("### Violations"));
        assert!(markdown.contains("## Anchored parameter profile"));
        assert!(markdown.contains("### Effective parameters"));
        assert!(markdown.contains("capacity  file 250 · folder 20"));
        assert!(markdown.contains("objective imbalance 0.1 · naming 0.3"));
        assert!(markdown.contains("dependency-only 0.05"));
        assert!(markdown.contains("same-file-symbol 1.0 · same-file-type 3.0"));
        assert!(markdown.contains("solver    ilp-threshold 300"));
        assert!(markdown.contains("diversity seeds-per-candidate 10"));
        assert!(markdown.contains("tests     helper-cap 250"));
        assert!(markdown.contains("### Candidate 1 (improvement `+1.0000`, score `1.0000`)"));
        assert!(markdown.contains("Conditional splits"));
        assert!(markdown.contains("solution space converged"));
    }

    #[test]
    fn should_number_each_file_of_a_large_move_group_as_a_step() {
        let mut result = sample();
        if let Some(mode) = result.profiles.anchored.as_mut()
            && let Some(subject) = mode.candidates.first_mut()
        {
            subject.delta_narration = vec![Move {
                kind: MoveKind::Merge,
                files: ["a.ts", "b.ts", "c.ts", "d.ts"]
                    .into_iter()
                    .map(|path| FileMove {
                        path: path.to_owned(),
                        from: "old".to_owned(),
                    })
                    .collect(),
                to: "new".to_owned(),
                reason: MoveReason::Clustering,
            }];
        }

        let markdown = render_markdown(&result);

        // the moves render as a fenced block of numbered per-file steps, the
        // same voice the `diff` face speaks.
        assert!(
            markdown.contains("merge — regrouped by clustering\n"),
            "the group header names its reason: {markdown}"
        );
        for (step, file) in ["a.ts", "b.ts", "c.ts", "d.ts"].into_iter().enumerate() {
            assert!(
                markdown.contains(&format!("  {}. {file} [old → new]\n", step + 1)),
                "{file}: {markdown}"
            );
        }
    }

    #[test]
    fn should_write_the_report_to_a_file_when_output_is_set() {
        let input = std::env::temp_dir().join(format!("strata-report-in-{}.json", nanos()));
        let output = std::env::temp_dir().join(format!("strata-report-out-{}.md", nanos()));
        let _ = std::fs::write(&input, serde_json::to_vec(&sample()).unwrap_or_default());
        let args = ReportArgs {
            input: input.clone(),
            output: Some(output.clone()),
        };
        let mut sink = Vec::new();

        let outcome = run(&args, &mut sink);

        let written = std::fs::read_to_string(&output).unwrap_or_default();
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
        assert!(outcome.is_ok());
        assert!(sink.is_empty());
        assert!(written.contains("# Strata report"));
    }

    /// Returns a unique nanosecond stamp.
    fn nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    }
}
