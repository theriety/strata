//! `strata tree`: print a candidate's (or the current) file tree.
//!
//! The command reads a saved [`AnalyzeResult`], selects the requested view — the
//! current layout, a named candidate, or the list of available candidates — and
//! renders it with optional per-file symbol listings and a depth cut.

use std::io::Write;
use std::path::PathBuf;

use strata_engine::StrataError;

use crate::commands::{candidate_at, mode_result, read_result};
use crate::render::render_tree;

/// The parsed inputs of a `tree` run.
#[derive(Debug)]
pub struct TreeArgs {
    /// The saved `AnalyzeResult` JSON to read.
    pub input: PathBuf,
    /// The mode to draw a candidate from (`anchored`/`greenfield`).
    pub mode: Option<String>,
    /// The 1-based candidate index; absent lists the available candidates.
    pub candidate: Option<usize>,
    /// Print the current (as-is) layout instead of a candidate.
    pub current: bool,
    /// List each file's symbols with derived visibility.
    pub symbols: bool,
    /// Truncate the tree at this container depth.
    pub depth: Option<u32>,
}

/// Runs `tree`, writing the rendered tree (or candidate list) to `out`.
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when the input cannot be read and
/// [`StrataError::CandidateNotFound`] when the named mode or index is absent.
pub fn run(args: &TreeArgs, out: &mut impl Write) -> Result<(), StrataError> {
    let result = read_result(&args.input)?;

    if args.current {
        return render_tree(&result.current.tree, args.symbols, args.depth, out)
            .map_err(|error| write_error(&error));
    }

    let mode = args.mode.as_deref().unwrap_or("anchored");
    let Some(index) = args.candidate else {
        return list_candidates(&result, mode, out);
    };

    let candidate = candidate_at(&result, mode, index)?;
    render_tree(&candidate.tree, args.symbols, args.depth, out).map_err(|error| write_error(&error))
}

/// Writes the available candidate indices and scores for `mode` to `out`.
fn list_candidates(
    result: &strata_engine::AnalyzeResult,
    mode: &str,
    out: &mut impl Write,
) -> Result<(), StrataError> {
    let mode_result = mode_result(result, mode)?;
    writeln!(out, "available candidates for {mode}:").map_err(|error| write_error(&error))?;
    for candidate in &mode_result.candidates {
        writeln!(out, "  {} (score {:.4})", candidate.index, candidate.score)
            .map_err(|error| write_error(&error))?;
    }
    Ok(())
}

/// Wraps a rendering I/O failure into a [`StrataError`].
fn write_error(error: &std::io::Error) -> StrataError {
    StrataError::InputUnreadable {
        path: PathBuf::from("<stdout>"),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use strata_engine::{
        AnalyzeResult, Candidate, ContainerNode, CurrentTree, Level, ModeResult, Modes,
        ScoreBreakdown, Summary, SymbolPlacement,
    };

    use super::*;

    /// Builds a result with one anchored candidate over a two-file tree.
    fn sample() -> AnalyzeResult {
        AnalyzeResult {
            snapshot_hash: "h".to_owned(),
            summary: Summary {
                symbols: 1,
                edges: 0,
                files: 1,
                files_by_language: BTreeMap::new(),
            },
            current: CurrentTree {
                tree: file("current_lib"),
                score: 1.0,
                score_breakdown: zero(),
                violations: Vec::new(),
            },
            modes: Modes {
                anchored: Some(ModeResult {
                    candidates: vec![Candidate {
                        index: 1,
                        score: 0.5,
                        score_breakdown: zero(),
                        tree: file("candidate_lib"),
                        conditional_splits: Vec::new(),
                        delta_narration: Vec::new(),
                    }],
                    pairwise_distance: vec![vec![0.0]],
                    solution_space_converged: true,
                }),
                greenfield: None,
            },
        }
    }

    /// Builds a file node with one symbol.
    fn file(name: &str) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(vec![SymbolPlacement {
                name: "sym".to_owned(),
                visibility: Level::File,
            }]),
            production_sloc: Some(1),
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
        }
    }

    /// Writes `result` to a temp file and returns the path.
    fn saved(result: &AnalyzeResult) -> PathBuf {
        let path = std::env::temp_dir().join(format!("strata-tree-{}.json", nanos()));
        let _ = std::fs::write(&path, serde_json::to_vec(result).unwrap_or_default());
        path
    }

    /// Returns a unique nanosecond stamp.
    fn nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    }

    #[test]
    fn should_print_the_current_tree_when_current_is_set() {
        let input = saved(&sample());
        let args = TreeArgs {
            input: input.clone(),
            mode: None,
            candidate: None,
            current: true,
            symbols: false,
            depth: None,
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(outcome.is_ok());
        assert!(String::from_utf8_lossy(&buffer).contains("current_lib"));
    }

    #[test]
    fn should_print_a_named_candidate_tree() {
        let input = saved(&sample());
        let args = TreeArgs {
            input: input.clone(),
            mode: Some("anchored".to_owned()),
            candidate: Some(1),
            current: false,
            symbols: false,
            depth: None,
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(outcome.is_ok());
        assert!(String::from_utf8_lossy(&buffer).contains("candidate_lib"));
    }

    #[test]
    fn should_list_candidates_when_no_index_is_given() {
        let input = saved(&sample());
        let args = TreeArgs {
            input: input.clone(),
            mode: Some("anchored".to_owned()),
            candidate: None,
            current: false,
            symbols: false,
            depth: None,
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(outcome.is_ok());
        assert!(String::from_utf8_lossy(&buffer).contains("available candidates"));
    }

    #[test]
    fn should_fail_when_the_candidate_index_is_out_of_range() {
        let input = saved(&sample());
        let args = TreeArgs {
            input: input.clone(),
            mode: Some("anchored".to_owned()),
            candidate: Some(9),
            current: false,
            symbols: false,
            depth: None,
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(matches!(
            outcome,
            Err(StrataError::CandidateNotFound { .. })
        ));
    }
}
