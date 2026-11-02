//! Visibility findings: symbols declared wider than the scope their uses derive.

use std::collections::BTreeMap;

use strata_core::visibility::{Finding, derive_visibility};
use strata_ir::{Node, ScopeLadder, ScopeLevel, Snapshot};

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

    // a language that states a scope ladder is judged by the ladder alone: it
    // advises only a strictly narrower rung that holds every user (ADR-25).
    let ladders: BTreeMap<u32, &ScopeLadder> = ir
        .scope_ladders
        .iter()
        .map(|ladder| (ladder.node.0, ladder))
        .collect();

    result
        .findings
        .iter()
        .filter_map(|finding| match ladders.get(&finding.node.0) {
            Some(ladder) => {
                let (declared, derived) = ladder.declared.zip(ladder.needed)?;
                // equal labels would read "exported at Folder but needed only at
                // Folder": a different rung with the same label stays silent.
                if declared == derived {
                    return None;
                }
                Some((
                    Finding {
                        declared,
                        derived,
                        ..*finding
                    },
                    ladder.spelling.as_deref(),
                ))
            }
            // no ladder: only a file-local need is known to be spellable.
            None => (finding.derived == ScopeLevel::File && finding.declared > ScopeLevel::File)
                .then_some((*finding, None)),
        })
        .map(|(finding, spelling)| {
            let name = names.get(&finding.node.0).cloned().unwrap_or_default();
            let source_path = nodes
                .get(&finding.node.0)
                .and_then(|node| files.get(&node.container.0));
            let mut detail = source_path.map_or_else(
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
            if let Some(spelling) = spelling {
                detail.push_str("; narrowest spelling: ");
                detail.push_str(spelling);
            }
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
