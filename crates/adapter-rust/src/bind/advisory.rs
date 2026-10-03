//! Advisory uses (ADR-25): name-matched uses from `cfg(not(test))` code.
//!
//! The semantic loader cannot see that code, so its identifiers are matched to
//! declarations by name alone. The result informs visibility advice and nothing
//! else: it is returned apart from the edges and never joins them, so no score,
//! cohesion, cycle or move can read it.

use std::collections::{BTreeMap, HashSet};

use smol_str::SmolStr;
use strata_ir::{AdvisoryUse, Node, NodeId};

use super::assignment::NodeAssignment;
use crate::parse::ParsedFile;

/// Every `(user, used)` pair of a declaration with advisory names and a
/// declaration sharing one of them, de-duplicated and without self-uses.
pub(super) fn candidates(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    unloaded: &HashSet<SmolStr>,
) -> BTreeMap<(NodeId, NodeId), bool> {
    // the flag stays true only while every source of a pair is test-only code.
    let mut found: BTreeMap<(NodeId, NodeId), bool> = BTreeMap::new();
    let mut note = |pair: (NodeId, NodeId), from_test: bool| {
        let entry = found.entry(pair).or_insert(true);
        *entry &= from_test;
    };
    for file in files {
        // A dropped `use` has no declaration; a declaration of the file stands
        // in as the user: the first non-test one that is not the used item, else
        // the first (a file with none yields no pair; backlog).
        let stand_ins: Vec<NodeId> = file
            .declarations
            .iter()
            // a test declaration would turn a production file's names into test names.
            .filter(|declaration| !declaration.cfg_test && !declaration.test_case)
            .filter_map(|declaration| assignment.node_at(&file.path, declaration.byte_start))
            .take(2)
            .collect();
        for (names, from_test) in [(&file.dropped_uses, false), (&file.test_dropped_uses, true)] {
            for used in names.iter().flat_map(|name| assignment.nodes_named(name)) {
                // the used item stands in for itself when it is the file's only
                // production declaration: the use is still a production use.
                if let Some(user) = stand_ins
                    .iter()
                    .find(|user| *user != used)
                    .or_else(|| stand_ins.first())
                {
                    note((*user, *used), from_test);
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
                    note((user, *used), false);
                }
            }
        }
    }
    found
}

/// Keeps the pairs whose target is production code in the source (never the
/// final polarity: tests-only users demote a production item to test support): a
/// name matching test-only code is no reason to keep a production item wide.
pub(super) fn settle(
    candidates: BTreeMap<(NodeId, NodeId), bool>,
    nodes: &[Node],
) -> Vec<AdvisoryUse> {
    let production = |id: NodeId| {
        nodes
            .get(id.0 as usize)
            .is_some_and(|node| node.id == id && !node.test_source)
    };
    candidates
        .into_iter()
        .filter(|((_, used), _)| production(*used))
        .map(|((user, used), from_test)| AdvisoryUse {
            user,
            used,
            // a user the source declares as test code (a test file, an
            // inner `cfg(test)`) is no production user whatever its names.
            from_test: from_test
                || nodes
                    .get(user.0 as usize)
                    .is_some_and(|node| node.id == user && node.test_source),
        })
        .collect()
}
