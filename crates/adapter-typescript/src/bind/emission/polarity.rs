//! Node polarity classification from test-path conventions and edge reachability.

use std::collections::HashMap;

use smol_str::SmolStr;
use unlace_ir::{Edge, Node, NodeId, Polarity};

use std::collections::BTreeSet;

use crate::parse::ParsedModule;
use crate::test_paths::{is_test_case_path, is_test_path};

/// Classifies every node's polarity.
///
/// A test-named module (or one in a `__tests__` folder) contributes
/// [`Polarity::TestCase`] nodes; any other module under a test root contributes
/// [`Polarity::TestSupport`] nodes. Production helpers reachable (over edges)
/// only from test nodes become [`Polarity::TestSupport`] too; everything else
/// stays [`Polarity::Production`].
pub(in crate::bind) fn classify_polarity(
    modules: &[ParsedModule],
    nodes: &[Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    exported: &[bool],
    edges: &[Edge],
    package_roots: &BTreeSet<SmolStr>,
) -> Vec<Polarity> {
    let mut polarity = vec![Polarity::Production; nodes.len()];

    // Test code: every node declared in a test file, a case when the file is
    // test-named and support otherwise.
    let mut is_test_node = vec![false; nodes.len()];
    for module in modules {
        if !is_test_path(&module.path, package_roots) {
            continue;
        }
        let role = if is_test_case_path(&module.path) {
            Polarity::TestCase
        } else {
            Polarity::TestSupport
        };
        if let Some(table) = local.get(&module.path) {
            for &id in table.values() {
                if let Some(slot) = polarity.get_mut(id.0 as usize) {
                    *slot = role;
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
        if *slot != Polarity::Production {
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
    package_roots: &BTreeSet<SmolStr>,
) {
    for module in modules
        .iter()
        .filter(|module| is_test_path(&module.path, package_roots))
    {
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
    package_roots: &BTreeSet<SmolStr>,
) {
    for module in modules
        .iter()
        .filter(|module| !is_test_path(&module.path, package_roots))
    {
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
