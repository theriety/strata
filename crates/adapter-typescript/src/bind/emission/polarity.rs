//! Node polarity classification from test-path conventions and edge reachability.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{Edge, Node, NodeId, Polarity};

use crate::parse::ParsedModule;

/// Classifies every node's polarity.
///
/// A module whose path matches a test convention contributes [`Polarity::TestCase`]
/// nodes. Production helpers reachable (over edges) only from test nodes become
/// [`Polarity::TestSupport`]; everything else stays [`Polarity::Production`].
pub(in crate::bind) fn classify_polarity(
    modules: &[ParsedModule],
    nodes: &[Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    exported: &[bool],
    edges: &[Edge],
) -> Vec<Polarity> {
    let mut polarity = vec![Polarity::Production; nodes.len()];

    // Test cases: every node declared in a test file.
    let mut is_test_node = vec![false; nodes.len()];
    for module in modules {
        if !is_test_path(&module.path) {
            continue;
        }
        if let Some(table) = local.get(&module.path) {
            for &id in table.values() {
                if let Some(slot) = polarity.get_mut(id.0 as usize) {
                    *slot = Polarity::TestCase;
                }
                if let Some(slot) = is_test_node.get_mut(id.0 as usize) {
                    *slot = true;
                }
            }
        }
    }

    // Reachability: a production node consumed only by test nodes is TestSupport.
    // Build the set of production nodes reachable from production nodes; anything
    // referenced from a test but not reachable from production is support.
    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false);
        if let Some(slot) = (if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        })
        .get_mut(edge.target.0 as usize)
        {
            *slot = true;
        }
    }

    for (index, slot) in polarity.iter_mut().enumerate() {
        if *slot == Polarity::TestCase {
            continue;
        }
        // An exported symbol is part of the production API surface; only a
        // private helper consumed solely by tests is genuine test support.
        if exported.get(index).copied().unwrap_or(false) {
            continue;
        }
        let production = consumed_by_production.get(index).copied().unwrap_or(false);
        let test = consumed_by_test.get(index).copied().unwrap_or(false);
        if test && !production {
            *slot = Polarity::TestSupport;
        }
    }

    polarity
}

/// Overwrites each node's polarity from the classification vector.
pub(in crate::bind) fn apply_polarity(nodes: &mut [Node], polarity: &[Polarity]) {
    for (node, &class) in nodes.iter_mut().zip(polarity) {
        node.polarity = class;
    }
}

/// Returns `true` if `path` is a test file by convention.
fn is_test_path(path: &str) -> bool {
    if path.split('/').any(|segment| segment == "__tests__") {
        return true;
    }

    let Some(stem) = path
        .strip_suffix(".tsx")
        .or_else(|| path.strip_suffix(".ts"))
    else {
        return false;
    };
    let file_name = stem.rsplit('/').next().unwrap_or(stem);

    file_name
        .split('.')
        .skip(1)
        .any(|segment| matches!(segment, "spec" | "test"))
}

#[cfg(test)]
mod tests {
    use super::is_test_path;

    #[test]
    fn should_recognise_test_paths_by_convention() {
        assert!(is_test_path("src/__tests__/app.spec.ts"));
        assert!(is_test_path("src/app.test.ts"));
        assert!(is_test_path("src/__tests__/support.ts"));
        assert!(!is_test_path("src/app.ts"));
    }

    #[test]
    fn should_recognize_qualified_test_paths_without_matching_words() {
        assert!(is_test_path("src/worker.spec.int.ts"));
        assert!(is_test_path("src/worker.test.integration.ts"));
        assert!(is_test_path("src/worker.spec.browser.tsx"));
        assert!(!is_test_path("src/specification.ts"));
        assert!(!is_test_path("src/worker.testable.tsx"));
    }
}
