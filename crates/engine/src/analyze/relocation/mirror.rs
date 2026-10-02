//! Mirrors relocation outcomes back onto the layout views.

use std::collections::BTreeSet;

use smol_str::SmolStr;

use crate::analyze::relocation::collision::CollisionFold;
use crate::config::{
    MirrorCaptures, MirrorTemplate, ProfileConfig, TestMirrorRule, builtin_test_mirror_rules,
};
use crate::result::BlockedMirrorReason;

mod links;
mod projection;
mod shadow;
#[cfg(test)]
mod tests;
mod vetoes;

/// Placement decisions produced by the same deterministic solver restart that
/// must stay coupled to its partition: mirror follower outcomes, and the file
/// moves withdrawn for a path collision whose declarations the symbol pass is
/// offered instead (ADR-21).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analyze) struct PolishEvidence {
    pub(in crate::analyze) outcomes: Vec<MirrorOutcome>,
    pub(in crate::analyze) folds: Vec<CollisionFold>,
}

impl PolishEvidence {
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
