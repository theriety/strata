//! Test-follower placement: shadows each test file onto its subject twin and
//! attaches the outcomes to the reported moves.

use std::collections::BTreeSet;

use strata_core::cluster::Partition;

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::collision::CollisionFold;
use crate::analyze::relocation::mirror::{
    MirrorDisposition, MirrorOutcome, PolishEvidence, fill_mirror_template, parent_path,
};
use crate::narrate::physical_relocation_folders;
use crate::result::{BlockedMirror, BlockedMirrorReason, MirrorMove, Move};

impl PipelineSolver<'_> {
    pub(in crate::analyze::relocation) fn attach_test_mirrors(
        moves: &mut Vec<Move>,
        evidence: &PolishEvidence,
    ) {
        let mut follower_paths = BTreeSet::new();
        for outcome in &evidence.outcomes {
            let Some(entry) = moves.iter_mut().find(|entry| {
                entry
                    .files
                    .iter()
                    .any(|file| file.path == outcome.source_path)
            }) else {
                continue;
            };
            match outcome.disposition {
                MirrorDisposition::Applied => {
                    follower_paths.insert(outcome.path.clone());
                    entry.mirrors.push(MirrorMove {
                        source_path: outcome.source_path.clone(),
                        path: outcome.path.clone(),
                        from: outcome.from.clone(),
                        to: outcome.intended_to.clone(),
                    });
                }
                MirrorDisposition::Blocked(reason) => {
                    entry.blocked_mirrors.push(BlockedMirror {
                        source_path: outcome.source_path.clone(),
                        path: outcome.path.clone(),
                        from: outcome.from.clone(),
                        intended_to: outcome.intended_to.clone(),
                        reason,
                    });
                }
            }
        }
        for entry in moves.iter_mut() {
            entry.mirrors.sort_by(|left, right| {
                (&left.source_path, &left.path).cmp(&(&right.source_path, &right.path))
            });
            entry.mirrors.dedup();
            entry.blocked_mirrors.sort_by(|left, right| {
                (&left.source_path, &left.path).cmp(&(&right.source_path, &right.path))
            });
            entry.blocked_mirrors.dedup();
        }
        moves.retain(|entry| {
            entry
                .files
                .iter()
                .all(|file| !follower_paths.contains(&file.path))
        });
    }

    #[allow(clippy::too_many_lines)]
    /// Assigns test-zone files to their unique subject twin after polish.
    ///
    /// `folds` are the restart's withdrawn colliding moves (ADR-21); they ride
    /// the returned evidence and shape the layout followers are placed against.
    pub(in crate::analyze::relocation) fn shadow_tests(
        &self,
        parts: &mut Partition,
        folds: Vec<CollisionFold>,
    ) -> PolishEvidence {
        let mut evidence = PolishEvidence {
            folds,
            ..PolishEvidence::default()
        };
        if !self.mirror_enabled || self.mirror_rules.is_empty() {
            return evidence;
        }
        let assembled = self.assemble_with_polish_evidence(parts, &evidence);
        let (current_physical, proposed_physical) =
            physical_relocation_folders(&self.snapshot.ir().containers, &assembled.tree);
        for (mirror_vertex, claims) in self.exact_mirror_links() {
            for link in claims.values() {
                let Some(source_scc) = self
                    .condensation
                    .membership
                    .get(link.source_vertex as usize)
                    .map(|scc| scc.0)
                else {
                    continue;
                };
                let Some(target) = parts.cluster_of(source_scc) else {
                    continue;
                };
                let Some(source_file) = self.files.get(link.source_vertex as usize) else {
                    continue;
                };
                let source_path = self.repository_path(source_file.name.as_str());
                if current_physical.get(&source_path) == proposed_physical.get(&source_path) {
                    continue;
                }
                let Some(mirror_scc) = self
                    .condensation
                    .membership
                    .get(mirror_vertex as usize)
                    .map(|scc| scc.0)
                else {
                    continue;
                };
                let Some(mirror_file) = self.files.get(mirror_vertex as usize) else {
                    continue;
                };
                let mirror_path = self.repository_path(mirror_file.name.as_str());
                let intended = proposed_physical
                    .get(&source_path)
                    .and_then(|folder| self.project_mirror_path(link, folder));
                let fallback = self.repository_path(&fill_mirror_template(
                    &link.test_template,
                    &link.captures.dir,
                    &link.captures.stem,
                ));
                let intended_path = intended.as_deref().unwrap_or(&fallback);
                let outcome = MirrorOutcome {
                    mirror_scc,
                    source_path,
                    path: mirror_path.clone(),
                    from: parent_path(&mirror_path),
                    intended_to: parent_path(intended_path),
                    disposition: MirrorDisposition::Applied,
                };
                let mut prospective = evidence.clone();
                prospective.outcomes.push(outcome.clone());

                let reason = if claims.len() > 1 {
                    Some(BlockedMirrorReason::AmbiguousMapping)
                } else if !self
                    .relocation_identity
                    .permits_package_join(mirror_scc, target)
                {
                    Some(BlockedMirrorReason::PackageBoundary)
                } else if intended.is_none() || !self.test_only_scc(mirror_scc) {
                    Some(BlockedMirrorReason::NamespaceBoundary)
                } else if !self.permits_mirror_physical_capacity(
                    parts,
                    mirror_scc,
                    target,
                    &evidence,
                    &prospective,
                ) {
                    Some(BlockedMirrorReason::Capacity)
                } else {
                    let mut assignment = parts.assignment().to_vec();
                    let moved = if let Some(slot) = assignment.get_mut(mirror_scc as usize) {
                        *slot = target;
                        Some(Partition::from_assignment(
                            assignment,
                            parts.cluster_count(),
                        ))
                    } else {
                        None
                    };
                    if moved.as_ref().is_none_or(|moved| {
                        self.mirror_path_occupied(moved, mirror_vertex, intended_path, &prospective)
                    }) || !self
                        .relocation_identity
                        .permits_shadow_join(parts, mirror_scc, target)
                    {
                        Some(BlockedMirrorReason::PathCollision)
                    } else {
                        None
                    }
                };
                if let Some(reason) = reason {
                    evidence.outcomes.push(MirrorOutcome {
                        disposition: MirrorDisposition::Blocked(reason),
                        ..outcome
                    });
                    continue;
                }
                if parts.cluster_of(mirror_scc) != Some(target) {
                    let _moved = parts.move_node(mirror_scc, target);
                }
                evidence.outcomes.push(outcome);
            }
        }
        evidence.outcomes.sort_by(|left, right| {
            (&left.source_path, &left.path).cmp(&(&right.source_path, &right.path))
        });
        evidence.outcomes.dedup();
        evidence
    }
}
