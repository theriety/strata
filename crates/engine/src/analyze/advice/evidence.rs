//! The evidence index: assesses one relocation proposal against the pass-start
//! snapshot under a profile's qualification thresholds.
//!
//! The index owns the lookup tables; each child adds one family of methods to it
//! (resolution, placement predicates, signals, alternatives) or the arithmetic
//! they share.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{
    Container, ContainerId, IntermediateRepresentation, Node, NodeId, ScopeLevel, Snapshot,
};

use self::measure::{structural_evidence, weighted_evidence};
use self::places::insert_unique_path;
use crate::analyze::advice::proposal::AdviceProposal;
use crate::config::{ProfileConfig, ProfileName};
use crate::result::{EvidenceSignals, ProfileAssessment, QualificationThresholds};

mod alternatives;
mod measure;
mod places;
mod resolve;
mod signals;

#[cfg(test)]
mod tests;

pub(super) struct EvidenceIndex<'a> {
    ir: &'a IntermediateRepresentation,
    containers: BTreeMap<ContainerId, &'a Container>,
    nodes: BTreeMap<NodeId, &'a Node>,
    dataset_root: Option<String>,
    files_by_path: BTreeMap<String, Option<ContainerId>>,
    folders_by_path: BTreeMap<String, Option<ContainerId>>,
    file_paths_by_id: BTreeMap<ContainerId, Option<String>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum EvidencePlace {
    Container(ContainerId),
    PhysicalFolder {
        container: ContainerId,
        path: String,
    },
    ConceptualFolder(String),
}

impl<'a> EvidenceIndex<'a> {
    pub(super) fn new(snapshot: &'a Snapshot) -> Self {
        let ir = snapshot.ir();
        let dataset_roots = ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::PackageGroup)
            .map(|container| container.name.to_string())
            .collect::<BTreeSet<_>>();
        let dataset_root = (dataset_roots.len() == 1)
            .then(|| dataset_roots.into_iter().next())
            .flatten();
        let mut files_by_path = BTreeMap::new();
        let mut folders_by_path = BTreeMap::new();
        let mut file_paths_by_id = BTreeMap::new();
        for container in ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
        {
            files_by_path
                .entry(container.name.to_string())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(container.id));
            insert_unique_path(&mut file_paths_by_id, container.id, &container.name);
            if let Some(parent) = container.parent {
                let folder = container
                    .name
                    .rsplit_once('/')
                    .map_or("", |(folder, _)| folder);
                // Physical folders are derived from immutable file facts, not semantic scope
                // levels: a physical directory may be represented by a Domain or Package node.
                folders_by_path
                    .entry(folder.to_owned())
                    .and_modify(|entry| {
                        if *entry != Some(parent) {
                            *entry = None;
                        }
                    })
                    .or_insert(Some(parent));
            }
        }
        Self {
            ir,
            containers: ir
                .containers
                .containers()
                .iter()
                .map(|c| (c.id, c))
                .collect(),
            nodes: ir.nodes.iter().map(|n| (n.id, n)).collect(),
            dataset_root,
            files_by_path,
            folders_by_path,
            file_paths_by_id,
        }
    }

    pub(super) fn assess(
        &self,
        proposal: &AdviceProposal,
        profile: ProfileName,
        config: &ProfileConfig,
    ) -> ProfileAssessment {
        let subjects = self.subject_nodes(proposal);
        let Some(destination) = self.resolve_destination(proposal) else {
            return Self::empty_assessment(profile, &proposal.destination, config);
        };
        let evidence = self.signals(&subjects, &destination, proposal, config);
        let weighted_score = weighted_evidence(evidence, config);
        let structural_score = structural_evidence(evidence, config);
        // Qualification deliberately uses a conservative, immutable alternative pool rather
        // than replaying the search pass's mutable admission logic.  An extra alternative can
        // only demote advice to review; it can never authorize or suppress a searched move.
        let mut alternatives = self.alternatives(&subjects, &destination, proposal, config);
        alternatives.sort_by_key(|place| self.place_name(place, proposal));
        let best = alternatives
            .into_iter()
            .map(|place| {
                let signals = self.signals(&subjects, &place, proposal, config);
                let score = weighted_evidence(signals, config);
                (place, score)
            })
            .max_by(|(left_place, left_score), (right_place, right_score)| {
                left_score.total_cmp(right_score).then_with(|| {
                    self.place_name(right_place, proposal)
                        .cmp(&self.place_name(left_place, proposal))
                })
            });
        let (best_alternative, best_score) = best.map_or((None, 0.0), |(place, score)| {
            (Some(self.place_name(&place, proposal)), score)
        });
        let ambiguity_margin = (weighted_score - best_score).max(0.0);
        let q = config.qualification;
        let qualified = weighted_score >= q.minimum_evidence
            && structural_score > 0.0
            && structural_score >= q.minimum_structural
            && ambiguity_margin >= q.minimum_ambiguity_margin
            && weighted_score - best_score > 1e-12;
        ProfileAssessment {
            profile,
            destination: proposal.destination.clone(),
            evidence,
            weighted_score,
            structural_score,
            ambiguity_margin,
            best_alternative,
            thresholds: QualificationThresholds {
                minimum_evidence: q.minimum_evidence,
                minimum_structural: q.minimum_structural,
                minimum_ambiguity_margin: q.minimum_ambiguity_margin,
            },
            qualified,
        }
    }
    fn empty_assessment(
        profile: ProfileName,
        destination: &str,
        config: &ProfileConfig,
    ) -> ProfileAssessment {
        let q = config.qualification;
        ProfileAssessment {
            profile,
            destination: destination.to_owned(),
            evidence: EvidenceSignals {
                unique_owner: 0.0,
                role_affinity: 0.0,
                source_cohesion: 0.0,
                destination_cohesion: 0.0,
                producer_evidence: 0.0,
                architectural_reach: 0.0,
            },
            weighted_score: 0.0,
            structural_score: 0.0,
            ambiguity_margin: 0.0,
            best_alternative: None,
            thresholds: QualificationThresholds {
                minimum_evidence: q.minimum_evidence,
                minimum_structural: q.minimum_structural,
                minimum_ambiguity_margin: q.minimum_ambiguity_margin,
            },
            qualified: false,
        }
    }
}
