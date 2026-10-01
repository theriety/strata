//! Three-valued test polarity classification.
//!
//! Refines each node's base polarity using the `tests/` directory convention and
//! production-versus-test reachability over the resolved edges.

use std::path::Path;

use strata_ir::{ContainerId, Edge, Node, Polarity, ScopeLevel};

use super::containers::ContainerBuilder;
use crate::parse::ParsedFile;

/// Reclassifies node polarity into the three-valued scheme.
///
/// `#[test]` functions are already [`Polarity::TestCase`] and other `#[cfg(test)]`
/// items already [`Polarity::TestSupport`] from [`node_for`]; declarations under a
/// `tests/` directory are promoted to test cases here, since cargo compiles that
/// tree only under `cfg(test)`. Finally, a *production* node consumed only by test
/// nodes — never reachable from production — is a shared test utility and becomes
/// [`Polarity::TestSupport`]; an exported symbol stays production, as it is part
/// of the public surface regardless of who happens to use it in-tree.
pub(super) fn classify_polarity(files: &[ParsedFile], nodes: &mut [Node], edges: &[Edge]) {
    let test_containers = test_file_containers(files);
    for node in nodes.iter_mut() {
        if test_containers.contains(&node.container) {
            node.polarity = Polarity::TestCase;
        }
    }

    // Both test cases and test-support already carry test polarity, so anything
    // they consume is a test consumer for the reachability rule below.
    let is_test_node: Vec<bool> = nodes
        .iter()
        .map(|node| node.polarity != Polarity::Production)
        .collect();

    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false);
        let bucket = if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        };
        if let Some(slot) = bucket.get_mut(edge.target.0 as usize) {
            *slot = true;
        }
    }

    for (index, node) in nodes.iter_mut().enumerate() {
        // Only production nodes are candidates for promotion to test support;
        // nodes already in the test zone keep their polarity.
        if node.polarity != Polarity::Production {
            continue;
        }
        // An exported symbol is part of the production API surface; only a
        // private helper consumed solely by tests is genuine test support.
        if node.visibility >= ScopeLevel::Package {
            continue;
        }
        let production = consumed_by_production.get(index).copied().unwrap_or(false);
        let test = consumed_by_test.get(index).copied().unwrap_or(false);
        if test && !production {
            node.polarity = Polarity::TestSupport;
        }
    }
}

/// Returns the set of leaf container ids for files that live under a `tests/`
/// directory, recomputed via [`ContainerBuilder`] so it matches assignment.
fn test_file_containers(files: &[ParsedFile]) -> std::collections::HashSet<ContainerId> {
    let builder = ContainerBuilder::build(files, Path::new(""));
    files
        .iter()
        .filter(|file| is_test_path(&file.path))
        .map(|file| builder.file_of(&file.path))
        .collect()
}

/// Returns `true` when a repository-relative path is a test path by convention.
///
/// A path is a test path when any segment is exactly `tests` — cargo's integration
/// test directory — so `crates/app/tests/it.rs` classifies as test code.
fn is_test_path(path: &str) -> bool {
    path.split('/').any(|segment| segment == "tests")
}
