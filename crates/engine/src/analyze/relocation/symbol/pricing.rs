use std::collections::{BTreeMap, BTreeSet};

use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights};
use strata_ir::{ContainerId, Node, NodeKind};

use crate::analyze::relocation::SYMBOL_TARGETS;
use crate::analyze::relocation::symbol::inputs::PassInputs;
use crate::analyze::relocation::symbol::ledger::Ledger;
use crate::analyze::scoring::{Placed, ScoringContext};
use strata_ir::ScopeLevel;

/// Sentinel vertex for a node whose effective file is not a crossing-graph
/// vertex.
const NO_VERTEX: u32 = u32::MAX;

/// What a symbol pass prices placements with: the both-direction incidence
/// that nominates destinations, the full objective over a placement, and the
/// file crossing graph the cycle floor reads.
pub(super) struct Pricing<'a> {
    coefficients: &'a Coefficients,
    weights: &'a KindWeights,
    base: &'a BTreeMap<u32, ContainerId>,
    /// The tree's scoring facts, resolved once for every placement priced.
    context: ScoringContext<'a>,
    /// The assembly's placement in dense form; a trial layout is this plus the
    /// overlay.
    base_placed: Placed,
    /// Distinct `(source, target)` node pairs of the edges between different
    /// nodes, ascending. The crossing graph is a function of these pairs, so
    /// duplicate edges need not be walked again.
    node_pairs: Vec<(u32, u32)>,
    /// Crossing-graph vertex of each node under the assembly, by node id, or
    /// [`NO_VERTEX`].
    base_vertex: Vec<u32>,
    /// Crossing-graph vertex of each candidate file, by container id, or
    /// [`NO_VERTEX`].
    vertex_of_file: Vec<u32>,
    vertex_count: usize,
    /// Both-direction priced incidence per node, computed once: an edge
    /// priced 0.0 never nominates a destination (FIX04).
    pub(super) incident: BTreeMap<u32, Vec<(u32, f64)>>,
}

impl<'a> Pricing<'a> {
    /// `touches_zone` is the pass's test-zone predicate (FIX11): an edge with
    /// either endpoint inside the zone prices to zero.
    // reason: one linear pass that builds every lookup table from the same walk.
    #[allow(clippy::too_many_lines)]
    pub(super) fn new(inputs: PassInputs<'a>, touches_zone: &dyn Fn(u32) -> bool) -> Self {
        let PassInputs {
            snapshot,
            coefficients,
            weights,
            same_file_symbol,
            same_file_type,
            capacity,
            scoring,
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
        let mut vertex_of_file = vec![
            NO_VERTEX;
            file_vertices
                .keys()
                .map(|file| file.0 as usize + 1)
                .max()
                .unwrap_or(0)
        ];
        for (file, vertex) in &file_vertices {
            if let Some(cell) = vertex_of_file.get_mut(file.0 as usize) {
                *cell = *vertex;
            }
        }
        let node_pairs: Vec<(u32, u32)> = edges
            .iter()
            .filter(|edge| edge.source != edge.target)
            .map(|edge| (edge.source.0, edge.target.0))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let vertex_len = node_pairs
            .iter()
            .flat_map(|&(source, target)| [source, target])
            .max()
            .map_or(0, |id| id as usize + 1);
        let base_vertex = (0..vertex_len)
            .map(|id| {
                u32::try_from(id)
                    .ok()
                    .and_then(|id| base.get(&id))
                    .map_or(NO_VERTEX, |file| Self::vertex_of(&vertex_of_file, *file))
            })
            .collect();
        let context = ScoringContext::new(
            scoring,
            &assembled.tree,
            &assembled.namespace_by_file,
            &assembled.pass_start_file_by_candidate,
            &capacity,
        );
        let base_placed = scoring.place(&|id| base.get(&id).copied());
        Self {
            coefficients,
            weights,
            base,
            context,
            base_placed,
            node_pairs,
            base_vertex,
            vertex_of_file,
            vertex_count: file_vertices.len(),
            incident,
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
        let mut placed = self.base_placed.clone();
        for (id, file) in state {
            placed.set(*id, *file);
        }
        let distance = self.context.move_distance(&placed);
        self.context
            .score(&placed, distance, self.coefficients, self.weights)
            .total
    }

    /// The crossing-graph vertex of candidate file `file`, or [`NO_VERTEX`].
    fn vertex_of(vertex_of_file: &[u32], file: ContainerId) -> u32 {
        vertex_of_file
            .get(file.0 as usize)
            .copied()
            .unwrap_or(NO_VERTEX)
    }

    /// The crossing graph over candidate FILES induced by the ledger's
    /// current overlay, condensed-ready exactly as the file polish builds its
    /// quotient.
    pub(super) fn crossing_csr(&self, ledger: &Ledger<'_>) -> Csr {
        let mut vertex = self.base_vertex.clone();
        for (id, file) in &ledger.overlay {
            if let Some(cell) = vertex.get_mut(*id as usize) {
                *cell = Self::vertex_of(&self.vertex_of_file, *file);
            }
        }
        let mut pairs: Vec<(u32, u32)> = self
            .node_pairs
            .iter()
            .filter_map(|&(source, target)| {
                let source = vertex.get(source as usize).copied()?;
                let target = vertex.get(target as usize).copied()?;
                (source != NO_VERTEX && target != NO_VERTEX && source != target)
                    .then_some((source, target))
            })
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        Csr::from_sorted_edges(self.vertex_count, &pairs)
    }
}
