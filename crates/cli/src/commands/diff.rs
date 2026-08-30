//! `strata diff`: a narrated move list between two structures.
//!
//! Each reference is `current` or `mode/index` (e.g. `anchored/1`). A diff
//! against `current` is the candidate's stored delta narration — the grouped
//! move list the engine already computed versus the current tree. A diff between
//! two candidates of one mode also reports their variation-of-information
//! distance from the result's pairwise matrix.

use std::io::Write;
use std::path::PathBuf;

use strata_engine::{AnalyzeResult, StrataError};

use crate::commands::{candidate_at, mode_result, read_result};
use crate::render::render_diff;

/// The parsed inputs of a `diff` run.
#[derive(Debug)]
pub struct DiffArgs {
    /// The saved `AnalyzeResult` JSON to read.
    pub input: PathBuf,
    /// The first structure reference (`current` or `mode/index`).
    pub left: String,
    /// The second structure reference (`current` or `mode/index`).
    pub right: String,
}

/// A parsed structure reference: the current layout, or a candidate.
#[derive(Debug, PartialEq, Eq)]
enum Reference {
    /// The current (as-is) layout.
    Current,
    /// A 1-based candidate within a mode.
    Candidate {
        /// The mode name.
        mode: String,
        /// The 1-based candidate index.
        index: usize,
    },
}

/// Parses a `current` or `mode/index` reference.
///
/// # Errors
///
/// Returns [`StrataError::CandidateNotFound`] when the reference is malformed.
fn parse_reference(text: &str) -> Result<Reference, StrataError> {
    if text == "current" {
        return Ok(Reference::Current);
    }
    let (mode, index) = text.split_once('/').ok_or_else(|| not_found(text, 0))?;
    if !matches!(mode, "anchored" | "greenfield") {
        return Err(not_found(text, 0));
    }
    let index = index.parse::<usize>().map_err(|_| not_found(text, 0))?;
    Ok(Reference::Candidate {
        mode: mode.to_owned(),
        index,
    })
}

/// Builds a `CandidateNotFound` for a malformed reference.
fn not_found(text: &str, index: usize) -> StrataError {
    StrataError::CandidateNotFound {
        mode: text.to_owned(),
        index,
    }
}

/// Runs `diff`, writing the narrated move list (and any VI distance) to `out`.
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when the input cannot be read and
/// [`StrataError::CandidateNotFound`] when a reference does not resolve.
pub fn run(args: &DiffArgs, out: &mut impl Write) -> Result<(), StrataError> {
    let result = read_result(&args.input)?;
    let left = parse_reference(&args.left)?;
    let right = parse_reference(&args.right)?;

    match (&left, &right) {
        (Reference::Current, Reference::Candidate { mode, index })
        | (Reference::Candidate { mode, index }, Reference::Current) => {
            let candidate = candidate_at(&result, mode, *index)?;
            render_diff(candidate, out).map_err(|error| write_error(&error))
        }
        (
            Reference::Candidate {
                mode: left_mode,
                index: left_index,
            },
            Reference::Candidate {
                mode: right_mode,
                index: right_index,
            },
        ) => {
            // a candidate-vs-candidate diff narrates the right candidate's moves
            // against the current tree and reports the pair's VI distance when both
            // belong to the same mode (the only pairing the matrix covers). the
            // left reference is resolved too, so an out-of-range left ref fails
            // with CANDIDATE_NOT_FOUND instead of silently rendering.
            candidate_at(&result, left_mode, *left_index)?;
            let right_candidate = candidate_at(&result, right_mode, *right_index)?;
            render_diff(right_candidate, out).map_err(|error| write_error(&error))?;
            if left_mode == right_mode {
                write_distance(&result, left_mode, *left_index, *right_index, out)?;
            }
            Ok(())
        }
        (Reference::Current, Reference::Current) => {
            writeln!(out, "no moves").map_err(|error| write_error(&error))
        }
    }
}

/// Writes the variation-of-information distance between two same-mode candidates.
fn write_distance(
    result: &AnalyzeResult,
    mode: &str,
    left_index: usize,
    right_index: usize,
    out: &mut impl Write,
) -> Result<(), StrataError> {
    let mode_result = mode_result(result, mode)?;
    let distance = mode_result
        .pairwise_distance
        .get(left_index.wrapping_sub(1))
        .and_then(|row| row.get(right_index.wrapping_sub(1)))
        .copied();
    if let Some(distance) = distance {
        writeln!(out, "vi distance {distance:.4}").map_err(|error| write_error(&error))?;
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
        Candidate, ContainerNode, CurrentStanding, CurrentTree, FileMove, Level, ModeResult, Modes,
        Move, MoveKind, MoveReason, ProfileConfig, ProfileCurrent, ScoreBreakdown, Summary,
        SymbolPlacement,
    };

    use super::*;

    /// Builds a result with two anchored candidates.
    fn sample() -> AnalyzeResult {
        AnalyzeResult {
            schema_version: strata_engine::RESULT_SCHEMA_VERSION,
            snapshot_hash: "h".to_owned(),
            summary: Summary {
                symbols: 1,
                edges: 0,
                files: 1,
                files_by_language: BTreeMap::new(),
            },
            current: CurrentTree {
                tree: file("lib"),
                shared_findings: Vec::new(),
            },
            profiles: Modes {
                anchored: Some(ModeResult {
                    parameters: ProfileConfig::default(),
                    current: ProfileCurrent {
                        score: 1.0,
                        score_breakdown: zero(),
                        unique_findings: Vec::new(),
                        standing: CurrentStanding::Outscored,
                        capacity_breaks: 0,
                    },
                    candidates: vec![candidate(1), candidate(2)],
                    pairwise_distance: vec![vec![0.0, 0.7], vec![0.7, 0.0]],
                    solution_space_converged: true,
                }),
                greenfield: None,
            },
        }
    }

    /// Builds a candidate carrying a single narrated move.
    fn candidate(index: u32) -> Candidate {
        Candidate {
            index,
            score: f64::from(index),
            score_breakdown: zero(),
            improvement: 0.0,
            tree: file("lib"),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Move,
                files: vec![FileMove {
                    path: "sym".to_owned(),
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
            capacity: 0.0,
        }
    }

    /// Writes `result` to a temp file and returns the path.
    fn saved(result: &AnalyzeResult) -> PathBuf {
        let path = std::env::temp_dir().join(format!("strata-diff-{}.json", nanos()));
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
    fn should_parse_a_current_reference() {
        assert_eq!(parse_reference("current").ok(), Some(Reference::Current));
    }

    #[test]
    fn should_parse_a_mode_index_reference() {
        assert_eq!(
            parse_reference("anchored/2").ok(),
            Some(Reference::Candidate {
                mode: "anchored".to_owned(),
                index: 2,
            })
        );
    }

    #[test]
    fn should_reject_an_unknown_mode_reference() {
        assert!(matches!(
            parse_reference("bogus/1"),
            Err(StrataError::CandidateNotFound { .. })
        ));
    }

    #[test]
    fn should_narrate_current_against_a_candidate() {
        let input = saved(&sample());
        let args = DiffArgs {
            input: input.clone(),
            left: "current".to_owned(),
            right: "anchored/1".to_owned(),
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(outcome.is_ok());
        assert!(String::from_utf8_lossy(&buffer).contains("[old → new]"));
    }

    #[test]
    fn should_report_vi_distance_for_two_same_mode_candidates() {
        let input = saved(&sample());
        let args = DiffArgs {
            input: input.clone(),
            left: "anchored/1".to_owned(),
            right: "anchored/2".to_owned(),
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = std::fs::remove_file(&input);
        assert!(outcome.is_ok());
        assert!(String::from_utf8_lossy(&buffer).contains("vi distance 0.7000"));
    }
}
