//! The facts of a snapshot that no placement and no tree changes.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_ir::{ContainerId, EdgeKind, Node, NodeKind, Polarity, Snapshot};

use super::placed::Placed;
use crate::analyze::scoring::distance::folder_key_of_files;

/// One snapshot edge with everything about it that no placement changes.
pub(super) struct EdgeFacts {
    pub(super) source: u32,
    pub(super) target: u32,
    pub(super) kind: EdgeKind,
    pub(super) confidence: f64,
    /// Same-file multiplier: below one when both endpoints already share a
    /// file, one otherwise.
    pub(super) affinity: f64,
}

/// A declaration that can leave its file for one holding only its dependencies,
/// with the files of its dependencies and consumers.
pub(super) struct DependencyFacts {
    pub(super) node: u32,
    pub(super) container: ContainerId,
    /// Files holding a dependency of the node, ascending.
    pub(super) outgoing: Vec<ContainerId>,
    /// Files holding a consumer of the node, ascending.
    pub(super) incoming: Vec<ContainerId>,
}

/// A companion declaration and the immutable file of its owner.
pub(super) struct CompanionFacts {
    pub(super) companion: u32,
    pub(super) owner_container: ContainerId,
}

/// The current path key of a file and the folder key it sits under.
pub(super) struct HomeFile {
    pub(super) name: SmolStr,
    pub(super) folder: SmolStr,
}

/// The facts of a snapshot that every scored placement shares, whatever tree it
/// is scored on.
pub(in crate::analyze) struct SnapshotScoring {
    pub(super) edges: Vec<EdgeFacts>,
    /// `(id, effective size)` of every production declaration, in node order.
    pub(super) production: Vec<(u32, u32)>,
    pub(super) dependency_nodes: Vec<DependencyFacts>,
    pub(super) companions: Vec<CompanionFacts>,
    /// Folder key of each file path in the snapshot's own tree.
    pub(super) real_folder_of: BTreeMap<SmolStr, SmolStr>,
    /// The distinct current files that hold at least one declaration.
    pub(super) homes: Vec<HomeFile>,
    /// `(id, home)` per declaration in node order; `home` indexes `homes` and
    /// is `None` when the declaration's file has no folder key, so it can never
    /// count as moved.
    pub(super) node_home: Vec<(u32, Option<usize>)>,
    /// One past the largest node id a placement may be asked about.
    placement_len: u32,
}

impl SnapshotScoring {
    /// Resolves the placement-independent facts of `snapshot` under the given
    /// same-file multipliers.
    // reason: one linear pass that builds every lookup table from the same walk.
    #[allow(clippy::too_many_lines)]
    pub(in crate::analyze) fn new(
        snapshot: &Snapshot,
        same_file_symbol: f64,
        same_file_type: f64,
    ) -> Self {
        let ir = snapshot.ir();
        let by_id: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();

        let mut edges = Vec::with_capacity(ir.edges.len());
        let mut outgoing: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
        let mut incoming: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
        let mut placement_len = ir
            .nodes
            .iter()
            .map(|node| node.id.0)
            .max()
            .map_or(0, |id| id.saturating_add(1));
        for edge in &ir.edges {
            placement_len = placement_len
                .max(edge.source.0.saturating_add(1))
                .max(edge.target.0.saturating_add(1));
            let endpoints = (by_id.get(&edge.source.0), by_id.get(&edge.target.0));
            let affinity = match endpoints {
                (Some(source), Some(target)) if source.container == target.container => {
                    if source.kind == NodeKind::Type || target.kind == NodeKind::Type {
                        same_file_type
                    } else {
                        same_file_symbol
                    }
                }
                _ => 1.0,
            };
            edges.push(EdgeFacts {
                source: edge.source.0,
                target: edge.target.0,
                kind: edge.kind,
                confidence: edge.confidence,
                affinity,
            });
            if edge.source == edge.target {
                continue;
            }
            if let (Some(source), Some(target)) = endpoints {
                outgoing
                    .entry(edge.source.0)
                    .or_default()
                    .insert(target.container);
                incoming
                    .entry(edge.target.0)
                    .or_default()
                    .insert(source.container);
            }
        }

        let dependency_nodes = ir
            .nodes
            .iter()
            .filter(|node| {
                node.polarity == Polarity::Production
                    && matches!(node.kind, NodeKind::Symbol | NodeKind::Type)
            })
            .filter_map(|node| {
                // a declaration with no dependency file can never qualify.
                let files = outgoing.get(&node.id.0)?;
                Some(DependencyFacts {
                    node: node.id.0,
                    container: node.container,
                    outgoing: files.iter().copied().collect(),
                    incoming: incoming
                        .get(&node.id.0)
                        .map(|incoming_files| incoming_files.iter().copied().collect())
                        .unwrap_or_default(),
                })
            })
            .collect();

        let companions = ir
            .affinities
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|affinity| {
                let owner = by_id.get(&affinity.owner.0)?;
                placement_len = placement_len.max(affinity.companion.0.saturating_add(1));
                Some(CompanionFacts {
                    companion: affinity.companion.0,
                    owner_container: owner.container,
                })
            })
            .collect();

        let production = ir
            .nodes
            .iter()
            .filter(|node| node.polarity == Polarity::Production)
            .map(|node| (node.id.0, node.effective_size))
            .collect();

        let real_folder_of = folder_key_of_files(&ir.containers)
            .into_iter()
            .map(|(file, folder)| (SmolStr::new(file), SmolStr::new(folder)))
            .collect();
        let (homes, node_home) = Self::homes(snapshot);

        Self {
            edges,
            production,
            dependency_nodes,
            companions,
            real_folder_of,
            homes,
            node_home,
            placement_len,
        }
    }

    /// Resolves each declaration's current file path and folder key.
    fn homes(snapshot: &Snapshot) -> (Vec<HomeFile>, Vec<(u32, Option<usize>)>) {
        let ir = snapshot.ir();
        let current_folder = folder_key_of_files(&ir.containers);
        let current_name: BTreeMap<u32, &SmolStr> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, &container.name))
            .collect();
        let mut homes: Vec<HomeFile> = Vec::new();
        let mut home_of_container: BTreeMap<u32, Option<usize>> = BTreeMap::new();
        let node_home = ir
            .nodes
            .iter()
            .map(|node| {
                let home = *home_of_container
                    .entry(node.container.0)
                    .or_insert_with(|| {
                        let name = current_name.get(&node.container.0)?;
                        let folder = current_folder.get(name.as_str())?;
                        homes.push(HomeFile {
                            name: (*name).clone(),
                            folder: SmolStr::new(folder),
                        });
                        Some(homes.len() - 1)
                    });
                (node.id.0, home)
            })
            .collect();
        (homes, node_home)
    }

    /// Materializes a placement function into its dense form.
    pub(in crate::analyze) fn place(
        &self,
        placement: &dyn Fn(u32) -> Option<ContainerId>,
    ) -> Placed {
        let files: Vec<Option<ContainerId>> = (0..self.placement_len).map(placement).collect();
        let slots = files
            .iter()
            .flatten()
            .map(|file| file.0 as usize + 1)
            .max()
            .unwrap_or(0);
        Placed { files, slots }
    }
}
