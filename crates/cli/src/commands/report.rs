//! `strata report`: a saved analysis rendered as a Markdown report.
//!
//! The command reads saved analysis data and uses the same ordered presentation
//! as terminal reports: findings, candidate layouts with before/after trees,
//! and qualified advice. Verbose detail adds scores, evidence, and configuration.

use std::io::Write;
use std::path::PathBuf;

use strata_engine::StrataError;

use crate::commands::read_result;
use crate::render::{self, RenderOptions};

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
    run_with_options(args, RenderOptions::default(), out)
}

/// Renders a saved analysis with explicit report detail.
///
/// # Errors
/// Returns input or output errors.
pub fn run_with_options(
    args: &ReportArgs,
    options: RenderOptions,
    out: &mut impl Write,
) -> Result<(), StrataError> {
    let result = read_result(&args.input)?;
    let markdown = render::markdown(&result, options);

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use strata_engine::{
        ConditionalSplit, ContainerNode, CurrentTree, EdgeBreak, FileMove, Level, Modes, Move,
        MoveKind, MoveReason, ProfileConfig, ProfileCurrent, Severity, Summary, ViolationKind,
    };

    use super::*;
    use strata_engine::{
        AnalyzeResult, Candidate, CurrentStanding, ModeResult, ScoreBreakdown, Violation,
    };

    fn render_markdown(result: &AnalyzeResult) -> String {
        render::markdown(result, RenderOptions { verbose: true })
    }

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
            advice: strata_engine::Advice::default(),
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
                mirrors: Vec::new(),
                blocked_mirrors: Vec::new(),
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
            companion_separation: 0.0,
            capacity: 0.0,
        }
    }

    #[test]
    fn should_render_the_full_report_sections() {
        let markdown = render_markdown(&sample());

        assert!(markdown.contains("# Strata report"));
        assert!(markdown.contains("Snapshot `abc123`"));
        assert!(markdown.contains("## Structural findings"));
        assert!(markdown.contains("anchored — candidate 1"));
        assert!(markdown.contains("## Effective configuration"));
        assert!(markdown.contains("capacity  file 250 · folder 20"));
        assert!(markdown.contains("objective imbalance 0.1 · naming 0.3"));
        assert!(markdown.contains("dependency-only 0.05"));
        assert!(markdown.contains("same-file-symbol 1.0 · same-file-type 3.0"));
        assert!(markdown.contains("solver    ilp-threshold 300"));
        assert!(markdown.contains("diversity seeds-per-candidate 10"));
        assert!(markdown.contains("tests     helper-cap 250"));
        assert!(markdown.contains("Score: 2.0000 → 1.0000; improvement +1.0000"));
        assert!(markdown.contains("Conditional split"));
        assert!(markdown.contains("solution space converged"));
    }

    #[test]
    fn should_list_each_file_of_a_large_move_group() {
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
                mirrors: Vec::new(),
                blocked_mirrors: Vec::new(),
            }];
        }

        let markdown = render_markdown(&result);

        // the moves render as explicit per-file actions with their shared reason.
        assert!(
            markdown.contains("Why: clusters files that already import each other heavily."),
            "the group header names its reason: {markdown}"
        );
        for file in ["a.ts", "b.ts", "c.ts", "d.ts"] {
            assert!(
                markdown.contains(&format!("- Move file `{file}` → `new/{file}`")),
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
