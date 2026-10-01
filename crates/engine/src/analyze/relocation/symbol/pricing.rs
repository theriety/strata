use std::collections::{BTreeMap, BTreeSet};

use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights, score};
use strata_ir::{ContainerId, Edge, Node, NodeKind, Snapshot};

use crate::analyze::relocation::CandidateTree;
use crate::analyze::relocation::SYMBOL_TARGETS;
use crate::analyze::relocation::symbol::inputs::PassInputs;
use crate::analyze::relocation::symbol::ledger::Ledger;
use crate::analyze::scoring::{move_distance, score_candidate};
use crate::config::CapacityConfig;
use strata_ir::ScopeLevel;

/// What a symbol pass prices placements with: the both-direction incidence
/// that nominates destinations, the full objective over a placement, and the
/// file crossing graph the cycle floor reads.
pub(super) struct Pricing<'a> {
    snapshot: &'a Snapshot,
    coefficients: &'a Coefficients,
    weights: &'a KindWeights,
    same_file_symbol: f64,
    same_file_type: f64,
    capacity: CapacityConfig,
    assembled: &'a CandidateTree,
    base: &'a BTreeMap<u32, ContainerId>,
    edges: &'a [Edge],
    /// Both-direction priced incidence per node, computed once: an edge
    /// priced 0.0 never nominates a destination (FIX04).
    pub(super) incident: BTreeMap<u32, Vec<(u32, f64)>>,
    /// Dense vertex per candidate FILE, for the crossing graph.
    file_vertices: BTreeMap<ContainerId, u32>,
}

impl<'a> Pricing<'a> {
    /// `touches_zone` is the pass's test-zone predicate (FIX11): an edge with
    /// either endpoint inside the zone prices to zero.
    pub(super) fn new(inputs: PassInputs<'a>, touches_zone: &dyn Fn(u32) -> bool) -> Self {
        let PassInputs {
            snapshot,
            coefficients,
            weights,
            same_file_symbol,
            same_file_type,
            capacity,
            assembled,
            nodes,
            edges,
        } = inputs;
        let base = &assembled.placement;
        let mut incident: BTreeMap<u32, Vec<(u32, f64)>> = BTreeMap::new();
        // FIX11: the test-zone tie-cut rides placement into symbol grain. A
        // node's zone is its placed file's mark; an edge with either endpoint
        // inside the zone prices to zero exactly as `build_file_graph` prices
        // it — the single-pricing choke point, mirrored so no grain disagrees
        // about what binds placement.
        let node_by_id: BTreeMap<u32, &Node> = nodes.iter().map(|node| (node.id.0, node)).collect();
        for edge in edges {
            let affinity = match (
                node_by_id.get(&edge.source.0),
                node_by_id.get(&edge.target.0),
            ) {
                (Some(source), Some(target)) if source.container == target.container => {
                    if source.kind == NodeKind::Type || target.kind == NodeKind::Type {
                        same_file_type
                    } else {
                        same_file_symbol
                    }
                }
                _ => 1.0,
            };
            let weight = weights.edge_weight(edge.kind, edge.confidence) * affinity;
            if weight <= 0.0 || touches_zone(edge.source.0) || touches_zone(edge.target.0) {
                continue;
            }
            incident
                .entry(edge.source.0)
                .or_default()
                .push((edge.target.0, weight));
            incident
                .entry(edge.target.0)
                .or_default()
                .push((edge.source.0, weight));
        }
        if coefficients.companion_separation > 0.0 {
            for affinity in snapshot
                .ir()
                .affinities
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
            {
                incident
                    .entry(affinity.companion.0)
                    .or_default()
                    .push((affinity.owner.0, coefficients.companion_separation));
            }
        }
        let file_vertices: BTreeMap<ContainerId, u32> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .enumerate()
            .map(|(index, container)| (container.id, u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();
        Self {
            snapshot,
            coefficients,
            weights,
            same_file_symbol,
            same_file_type,
            capacity,
            assembled,
            base,
            edges,
            incident,
            file_vertices,
        }
    }

    /// Ranks candidate destination files for `node` by summed two-way priced
    /// pull from their *base-placed* residents (FIX12-B); strongest first, ties
    /// toward the lower id, capped at [`SYMBOL_TARGETS`]. For a type, a
    /// destination must exert more pull than its pass-start file, so
    /// repository-wide objective normalization cannot trade a strong local
    /// type affinity away for an unrelated global improvement. Runtime
    /// symbols retain their established 1x admission behavior.
    ///
    /// Reading the overlay here would let a symbol chase a neighbour that moved
    /// earlier in the same pass, nominating a destination justified by nothing
    /// but another suggestion.
    pub(super) fn nominate(&self, node: &Node, source_file: ContainerId) -> Vec<ContainerId> {
        let mut pull: BTreeMap<ContainerId, f64> = BTreeMap::new();
        let mut source_pull = 0.0;
        for &(neighbour, weight) in self.incident.get(&node.id.0).into_iter().flatten() {
            if neighbour == node.id.0 {
                continue;
            }
            let Some(place) = self.base.get(&neighbour).copied() else {
                continue;
            };
            if place == source_file {
                source_pull += weight;
                continue;
            }
            *pull.entry(place).or_insert(0.0) += weight;
        }
        let mut ranked: Vec<(ContainerId, f64)> = pull
            .into_iter()
            .filter(|(_, destination_pull)| {
                node.kind != NodeKind::Type || *destination_pull > source_pull
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        ranked
            .into_iter()
            .take(SYMBOL_TARGETS)
            .map(|(destination, _)| destination)
            .collect()
    }

    /// Full objective value of the layout `state` describes.
    pub(super) fn score_with(&self, state: &BTreeMap<u32, ContainerId>) -> f64 {
        let placement = |id: u32| {
            state
                .get(&id)
                .copied()
                .or_else(|| self.base.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &self.assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &self.assembled.pass_start_file_by_candidate,
            &self.assembled.tree,
            &self.assembled.namespace_by_file,
            distance,
            &self.capacity,
            self.same_file_symbol,
            self.same_file_type,
        );
        score(&candidate, self.coefficients, self.weights).total
    }

    /// The crossing graph over candidate FILES induced by the ledger's
    /// current overlay, condensed-ready exactly as the file polish builds its
    /// quotient.
    pub(super) fn crossing_csr(&self, ledger: &Ledger<'_>) -> Csr {
        let mut pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
        for edge in self.edges {
            let (Some(source), Some(target)) = (
                ledger.effective(edge.source.0),
                ledger.effective(edge.target.0),
            ) else {
                continue;
            };
            if source == target {
                continue;
            }
            let (Some(source), Some(target)) = (
                self.file_vertices.get(&source),
                self.file_vertices.get(&target),
            ) else {
                continue;
            };
            pairs.insert((*source, *target));
        }
        let sorted: Vec<(u32, u32)> = pairs.into_iter().collect();
        Csr::from_sorted_edges(self.file_vertices.len(), &sorted)
    }
}
