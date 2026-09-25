use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::relocation::PipelineSolver;
use crate::config::{
    MirrorCaptures, MirrorTemplate, ProfileConfig, TestMirrorRule, builtin_test_mirror_rules,
};
use crate::narrate::{normalize_physical_path, physical_relocation_folders, project_physical_path};
use crate::result::{BlockedMirror, BlockedMirrorReason, MirrorMove, Move};

#[cfg(test)]
mod tests;

/// Mirror follower decisions produced by the same deterministic solver restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analyze) struct MirrorEvidence {
    pub(in crate::analyze) outcomes: Vec<MirrorOutcome>,
}

impl MirrorEvidence {
    pub(super) fn applied_sccs(&self) -> BTreeSet<u32> {
        self.outcomes
            .iter()
            .filter_map(|outcome| {
                matches!(outcome.disposition, MirrorDisposition::Applied)
                    .then_some(outcome.mirror_scc)
            })
            .collect()
    }

    fn applied_destination(&self, scc: u32) -> Option<&str> {
        self.outcomes.iter().find_map(|outcome| {
            (outcome.mirror_scc == scc && matches!(outcome.disposition, MirrorDisposition::Applied))
                .then_some(outcome.intended_to.as_str())
        })
    }
}

/// One attempted exact test follower and the hard-constraint result it earned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::analyze) struct MirrorOutcome {
    mirror_scc: u32,
    source_path: String,
    path: String,
    from: String,
    intended_to: String,
    disposition: MirrorDisposition,
}

/// Whether an exact test follower changed placement or met a hard constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::analyze) enum MirrorDisposition {
    Applied,
    Blocked(BlockedMirrorReason),
}

/// Immutable template match between one production source and one present test.
#[derive(Debug, Clone)]
pub(in crate::analyze) struct ExactMirrorLink {
    source_vertex: u32,
    source_root: String,
    test_template: String,
    captures: MirrorCaptures,
}

/// A candidate folder together with the coordinate system its path uses.
#[derive(Debug, Clone)]
pub(in crate::analyze) enum ProjectedFolder {
    RepositoryRelative(SmolStr),
    Rooted(SmolStr),
}

impl ProjectedFolder {
    pub(super) fn repository_relative(&self, root: &str) -> SmolStr {
        match self {
            Self::RepositoryRelative(path) => path.clone(),
            Self::Rooted(path) => {
                let segments: Vec<&str> = path
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .collect();
                match segments.split_first() {
                    Some((head, tail)) if *head == root => SmolStr::new(tail.join("/")),
                    _ => path.clone(),
                }
            }
        }
    }
}

impl PipelineSolver<'_> {
    pub(super) fn attach_test_mirrors(moves: &mut Vec<Move>, evidence: &MirrorEvidence) {
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
    pub(super) fn shadow_tests(&self, parts: &mut Partition) -> MirrorEvidence {
        if !self.mirror_enabled || self.mirror_rules.is_empty() {
            return MirrorEvidence::default();
        }
        let assembled = self.assemble(parts);
        let (current_physical, proposed_physical) =
            physical_relocation_folders(&self.snapshot.ir().containers, &assembled.tree);
        let mut evidence = MirrorEvidence::default();
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

    fn exact_mirror_links(&self) -> BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> {
        let mut links: BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> = BTreeMap::new();
        let by_path: BTreeMap<String, u32> = self
            .files
            .iter()
            .enumerate()
            .filter(|(vertex, _)| self.test_zone.get(*vertex).copied().unwrap_or(false))
            .map(|(vertex, file)| {
                (
                    file.name.to_string(),
                    u32::try_from(vertex).unwrap_or(u32::MAX),
                )
            })
            .collect();
        for (source_vertex, source) in self.files.iter().enumerate() {
            if self.test_zone.get(source_vertex).copied().unwrap_or(true) {
                continue;
            }
            let relative_source = source.name.as_str();
            for rule in &self.mirror_rules {
                let Some(captures) = match_source_template(&rule.source, relative_source) else {
                    continue;
                };
                let source_root = rule
                    .source
                    .split_once("{dir}")
                    .map_or("", |(prefix, _)| prefix)
                    .trim_matches('/')
                    .to_owned();
                for template in &rule.tests {
                    let expected = fill_mirror_template(template, &captures.dir, &captures.stem);
                    let Some(&mirror_vertex) = by_path.get(&expected) else {
                        continue;
                    };
                    links
                        .entry(mirror_vertex)
                        .or_default()
                        .entry(u32::try_from(source_vertex).unwrap_or(u32::MAX))
                        .or_insert_with(|| ExactMirrorLink {
                            source_vertex: u32::try_from(source_vertex).unwrap_or(u32::MAX),
                            source_root: source_root.clone(),
                            test_template: template.clone(),
                            captures: captures.clone(),
                        });
                }
            }
        }
        links
    }

    fn project_mirror_path(
        &self,
        link: &ExactMirrorLink,
        rooted_folder: &[String],
    ) -> Option<String> {
        let source_root: Vec<String> = link
            .source_root
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let relative_segments = rooted_folder
            .strip_prefix(&[self.root_name.to_string()])
            .unwrap_or(rooted_folder);
        let source_namespace: Vec<String> = self
            .files
            .get(link.source_vertex as usize)?
            .namespace
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let directory = if relative_segments.is_empty() {
            String::new()
        } else if relative_segments.starts_with(&source_root) {
            relative_segments.get(source_root.len()..)?.join("/")
        } else if source_root.is_empty() {
            relative_segments.join("/")
        } else if source_namespace.ends_with(&source_root) {
            // Laminar folder keys deliberately omit transparent source roots.
            // Reapply the rule's immutable root; the identity guard separately
            // prevents the production move from crossing its render namespace.
            relative_segments.join("/")
        } else {
            return None;
        };
        Some(self.repository_path(&fill_mirror_template(
            &link.test_template,
            &directory,
            &link.captures.stem,
        )))
    }

    fn test_only_scc(&self, scc: u32) -> bool {
        self.condensation
            .members
            .get(scc as usize)
            .is_some_and(|members| {
                members.iter().all(|member| {
                    self.test_zone
                        .get(member.0 as usize)
                        .copied()
                        .unwrap_or(false)
                })
            })
    }

    fn mirror_path_occupied(
        &self,
        parts: &Partition,
        mirror_vertex: u32,
        intended_path: &str,
        evidence: &MirrorEvidence,
    ) -> bool {
        self.files.iter().enumerate().any(|(vertex, file)| {
            if vertex == mirror_vertex as usize {
                return false;
            }
            let identity = self.repository_path(file.name.as_str());
            let Some(folder) = self.projected_folder_for_vertex(
                parts,
                u32::try_from(vertex).unwrap_or(u32::MAX),
                evidence,
            ) else {
                return false;
            };
            let leaf = identity.rsplit('/').next().unwrap_or(identity.as_str());
            let projected = match folder {
                ProjectedFolder::RepositoryRelative(folder) => {
                    self.repository_path(&format!("{folder}/{leaf}"))
                }
                ProjectedFolder::Rooted(folder) => self.candidate_path(&format!("{folder}/{leaf}")),
            };
            projected == intended_path
        })
    }

    pub(super) fn projected_folder_for_vertex(
        &self,
        parts: &Partition,
        vertex: u32,
        evidence: &MirrorEvidence,
    ) -> Option<ProjectedFolder> {
        let scc = self.condensation.membership.get(vertex as usize)?.0;
        if let Some(intended) = evidence.applied_destination(scc) {
            return Some(ProjectedFolder::Rooted(SmolStr::new(intended)));
        }
        let cluster = parts.cluster_of(scc)?;
        if let Some(home) = self.retained_cycle_home(parts, vertex, evidence) {
            return Some(ProjectedFolder::RepositoryRelative(home.clone()));
        }
        self.real_folder_names
            .get(cluster.0 as usize)
            .cloned()
            .map(ProjectedFolder::RepositoryRelative)
    }

    /// Original physical home only while the SCC has not moved or mirrored.
    pub(super) fn retained_cycle_home(
        &self,
        parts: &Partition,
        vertex: u32,
        evidence: &MirrorEvidence,
    ) -> Option<&SmolStr> {
        let scc = self.condensation.membership.get(vertex as usize)?.0;
        let cluster = parts.cluster_of(scc)?;
        if evidence.applied_destination(scc).is_some()
            || self.pass_start_partition.cluster_of(scc) != Some(cluster)
        {
            return None;
        }
        self.cycle_home_by_vertex.get(&vertex)
    }

    fn repository_path(&self, path: &str) -> String {
        let relative: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        project_physical_path(self.root_name.as_str(), &relative).join("/")
    }

    fn candidate_path(&self, path: &str) -> String {
        let path: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        normalize_physical_path(self.root_name.as_str(), &path).join("/")
    }

    /// Whether relocating `scc` from `source` into `target` would leave part of
    /// its priced neighborhood behind in some third folder — the bridge-absorption
    /// shape the polish veto bars (see the FIX05 comment at the call site). An
    /// SCC every one of whose priced edges terminates inside {source, target} is
    /// consolidating; one that also reaches elsewhere is orchestrating.
    pub(in crate::analyze) fn absorbs_a_foreign_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement,
                    // so it cannot anchor a bridge either.
                    continue;
                }
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster != source && cluster != target {
                    return true;
                }
            }
        }
        false
    }

    /// Whether relocating `scc` from `source` into `target` would strand priced
    /// pull in `source` at least equal to what awaits in `target` — the
    /// tearing-side veto (see the FIX05 companion comment at the call site).
    /// Only strictly stronger destinations justify leaving.
    pub(in crate::analyze) fn strands_a_comparable_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        let mut stranded = 0.0_f64;
        let mut awaiting = 0.0_f64;
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let weight = f64::from(weights.get(slot).copied().unwrap_or(0.0));
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                match parts.cluster_of(neighbour) {
                    Some(cluster) if cluster == source => stranded += weight,
                    Some(cluster) if cluster == target => awaiting += weight,
                    _ => {}
                }
            }
        }
        awaiting <= stranded
    }

    /// Whether `target` is the synthetic `workspace` bucket and `scc` would have
    /// to abandon priced company in its own folder to get there — the
    /// bucket-flight veto (see the FIX05 third-veto comment at the call site).
    pub(in crate::analyze) fn flees_into_the_synthetic_bucket(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        if !self
            .real_folder_synthetic
            .get(target.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                if parts.cluster_of(neighbour) == Some(source) {
                    return true;
                }
            }
        }
        false
    }
}

pub(in crate::analyze) fn match_source_template(
    template: &str,
    path: &str,
) -> Option<MirrorCaptures> {
    MirrorTemplate::parse(template)?.captures(path)
}

pub(in crate::analyze) fn fill_mirror_template(template: &str, dir: &str, stem: &str) -> String {
    MirrorTemplate::parse(template)
        .map(|parsed| parsed.fill(dir, stem))
        .unwrap_or_default()
}

pub(in crate::analyze) fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_owned())
}

pub(in crate::analyze) fn mirror_rules(profile: &ProfileConfig) -> Vec<TestMirrorRule> {
    let mut rules = profile.relocation.test_mirroring.rules.clone();
    if profile.relocation.test_mirroring.builtins {
        rules.extend(builtin_test_mirror_rules());
    }
    rules
}
