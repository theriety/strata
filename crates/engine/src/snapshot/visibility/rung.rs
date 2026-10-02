//! Rung selection (ADR-25): compares ladder rungs by their file sets.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use strata_ir::ScopeRung;

/// Returns the index of the narrowest rung that holds every `footprint` file.
///
/// Rungs are compared by identity, not by level label. Among following rungs
/// with the very same file set the last one wins, since the wider spelling is
/// the one that is never a no-op to name. A rung whose file list is not strictly
/// sorted is never chosen; passing over it can only widen the answer.
pub(super) fn needed_index(rungs: &[ScopeRung], footprint: &BTreeSet<&SmolStr>) -> Option<usize> {
    let first = rungs.iter().position(|rung| {
        sorted(&rung.files)
            && footprint
                .iter()
                .all(|path| rung.files.binary_search(path).is_ok())
    })?;
    let mut index = first;
    while let Some((current, next)) = rungs.get(index).zip(rungs.get(index + 1)) {
        if current.files != next.files {
            break;
        }
        index += 1;
    }
    Some(index)
}

/// Whether `files` is strictly ascending.
fn sorted(files: &[SmolStr]) -> bool {
    files.windows(2).all(|pair| {
        pair.first()
            .zip(pair.get(1))
            .is_some_and(|(left, right)| left < right)
    })
}
