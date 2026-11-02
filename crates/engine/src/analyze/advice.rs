//! Relocation advice: reconciles the per-profile candidates into recommended and
//! review-only moves by profile consensus.
//!
//! [`build_advice`] atomizes each profile's leading candidate, assesses every
//! atom with the [`evidence`] index, and partitions the atoms by majority
//! support; the child modules own the atom model and the evidence arithmetic.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::Snapshot;

use crate::config::{AnalyzeConfig, ProfileName};
use crate::result::{Advice, ProfileConflict, Profiles, RelocationAdvice, ReviewReason};

mod evidence;
mod proposal;

#[cfg(test)]
mod tests;

use evidence::EvidenceIndex;
use proposal::{AdviceProposal, advice_key, atomize_candidate};

#[allow(clippy::too_many_lines)]
pub(in crate::analyze) fn build_advice(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    profiles: &Profiles,
) -> Advice {
    let index = EvidenceIndex::new(snapshot);
    let executed = [
        (ProfileName::Anchored, profiles.anchored.as_ref()),
        (ProfileName::Greenfield, profiles.greenfield.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, result)| result.map(|result| (name, result)))
    .collect::<Vec<_>>();
    let mut selections = BTreeMap::<ProfileName, Vec<AdviceProposal>>::new();
    let mut union = BTreeMap::<String, AdviceProposal>::new();
    for (name, result) in &executed {
        let proposals = result
            .candidates
            .first()
            .map_or_else(Vec::new, atomize_candidate);
        for proposal in &proposals {
            union
                .entry(proposal.key())
                .or_insert_with(|| proposal.clone());
        }
        selections.insert(*name, proposals);
    }
    let mut advice = Advice::default();
    for proposal in union.into_values() {
        let mut supporting_profiles = Vec::new();
        let mut qualified_profiles = Vec::new();
        let mut absent_profiles = Vec::new();
        let mut conflicting_destinations = Vec::new();
        let mut assessments = Vec::new();
        for (profile_name, _) in &executed {
            let selected = selections
                .get(profile_name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if let Some(same) = selected.iter().find(|item| item.key() == proposal.key()) {
                supporting_profiles.push(*profile_name);
                let assessment = index.assess(same, *profile_name, config.profile(*profile_name));
                if assessment.qualified {
                    qualified_profiles.push(*profile_name);
                }
                assessments.push(assessment);
            } else if let Some(conflict) = selected
                .iter()
                .find(|item| item.subject == proposal.subject)
            {
                conflicting_destinations.push(ProfileConflict {
                    profile: *profile_name,
                    destination: conflict.destination.clone(),
                });
            } else {
                absent_profiles.push(*profile_name);
            }
        }
        let majority = qualified_profiles.len() * 2 > executed.len();
        let mut review_reasons = BTreeSet::new();
        if supporting_profiles.len() != executed.len() {
            review_reasons.insert(ReviewReason::PartialProfileSupport);
        }
        if !conflicting_destinations.is_empty() {
            review_reasons.insert(ReviewReason::ConflictingDestinations);
        }
        for assessment in &assessments {
            if assessment.weighted_score < assessment.thresholds.minimum_evidence {
                review_reasons.insert(ReviewReason::WeakEvidence);
            }
            if assessment.structural_score <= 0.0
                || assessment.structural_score < assessment.thresholds.minimum_structural
            {
                review_reasons.insert(ReviewReason::WeakStructuralEvidence);
            }
            if assessment.ambiguity_margin < assessment.thresholds.minimum_ambiguity_margin {
                review_reasons.insert(ReviewReason::WeakAmbiguityMargin);
            }
        }
        if !majority {
            review_reasons.insert(ReviewReason::NoMajoritySupport);
        }
        let item = RelocationAdvice {
            proposal: proposal.proposal,
            destination: proposal.destination,
            supporting_profiles,
            qualified_profiles,
            absent_profiles,
            conflicting_destinations,
            assessments,
            review_reasons: review_reasons.into_iter().collect(),
        };
        if majority {
            advice.recommended.push(item);
        } else {
            advice.review_candidates.push(item);
        }
    }
    advice.recommended.sort_by_key(advice_key);
    advice.review_candidates.sort_by_key(advice_key);
    advice
}
