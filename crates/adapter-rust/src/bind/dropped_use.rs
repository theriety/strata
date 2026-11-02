//! Declarations exempted from advice by `use` items the loader may drop
//! (ADR-25 rule 7).
//!
//! A dropped `use` is invisible to the semantic view, yet with its cfg on it
//! needs its targets visible from its file and, when not private, re-exports
//! them. Advice only ever narrows, so every uncertainty here suppresses it.
//! These nodes are set apart from the public API: nobody outside the tree
//! uses them, code the adapter cannot see may.

use std::collections::HashSet;

use smol_str::SmolStr;
use strata_ir::NodeId;

use super::assignment::NodeAssignment;
use crate::parse::{ParsedFile, VisibilityKind};

/// Nodes that [`Node::advice_exempt`](strata_ir::Node::advice_exempt) marks:
/// the names of dropped re-exports, the names a dropped `use` imports into a
/// file with no declaration to carry them, and every declaration of the files
/// a dropped glob may import from (any visibility).
/// An `unloaded` file (the loader never reached it) is one maybe-dropped
/// body: its `pub` items are exempt, and when it has no declaration its
/// identifiers exempt the same-named ones.
pub(super) fn exempt_nodes(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    unloaded: &HashSet<SmolStr>,
) -> Vec<NodeId> {
    let named = |names: &[smol_str::SmolStr]| -> Vec<NodeId> {
        names
            .iter()
            .flat_map(|name| assignment.nodes_named(name).iter().copied())
            .collect()
    };
    let mut found = Vec::new();
    for file in files {
        found.extend(named(&file.dropped_re_exports));
        // no declaration of the file can stand in as the advisory user, and
        // borrowing another file's node would misplace the rung.
        if file.declarations.is_empty() {
            found.extend(named(&file.dropped_uses));
            if unloaded.contains(&file.path) {
                found.extend(named(&file.names));
            }
        }
        if unloaded.contains(&file.path) {
            found.extend(
                file.declarations
                    .iter()
                    .filter(|declaration| declaration.visibility_kind == VisibilityKind::Public)
                    .filter_map(|declaration| {
                        assignment.node_at(&file.path, declaration.byte_start)
                    }),
            );
        }
        for path in &file.dropped_glob_files {
            let Some(target) = files.iter().find(|other| other.path == *path) else {
                continue;
            };
            found.extend(target.declarations.iter().filter_map(|declaration| {
                assignment.node_at(&target.path, declaration.byte_start)
            }));
        }
    }
    found
}
