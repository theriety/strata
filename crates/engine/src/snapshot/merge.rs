//! Fragment merging: re-interns per-adapter ids into one namespace and rebuilds
//! the container tree from file paths.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{
    ContainerId, IntermediateRepresentation, IrFragment, Layout, Node, NodeId, ScopeLevel,
    build_laminar_tree,
};

use super::visibility::apply_visibility_scopes;

/// Merges per-adapter fragments into one [`IntermediateRepresentation`],
/// re-interning every fragment's node ids into a single dense namespace and
/// rebuilding the container tree from every file path so package, domain, and
/// folder boundaries follow the manifest-derived `layout` — not the per-adapter
/// positional path slices the fragments arrive with.
///
/// Each fragment's own file-level containers name the repo-relative path of the
/// file every node lives in; those paths seed [`build_laminar_tree`], and each
/// node is reattached to its file's container in the rebuilt tree.
pub(super) fn merge_fragments(
    fragments: Vec<IrFragment>,
    root_name: &str,
    layout: &Layout,
) -> IntermediateRepresentation {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges = Vec::new();
    let mut affinities = Vec::new();
    let mut visibility_scopes = Vec::new();
    // the file path each merged node belongs to, parallel to `nodes`.
    let mut node_paths: Vec<SmolStr> = Vec::new();
    let mut all_paths: Vec<SmolStr> = Vec::new();

    let mut node_offset = 0_u32;
    for fragment in fragments {
        // file-level containers are keyed by full repo-relative path; map each
        // fragment-local container id to that path so nodes can be re-homed.
        let file_path: HashMap<u32, SmolStr> = fragment
            .containers
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.clone()))
            .collect();
        all_paths.extend(file_path.values().cloned());

        // each fragment's ids span 0..=max; advancing the base by one past its
        // own maximum guarantees the next fragment's re-interned ids never
        // collide.
        let node_span = id_span(fragment.nodes.iter().map(|node| node.id.0));
        for mut node in fragment.nodes {
            let path = file_path
                .get(&node.container.0)
                .cloned()
                .unwrap_or_default();
            node.id = NodeId(node.id.0 + node_offset);
            nodes.push(node);
            node_paths.push(path);
        }
        for mut edge in fragment.edges {
            edge.source = NodeId(edge.source.0 + node_offset);
            edge.target = NodeId(edge.target.0 + node_offset);
            edges.push(edge);
        }
        for mut affinity in fragment.affinities {
            affinity.owner = NodeId(affinity.owner.0 + node_offset);
            affinity.companion = NodeId(affinity.companion.0 + node_offset);
            affinities.push(affinity);
        }
        for mut visibility_scope in fragment.visibility_scopes {
            visibility_scope.node = NodeId(visibility_scope.node.0 + node_offset);
            visibility_scopes.push(visibility_scope);
        }
        node_offset += node_span;
    }

    let built = build_laminar_tree(&all_paths, root_name, layout);
    for (node, path) in nodes.iter_mut().zip(&node_paths) {
        node.container = built.files.get(path).copied().unwrap_or(ContainerId(0));
    }
    let ladders = apply_visibility_scopes(
        &mut nodes,
        &built.tree,
        &built.files,
        &edges,
        &visibility_scopes,
    );

    let mut ir = IntermediateRepresentation::new(nodes, edges, built.tree);
    ir.affinities = affinities;
    ir.scope_ladders = ladders;
    ir
}

/// Returns one past the maximum id in `ids`, i.e. the id range width a fragment
/// occupies, or zero when the fragment is empty.
pub(super) fn id_span(ids: impl Iterator<Item = u32>) -> u32 {
    ids.max().map_or(0, |max| max + 1)
}
