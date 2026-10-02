//! Preconditions, target assertions, and non-gating observations.

use std::collections::BTreeSet;

use crate::metrics;
use crate::target::{AssertBlock, FaceMode, ReferenceSet};

use super::inputs::EvalInputs;
use super::{PairF1Report, Verdict};

mod churn;
#[cfg(test)]
pub(in crate::harness) mod fixtures;
mod membership;
mod observations;
mod preconditions;
mod shape;

use churn::{evaluate_capacity_relief, evaluate_move_budget, evaluate_non_inversion};
use membership::{evaluate_path_set, evaluate_preserve_dir, evaluate_preserve_symbol_home};
use observations::reference_pairs;
use shape::{evaluate_name_alignment, evaluate_no_synthetic_bucket, evaluate_size_band};

pub(super) use observations::observe_modes;
pub(super) use preconditions::{all_findings, evaluate_preconditions};

/// Evaluates every assertion in the block once per face it applies to, then the
/// report-only pair-F1 per evaluated face.
pub(super) fn evaluate_assertions(
    block: &AssertBlock,
    inputs: &EvalInputs<'_>,
    reference: Option<&ReferenceSet>,
) -> (Vec<Verdict>, PairF1Report) {
    let mut verdicts = Vec::new();
    for assertion in &block.preserve_dir {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_preserve_dir(assertion, face, inputs));
        }
    }
    for assertion in &block.keep_together {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_path_set(assertion, face, inputs, true));
        }
    }
    for assertion in &block.separate {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_path_set(assertion, face, inputs, false));
        }
    }
    for band in &block.size_band {
        for face in faces_for(band.mode, &block.modes) {
            verdicts.push(evaluate_size_band(band, face, inputs));
        }
    }
    for budget in &block.move_budget {
        verdicts.push(evaluate_move_budget(budget, inputs));
    }
    for bucket in &block.no_synthetic_bucket {
        for face in faces_for(bucket.mode, &block.modes) {
            verdicts.push(evaluate_no_synthetic_bucket(bucket, face, inputs));
        }
    }
    for alignment in &block.name_alignment {
        for face in faces_for(alignment.mode, &block.modes) {
            verdicts.push(evaluate_name_alignment(alignment, face, inputs));
        }
    }
    for relief in &block.capacity_relief {
        verdicts.push(evaluate_capacity_relief(relief, inputs));
    }
    for inversion in &block.non_inversion {
        verdicts.push(evaluate_non_inversion(inversion, inputs));
    }
    for home in &block.preserve_symbol_home {
        for face in faces_for(home.mode, &block.modes) {
            verdicts.push(evaluate_preserve_symbol_home(home, face, inputs));
        }
    }

    let mut pair_f1: PairF1Report = Vec::new();
    if let Some(reference) = reference {
        let universe: BTreeSet<String> = reference
            .container
            .iter()
            .flat_map(|container| container.files.iter().cloned())
            .collect();
        let reference_pairs = reference_pairs(reference);
        for (face, face_inputs) in &inputs.faces {
            let predicted = metrics::co_membership_pairs(face_inputs.tree, &universe);
            let (precision, recall, f1) = metrics::pair_f1(&reference_pairs, &predicted);
            pair_f1.push((*face, precision, recall, f1));
        }
    }
    (verdicts, pair_f1)
}

/// The faces an assertion applies to: its own override, else the block default.
fn faces_for(own: Option<FaceMode>, block_modes: &[FaceMode]) -> Vec<FaceMode> {
    match own {
        Some(face) => vec![face],
        None => block_modes.to_vec(),
    }
}
