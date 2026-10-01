//! Edge resolution, re-export linking, and deduplication.
//!
//! Resolves every recorded reference into a typed dependency edge, through the
//! semantic database first and the name-based fallback second, then collapses
//! duplicate edges to the strongest one per `(source, target, kind)`.

use std::collections::BTreeMap;
use std::path::Path;

use strata_ir::{Edge, EdgeKind, Hardness, NodeId, NodeKind};

use super::assignment::NodeAssignment;
use super::database::{Database, ResolvedTarget};
use crate::parse::{ParsedFile, RefKind, Reference};

/// Confidence assigned to a soft re-export edge: the `pub use` binding is
/// statically certain even though it imposes no runtime dependency of its own.
const CONFIDENCE_REEXPORT: f64 = 1.0;

/// Confidence assigned to a statically resolved (non-macro) edge.
const CONFIDENCE_STATIC: f64 = 1.0;

/// Confidence assigned to a macro-expanded reference's edge: resolved, but with
/// honest uncertainty because the reference text is produced by a macro.
const CONFIDENCE_MACRO: f64 = 0.5;

/// Confidence assigned to an edge resolved only by the name-based fallback: the
/// semantic database could not pin the reference, but a uniquely-named
/// in-workspace declaration matches, so the dependency is likely but uncertain.
const CONFIDENCE_NAME_FALLBACK: f64 = 0.6;

/// Resolves every recorded reference into the typed edge set.
pub(super) fn resolve_edges(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for file in files {
        for declaration in &file.declarations {
            let Some(source) = assignment.node_at(&file.path, declaration.byte_start) else {
                continue;
            };
            for reference in &declaration.references {
                resolve_reference(
                    source,
                    &file.path,
                    reference,
                    assignment,
                    database,
                    workspace_root,
                    &mut edges,
                );
            }
        }
    }
    edges
}

/// Resolves a single reference and appends its edge when it lands on a node.
fn resolve_reference(
    source: NodeId,
    path: &str,
    reference: &Reference,
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
    edges: &mut Vec<Edge>,
) {
    let (kind, hardness) = edge_shape(reference.kind);

    // a known definition stays authoritative even when absent from this snapshot.
    if let Some(resolved) = database.resolve(path, reference.offset, workspace_root) {
        if let ResolvedTarget::Workspace {
            path: target_path,
            offset: target_offset,
            module,
        } = resolved
            && !(module && reference.kind == RefKind::Qualifier)
            && let Some(target) = assignment.node_at(&target_path, target_offset)
            && binds(reference.kind, assignment, target)
        {
            let confidence = if reference.macro_expanded {
                CONFIDENCE_MACRO
            } else {
                CONFIDENCE_STATIC
            };
            push_edge(edges, source, target, kind, hardness, confidence);
        }
        return;
    }

    // unresolved receiver calls require type information; a matching global
    // name alone cannot identify their target.
    if database.allows_name_fallback(path, reference, workspace_root)
        && (reference.kind != RefKind::Qualifier
            || database.roots_in_workspace(path, reference, assignment, workspace_root))
        && let Some(target) = assignment.unique_node_named(&reference.name)
        && binds(reference.kind, assignment, target)
    {
        push_edge(
            edges,
            source,
            target,
            kind,
            Hardness::Soft,
            CONFIDENCE_NAME_FALLBACK,
        );
    }
}

/// Whether a reference of `kind` may bind to `target`. A path qualifier is a
/// type reference only when it names a type; landing on a value (a module file
/// whose first item is a function, or a same-named function reached by name
/// fallback) is not a dependency on a type, so no edge is emitted.
fn binds(kind: RefKind, assignment: &NodeAssignment, target: NodeId) -> bool {
    kind != RefKind::Qualifier || assignment.kind_of(target) == Some(NodeKind::Type)
}

/// Resolves every `pub use` re-export to its original declaration and emits a
/// soft [`EdgeKind::ReExport`] edge from the barrel-local node to that original.
///
/// The edge is emitted as-is — chain flattening is the engine's job — and is soft
/// because a re-export imposes no runtime dependency of its own.
pub(super) fn resolve_re_exports(
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
    edges: &mut Vec<Edge>,
) {
    for link in &assignment.re_export_links {
        let Some(ResolvedTarget::Workspace { path, offset, .. }) =
            database.resolve(&link.path, link.offset, workspace_root)
        else {
            continue;
        };
        let Some(target) = assignment.node_at(&path, offset) else {
            continue;
        };
        push_edge(
            edges,
            link.source,
            target,
            EdgeKind::ReExport,
            Hardness::Soft,
            CONFIDENCE_REEXPORT,
        );
    }
}
/// Maps a reference category to its emitted edge kind and hardness.
///
/// Rust types constrain compilation, so type references are hard; only erased
/// positions would be soft, and the parser does not surface those distinctly, so
/// every emitted type reference is hard here.
fn edge_shape(kind: RefKind) -> (EdgeKind, Hardness) {
    match kind {
        RefKind::UsePath => (EdgeKind::ValueImport, Hardness::Hard),
        RefKind::Call => (EdgeKind::Call, Hardness::Hard),
        RefKind::TypeRef | RefKind::Qualifier => (EdgeKind::TypeReference, Hardness::Hard),
        RefKind::TraitImpl => (EdgeKind::Inheritance, Hardness::Hard),
    }
}

/// Collapses duplicate edges into one per `(source, target, kind)`, keeping the
/// strongest: a hard edge dominates a soft one, and within the same hardness the
/// higher confidence wins. The result is ordered deterministically by endpoints
/// and kind, so a reference cited many ways contributes a single, meaningful
/// edge rather than an inflated weight.
pub(super) fn dedup_edges(edges: Vec<Edge>) -> Vec<Edge> {
    use std::collections::btree_map::Entry;
    let mut best: BTreeMap<(u32, u32, u8), Edge> = BTreeMap::new();
    for edge in edges {
        let key = (edge.source.0, edge.target.0, edge_kind_rank(edge.kind));
        match best.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(edge);
            }
            Entry::Occupied(mut slot) => {
                if is_stronger(&edge, slot.get()) {
                    slot.insert(edge);
                }
            }
        }
    }
    best.into_values().collect()
}

/// Returns `true` when `candidate` is a stronger edge than `current`: a hard edge
/// beats a soft one, and within equal hardness a higher confidence beats a lower.
fn is_stronger(candidate: &Edge, current: &Edge) -> bool {
    let candidate_hard = candidate.hardness == Hardness::Hard;
    let current_hard = current.hardness == Hardness::Hard;
    if candidate_hard != current_hard {
        return candidate_hard;
    }
    candidate.confidence > current.confidence
}

/// Maps an edge kind to a stable rank, so `(source, target, kind)` keys order
/// deterministically without relying on an `Ord` impl for [`EdgeKind`].
fn edge_kind_rank(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::ValueImport => 0,
        EdgeKind::TypeReference => 1,
        EdgeKind::Inheritance => 2,
        EdgeKind::Call => 3,
        EdgeKind::ReExport => 4,
    }
}

/// Appends an edge, skipping self-loops which carry no dependency information.
fn push_edge(
    edges: &mut Vec<Edge>,
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    hardness: Hardness,
    confidence: f64,
) {
    if source == target {
        return;
    }
    edges.push(Edge {
        source,
        target,
        kind,
        hardness,
        confidence,
    });
}

#[cfg(test)]
mod tests {
    use strata_ir::{EdgeKind, Hardness};

    use super::edge_shape;
    use crate::parse::RefKind;

    #[test]
    fn should_map_a_bare_package_specifier_edge_shape() {
        assert_eq!(
            edge_shape(RefKind::UsePath),
            (EdgeKind::ValueImport, Hardness::Hard)
        );
        assert_eq!(edge_shape(RefKind::Call), (EdgeKind::Call, Hardness::Hard));
        assert_eq!(
            edge_shape(RefKind::TypeRef),
            (EdgeKind::TypeReference, Hardness::Hard)
        );
        assert_eq!(
            edge_shape(RefKind::Qualifier),
            (EdgeKind::TypeReference, Hardness::Hard)
        );
        assert_eq!(
            edge_shape(RefKind::TraitImpl),
            (EdgeKind::Inheritance, Hardness::Hard)
        );
    }
}
