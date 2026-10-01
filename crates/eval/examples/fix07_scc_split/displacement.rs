//! Displacement attribution: which pipeline stage sealed each moved file.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::result::MoveReason;

use super::rows::FileRow;

/// One displaced file of the anchored best candidate, with its attribution.
pub(super) struct Displacement {
    /// Full path of the moved file.
    pub(super) path: String,
    /// The folded source folder the move narrates.
    pub(super) from: String,
    /// Which pipeline stage sealed the file's placement.
    pub(super) origin: Origin,
}

/// Attribution class of a displaced file, in evaluation precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// The move narrates `RelievesOverCap`: a FIX03 capacity half split.
    Relief,
    /// The file belongs to a mixed-home SCC: its individual placement was
    /// sealed at seed time; polish translates the SCC wholesale but can never
    /// place the file back home on its own.
    SeedGranularity,
    /// The file's SCC is single-home: seed placed it correctly; polish chose
    /// to move it.
    Polish,
}

/// Flattens the best candidate's narration into displaced files and applies the
/// attribution precedence: relief by reason tag, then mixed-home SCC, then
/// polish. Returns the displacements plus how many paths failed to map back to
/// the replica's file identity.
pub(super) fn collect_displacements(
    narration: &[strata_engine::result::Move],
    index_of_path: &BTreeMap<String, usize>,
    scc_of_vertex: &[Option<usize>],
    mixed_of_scc: &[bool],
) -> (Vec<Displacement>, usize) {
    let mut displacements: Vec<Displacement> = Vec::new();
    for mv in narration {
        let origin = if matches!(mv.reason, MoveReason::RelievesOverCap { .. }) {
            Origin::Relief
        } else {
            Origin::Polish
        };
        for file_move in &mv.files {
            displacements.push(Displacement {
                path: file_move.path.clone(),
                from: file_move.from.clone(),
                origin,
            });
        }
    }
    // Re-classify: any non-relief displacement inside a mixed-home SCC is a
    // seed-granularity tear, whatever pull motivated the whole-SCC translation.
    let mut unmapped = 0_usize;
    for displacement in &mut displacements {
        if displacement.origin == Origin::Relief {
            continue;
        }
        let owner = index_of_path
            .get(displacement.path.as_str())
            .copied()
            .map(|index| (index, scc_of_vertex.get(index).copied().flatten()));
        match owner {
            Some((_, Some(scc))) => {
                if mixed_of_scc.get(scc).copied().unwrap_or(false) {
                    displacement.origin = Origin::SeedGranularity;
                }
            }
            _ => unmapped += 1,
        }
    }
    (displacements, unmapped)
}

/// Prints one line per displaced file with its attribution class.
pub(super) fn print_displacements(displacements: &[Displacement]) {
    for displacement in displacements {
        let tag = match displacement.origin {
            Origin::Relief => "relief",
            Origin::SeedGranularity => "seed-granularity",
            Origin::Polish => "polish",
        };
        println!(
            "  moved {} [{} from '{}']",
            displacement.path, tag, displacement.from
        );
    }
}

/// Reports any displaced file whose SCC mates did not move with it: polish
/// moves whole SCCs, so a partial group means the replica's grain diverged
/// from the engine's.
pub(super) fn check_scc_atomicity(
    rows: &[FileRow],
    index_of_path: &BTreeMap<String, usize>,
    scc_members: &[Vec<usize>],
    scc_of_vertex: &[Option<usize>],
    displacements: &[Displacement],
) {
    let moved: BTreeSet<&str> = displacements
        .iter()
        .map(|displacement| displacement.path.as_str())
        .collect();
    let mut partial = 0_usize;
    let mut shown = 0_usize;
    for displacement in displacements {
        let owner = index_of_path
            .get(displacement.path.as_str())
            .and_then(|index| scc_of_vertex.get(*index).copied().flatten());
        let Some(scc) = owner else {
            continue;
        };
        for &vertex in scc_members.get(scc).into_iter().flatten() {
            let Some(row) = rows.get(vertex) else {
                continue;
            };
            if !moved.contains(row.path.as_str()) {
                partial += 1;
                if shown < 3 {
                    println!(
                        "  atomicity: {} moved without SCC mate {}",
                        displacement.path, row.path
                    );
                    shown += 1;
                }
            }
        }
    }
    if partial > shown {
        println!("  atomicity: {partial} unreported mates in total");
    }
}
