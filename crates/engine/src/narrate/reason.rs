//! Move-reason computation: followed subject, cap relief, dependency pull,
//! naming cohesion, or the clustering fallback.

use std::collections::BTreeSet;

use super::FileFacts;
use super::placements::FilePlacements;
use super::tokens::{jaccard, tokenize};
use crate::result::{CapacityUnit, LeftBehind, MoveReason};

/// Everything a group's reason computation reads.
pub(super) struct GroupContext<'a> {
    /// The moved file paths in the group.
    pub(super) files: &'a [&'a String],
    /// The distinct folded source folders.
    pub(super) origins: &'a BTreeSet<&'a Vec<String>>,
    /// The folded destination folder.
    pub(super) destination: &'a Vec<String>,
    /// The followed subject path, when the group follows one.
    pub(super) follows: Option<&'a str>,
    /// File placements in the current tree.
    pub(super) before: &'a FilePlacements,
    /// File placements in the candidate tree.
    pub(super) after: &'a FilePlacements,
    /// The per-file facts.
    pub(super) facts: &'a FileFacts,
}

/// Threshold on the mean pairwise basename-token Jaccard above which a group's
/// move narrates as naming cohesion with its destination.
const NAMING_COHESION_THRESHOLD: f64 = 0.5;

/// Computes a group's dominant reason, first match wins: followed subject >
/// cap relief > dependency pull > naming cohesion > clustering fallback.
pub(super) fn group_reason(ctx: &GroupContext<'_>) -> MoveReason {
    if let Some(subject) = ctx.follows {
        return MoveReason::Follows {
            subject: subject.to_owned(),
        };
    }

    // cap relief: among the over-cap source folders, attribute to the one that
    // contributes the most files to *this* group — the dominant contributor —
    // not the lexicographically-first origin, which may barely feature in the
    // move. Ties keep the first (lexicographic) origin, since `origins` iterates
    // in sorted order.
    let mut dominant: Option<(&Vec<String>, usize, u32)> = None;
    for origin in ctx.origins {
        let Some(members) = ctx.before.members_of.get(*origin) else {
            continue;
        };
        let size = ctx
            .before
            .entry_count
            .get(*origin)
            .copied()
            .unwrap_or_else(|| u32::try_from(members.len()).unwrap_or(u32::MAX));
        if size <= ctx.facts.folder_cap {
            continue;
        }
        let contributed = ctx
            .files
            .iter()
            .filter(|&&file| members.contains(file))
            .count();
        if dominant.is_none_or(|(_, top, _)| contributed > top) {
            dominant = Some((*origin, contributed, size));
        }
    }
    if let Some((origin, _, size)) = dominant {
        return MoveReason::RelievesOverCap {
            container: origin.join("/"),
            count: size,
            cap: ctx.facts.folder_cap,
            unit: CapacityUnit::Entries,
            config_key: "capacity.folder".to_owned(),
        };
    }

    // residents: destination files that are not part of this group.
    let residents: Vec<&String> =
        ctx.after
            .members_of
            .get(ctx.destination)
            .map_or_else(Vec::new, |members| {
                members
                    .iter()
                    .filter(|member| ctx.files.iter().all(|file| *file != *member))
                    .collect()
            });

    // dependency pull: the resident with the strongest two-way tie to the group,
    // narrated only when it beats the strongest tie the move leaves behind.
    let pull = strongest_tie(ctx, residents.iter().copied());
    let left_behind = strongest_tie(ctx, source_residents(ctx));
    if let Some((partner, total)) = pull
        && left_behind.as_ref().is_none_or(|(_, kept)| total > *kept)
    {
        return MoveReason::PulledBy {
            partner: partner.clone(),
            weight: total,
            left_behind: left_behind.map(|(left, weight)| LeftBehind {
                partner: left.clone(),
                weight,
            }),
        };
    }

    // naming cohesion: mean pairwise Jaccard between moved and resident stems.
    if !residents.is_empty() {
        let mut sum = 0.0;
        let mut pairs = 0_u32;
        for file in ctx.files {
            let file_tokens = tokenize(file);
            for resident in &residents {
                sum += jaccard(&file_tokens, &tokenize(resident));
                pairs = pairs.saturating_add(1);
            }
        }
        let mean = if pairs == 0 {
            0.0
        } else {
            sum / f64::from(pairs)
        };
        if mean >= NAMING_COHESION_THRESHOLD {
            return MoveReason::NamingCohesion { cohesion: mean };
        }
    }

    MoveReason::Clustering
}

/// The files that stay in the group's source folders: members of an origin
/// both before and after the move, other than the group itself.
fn source_residents<'a>(ctx: &GroupContext<'a>) -> impl Iterator<Item = &'a String> {
    let origins = ctx.origins;
    let (before, after, files) = (ctx.before, ctx.after, ctx.files);
    origins.iter().flat_map(move |origin| {
        let staying = after.members_of.get(*origin);
        before
            .members_of
            .get(*origin)
            .into_iter()
            .flatten()
            .filter(move |member| {
                staying.is_some_and(|kept| kept.contains(*member))
                    && files.iter().all(|file| *file != *member)
            })
    })
}

/// The pull rule applied to both sides of a move: a candidate's pull is the sum
/// of the edge weights in both directions between it and every moved file, and
/// the strongest single candidate with a positive pull wins (first on ties).
fn strongest_tie<'a>(
    ctx: &GroupContext<'_>,
    candidates: impl Iterator<Item = &'a String>,
) -> Option<(&'a String, f64)> {
    let mut best: Option<(&String, f64)> = None;
    for candidate in candidates {
        let mut total = 0.0;
        for file in ctx.files {
            for key in [
                ((*file).clone(), candidate.clone()),
                (candidate.clone(), (*file).clone()),
            ] {
                total += ctx.facts.edge_weights.get(&key).copied().unwrap_or(0.0);
            }
        }
        if total > 0.0 && best.as_ref().is_none_or(|(_, top)| total > *top) {
            best = Some((candidate, total));
        }
    }
    best
}
