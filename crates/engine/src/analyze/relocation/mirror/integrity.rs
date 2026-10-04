//! The last guard on a settled layout: a source and its paired test end in
//! the same place or neither moves (ADR-26). It does not depend on how many
//! settle rounds ran, so a round cap can never leave a split pair behind, and
//! it settles what its own reverts disturb (a path another mover took, a
//! folder pushed past its cap) until nothing more needs sending home.

use std::collections::{BTreeMap, BTreeSet};

use strata_core::cluster::Partition;

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::narrate::physical_relocation_folders;

/// Source SCCs whose pair is split between `start` and `parts`. A follower is
/// placed by joining its source's cluster (the folder it lands in is derived
/// from the link's mirrored destination when the layout is projected), so the
/// pair is intact when the source stayed and the test stayed, or the source
/// moved and the test sits in the source's final cluster, wherever it began.
/// "Moved" is a change of physical folder (`moved`), not of cluster: a cluster
/// change that draws the same folder moves nothing. `pairs` holds
/// `(source_scc, test_scc)`.
fn split_sources(
    pairs: &[(u32, u32)],
    parts: &Partition,
    moved: impl Fn(u32) -> bool,
) -> BTreeSet<u32> {
    pairs
        .iter()
        .filter(|(source, test)| source != test)
        .filter(|(source, test)| {
            if moved(*source) {
                parts.cluster_of(*source) != parts.cluster_of(*test)
            } else {
                moved(*test)
            }
        })
        .map(|(source, _)| *source)
        .collect()
}

impl PipelineSolver<'_> {
    /// Sends every split pair home, then whatever that disturbs: a mover whose
    /// final path a reverted file now takes, and the movers into a folder the
    /// reverts pushed past its cap. Each step sends at least one moved file to
    /// its pass-start cluster, so it terminates; the reverted tests' outcomes
    /// are dropped from `evidence` so no report states a move that did not
    /// happen. Deterministic and language-neutral.
    pub(in crate::analyze::relocation) fn revert_split_pairs(
        &self,
        parts: &mut Partition,
        evidence: &mut PolishEvidence,
    ) {
        // the follower pass is off exactly when no link is ever derived, so
        // there is no pair to keep whole (same gate as `shadow_tests`).
        if !self.mirror_enabled || (self.mirror_rules.is_empty() && self.adapter_pairs.is_empty()) {
            return;
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (test_vertex, claims) in self.exact_mirror_links() {
            for link in claims.into_values() {
                if let (Some(source), Some(test)) = (
                    self.scc_of_vertex(link.source_vertex),
                    self.scc_of_vertex(test_vertex),
                ) {
                    pairs.push((source, test));
                }
            }
        }
        let mut reverted_any = false;
        for _ in 0..=self.files.len() {
            let assembled = self.assemble_with_polish_evidence(parts, evidence);
            let (current, proposed) =
                physical_relocation_folders(&self.snapshot.ir().containers, &assembled.tree);
            let mut moved_vertices: BTreeMap<usize, Vec<String>> = BTreeMap::new();
            for (vertex, file) in self.files.iter().enumerate() {
                let path = self.repository_path(file.name.as_str());
                if let Some(folder) = proposed.get(&path)
                    && current.get(&path) != Some(folder)
                {
                    moved_vertices.insert(vertex, folder.clone());
                }
            }
            let moved_sccs: BTreeSet<u32> = moved_vertices
                .keys()
                .filter_map(|vertex| self.scc_of_vertex(u32::try_from(*vertex).ok()?))
                .collect();
            let split = split_sources(&pairs, parts, |scc| moved_sccs.contains(&scc));
            let mut back: BTreeSet<u32> = pairs
                .iter()
                .filter(|(source, _)| split.contains(source))
                .flat_map(|&(source, test)| [source, test])
                .collect();
            if reverted_any {
                for (mover, _) in self.path_collisions(parts, evidence) {
                    back.extend(
                        u32::try_from(mover)
                            .ok()
                            .and_then(|v| self.scc_of_vertex(v)),
                    );
                }
                back.extend(self.movers_into_overfull_folders(parts, evidence, &moved_vertices));
            }
            let mut changed = false;
            for scc in &back {
                if let Some(home) = self.pass_start_partition.cluster_of(*scc)
                    && parts.cluster_of(*scc) != Some(home)
                {
                    changed |= parts.move_node(*scc, home);
                }
            }
            if !changed {
                break;
            }
            reverted_any = true;
            evidence.forget_tests(&back);
        }
    }

    /// SCCs of the moved files whose proposed folder holds more entries than
    /// its cap and more than it did at pass start.
    fn movers_into_overfull_folders(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
        moved_vertices: &BTreeMap<usize, Vec<String>>,
    ) -> BTreeSet<u32> {
        if self.caps.folder == 0 {
            return BTreeSet::new();
        }
        let after = self.physical_entry_counts_with_polish_evidence(parts, evidence);
        let before = self.physical_entry_counts(&self.pass_start_partition);
        let cap = self.caps.folder;
        let overfull: BTreeSet<&Vec<String>> = after
            .iter()
            .filter(|(folder, count)| {
                **count > cap && **count > before.get(*folder).copied().unwrap_or(0)
            })
            .map(|(folder, _)| folder)
            .collect();
        moved_vertices
            .iter()
            .filter(|(_, folder)| overfull.contains(folder))
            .filter_map(|(vertex, _)| self.scc_of_vertex(u32::try_from(*vertex).ok()?))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use strata_core::cluster::{ClusterId, Partition};

    use std::collections::BTreeSet;

    use super::split_sources as split_by;

    /// Cluster-change reading of "moved", the stand-in for a physical move.
    fn split_sources(pairs: &[(u32, u32)], start: &Partition, parts: &Partition) -> BTreeSet<u32> {
        split_by(pairs, parts, |scc| {
            start.cluster_of(scc) != parts.cluster_of(scc)
        })
    }
    use crate::analyze::relocation::mirror::{MirrorDisposition, MirrorOutcome, PolishEvidence};

    fn applied(scc: u32, destination: &str) -> MirrorOutcome {
        MirrorOutcome {
            mirror_scc: scc,
            source_path: "src/a.ts".to_owned(),
            path: format!("test/{scc}.spec.ts"),
            from: "test".to_owned(),
            intended_to: destination.to_owned(),
            disposition: MirrorDisposition::Applied,
        }
    }

    #[test]
    fn should_hide_a_reverted_tests_outcome_from_the_evidence_the_projection_reads() {
        let mut evidence = PolishEvidence {
            outcomes: vec![applied(1, "moved/a"), applied(2, "moved/b")],
            ..PolishEvidence::default()
        };
        assert_eq!(evidence.applied_destination(1), Some("moved/a"));

        evidence.forget_tests(&BTreeSet::from([1]));

        assert_eq!(evidence.applied_destination(1), None);
        assert_eq!(evidence.applied_destination(2), Some("moved/b"));
    }

    fn partition(clusters: &[u32]) -> Partition {
        Partition::from_assignment(clusters.iter().map(|c| ClusterId(*c)).collect(), 4)
    }

    #[test]
    fn should_revert_a_pair_when_only_the_source_moved() {
        let start = partition(&[0, 1]);
        let split = split_sources(&[(0, 1)], &start, &partition(&[2, 1]));
        assert_eq!(split.into_iter().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn should_revert_a_pair_when_only_the_test_moved() {
        let start = partition(&[0, 1]);
        let split = split_sources(&[(0, 1)], &start, &partition(&[0, 2]));
        assert_eq!(split.into_iter().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn should_revert_a_pair_when_both_moved_into_different_clusters() {
        let start = partition(&[0, 1]);
        let split = split_sources(&[(0, 1)], &start, &partition(&[2, 3]));
        assert_eq!(split.into_iter().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn should_leave_a_mirror_tree_test_that_followed_its_source_untouched() {
        // src/foo.ts in cluster 0 and test/foo.spec.ts in cluster 1 start
        // apart (a mirror tree); both end in cluster 2.
        let start = partition(&[0, 1, 1]);
        let moved = partition(&[2, 2, 1]);
        assert_eq!(
            split_sources(&[(0, 1)], &start, &moved),
            std::collections::BTreeSet::new()
        );
    }

    #[test]
    fn should_leave_a_test_already_in_the_destination_cluster_untouched() {
        let start = partition(&[0, 2]);
        let moved = partition(&[2, 2]);
        assert_eq!(
            split_sources(&[(0, 1)], &start, &moved),
            std::collections::BTreeSet::new()
        );
    }

    #[test]
    fn should_leave_an_intact_pair_untouched() {
        let start = partition(&[0, 1, 0, 1]);
        let moved_together = partition(&[2, 2, 0, 1]);
        assert_eq!(
            split_sources(&[(0, 1), (2, 3)], &start, &moved_together),
            std::collections::BTreeSet::new()
        );
        assert_eq!(
            split_sources(&[(0, 1)], &start, &start),
            std::collections::BTreeSet::new()
        );
    }

    #[test]
    fn should_not_revert_a_pair_whose_source_changed_cluster_but_not_folder() {
        let parts = partition(&[2, 1]);
        assert_eq!(
            split_by(&[(0, 1)], &parts, |_| false),
            BTreeSet::new(),
            "a cluster change that draws the same folder moves nothing"
        );
    }
}
