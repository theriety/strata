//! Test-polarity classification by path convention and edge reachability.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{Edge, Node, NodeId, Polarity};

use crate::parse::ParsedModule;

/// Classifies every node's polarity.
///
/// A module whose path matches a test convention contributes [`Polarity::TestCase`]
/// nodes; `conftest.py` and production helpers reachable (over edges) only from
/// test nodes become [`Polarity::TestSupport`]; everything else is production.
pub(super) fn classify_polarity(
    modules: &[ParsedModule],
    nodes: &[Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    exported: &[bool],
    edges: &[Edge],
) -> Vec<Polarity> {
    let mut polarity = vec![Polarity::Production; nodes.len()];

    let mut is_test_node = vec![false; nodes.len()];
    let mut is_support_node = vec![false; nodes.len()];
    for module in modules {
        let support = is_support_path(&module.path);
        let test = is_test_path(&module.path);
        if !support && !test {
            continue;
        }
        if let Some(table) = local.get(&module.path) {
            for &id in table.values() {
                let index = id.0 as usize;
                if support {
                    if let Some(slot) = polarity.get_mut(index) {
                        *slot = Polarity::TestSupport;
                    }
                    if let Some(slot) = is_support_node.get_mut(index) {
                        *slot = true;
                    }
                } else {
                    if let Some(slot) = polarity.get_mut(index) {
                        *slot = Polarity::TestCase;
                    }
                    if let Some(slot) = is_test_node.get_mut(index) {
                        *slot = true;
                    }
                }
            }
        }
    }

    // Reachability: a production helper consumed only by tests is TestSupport.
    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false)
            || is_support_node
                .get(edge.source.0 as usize)
                .copied()
                .unwrap_or(false);
        let table = if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        };
        if let Some(slot) = table.get_mut(edge.target.0 as usize) {
            *slot = true;
        }
    }

    for (index, slot) in polarity.iter_mut().enumerate() {
        if *slot != Polarity::Production {
            continue;
        }
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
pub(super) fn apply_polarity(nodes: &mut [Node], polarity: &[Polarity]) {
    for (node, &class) in nodes.iter_mut().zip(polarity) {
        node.polarity = class;
    }
}

/// Returns `true` if `path` is a test case file by convention.
fn is_test_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.strip_suffix(".py").unwrap_or(file);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || (path.contains("tests/") && !is_support_path(path))
}

/// Returns `true` if `path` is test-support by convention (`conftest.py`).
fn is_support_path(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == "conftest.py"
}

#[cfg(test)]
mod tests {
    use super::{is_support_path, is_test_path};

    #[test]
    fn should_recognise_test_paths_by_convention() {
        assert!(is_test_path("pkg/test_app.py"));
        assert!(is_test_path("pkg/app_test.py"));
        assert!(is_test_path("tests/test_thing.py"));
        assert!(!is_test_path("pkg/app.py"));
        assert!(is_support_path("tests/conftest.py"));
        assert!(!is_support_path("pkg/app.py"));
    }
}
