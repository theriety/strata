//! Placement predicates of the evidence index: ancestry, place naming, and
//! whether a node lives at an evidence place.

use std::collections::BTreeMap;

use strata_ir::{ContainerId, Node};

use crate::analyze::advice::evidence::{EvidenceIndex, EvidencePlace};
use crate::analyze::advice::proposal::AdviceProposal;
use crate::result::RelocationProposal;

impl EvidenceIndex<'_> {
    fn is_ancestor(&self, ancestor: ContainerId, mut child: ContainerId) -> bool {
        loop {
            if ancestor == child {
                return true;
            }
            let Some(parent) = self.containers.get(&child).and_then(|c| c.parent) else {
                return false;
            };
            child = parent;
        }
    }
    fn node_at_place(&self, node: &Node, place: ContainerId) -> bool {
        node.container == place || self.is_ancestor(place, node.container)
    }
    pub(super) fn place_name(&self, place: &EvidencePlace, proposal: &AdviceProposal) -> String {
        let relative = match place {
            EvidencePlace::Container(id) => self
                .file_paths_by_id
                .get(id)
                .and_then(Clone::clone)
                .unwrap_or_default(),
            EvidencePlace::PhysicalFolder { path, .. } | EvidencePlace::ConceptualFolder(path) => {
                path.clone()
            }
        };
        if matches!(proposal.proposal, RelocationProposal::File { .. }) {
            self.dataset_root
                .as_deref()
                .filter(|root| !root.is_empty())
                .map_or(relative.clone(), |root| {
                    if relative.is_empty() {
                        root.to_owned()
                    } else {
                        format!("{root}/{relative}")
                    }
                })
        } else {
            relative
        }
    }
    pub(super) fn physical_folder_name(&self, place: &EvidencePlace) -> Option<String> {
        match place {
            EvidencePlace::ConceptualFolder(path) | EvidencePlace::PhysicalFolder { path, .. } => {
                Some(path.clone())
            }
            EvidencePlace::Container(id) => self.physical_folder_for_file(*id).and_then(|folder| {
                if let EvidencePlace::PhysicalFolder { path, .. } = folder {
                    Some(path)
                } else {
                    None
                }
            }),
        }
    }
    pub(super) fn node_at_evidence_place(&self, node: &Node, place: &EvidencePlace) -> bool {
        match place {
            EvidencePlace::Container(id) => self.node_at_place(node, *id),
            EvidencePlace::PhysicalFolder { container, path } => {
                self.node_at_place(node, *container)
                    && self.physical_folder_for_file(node.container).is_some_and(|folder| {
                        matches!(folder, EvidencePlace::PhysicalFolder { path: node_path, .. } if physical_path_contains(path, &node_path))
                    })
            }
            EvidencePlace::ConceptualFolder(_) => false,
        }
    }
    pub(super) fn destination_nodes_at(&self, place: &EvidencePlace) -> Vec<&Node> {
        self.ir
            .nodes
            .iter()
            .filter(|node| self.node_at_evidence_place(node, place))
            .collect()
    }
}

pub(super) fn insert_unique_path(
    paths: &mut BTreeMap<ContainerId, Option<String>>,
    id: ContainerId,
    path: &str,
) {
    paths
        .entry(id)
        .and_modify(|entry| {
            if entry.as_deref() != Some(path) {
                *entry = None;
            }
        })
        .or_insert_with(|| Some(path.to_owned()));
}

fn physical_path_contains(ancestor: &str, candidate: &str) -> bool {
    candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| ancestor.is_empty() || suffix.starts_with('/'))
}
