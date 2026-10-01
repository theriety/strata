//! Assertions over change volume between the current layout and a candidate.

use std::collections::BTreeMap;

use crate::harness::Verdict;
use crate::harness::inputs::EvalInputs;
use crate::metrics;
use crate::target::{CapacityRelief, FaceMode, MoveBudget, NonInversion};

/// `move_budget`: structural moved-file count between current and the asserted
/// candidate, bounded inclusively. Narration never feeds this count.
pub(super) fn evaluate_move_budget(budget: &MoveBudget, inputs: &EvalInputs<'_>) -> Verdict {
    let face = budget.mode;
    let bound_label = match budget.max_moved_files {
        Some(max) => format!("max_moved_files={max}"),
        None => format!(
            "min_moved_files={}",
            budget.min_moved_files.unwrap_or_default()
        ),
    };
    let label = format!("move_budget({face:?},{bound_label})");

    let placement = face_placement(inputs, face);
    let moved = metrics::moved_files(&inputs.current_placement, &placement);
    let count = moved.len();
    let within = if let Some(max) = budget
        .max_moved_files
        .and_then(|max| usize::try_from(max).ok())
    {
        count <= max
    } else {
        budget
            .min_moved_files
            .and_then(|min| usize::try_from(min).ok())
            .is_some_and(|min| count >= min)
    };
    Verdict {
        label,
        passed: within,
        detail: if within {
            format!("{face:?} moves {count} files structurally, within its budget")
        } else {
            format!(
                "{face:?} moves {count} files structurally against its budget ({bound_label}): [{}]",
                moved.join(", ")
            )
        },
    }
}

/// The structural placement of a face's asserted candidate tree.
fn face_placement(inputs: &EvalInputs<'_>, face: FaceMode) -> BTreeMap<String, String> {
    inputs
        .faces
        .get(&face)
        .map(|face_inputs| metrics::structural_placement(face_inputs.tree))
        .unwrap_or_default()
}

/// `capacity_relief`: the asserted candidate leaves no hard capacity finding.
pub(super) fn evaluate_capacity_relief(
    relief: &CapacityRelief,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let face = relief.mode;
    let label = format!("capacity_relief({face:?})");
    let remaining = inputs
        .faces
        .get(&face)
        .and_then(|face_inputs| face_inputs.capacity_remaining);
    let passed = remaining.is_none_or(|count| count == 0);
    let detail = match remaining {
        None => format!("{face:?} candidate leaves no capacityRemainder"),
        Some(0) => {
            format!("{face:?} candidate leaves capacityRemainder.remaining = 0")
        }
        Some(count) => format!(
            "unrelieved capacity: {face:?} candidate still carries capacityRemainder.remaining = {count} hard findings"
        ),
    };
    Verdict {
        label,
        passed,
        detail,
    }
}

/// `non_inversion`: anchored may never move more files structurally than
/// greenfield on the same snapshot.
pub(super) fn evaluate_non_inversion(inversion: &NonInversion, inputs: &EvalInputs<'_>) -> Verdict {
    let label = "non_inversion".to_owned();
    let anchored = face_placement(inputs, FaceMode::Anchored);
    let greenfield = face_placement(inputs, FaceMode::Greenfield);
    let anchored_moved = metrics::moved_files(&inputs.current_placement, &anchored).len();
    let greenfield_moved = metrics::moved_files(&inputs.current_placement, &greenfield).len();
    let passed = anchored_moved <= greenfield_moved;
    let detail = if passed {
        format!(
            "anchored moves {anchored_moved} files, greenfield moves {greenfield_moved}: no inversion ({})",
            inversion.because
        )
    } else {
        format!(
            "anchored inversion: anchored moves {anchored_moved} files while greenfield moves {greenfield_moved} on the same snapshot"
        )
    };
    Verdict {
        label,
        passed,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::assertions::fixtures::{inputs_with, laminar_candidate, torn_candidate};
    use crate::target::{FaceMode, MoveBudget};

    use super::evaluate_move_budget;

    #[test]
    fn move_budget_counts_structural_placement_changes_only() {
        let torn = torn_candidate();
        let inputs = inputs_with(&torn);
        let budget = MoveBudget {
            mode: FaceMode::Anchored,
            max_moved_files: Some(0),
            min_moved_files: None,
            because: "a best state leaves billing alone".to_owned(),
        };
        let verdict = evaluate_move_budget(&budget, &inputs);
        assert!(!verdict.passed);
        // invoice + pricing + pipeline changed placement into helpers, and
        // telemetry/sink.py exists only in current (vanished counts as moved).
        assert!(verdict.detail.contains("moves 4 files structurally"));

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_verdict = evaluate_move_budget(&budget, &laminar_inputs);
        assert!(
            laminar_verdict.passed,
            "identical placement moves nothing: {}",
            laminar_verdict.detail
        );
    }
}
