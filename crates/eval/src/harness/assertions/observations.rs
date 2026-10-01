//! Report-only observations: reference pair sets and candidate distinctness.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::result::{AnalyzeResult, Candidate};

use crate::harness::ModeObservation;
use crate::harness::inputs::{FaceInputs, mode_result_of};
use crate::metrics;
use crate::target::{FaceMode, ReferenceSet};

/// The reference pair set: unordered pairs within each labeled container.
pub(super) fn reference_pairs(reference: &ReferenceSet) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for container in &reference.container {
        for (index, left) in container.files.iter().enumerate() {
            for right in container.files.iter().skip(index + 1) {
                pairs.insert(normalize_pair(left, right));
            }
        }
    }
    pairs
}

/// Normalizes an unordered pair so set comparisons never depend on order.
fn normalize_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

/// Non-gating diversity observations per evaluated mode.
pub(in crate::harness) fn observe_modes(
    result: &AnalyzeResult,
    faces: &BTreeMap<FaceMode, FaceInputs<'_>>,
) -> Vec<ModeObservation> {
    faces
        .keys()
        .filter_map(|face| {
            let mode_result = mode_result_of(result, *face)?;
            let signatures: Vec<Vec<(String, String)>> = mode_result
                .candidates
                .iter()
                .map(|candidate: &Candidate| metrics::placement_signature(&candidate.tree))
                .collect();
            Some(ModeObservation {
                face: *face,
                candidates: mode_result.candidates.len(),
                solution_space_converged: mode_result.solution_space_converged,
                pairwise_distinct_trees: all_pairs_distinct(&signatures),
            })
        })
        .collect()
}

/// Whether every pair of placement signatures differs.
fn all_pairs_distinct(signatures: &[Vec<(String, String)>]) -> bool {
    for (index, left) in signatures.iter().enumerate() {
        for right in signatures.iter().skip(index + 1) {
            if left == right {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::harness::assertions::fixtures::laminar_candidate;
    use crate::metrics::{co_membership_pairs, pair_f1};
    use crate::target::{ReferenceContainer, ReferenceSet};

    use super::reference_pairs;

    #[test]
    fn pair_f1_excludes_envelope_pairs_and_is_report_only() {
        let universe: BTreeSet<String> = ["billing/invoice.py", "billing/pricing.py"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let reference = ReferenceSet {
            container: vec![ReferenceContainer {
                path: "ref/billing".to_owned(),
                files: vec![
                    "billing/invoice.py".to_owned(),
                    "billing/pricing.py".to_owned(),
                ],
            }],
        };
        let pairs = reference_pairs(&reference);
        let predicted = co_membership_pairs(&laminar_candidate(), &universe);
        assert_eq!(
            predicted.len(),
            1,
            "folder-level pair only; package envelope excluded"
        );
        let (_, _, f1) = pair_f1(&pairs, &predicted);
        assert!((f1 - 1.0).abs() < 1e-12);
    }
}
