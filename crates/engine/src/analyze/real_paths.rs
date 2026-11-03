//! The result boundary: rewrites the analysis-internal path identities into
//! real repo-relative paths.
//!
//! Inside the analysis every file move, move reason and file advice is keyed
//! under its package-group name (`project/hub/a.py`), because that key is stable
//! across trees. The group name is a label, not a directory, so no serialized
//! path may carry it. This is the single place that rewrites it, and it reaches
//! only the paths the result types hand out as [`GroupKeyed`](crate::result::GroupKeyed):
//! symbol moves carry real paths from the start and are never touched, even
//! when a flat layout (`project/project/a.py`) makes one begin with the
//! group's own name. The same holds for finding locations: cycle, polarity,
//! visibility and test-only findings name a file's own real path, and a
//! capacity breach with real paths locates itself by them; only a capacity
//! breach with no real path names group-keyed ancestry, and only that is
//! rewritten.

use crate::result::{AnalyzeResult, Move, RelocationAdvice};

#[cfg(test)]
mod tests;

/// Rewrites every group-keyed path in `result`'s moves, move reasons and
/// advice to its real repo-relative form.
pub(super) fn strip_group_prefix(result: &mut AnalyzeResult) {
    let group = result.current.tree.name.clone();
    let keyed = result
        .profiles
        .anchored
        .iter_mut()
        .chain(&mut result.profiles.greenfield)
        .flat_map(|profile| &mut profile.candidates)
        .flat_map(|candidate| &mut candidate.delta_narration)
        .flat_map(Move::group_keyed)
        .chain(
            result
                .advice
                .recommended
                .iter_mut()
                .chain(&mut result.advice.review_candidates)
                .flat_map(RelocationAdvice::group_keyed),
        );
    for path in keyed {
        path.into_real(&group);
    }
    let profile_findings = result
        .profiles
        .anchored
        .iter_mut()
        .chain(&mut result.profiles.greenfield)
        .flat_map(|profile| &mut profile.current.unique_findings);
    for finding in result
        .current
        .shared_findings
        .iter_mut()
        .chain(profile_findings)
    {
        finding.rewrite_group_keyed_location(&group);
    }
}
