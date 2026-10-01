use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerId, Node, ScopeLevel};

use crate::analyze::relocation::{CandidateTree, PipelineSolver};
use crate::result::{SymbolKind, SymbolMove};

use super::outcome::SymbolOutcome;

impl PipelineSolver<'_> {
    /// Narrates the accepted symbol relocations as scored [`SymbolMove`] DTOs.
    ///
    /// `from_path` reads the current tree and `to_path` the assembled candidate.
    /// `broken_imports` counts distinct external callers or callees to repoint.
    pub(in crate::analyze::relocation) fn symbol_narrate(
        &self,
        assembled: &CandidateTree,
        symbols: &SymbolOutcome,
    ) -> Vec<SymbolMove> {
        if symbols.relocations.is_empty() {
            return Vec::new();
        }
        let ir = self.snapshot.ir();
        let base = &assembled.placement;
        let effective = |id: u32| -> Option<ContainerId> {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| base.get(&id).copied())
        };
        let current_name: BTreeMap<u32, &str> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let candidate_name: BTreeMap<u32, &str> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let kind_of = |node: &Node| match node.kind {
            strata_ir::NodeKind::Symbol => SymbolKind::Symbol,
            strata_ir::NodeKind::Type => SymbolKind::Type,
            strata_ir::NodeKind::FileBody => {
                unreachable!("file-body nodes are immobile and cannot appear in symbol narration")
            }
        };

        let by_node: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
        let mut moves = Vec::with_capacity(symbols.relocations.len());
        for relocation in &symbols.relocations {
            let Some(symbol) = by_node.get(&relocation.node).copied() else {
                continue;
            };
            let Some(&home) = base.get(&relocation.node) else {
                continue;
            };
            debug_assert_eq!(
                home, relocation.from_file,
                "origin file is the symbol's base placement"
            );
            let mut severed: BTreeSet<ContainerId> = BTreeSet::new();
            for edge in &ir.edges {
                let neighbour = if edge.source.0 == relocation.node {
                    edge.target.0
                } else if edge.target.0 == relocation.node {
                    edge.source.0
                } else {
                    continue;
                };
                let Some(place) = effective(neighbour) else {
                    continue;
                };
                if place == relocation.to_file {
                    continue;
                }
                severed.insert(place);
            }
            moves.push(SymbolMove {
                symbol: symbol.name.to_string(),
                kind: kind_of(symbol),
                from_path: current_name
                    .get(&symbol.container.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                to_path: candidate_name
                    .get(&relocation.to_file.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                delta: -relocation.delta,
                broken_imports: u32::try_from(severed.len()).unwrap_or(u32::MAX),
            });
        }
        moves
    }
}
