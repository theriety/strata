//! The evidence signals of one candidate destination: ownership, role affinity,
//! cohesion, and architectural reach.

use std::collections::BTreeSet;

use strata_ir::{AffinityKind, ContainerId, NodeId, Polarity};

use crate::analyze::advice::evidence::measure::{
    dependency_weight, jaccard_tokens, usize_as_f64, usize_ratio,
};
use crate::analyze::advice::evidence::{EvidenceIndex, EvidencePlace};
use crate::analyze::advice::proposal::AdviceProposal;
use crate::config::ProfileConfig;
use crate::narrate::tokenize;
use crate::result::EvidenceSignals;

impl EvidenceIndex<'_> {
    pub(super) fn signals(
        &self,
        subjects: &BTreeSet<NodeId>,
        destination: &EvidencePlace,
        proposal: &AdviceProposal,
        config: &ProfileConfig,
    ) -> EvidenceSignals {
        let owners = self
            .ir
            .affinities
            .iter()
            .filter(|a| a.kind == AffinityKind::CompanionOwner && subjects.contains(&a.companion))
            .filter_map(|a| self.nodes.get(&a.owner).copied())
            .collect::<Vec<_>>();
        let owners_in_destination = owners
            .iter()
            .filter(|node| self.node_at_evidence_place(node, destination))
            .count();
        let subject_tokens = subjects
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .flat_map(|node| tokenize(&node.name))
            .collect::<BTreeSet<_>>();
        let destination_tokens = self
            .destination_nodes_at(destination)
            .into_iter()
            .flat_map(|node| tokenize(&node.name))
            .chain(
                Some(self.place_name(destination, proposal))
                    .into_iter()
                    .flat_map(|name| tokenize(&name)),
            )
            .collect::<BTreeSet<_>>();
        let source = self.resolve_source(proposal);
        let mut wd = 0.0;
        let mut ws = 0.0;
        let mut wall = 0.0;
        let mut neighbors = BTreeSet::new();
        let mut consumers = BTreeSet::new();
        for edge in &self.ir.edges {
            let (neighbor, incoming) =
                if subjects.contains(&edge.source) && !subjects.contains(&edge.target) {
                    (edge.target, false)
                } else if subjects.contains(&edge.target) && !subjects.contains(&edge.source) {
                    (edge.source, true)
                } else {
                    continue;
                };
            let Some(node) = self
                .nodes
                .get(&neighbor)
                .copied()
                .filter(|node| node.polarity == Polarity::Production)
            else {
                continue;
            };
            let weight = edge.confidence * dependency_weight(edge.kind, config);
            if weight <= 0.0 {
                continue;
            }
            wall += weight;
            if self.node_at_evidence_place(node, destination) {
                wd += weight;
                neighbors.insert(node.id);
            }
            if source
                .as_ref()
                .is_some_and(|place| self.node_at_evidence_place(node, place))
            {
                ws += weight;
            }
            if incoming {
                consumers.insert(node.container);
            }
        }
        let source_cohesion = ((wd - ws) / wd.max(ws).max(f64::EPSILON)).clamp(0.0, 1.0);
        let destination_cohesion = if wall > 0.0 {
            let breadth = match neighbors.len() {
                0 => 0.0,
                1 => 0.5,
                _ => 1.0,
            };
            (wd / wall) * breadth
        } else {
            0.0
        };
        EvidenceSignals {
            unique_owner: f64::from(owners.len() == 1 && owners_in_destination == 1),
            role_affinity: jaccard_tokens(&subject_tokens, &destination_tokens),
            source_cohesion,
            destination_cohesion,
            producer_evidence: if owners.is_empty() {
                0.0
            } else {
                usize_ratio(owners_in_destination, owners.len())
            },
            architectural_reach: self.architectural_reach(destination, &consumers),
        }
    }
    fn architectural_reach(
        &self,
        destination: &EvidencePlace,
        consumer_files: &BTreeSet<ContainerId>,
    ) -> f64 {
        if consumer_files.len() < 2 {
            return 0.0;
        }
        let folders = consumer_files
            .iter()
            .filter_map(|id| self.physical_folder_for_file(*id))
            .filter_map(|place| match place {
                EvidencePlace::PhysicalFolder { path, .. } => Some(path),
                _ => None,
            })
            .map(|name| name.split('/').map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let Some(first) = folders.first() else {
            return 0.0;
        };
        let common_len = (0..first.len())
            .take_while(|index| {
                folders
                    .iter()
                    .all(|parts| parts.get(*index) == first.get(*index))
            })
            .count();
        let Some(destination_name) = self.physical_folder_name(destination) else {
            return 0.0;
        };
        let destination_parts = destination_name
            .split('/')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if destination_parts.len() == common_len
            && first.get(..common_len) == Some(destination_parts.as_slice())
        {
            1.0
        } else if destination_parts.len() < common_len
            && first.get(..destination_parts.len()) == Some(destination_parts.as_slice())
        {
            1.0 / (1.0 + usize_as_f64(common_len - destination_parts.len()))
        } else {
            0.0
        }
    }
}
