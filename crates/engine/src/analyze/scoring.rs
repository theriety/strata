use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::LevelCaps;
use strata_core::condense::condense;
use strata_core::graph::csr::Csr;
use strata_core::score::{
    Candidate as ScoreCandidate, Coefficients, CohesionGroup, ContainerSizes, KindWeights,
    ScoreBreakdown as CoreBreakdown, ScoredEdge, score,
};
use strata_ir::{
    Container, ContainerId, ContainerTree, Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};

use crate::analyze::findings::physical_folder_entries_repository_relative;
use crate::analyze::relocation::file_inventory;
use crate::config::{AnalyzeConfig, CapacityConfig, ProfileConfig};
use crate::narrate::tokenize;

#[cfg(test)]
mod tests;

pub(in crate::analyze) trait ProfileSource {
    fn profile(&self) -> &ProfileConfig;
}

impl ProfileSource for ProfileConfig {
    fn profile(&self) -> &ProfileConfig {
        self
    }
}

impl ProfileSource for AnalyzeConfig {
    fn profile(&self) -> &ProfileConfig {
        &self.profiles.anchored
    }
}

pub(in crate::analyze) trait CapacitySource {
    fn capacity(&self) -> &CapacityConfig;
}

impl CapacitySource for CapacityConfig {
    fn capacity(&self) -> &CapacityConfig {
        self
    }
}

impl CapacitySource for AnalyzeConfig {
    fn capacity(&self) -> &CapacityConfig {
        &self.profiles.anchored.capacity
    }
}

/// Scores the snapshot's current layout under `coefficients`.
///
/// The current candidate carries the snapshot's own edges (each crossing the LCA
/// level of its endpoints in the current tree), the file-level container sizes,
/// and a zero move distance, so its objective is the genuine `J(T0)` baseline the
/// candidates are measured against.
#[cfg(test)]
pub(in crate::analyze) fn score_current(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    folder_budget: u32,
) -> CoreBreakdown {
    let capacity = CapacityConfig {
        folder: folder_budget,
        ..CapacityConfig::default()
    };
    score_current_with_affinity(snapshot, coefficients, weights, &capacity, 1.0, 3.0)
}

pub(in crate::analyze) fn score_current_with_affinity(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
) -> CoreBreakdown {
    score_current_with_overlay(
        snapshot,
        coefficients,
        weights,
        capacity,
        same_file_symbol,
        same_file_type,
        &BTreeMap::new(),
    )
}

/// Scores the current tree with some declarations placed in other current
/// files: the price of a candidate that moves no file but relocates symbols.
///
/// `overlay` maps a node id to the current file container it moves to. Every
/// other node stays where it is, and the move-distance term counts exactly the
/// relocated nodes, so an empty overlay prices the current layout itself.
pub(in crate::analyze) fn score_current_with_overlay(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
    overlay: &BTreeMap<u32, ContainerId>,
) -> CoreBreakdown {
    let ir = snapshot.ir();
    let (files, _) = file_inventory(ir);
    let namespaces: BTreeMap<ContainerId, SmolStr> = files
        .iter()
        .map(|file| (ContainerId(file.container), file.namespace.clone()))
        .collect();
    let container_of: BTreeMap<u32, ContainerId> = ir
        .nodes
        .iter()
        .map(|node| {
            let file = overlay.get(&node.id.0).copied().unwrap_or(node.container);
            (node.id.0, file)
        })
        .collect();
    let placement = |id: u32| container_of.get(&id).copied();
    let pass_start_file_by_candidate: BTreeMap<ContainerId, ContainerId> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id, container.id))
        .collect();
    let distance = if overlay.is_empty() {
        0.0
    } else {
        move_distance(snapshot, &ir.containers, &placement)
    };
    let candidate = score_candidate(
        snapshot,
        &placement,
        &pass_start_file_by_candidate,
        &ir.containers,
        &namespaces,
        distance,
        capacity,
        same_file_symbol,
        same_file_type,
    );
    score(&candidate, coefficients, weights)
}

/// The description of one container to intern: the name key it carries, the
/// level it sits at, and its parent.
#[derive(Clone, Copy)]
pub(in crate::analyze) struct ContainerSpec<'name> {
    /// The name key the container's cluster carries.
    pub(in crate::analyze) name: &'name SmolStr,
    /// The level the container sits at.
    pub(in crate::analyze) level: ScopeLevel,
    /// The container's parent, or `None` at the root.
    pub(in crate::analyze) parent: Option<ContainerId>,
    /// True for the synthetic `workspace` folder bucket the render collapses.
    /// Only a root-file folder is ever synthetic; every upper level is false.
    pub(in crate::analyze) synthetic: bool,
}

/// cycle mass used as a component-wise admission budget
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::analyze) struct CycleCounts {
    pub(in crate::analyze) vertices: usize,
    pub(in crate::analyze) edges: usize,
}

impl CycleCounts {
    pub(in crate::analyze) fn from_graph(graph: &Csr) -> Self {
        let condensation = condense(graph);
        let cyclic: Vec<bool> = condensation
            .members
            .iter()
            .map(|members| members.len() > 1)
            .collect();
        let vertices = condensation
            .members
            .iter()
            .filter(|members| members.len() > 1)
            .map(Vec::len)
            .sum();
        let mut edges = 0;
        for source in 0..graph.vertex_count() {
            let Ok(source) = u32::try_from(source) else {
                continue;
            };
            let Some(&component) = condensation.membership.get(source as usize) else {
                continue;
            };
            if !cyclic.get(component.0 as usize).copied().unwrap_or(false) {
                continue;
            }
            edges += graph
                .neighbors(source)
                .iter()
                .filter(|&&target| {
                    condensation
                        .membership
                        .get(target as usize)
                        .is_some_and(|owner| *owner == component)
                })
                .count();
        }
        Self { vertices, edges }
    }

    pub(in crate::analyze) fn exceeds(self, baseline: Self) -> bool {
        self.vertices > baseline.vertices || self.edges > baseline.edges
    }
}

/// Builds the scorer's [`ScoreCandidate`] view from a node-placement function over
/// the candidate `tree`.
///
/// `placement` maps each node id to the file container it occupies in the
/// candidate tree; the edge LCA levels, per-container child sizes, and naming
/// groups are derived from that placement and the tree. `move_distance` is the
/// already-computed fraction of relocated symbols.
pub(in crate::analyze) fn score_candidate(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    move_distance: f64,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
) -> ScoreCandidate {
    let ir = snapshot.ir();
    let parent_of: BTreeMap<u32, Option<ContainerId>> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.parent))
        .collect();
    let level_of: BTreeMap<u32, ScopeLevel> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.level))
        .collect();

    let original_nodes: BTreeMap<u32, &Node> =
        ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let edges = ir
        .edges
        .iter()
        .filter_map(|edge| {
            let source = placement(edge.source.0)?;
            let target = placement(edge.target.0)?;
            let lca_level = lca_container(&parent_of, source, target)
                .and_then(|id| level_of.get(&id.0).copied())
                .unwrap_or(ScopeLevel::PackageGroup);
            Some(ScoredEdge {
                kind: edge.kind,
                confidence: edge.confidence,
                affinity: match (
                    original_nodes.get(&edge.source.0),
                    original_nodes.get(&edge.target.0),
                ) {
                    (Some(source_node), Some(target_node))
                        if source_node.container == target_node.container =>
                    {
                        if source_node.kind == NodeKind::Type || target_node.kind == NodeKind::Type
                        {
                            same_file_type
                        } else {
                            same_file_symbol
                        }
                    }
                    _ => 1.0,
                },
                lca_level,
            })
        })
        .collect();

    let containers = container_sizes(snapshot, placement, tree);
    let (cohesion_groups, path_cohesion) = cohesion_inputs(snapshot, placement, tree);
    let capacity_pressure = capacity_pressure(snapshot, placement, tree, namespaces, capacity);
    let dependency_only_relocations =
        dependency_only_relocations(snapshot, placement, pass_start_file_by_candidate);
    let companion_separations =
        companion_separations(snapshot, placement, pass_start_file_by_candidate);

    ScoreCandidate {
        edges,
        containers,
        cohesion_groups,
        path_cohesion,
        move_distance,
        capacity_pressure,
        dependency_only_relocations,
        companion_separations,
    }
}

/// Counts companions not placed in the immutable pass-start file of their owner.
pub(in crate::analyze) fn companion_separations(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
) -> u32 {
    let nodes: BTreeMap<NodeId, &Node> = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect();
    snapshot
        .ir()
        .affinities
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|affinity| {
            let Some(owner) = nodes.get(&affinity.owner) else {
                return false;
            };
            placement(affinity.companion.0)
                .and_then(|file| pass_start_file_by_candidate.get(&file).copied())
                != Some(owner.container)
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Counts production declarations that leave their pass-start file for a file
/// holding one of their dependencies but none of their consumers. Candidate
/// file ids are mapped back to immutable file identity so moving a whole file
/// between folders contributes zero.
pub(in crate::analyze) fn dependency_only_relocations(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
) -> u32 {
    let ir = snapshot.ir();
    let nodes: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let mut outgoing_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
    let mut incoming_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
    for edge in &ir.edges {
        if edge.source == edge.target {
            continue;
        }
        let (Some(source), Some(target)) = (nodes.get(&edge.source.0), nodes.get(&edge.target.0))
        else {
            continue;
        };
        outgoing_files
            .entry(edge.source.0)
            .or_default()
            .insert(target.container);
        incoming_files
            .entry(edge.target.0)
            .or_default()
            .insert(source.container);
    }

    ir.nodes
        .iter()
        .filter(|node| {
            node.polarity == Polarity::Production
                && matches!(node.kind, NodeKind::Symbol | NodeKind::Type)
        })
        .filter(|node| {
            let Some(candidate_file) = placement(node.id.0) else {
                return false;
            };
            let Some(destination) = pass_start_file_by_candidate.get(&candidate_file).copied()
            else {
                return false;
            };
            if destination == node.container {
                return false;
            }

            let destination_has_dependency = outgoing_files
                .get(&node.id.0)
                .is_some_and(|files| files.contains(&destination));
            let destination_has_consumer = incoming_files
                .get(&node.id.0)
                .is_some_and(|files| files.contains(&destination));
            destination_has_dependency && !destination_has_consumer
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Prices every configured capacity level with the same measures used by
/// capacity findings.
pub(in crate::analyze) fn capacity_pressure(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    capacity: &CapacityConfig,
) -> f64 {
    let mut pressure = 0.0;
    let mut file_sloc: BTreeMap<ContainerId, u32> = BTreeMap::new();
    for node in &snapshot.ir().nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(file) = placement(node.id.0) {
            let measure = file_sloc.entry(file).or_default();
            *measure = measure.saturating_add(node.effective_size);
        }
    }
    pressure += file_sloc
        .values()
        .map(|&measure| normalized_overage(measure, capacity.file))
        .sum::<f64>();
    pressure += physical_folder_entries_repository_relative(tree, Some(namespaces))
        .values()
        .map(|&measure| normalized_overage(measure, capacity.folder))
        .sum::<f64>();

    let children = children_by_container(tree);
    for container in tree.containers() {
        let (member_level, cap) = match container.level {
            ScopeLevel::Domain => (ScopeLevel::Folder, capacity.domain),
            ScopeLevel::Package => (ScopeLevel::Domain, capacity.package),
            ScopeLevel::PackageGroup => (ScopeLevel::Package, capacity.package_group),
            ScopeLevel::Folder | ScopeLevel::File => continue,
        };
        let measure = count_bound_structural_members(tree, &children, container.id, member_level);
        pressure += normalized_overage(measure, cap);
    }
    pressure
}

pub(in crate::analyze) fn normalized_overage(measure: u32, cap: u32) -> f64 {
    if measure <= cap {
        0.0
    } else if cap == 0 {
        f64::from(measure)
    } else {
        f64::from(measure - cap) / f64::from(cap)
    }
}

pub(in crate::analyze) fn children_by_container(
    tree: &ContainerTree,
) -> BTreeMap<ContainerId, Vec<ContainerId>> {
    let mut children: BTreeMap<ContainerId, Vec<ContainerId>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children.entry(parent).or_default().push(container.id);
        }
    }
    children
}

pub(in crate::analyze) fn count_bound_structural_members(
    tree: &ContainerTree,
    children: &BTreeMap<ContainerId, Vec<ContainerId>>,
    root: ContainerId,
    member_level: ScopeLevel,
) -> u32 {
    let by_id: BTreeMap<ContainerId, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id, container))
        .collect();
    let mut pending = children.get(&root).cloned().unwrap_or_default();
    let mut count = 0_u32;
    while let Some(id) = pending.pop() {
        let Some(container) = by_id.get(&id).copied() else {
            continue;
        };
        let binds_file = subtree_binds_file(children, &by_id, id);
        let binds_member = if member_level == ScopeLevel::Folder {
            children.get(&id).into_iter().flatten().any(|child| {
                by_id
                    .get(child)
                    .is_some_and(|entry| entry.level == ScopeLevel::File)
            })
        } else {
            binds_file
        };
        if container.level == member_level && binds_member {
            count = count.saturating_add(1);
        }
        pending.extend(children.get(&id).into_iter().flatten().copied());
    }
    count
}

pub(in crate::analyze) fn subtree_binds_file(
    children: &BTreeMap<ContainerId, Vec<ContainerId>>,
    by_id: &BTreeMap<ContainerId, &Container>,
    root: ContainerId,
) -> bool {
    let mut pending = children.get(&root).cloned().unwrap_or_default();
    while let Some(id) = pending.pop() {
        let Some(container) = by_id.get(&id).copied() else {
            continue;
        };
        if container.level == ScopeLevel::File {
            return true;
        }
        pending.extend(children.get(&id).into_iter().flatten().copied());
    }
    false
}

#[cfg(test)]
pub(in crate::analyze) fn physical_binding_pressure(
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    folder_budget: u32,
) -> f64 {
    physical_folder_entries_repository_relative(tree, Some(namespaces))
        .values()
        .map(|&measure| normalized_overage(measure, folder_budget))
        .sum()
}

/// Sums the scoped over-capacity binding pressure of a rendered tree: over
/// every folder and domain container, the share its transitively-bound file
/// count exceeds `folder_budget` — `Σ max(0, bound − budget) / budget` (FIX03).
///
/// Ancestors bind, so nesting cannot dodge the budget: a domain whose folders
/// together hold more than the budget pays for the whole binding even when
/// each folder sits within it. The rendered tree is the flat IR form, so the
/// count accumulates upward: each container folds its subtree total into its
/// parent, charging folders and domains on the way. This is the priced
/// counterpart of the eval's ancestor-binding rule; the configured per-level
/// caps stay findings-only semantics (`walk_capacity`). A zero budget disables
/// the term.
#[cfg(test)]
pub(in crate::analyze) fn binding_pressure(tree: &ContainerTree, folder_budget: u32) -> f64 {
    if folder_budget == 0 {
        return 0.0;
    }
    // Subtree file totals remain the domain-grain measure. Folders instead
    // bind only their immediate entries: direct files and direct child
    // folders.
    let mut totals: BTreeMap<u32, u32> = BTreeMap::new();
    let mut entries: BTreeMap<u32, u32> = BTreeMap::new();
    let mut pressure = 0.0_f64;
    for container in tree.containers().iter().rev() {
        if let Some(parent) = container.parent
            && matches!(container.level, ScopeLevel::File | ScopeLevel::Folder)
        {
            *entries.entry(parent.0).or_insert(0) += 1;
        }
        match container.level {
            ScopeLevel::File => {
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += 1;
                }
            }
            level => {
                let bound = totals.remove(&container.id.0).unwrap_or(0);
                if matches!(level, ScopeLevel::Folder | ScopeLevel::Domain) {
                    let measured = if level == ScopeLevel::Folder {
                        entries.remove(&container.id.0).unwrap_or(0)
                    } else {
                        bound
                    };
                    let over = measured.saturating_sub(folder_budget);
                    pressure += f64::from(over) / f64::from(folder_budget);
                }
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += bound;
                }
            }
        }
    }
    pressure
}

/// Derives the naming-cohesion groups and the path-cohesion fraction of a
/// placement (the α and β scoring inputs, previously stubbed).
///
/// Every parent container that directly holds files forms one group carrying
/// its production SLOC and the basename token set of each member file. Path
/// cohesion is the production-SLOC-weighted fraction of files placed under the
/// same folder key their own directory already resolves to in the snapshot's
/// laminar tree — an unchanged layout scores 1.0 and every relocation dilutes
/// it.
pub(in crate::analyze) fn cohesion_inputs(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> (Vec<CohesionGroup>, f64) {
    let ir = snapshot.ir();
    // production SLOC landing in each file container under this placement.
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let slot = file_sloc.entry(container.0).or_default();
            *slot = slot.saturating_add(node.effective_size);
        }
    }

    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();

    // the real folder key each file currently lives under, read from the
    // snapshot's own laminar tree; both trees key a file container by its raw
    // repo path, so the path joins a placed file back to its real directory.
    let real_folder_of = folder_key_of_files(&ir.containers);

    let mut groups: BTreeMap<u32, CohesionGroup> = BTreeMap::new();
    let mut matched = 0_u64;
    let mut total = 0_u64;
    for container in tree.containers() {
        if container.level != ScopeLevel::File {
            continue;
        }
        let Some(parent) = container.parent else {
            continue;
        };
        let sloc = file_sloc.get(&container.id.0).copied().unwrap_or(0);
        let group = groups.entry(parent.0).or_insert_with(|| CohesionGroup {
            production_sloc: 0,
            members: Vec::new(),
        });
        group.production_sloc = group.production_sloc.saturating_add(sloc);
        group.members.push(tokenize(&container.name));

        let placed = name_of.get(&parent.0).map_or("", |name| name.as_str());
        let real = real_folder_of
            .get(container.name.as_str())
            .copied()
            .unwrap_or("");
        total = total.saturating_add(u64::from(sloc));
        if !real.is_empty() && placed == real {
            matched = matched.saturating_add(u64::from(sloc));
        }
    }

    let path_cohesion = if total == 0 {
        // No production SLOC exists under this placement (an all-test fixture,
        // say), so no file could demonstrably have left its folder: the
        // weighted fraction has an empty denominator, and the documented
        // invariant credits such a layout in full instead of collapsing the
        // empty ratio to a signed-zero term that reads as zero cohesion.
        1.0
    } else {
        // reason: sloc totals fit u32 sums; the f64 mantissa loses nothing material
        #[allow(clippy::cast_precision_loss)]
        let ratio = matched as f64 / total as f64;
        ratio
    };
    (groups.into_values().collect(), path_cohesion)
}

/// Returns the lowest common ancestor container of two containers.
///
/// Walks the ancestor chain of `left` into a set, then ascends `right` until a
/// shared ancestor is found; the highest endpoints share is the package-group root
/// of an empty intersection, so disjoint subtrees cross at the coarsest level.
pub(in crate::analyze) fn lca_container(
    parent_of: &BTreeMap<u32, Option<ContainerId>>,
    left: ContainerId,
    right: ContainerId,
) -> Option<ContainerId> {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut up_left = Some(left);
    while let Some(id) = up_left {
        if !ancestors.insert(id.0) {
            break;
        }
        up_left = parent_of.get(&id.0).copied().flatten();
    }

    let mut up_right = Some(right);
    while let Some(id) = up_right {
        if ancestors.contains(&id.0) {
            return Some(id);
        }
        up_right = parent_of.get(&id.0).copied().flatten();
    }
    None
}

/// Computes the per-container child subtree sizes (in production SLOC) for the
/// imbalance term.
pub(in crate::analyze) fn container_sizes(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> Vec<ContainerSizes> {
    let ir = snapshot.ir();
    // each file's production SLOC is the sum of effective_size over the production
    // nodes placed in it (test-zoned nodes already carry zero size).
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let entry = file_sloc.entry(container.0).or_default();
            *entry = entry.saturating_add(node.effective_size);
        }
    }

    // bottom-up subtree size of each container.
    let mut subtree: BTreeMap<u32, u32> = file_sloc.clone();
    let mut children_by_parent: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container.id.0);
        }
    }

    // accumulate sizes bottom-up by repeatedly summing known children; the
    // laminar tree is at most five levels deep, so five passes reach fixpoint.
    for _ in 0..5 {
        for container in tree.containers() {
            let total: u32 = children_by_parent
                .get(&container.id.0)
                .into_iter()
                .flatten()
                .filter_map(|child| subtree.get(child).copied())
                .sum();
            if total > 0 {
                subtree.insert(container.id.0, total);
            }
        }
    }

    children_by_parent
        .into_values()
        .map(|children| ContainerSizes {
            child_sizes: children
                .iter()
                .map(|child| subtree.get(child).copied().unwrap_or(0))
                .collect(),
        })
        .collect()
}

/// Computes the move distance of a candidate tree: the fraction of symbols whose
/// owning file changes real folder — or, since FIX08, whose effective placement
/// lands it in a DIFFERENT file than the one that houses it today.
///
/// A file's location is exactly its folder key — the path it would be moved to —
/// so the comparison reads folder keys on both sides and never composes a
/// root-to-leaf path. Labels above the folder are display, not location:
/// renaming a domain relocates nothing and must not register here. The second,
/// placement-aware rule prices symbol-grain relocation: `placement` maps each
/// node to the candidate file container it occupies, and a node whose placed
/// file carries another path has left its home even when its folder key is
/// unchanged. File-only layouts pass the assembly's own placement, which maps
/// every node to its current file's candidate id, so the extension changes
/// nothing for them — μ and β stop being blind to symbol moves without any
/// coefficient moving.
pub(in crate::analyze) fn move_distance(
    snapshot: &Snapshot,
    candidate: &ContainerTree,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
) -> f64 {
    let ir = snapshot.ir();
    let total = ir.nodes.len();
    if total == 0 {
        return 0.0;
    }
    let current_folder = folder_key_of_files(&ir.containers);
    let candidate_folder = folder_key_of_files(candidate);

    // match candidate files by the original file's path, which the assembly
    // preserves; the id → name map avoids a per-node linear scan.
    let current_name: BTreeMap<u32, &SmolStr> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    let candidate_file_name: BTreeMap<u32, &str> = candidate
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.as_str()))
        .collect();
    let moved = ir
        .nodes
        .iter()
        .filter(|node| {
            let Some(name) = current_name.get(&node.container.0) else {
                return false;
            };
            let Some(current) = current_folder.get(name.as_str()) else {
                return false;
            };
            // FIX08: a symbol whose effective placement sits in a file of
            // another path has relocated between files, whatever its folder key
            // did. Unplaced nodes fall through to the file-key rule alone.
            let left_file = placement(node.id.0).is_some_and(|file| {
                candidate_file_name
                    .get(&file.0)
                    .is_some_and(|placed| *placed != name.as_str())
            });
            left_file
                || candidate_folder
                    .get(name.as_str())
                    .is_none_or(|placed| placed != current)
        })
        .count();

    f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Maps each file container's path key to the folder key holding it directly.
///
/// This is the one primitive both location-sensitive terms read: a file's real
/// place is the folder key it sits under, so β (does the file still sit where it
/// already lives?) and μ (did the file leave?) ask the same question of the same
/// key space. Comparing keys rather than a raw-path prefix is what keeps
/// source-root transparency intact — one folder legitimately merges `src/x` with
/// `spec/x`, so it has no single raw directory to compare against.
pub(in crate::analyze) fn folder_key_of_files(tree: &ContainerTree) -> BTreeMap<&str, &str> {
    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    tree.containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|container| {
            let folder = name_of.get(&container.parent?.0)?;
            Some((container.name.as_str(), folder.as_str()))
        })
        .collect()
}

/// Extracts the per-level member caps from the engine config.
pub(in crate::analyze) fn level_caps(source: &impl CapacitySource) -> LevelCaps {
    let capacity = source.capacity();
    LevelCaps {
        folder: capacity.folder,
        domain: capacity.domain,
        package: capacity.package,
        package_group: capacity.package_group,
    }
}
