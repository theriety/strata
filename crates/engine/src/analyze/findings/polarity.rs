//! Polarity-matrix findings: production or test support depending on test code.

use std::collections::BTreeMap;

use strata_ir::{Polarity, Snapshot};

use crate::analyze::search::node_names;
use crate::result::{Severity, Violation, ViolationKind};

/// Reports polarity-matrix breaches: production code depending on test code,
/// and test support depending on a test case.
pub(super) fn polarity_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let polarity_by_id: BTreeMap<u32, Polarity> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.polarity))
        .collect();
    let names = node_names(snapshot);

    ir.edges
        .iter()
        .filter_map(|edge| {
            let source_polarity = polarity_by_id.get(&edge.source.0)?;
            let target_polarity = polarity_by_id.get(&edge.target.0)?;
            let (source_word, target_word) = match (source_polarity, target_polarity) {
                (Polarity::Production, Polarity::TestCase | Polarity::TestSupport) => {
                    ("production symbol", "test code")
                }
                (Polarity::TestSupport, Polarity::TestCase) => ("test support", "test case"),
                _ => return None,
            };
            let source = names.get(&edge.source.0).cloned().unwrap_or_default();
            let target = names.get(&edge.target.0).cloned().unwrap_or_default();
            Some(Violation {
                kind: ViolationKind::Polarity,
                severity: Severity::Violation,
                detail: format!("{source_word} `{source}` depends on {target_word} `{target}`"),
                location: vec![source, target],
                break_suggestions: None,
                capacity: None,
            })
        })
        .collect()
}
