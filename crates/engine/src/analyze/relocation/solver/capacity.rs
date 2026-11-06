//! Physical-capacity vetoes and the capacity remainder for polish evidence.

use std::collections::BTreeMap;

use unlace_core::cluster::{ClusterId, Partition};

use crate::analyze::findings::{
    RealRoots, physical_folder_entries_repository_relative, physical_folder_findings,
    walk_all_capacity,
};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, PipelineSolver};
use crate::config::CapacityConfig;
use crate::result::{CapacityRemainder, ContainerNode, Level, Severity};

impl PipelineSolver<'_> {
    pub(in crate::analyze::relocation) fn physical_entry_counts(
        &self,
        parts: &Partition,
    ) -> BTreeMap<Vec<String>, u32> {
        let assembled = self.assemble(parts);
        physical_folder_entries_repository_relative(
            &assembled.tree,
            Some(&assembled.namespace_by_file),
        )
    }

    pub(in crate::analyze::relocation) fn physical_entry_counts_with_polish_evidence(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> BTreeMap<Vec<String>, u32> {
        let assembled = self.assemble_with_polish_evidence(parts, evidence);
        physical_folder_entries_repository_relative(
            &assembled.tree,
            Some(&assembled.namespace_by_file),
        )
    }

    pub(in crate::analyze) fn permits_physical_capacity(
        &self,
        parts: &Partition,
        scc: u32,
        target: ClusterId,
    ) -> bool {
        if self.caps.folder == 0 {
            return true;
        }
        let Some(source) = parts.cluster_of(scc) else {
            return false;
        };
        if source == target {
            return true;
        }
        let before = self.physical_entry_counts(parts);
        let mut assignment = parts.assignment().to_vec();
        let Some(slot) = assignment.get_mut(scc as usize) else {
            return false;
        };
        *slot = target;
        let moved = Partition::from_assignment(assignment, parts.cluster_count());
        let after = self.physical_entry_counts(&moved);
        before.keys().chain(after.keys()).all(|path| {
            let prior = before
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            let next = after
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            next <= prior
        })
    }

    pub(in crate::analyze::relocation) fn permits_mirror_physical_capacity(
        &self,
        parts: &Partition,
        scc: u32,
        target: ClusterId,
        evidence: &PolishEvidence,
        prospective: &PolishEvidence,
    ) -> bool {
        if self.caps.folder == 0 {
            return true;
        }
        let before = self.physical_entry_counts_with_polish_evidence(parts, evidence);
        let mut assignment = parts.assignment().to_vec();
        let Some(slot) = assignment.get_mut(scc as usize) else {
            return false;
        };
        *slot = target;
        let moved = Partition::from_assignment(assignment, parts.cluster_count());
        let after = self.physical_entry_counts_with_polish_evidence(&moved, prospective);
        before.keys().chain(after.keys()).all(|path| {
            let prior = before
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            let next = after
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            next <= prior
        })
    }

    /// Re-checks a candidate with physical folder semantics while retaining
    /// the DTO walk for file and upper-level findings.
    pub(in crate::analyze) fn capacity_remainder_with_polish_evidence(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
        rendered: &ContainerNode,
        capacity: &CapacityConfig,
    ) -> CapacityRemainder {
        let assembled = self.assemble_with_polish_evidence(parts, evidence);
        Self::capacity_remainder_for_assembled(&assembled, rendered, capacity)
    }

    fn capacity_remainder_for_assembled(
        assembled: &CandidateTree,
        rendered: &ContainerNode,
        capacity: &CapacityConfig,
    ) -> CapacityRemainder {
        let folder_hard = physical_folder_findings(
            &physical_folder_entries_repository_relative(
                &assembled.tree,
                Some(&assembled.namespace_by_file),
            ),
            capacity.folder,
        )
        .into_iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .count();
        let roots = RealRoots::from_tree_keyed(&assembled.tree, &assembled.key_by_id);
        let other: Vec<Level> = walk_all_capacity(rendered, capacity, &roots)
            .into_iter()
            .filter(|(level, finding)| {
                *level != Level::Folder && finding.severity == Severity::Violation
            })
            .map(|(level, _)| level)
            .collect();
        let remaining = folder_hard.saturating_add(other.len());
        let file_level = other.iter().filter(|&&level| level == Level::File).count();
        CapacityRemainder {
            remaining: u32::try_from(remaining).unwrap_or(u32::MAX),
            file_level: u32::try_from(file_level).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use unlace_ir::{ContainerTree, ScopeLevel};

    use super::*;
    use crate::analyze::rendering::render_tree;
    use crate::analyze::test_support::container;

    /// A candidate whose two sibling domains both echo their package `cts` is
    /// dropped by the renderer, yet its overrun is a hard capacity finding the
    /// remainder must count, as pricing and the findings do.
    #[test]
    fn should_count_a_domain_echoing_its_package_in_the_capacity_remainder() {
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
            container(3, "cts (cts.zz)", ScopeLevel::Domain, Some(1)),
            container(4, "cts/core/a", ScopeLevel::Folder, Some(2)),
            container(5, "cts/core/a/f.ts", ScopeLevel::File, Some(4)),
            container(6, "cts/io/b", ScopeLevel::Folder, Some(2)),
            container(7, "cts/io/b/f.ts", ScopeLevel::File, Some(6)),
            container(8, "cts/zz", ScopeLevel::Folder, Some(3)),
            container(9, "cts/zz/f.ts", ScopeLevel::File, Some(8)),
        ]);
        let key_by_id: BTreeMap<u32, SmolStr> =
            [(2, SmolStr::new("cts")), (3, SmolStr::new("cts"))]
                .into_iter()
                .collect();
        let rendered = render_tree(&tree, &[], &|_| None, &key_by_id).ok();
        let assembled = CandidateTree {
            tree,
            placement: BTreeMap::new(),
            pass_start_file_by_candidate: BTreeMap::new(),
            zone_by_file: BTreeMap::new(),
            namespace_by_file: BTreeMap::new(),
            package_by_file: BTreeMap::new(),
            key_by_id,
        };
        let capacity = CapacityConfig {
            file: 1000,
            folder: 1000,
            domain: 1,
            package: 1000,
            package_group: 1000,
        };

        let remainder = rendered.map(|rendered| {
            PipelineSolver::capacity_remainder_for_assembled(&assembled, &rendered, &capacity)
        });

        assert_eq!(
            remainder,
            Some(CapacityRemainder {
                remaining: 1,
                file_level: 0
            })
        );
    }
}
