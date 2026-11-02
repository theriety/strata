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

/// Records [`Node::test_source`] for every node declared in a test file, before
/// [`classify_polarity`] demotes production helpers that only tests use.
pub(in crate::bind) fn mark_test_source(
    modules: &[ParsedModule],
    nodes: &mut [Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
) {
    for module in modules.iter().filter(|module| is_test_path(&module.path)) {
        for id in local
            .get(&module.path)
            .into_iter()
            .flat_map(HashMap::values)
        {
            if let Some(node) = nodes.get_mut(id.0 as usize) {
                node.test_source = true;
            }
        }
    }
}

/// Exempts from advice every export of a module a production file loads as a
/// whole (`import * as ns`, literal `require('./a')`): the member read through
/// the module object is not tracked, so no export may be called test-only.
/// The dependency graph is untouched.
pub(in crate::bind) fn exempt_module_objects(
    modules: &[ParsedModule],
    resolver: &crate::bind::resolution::Resolver,
    exports: &crate::bind::ExportTable,
    nodes: &mut [Node],
) {
    for module in modules.iter().filter(|module| !is_test_path(&module.path)) {
        let namespaces = module
            .imports
            .iter()
            .filter(|import| import.namespace)
            .map(|import| &import.source);
        let requires = module
            .declarations
            .iter()
            .flat_map(|declaration| &declaration.requires);
        for source in namespaces.chain(requires) {
            let Some(target) = resolver.resolve(&module.path, source) else {
                continue;
            };
            for id in exports.get(&target).into_iter().flat_map(HashMap::values) {
                if let Some(node) = nodes.get_mut(id.0 as usize) {
                    node.advice_exempt = true;
                }
            }
        }
    }
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
