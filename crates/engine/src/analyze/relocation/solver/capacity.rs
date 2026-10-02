//! Physical-capacity vetoes and the capacity remainder for polish evidence.

use std::collections::BTreeMap;

use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::findings::{
    physical_folder_entries_repository_relative, physical_folder_findings, walk_all_capacity,
};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, PipelineSolver};
use crate::config::CapacityConfig;
use crate::result::{CapacityRemainder, ContainerNode, Level, Severity};

impl PipelineSolver<'_> {
    fn physical_entry_counts(&self, parts: &Partition) -> BTreeMap<Vec<String>, u32> {
        let assembled = self.assemble(parts);
        physical_folder_entries_repository_relative(
            &assembled.tree,
            Some(&assembled.namespace_by_file),
        )
    }

    fn physical_entry_counts_with_polish_evidence(
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
        let other: Vec<Level> = walk_all_capacity(rendered, capacity)
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
