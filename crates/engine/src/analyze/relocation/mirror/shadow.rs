//! Test-follower placement: shadows each paired test onto its source and drops
//! a source's whole move when one of its tests cannot follow (ADR-26).

use std::collections::{BTreeMap, BTreeSet};

use strata_core::cluster::Partition;

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::collision::CollisionFold;
use crate::analyze::relocation::mirror::{
    ExactMirrorLink, MirrorBlock, MirrorDisposition, MirrorOutcome, PolishEvidence,
    fill_mirror_template, parent_path,
};
use crate::narrate::physical_relocation_folders;
use crate::result::{MirrorMove, Move};

type Layout = std::collections::BTreeMap<String, Vec<String>>;

impl PipelineSolver<'_> {
    pub(in crate::analyze::relocation) fn attach_test_mirrors(
        moves: &mut Vec<Move>,
        evidence: &PolishEvidence,
    ) {
        let mut follower_paths = BTreeSet::new();
        for outcome in &evidence.outcomes {
            if !matches!(outcome.disposition, MirrorDisposition::Applied) {
                continue;
            }
            let Some(entry) = moves.iter_mut().find(|entry| {
                entry
                    .files
                    .iter()
                    .any(|file| file.path == outcome.source_path)
            }) else {
                continue;
            };
            follower_paths.insert(outcome.path.clone());
            entry.mirrors.push(MirrorMove {
                source_path: outcome.source_path.clone(),
                path: outcome.path.clone(),
                from: outcome.from.clone(),
                to: outcome.intended_to.clone(),
            });
        }
        for entry in moves.iter_mut() {
            entry.mirrors.sort_by(|left, right| {
                (&left.source_path, &left.path).cmp(&(&right.source_path, &right.path))
            });
            entry.mirrors.dedup();
        }
        moves.retain(|entry| {
            entry
                .files
                .iter()
                .all(|file| !follower_paths.contains(&file.path))
        });
    }

    /// Assigns each paired test to its source's cluster after polish, or drops
    /// the source's whole move.
    ///
    /// A paired test never moves alone (ADR-26): when any test of a source
    /// cannot follow, the source and all its tests return to their pass-start
    /// clusters and the refused follower is reported as a blocked outcome; the
    /// caller settles the layout again while a round blocks anything. A source
    /// that did not move keeps its tests at home too.
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
        if !self.mirror_enabled || (self.mirror_rules.is_empty() && self.adapter_pairs.is_empty()) {
            return evidence;
        }
        let assembled = self.assemble_with_polish_evidence(parts, &evidence);
        let (current, proposed) =
            physical_relocation_folders(&self.snapshot.ir().containers, &assembled.tree);
        let mut by_source: BTreeMap<u32, Vec<(u32, ExactMirrorLink)>> = BTreeMap::new();
        for (mirror_vertex, claims) in self.exact_mirror_links() {
            for link in claims.into_values() {
                by_source
                    .entry(link.source_vertex)
                    .or_default()
                    .push((mirror_vertex, link));
            }
        }
        for (source_vertex, followers) in by_source {
            let Some(source_file) = self.files.get(source_vertex as usize) else {
                continue;
            };
            let source_path = self.repository_path(source_file.name.as_str());
            let Some(source_scc) = self.scc_of_vertex(source_vertex) else {
                continue;
            };
            if current.get(&source_path) == proposed.get(&source_path) {
                self.return_followers_home(parts, &followers);
                continue;
            }
            let before = (parts.clone(), evidence.outcomes.len());
            let target = parts.cluster_of(source_scc);
            let mut blocked = false;
            for (mirror_vertex, link) in &followers {
                let Some(target) = target else { break };
                let outcome = self.attempt_follower(
                    parts,
                    &evidence,
                    &proposed,
                    (&source_path, *mirror_vertex, link),
                    target,
                );
                blocked = matches!(outcome.disposition, MirrorDisposition::Blocked(_));
                evidence.outcomes.push(outcome);
                if blocked {
                    break;
                }
            }
            if blocked {
                let (restored, applied) = before;
                *parts = restored;
                let blocker = evidence.outcomes.split_off(applied).pop();
                evidence.outcomes.extend(blocker);
                self.return_followers_home(parts, &followers);
                if let Some(home) = self.pass_start_partition.cluster_of(source_scc) {
                    let _moved = parts.move_node(source_scc, home);
                }
            }
        }
        evidence.outcomes.sort_by(|left, right| {
            (&left.source_path, &left.path).cmp(&(&right.source_path, &right.path))
        });
        evidence.outcomes.dedup();
        evidence
    }

    pub(super) fn scc_of_vertex(&self, vertex: u32) -> Option<u32> {
        self.condensation
            .membership
            .get(vertex as usize)
            .map(|scc| scc.0)
    }

    /// Puts each follower's SCC back in its pass-start cluster.
    fn return_followers_home(&self, parts: &mut Partition, followers: &[(u32, ExactMirrorLink)]) {
        for (mirror_vertex, _) in followers {
            let Some(scc) = self.scc_of_vertex(*mirror_vertex) else {
                continue;
            };
            if let Some(home) = self.pass_start_partition.cluster_of(scc)
                && parts.cluster_of(scc) != Some(home)
            {
                let _moved = parts.move_node(scc, home);
            }
        }
    }

    /// Tries one follower: applies it to `parts` when every hard constraint
    /// passes, otherwise reports the first one that refused it.
    fn attempt_follower(
        &self,
        parts: &mut Partition,
        evidence: &PolishEvidence,
        proposed: &Layout,
        (source_path, mirror_vertex, link): (&str, u32, &ExactMirrorLink),
        target: strata_core::cluster::ClusterId,
    ) -> MirrorOutcome {
        let mirror_scc = self.scc_of_vertex(mirror_vertex).unwrap_or(u32::MAX);
        let mirror_path = self
            .files
            .get(mirror_vertex as usize)
            .map_or_else(String::new, |file| self.repository_path(file.name.as_str()));
        let intended = proposed
            .get(source_path)
            .and_then(|folder| self.project_mirror_path(link, folder));
        let fallback = self.repository_path(&fill_mirror_template(
            &link.test_template,
            &link.captures.dir,
            &link.captures.stem,
        ));
        let intended_path = intended.as_deref().unwrap_or(&fallback);
        let mut outcome = MirrorOutcome {
            mirror_scc,
            source_path: source_path.to_owned(),
            path: mirror_path.clone(),
            from: parent_path(&mirror_path),
            intended_to: parent_path(intended_path),
            disposition: MirrorDisposition::Applied,
        };
        let mut prospective = evidence.clone();
        prospective.outcomes.push(outcome.clone());
        let reason = if !self
            .relocation_identity
            .permits_package_join(mirror_scc, target)
        {
            Some(MirrorBlock::PackageBoundary)
        } else if intended.is_none() || !self.test_only_scc(mirror_scc) {
            Some(MirrorBlock::NamespaceBoundary)
        } else if !self.permits_mirror_physical_capacity(
            parts,
            mirror_scc,
            target,
            evidence,
            &prospective,
        ) {
            Some(MirrorBlock::Capacity)
        } else {
            self.path_or_identity_block(parts, mirror_vertex, intended_path, target, &prospective)
        };
        if let Some(reason) = reason {
            outcome.disposition = MirrorDisposition::Blocked(reason);
        } else if parts.cluster_of(mirror_scc) != Some(target) {
            let _moved = parts.move_node(mirror_scc, target);
        }
        outcome
    }

    /// The path-collision check: the follower's destination path must be free
    /// and its render identity must not clash in the target cluster.
    fn path_or_identity_block(
        &self,
        parts: &Partition,
        mirror_vertex: u32,
        intended_path: &str,
        target: strata_core::cluster::ClusterId,
        prospective: &PolishEvidence,
    ) -> Option<MirrorBlock> {
        let mirror_scc = self.scc_of_vertex(mirror_vertex)?;
        let mut assignment = parts.assignment().to_vec();
        let slot = assignment.get_mut(mirror_scc as usize)?;
        *slot = target;
        let moved = Partition::from_assignment(assignment, parts.cluster_count());
        let clash = self.mirror_path_occupied(&moved, mirror_vertex, intended_path, prospective)
            || !self
                .relocation_identity
                .permits_shadow_join(parts, mirror_scc, target);
        clash.then_some(MirrorBlock::PathCollision)
    }
}
