//! The no-new-exports guard of the symbol pass (ADR-34): a symbol move must
//! not make a declaration that only its own file uses reachable from another
//! file.

use std::collections::{BTreeMap, BTreeSet};

use unlace_ir::{ContainerId, Edge, Node, ScopeLevel};

use crate::analyze::relocation::symbol::ledger::Ledger;

/// Pass-start facts the no-new-exports guard reads: which declarations are
/// private to their file, and who depends on whom.
pub(super) struct ExportGuard {
    /// Declarations private to their pass-start file: declared at file scope,
    /// not public API, not a re-export, and with every consumer in the same
    /// file.
    private: BTreeSet<u32>,
    /// Distinct declarations each declaration depends on.
    dependencies: BTreeMap<u32, BTreeSet<u32>>,
    /// Distinct declarations that depend on each declaration.
    dependants: BTreeMap<u32, BTreeSet<u32>>,
}

impl ExportGuard {
    /// Builds the guard over the pass-start placement `base`.
    pub(super) fn new(base: &BTreeMap<u32, ContainerId>, nodes: &[Node], edges: &[Edge]) -> Self {
        let mut dependencies: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut dependants: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        let mut consumed_elsewhere: BTreeSet<u32> = BTreeSet::new();
        for edge in edges.iter().filter(|edge| edge.source != edge.target) {
            let (source, target) = (edge.source.0, edge.target.0);
            dependencies.entry(source).or_default().insert(target);
            dependants.entry(target).or_default().insert(source);
            if base.get(&source) != base.get(&target) {
                consumed_elsewhere.insert(target);
            }
        }
        let private = nodes
            .iter()
            .filter(|node| {
                base.contains_key(&node.id.0)
                    && node.visibility == ScopeLevel::File
                    && !node.public_api
                    && !node.re_export
                    && !consumed_elsewhere.contains(&node.id.0)
            })
            .map(|node| node.id.0)
            .collect();
        Self {
            private,
            dependencies,
            dependants,
        }
    }

    /// Whether moving `node` out of `source_file` would expose a private
    /// declaration across files: a private helper staying behind that `node`
    /// depends on, or `node` itself when it is private and a declaration
    /// staying behind depends on it. Either one would have to become exported
    /// and newly imported, which the move's gain does not pay for.
    pub(super) fn exposes(&self, node: u32, source_file: ContainerId, ledger: &Ledger<'_>) -> bool {
        let stays = |other: &u32| *other != node && ledger.effective(*other) == Some(source_file);
        let exposes_helper = self.dependencies.get(&node).is_some_and(|targets| {
            targets
                .iter()
                .any(|target| stays(target) && self.private.contains(target))
        });
        let exposes_itself = self.private.contains(&node)
            && self
                .dependants
                .get(&node)
                .is_some_and(|sources| sources.iter().any(stays));
        exposes_helper || exposes_itself
    }
}
