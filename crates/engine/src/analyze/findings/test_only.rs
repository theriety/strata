//! Test-only findings: production items whose every user is test code.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{Node, NodeId, NodeKind, ScopeLevel, Snapshot};

use crate::analyze::search::node_names;
use crate::result::{Severity, Violation, ViolationKind};

/// The ids of the nodes that earn a test-only finding.
///
/// A node qualifies when the source does not declare it as test code, it is
/// neither a re-export nor exempt from advice (public API is allowed: it is
/// reported with its own wording), it is not a file body, it has users, and
/// every user is declared as test code in the source. The source fact
/// `Node::test_source` is read, never the final polarity: adapters demote
/// production code that only tests use to test support, so polarity cannot
/// tell a real test from a demoted item. An advisory use is a production user
/// the loader cannot see, so it silences the finding. When unsure the node is
/// left out. Items in a test-support crate (`test_support_roots`, stated by the
/// adapter) are test infrastructure and are skipped.
pub(super) fn test_only_nodes(snapshot: &Snapshot) -> BTreeSet<u32> {
    // a type is also "used" by its own members (`Type::new` names `Type`):
    // such a member speaks for the type only when it is test-only itself.
    let first = qualifying(snapshot, &BTreeSet::new());
    qualifying(snapshot, &first)
}

/// One pass of the rule; `settled` are members already known to be test-only.
fn qualifying(snapshot: &Snapshot, settled: &BTreeSet<u32>) -> BTreeSet<u32> {
    let ir = snapshot.ir();
    let nodes: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let advised: BTreeSet<u32> = ir
        .advisory_uses
        .iter()
        .filter(|used| !used.from_test)
        .map(|used| used.used.0)
        .collect();

    // `Some(true)` while every user seen so far is test code.
    let mut all_tests: BTreeMap<u32, bool> = BTreeMap::new();
    for edge in &ir.edges {
        if edge.source == edge.target {
            continue;
        }
        // an edge from an unknown node is a user we cannot judge.
        let source_is_test = nodes
            .get(&edge.source.0)
            .is_some_and(|source| source.test_source);
        // a test-only member of the target type is no production user of it.
        let own_settled_member = settled.contains(&edge.source.0)
            && nodes
                .get(&edge.source.0)
                .is_some_and(|source| source.owner == Some(NodeId(edge.target.0)));
        let entry = all_tests.entry(edge.target.0).or_insert(true);
        *entry &= source_is_test || own_settled_member;
    }

    // items of a test-support crate are test infrastructure, never advice.
    let harness_files: BTreeSet<u32> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter(|container| {
            ir.test_support_roots.iter().any(|root| {
                container
                    .name
                    .strip_prefix(root.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .map(|container| container.id.0)
        .collect();

    all_tests
        .into_iter()
        .filter(|&(id, tests_only)| {
            tests_only
                && !advised.contains(&id)
                && nodes.get(&id).is_some_and(|node| {
                    !node.test_source
                        && !node.re_export
                        && !node.advice_exempt
                        && node.kind != NodeKind::FileBody
                        && !harness_files.contains(&node.container.0)
                })
        })
        .map(|(id, _)| id)
        .collect()
}

/// Reports each production item that only tests use.
pub(super) fn test_only_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let test_only = test_only_nodes(snapshot);
    let ir = snapshot.ir();
    let names = node_names(snapshot);
    let files: BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();

    ir.nodes
        .iter()
        .filter(|node| test_only.contains(&node.id.0))
        .map(|node| {
            let name = names.get(&node.id.0).cloned().unwrap_or_default();
            let location = files.get(&node.container.0).map_or_else(
                || vec![name.clone()],
                |path| vec![path.clone(), name.clone()],
            );
            let detail = if node.public_api {
                format!(
                    "Only tests in this repo use `{name}`. It is public, so code outside this repo may use it too; check before moving or deleting it."
                )
            } else {
                format!("`{name}` is used only by tests; move it under test-only code or delete it")
            };
            Violation {
                kind: ViolationKind::TestOnly,
                severity: Severity::Violation,
                detail,
                location,
                break_suggestions: None,
                capacity: None,
            }
        })
        .collect()
}
