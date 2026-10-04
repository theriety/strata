//! Exact mirror links between production sources and present test files, and
//! the paths a mirrored test would land on.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::Partition;

use crate::analyze::relocation::mirror::pairs::derive_link;
use crate::analyze::relocation::mirror::{
    ExactMirrorLink, PolishEvidence, ProjectedFolder, fill_mirror_template, match_source_template,
};
use crate::analyze::relocation::{FileInfo, PipelineSolver};
use crate::config::TestMirrorRule;

/// Links each paired test to its one source: adapter pairs first, then user
/// rules, over the given files. A test claimed by more than one source is left
/// unpaired. Shared by the solver's construction (to pin paired tests) and the
/// follower pass.
pub(in crate::analyze) fn exact_links(
    files: &[FileInfo],
    test_zone: &[bool],
    adapter_pairs: &[(SmolStr, SmolStr)],
    mirror_rules: &[TestMirrorRule],
) -> BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> {
    let mut links: BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> = BTreeMap::new();
    let by_path: BTreeMap<String, u32> = files
        .iter()
        .enumerate()
        .filter(|(vertex, _)| test_zone.get(*vertex).copied().unwrap_or(false))
        .map(|(vertex, file)| {
            (
                file.name.to_string(),
                u32::try_from(vertex).unwrap_or(u32::MAX),
            )
        })
        .collect();
    let by_name: BTreeMap<&str, u32> = files
        .iter()
        .enumerate()
        .map(|(vertex, file)| {
            (
                file.name.as_str(),
                u32::try_from(vertex).unwrap_or(u32::MAX),
            )
        })
        .collect();
    // adapter-stated pairs first: the production side must be outside the test
    // zone and the test side inside it, exactly as for user rules. A stated pair
    // claims its test even when no link derives from the paths, so a user rule
    // never takes that test over.
    let mut claimed: BTreeSet<u32> = BTreeSet::new();
    for (test, source) in adapter_pairs {
        let (Some(&mirror_vertex), Some(&source_vertex)) =
            (by_path.get(test.as_str()), by_name.get(source.as_str()))
        else {
            continue;
        };
        if test_zone
            .get(source_vertex as usize)
            .copied()
            .unwrap_or(true)
        {
            continue;
        }
        claimed.insert(mirror_vertex);
        if let Some(link) = derive_link(source_vertex, source, test) {
            links
                .entry(mirror_vertex)
                .or_default()
                .entry(source_vertex)
                .or_insert(link);
        }
    }
    // a test has at most one source: an adapter pair wins outright, and
    // user rules that disagree about a test's source leave it unpaired.
    let mut from_rules: BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> = BTreeMap::new();
    for (source_vertex, source) in files.iter().enumerate() {
        if test_zone.get(source_vertex).copied().unwrap_or(true) {
            continue;
        }
        let relative_source = source.name.as_str();
        for rule in mirror_rules {
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
                if links.contains_key(&mirror_vertex) || claimed.contains(&mirror_vertex) {
                    continue;
                }
                from_rules
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
    links.extend(
        from_rules
            .into_iter()
            .filter(|(_, claims)| claims.len() == 1),
    );
    links
}

impl PipelineSolver<'_> {
    pub(super) fn exact_mirror_links(&self) -> BTreeMap<u32, BTreeMap<u32, ExactMirrorLink>> {
        exact_links(
            &self.files,
            &self.test_zone,
            &self.adapter_pairs,
            &self.mirror_rules,
        )
    }

    pub(super) fn project_mirror_path(
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

    pub(super) fn test_only_scc(&self, scc: u32) -> bool {
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

    pub(super) fn mirror_path_occupied(
        &self,
        parts: &Partition,
        mirror_vertex: u32,
        intended_path: &str,
        evidence: &PolishEvidence,
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
}
