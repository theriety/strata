//! Advisory uses (ADR-25): name-matched uses from `cfg(not(test))` code.
//!
//! The semantic loader cannot see that code, so its identifiers are matched to
//! declarations by name alone. The result informs visibility advice and nothing
//! else: it is returned apart from the edges and never joins them, so no score,
//! cohesion, cycle or move can read it.

use std::collections::{BTreeSet, HashSet};

use smol_str::SmolStr;
use strata_ir::{AdvisoryUse, Node, NodeId, Polarity};

use super::assignment::NodeAssignment;
use crate::parse::ParsedFile;

/// Every `(user, used)` pair of a declaration with advisory names and a
/// declaration sharing one of them, de-duplicated and without self-uses.
pub(super) fn candidates(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    unloaded: &HashSet<SmolStr>,
) -> BTreeSet<(NodeId, NodeId)> {
    let mut found = BTreeSet::new();
    for file in files {
        // A dropped `use` has no declaration; the file's first one stands in
        // as the user (a file with none yields no pair; backlog).
        let file_user = file
            .declarations
            .first()
            .and_then(|declaration| assignment.node_at(&file.path, declaration.byte_start));
        if let Some(user) = file_user {
            for used in file
                .dropped_uses
                .iter()
                .flat_map(|name| assignment.nodes_named(name))
            {
                if *used != user {
                    found.insert((user, *used));
                }
            }
        }
        for declaration in &file.declarations {
            let Some(user) = assignment.node_at(&file.path, declaration.byte_start) else {
                continue;
            };
            // an unloaded file is one maybe-dropped body for each declaration.
            let whole = unloaded.contains(&file.path).then_some(&file.names);
            for used in declaration
                .advisory_names
                .iter()
                .chain(whole.into_iter().flatten())
                .flat_map(|name| assignment.nodes_named(name))
            {
                if *used != user {
                    found.insert((user, *used));
                }
            }
        }
    }
    found
}

/// Keeps the pairs whose target is production code, once polarity is final: a
/// name matching test-only code is no reason to keep a production item wide.
pub(super) fn settle(candidates: BTreeSet<(NodeId, NodeId)>, nodes: &[Node]) -> Vec<AdvisoryUse> {
    let production = |id: NodeId| {
        nodes
            .get(id.0 as usize)
            .is_some_and(|node| node.id == id && node.polarity == Polarity::Production)
    };
    candidates
        .into_iter()
        .filter(|(_, used)| production(*used))
        .map(|(user, used)| AdvisoryUse { user, used })
        .collect()
}
