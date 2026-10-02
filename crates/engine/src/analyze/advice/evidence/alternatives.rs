//! The conservative pool of alternative places a proposal is compared against.

use std::collections::BTreeSet;

use strata_ir::{NodeId, Polarity};

use crate::analyze::advice::evidence::measure::dependency_weight;
use crate::analyze::advice::evidence::{EvidenceIndex, EvidencePlace};
use crate::analyze::advice::proposal::AdviceProposal;
use crate::config::ProfileConfig;
use crate::result::RelocationProposal;

impl EvidenceIndex<'_> {
    pub(super) fn alternatives(
        &self,
        subjects: &BTreeSet<NodeId>,
        destination: &EvidencePlace,
        proposal: &AdviceProposal,
        config: &ProfileConfig,
    ) -> Vec<EvidencePlace> {
        let wants_file = matches!(proposal.proposal, RelocationProposal::Symbol { .. });
        let mut places = BTreeSet::new();
        let mut consumer_folders = BTreeSet::new();
        if let Some(source) = self.resolve_source(proposal) {
            places.insert(source);
        }
        for edge in &self.ir.edges {
            if dependency_weight(edge.kind, config) * edge.confidence <= 0.0 {
                continue;
            }
            let other = if subjects.contains(&edge.source) && !subjects.contains(&edge.target) {
                Some(edge.target)
            } else if subjects.contains(&edge.target) && !subjects.contains(&edge.source) {
                Some(edge.source)
            } else {
                None
            };
            if let Some(node) = other
                .and_then(|id| self.nodes.get(&id))
                .filter(|node| node.polarity == Polarity::Production)
            {
                places.insert(if wants_file {
                    EvidencePlace::Container(node.container)
                } else {
                    self.physical_folder_for_file(node.container)
                        .unwrap_or(EvidencePlace::Container(node.container))
                });
            }
            if subjects.contains(&edge.target)
                && !subjects.contains(&edge.source)
                && let Some(folder) = self
                    .nodes
                    .get(&edge.source)
                    .filter(|node| node.polarity == Polarity::Production)
                    .and_then(|node| self.physical_folder_for_file(node.container))
                    .and_then(|place| match place {
                        EvidencePlace::PhysicalFolder { path, .. } => Some(path),
                        _ => None,
                    })
            {
                consumer_folders.insert(folder);
            }
        }
        // The consumer LCA is a conceptual alternative for either relocation grain.  In
        // particular, a symbol proposed into one sibling branch must clearly beat the shared
        // folder even though symbol search itself ultimately nominates concrete files.
        if consumer_folders.len() >= 2 {
            let names = consumer_folders
                .iter()
                .map(|name| name.split('/').collect::<Vec<_>>())
                .collect::<Vec<_>>();
            if let Some(first) = names.first() {
                let common_len = (0..first.len())
                    .take_while(|i| names.iter().all(|parts| parts.get(*i) == first.get(*i)))
                    .count();
                let common = first
                    .iter()
                    .take(common_len)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("/");
                if !common.is_empty() {
                    let place = self
                        .resolve_folder_place(&common)
                        .unwrap_or(EvidencePlace::ConceptualFolder(common));
                    places.insert(place);
                }
            }
        }
        for affinity in &self.ir.affinities {
            if subjects.contains(&affinity.companion)
                && let Some(owner) = self.nodes.get(&affinity.owner)
            {
                places.insert(if wants_file {
                    EvidencePlace::Container(owner.container)
                } else {
                    self.physical_folder_for_file(owner.container)
                        .unwrap_or(EvidencePlace::Container(owner.container))
                });
            }
        }
        places.remove(destination);
        places.into_iter().collect()
    }
}
