//! Visibility findings: symbols declared wider than the scope their uses derive.

use std::collections::BTreeMap;

use strata_core::visibility::{Finding, derive_visibility};
use strata_ir::{Node, ScopeLevel, Snapshot};

use crate::analyze::search::node_names;
use crate::result::{Severity, Violation, ViolationKind};

/// Reports symbols whose declared visibility is wider than their derived scope.
pub(super) fn visibility_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let result = derive_visibility(&ir.containers, &ir.nodes, &ir.edges);
    let names = node_names(snapshot);
    let nodes: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let files: BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();

    // a language with a fixed ladder of spellable scopes cannot narrow below its
    // smallest rung that still covers the derived scope.
    let ladders: BTreeMap<u32, &[ScopeLevel]> = ir
        .scope_ladders
        .iter()
        .map(|ladder| (ladder.node.0, ladder.levels.as_slice()))
        .collect();

    result
        .findings
        .iter()
        .filter_map(|finding| {
            let levels = ladders.get(&finding.node.0).copied().unwrap_or_default();
            let derived = expressible_floor(finding.derived, levels);
            (finding.declared > derived).then_some(Finding {
                derived,
                ..*finding
            })
        })
        .map(|finding| {
            let name = names.get(&finding.node.0).cloned().unwrap_or_default();
            let source_path = nodes
                .get(&finding.node.0)
                .and_then(|node| files.get(&node.container.0));
            let detail = source_path.map_or_else(
                || {
                    format!(
                        "`{name}` is exported at {:?} but needed only at {:?}",
                        finding.declared, finding.derived
                    )
                },
                |path| {
                    format!(
                        "`{name}` in `{path}` is exported at {:?} but needed only at {:?}",
                        finding.declared, finding.derived
                    )
                },
            );
            let location = source_path.map_or_else(
                || vec![name.clone()],
                |path| vec![path.clone(), name.clone()],
            );
            Violation {
                kind: ViolationKind::Visibility,
                severity: Severity::Violation,
                detail,
                location,
                break_suggestions: None,
                capacity: None,
            }
        })
        .collect()
}

/// Raises `derived` to the smallest expressible level that covers it.
///
/// With no ladder, or when no rung reaches `derived`, the level is unchanged.
fn expressible_floor(derived: ScopeLevel, ladder: &[ScopeLevel]) -> ScopeLevel {
    ladder
        .iter()
        .copied()
        .filter(|level| *level >= derived)
        .min()
        .unwrap_or(derived)
}
