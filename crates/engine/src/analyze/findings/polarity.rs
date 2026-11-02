//! Polarity-matrix findings: production or test support depending on test code.

use std::collections::BTreeMap;

use strata_ir::{Node, Polarity, ScopeLevel, Snapshot};

use crate::analyze::search::node_names;
use crate::result::{Severity, Violation, ViolationKind};

/// Reports polarity-matrix breaches: production code depending on test code,
/// and test support depending on a test case.
///
/// The location holds the source and target file paths (one path when both
/// sides share a file), like every other finding kind; the detail names each
/// side with its file, so two same-named nodes in different files stay apart.
pub(super) fn polarity_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let nodes: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let files: BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();
    let names = node_names(snapshot);
    let file_of = |id: u32| {
        nodes
            .get(&id)
            .and_then(|node| files.get(&node.container.0))
            .cloned()
    };

    ir.edges
        .iter()
        .filter_map(|edge| {
            let source_polarity = nodes.get(&edge.source.0)?.polarity;
            let target_polarity = nodes.get(&edge.target.0)?.polarity;
            let (source_word, target_word) = match (source_polarity, target_polarity) {
                (Polarity::Production, Polarity::TestCase | Polarity::TestSupport) => {
                    ("production symbol", "test code")
                }
                (Polarity::TestSupport, Polarity::TestCase) => ("test support", "test case"),
                _ => return None,
            };
            let source = side(&names, &file_of, edge.source.0);
            let target = side(&names, &file_of, edge.target.0);
            let mut location: Vec<String> = Vec::new();
            for path in [file_of(edge.source.0), file_of(edge.target.0)]
                .into_iter()
                .flatten()
            {
                if !location.contains(&path) {
                    location.push(path);
                }
            }
            Some(Violation {
                kind: ViolationKind::Polarity,
                severity: Severity::Violation,
                detail: format!("{source_word} {source} depends on {target_word} {target}"),
                location,
                break_suggestions: None,
                capacity: None,
            })
        })
        .collect()
}

/// One side of the edge as ``` `name` (path)```; the path is left out when the
/// node has no file container.
fn side(
    names: &BTreeMap<u32, String>,
    file_of: &impl Fn(u32) -> Option<String>,
    id: u32,
) -> String {
    let name = names.get(&id).cloned().unwrap_or_default();
    match file_of(id) {
        Some(path) => format!("`{name}` ({path})"),
        None => format!("`{name}`"),
    }
}
