//! Clusters the upper levels of the candidate tree over their weighted quotients.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::cluster::coarsen::{CoarseGraph, coarsen_chain, tight_layers};
use strata_core::cluster::refine::{GainFn, refine};
use strata_core::cluster::seed::{SeedLevel, seed};
use strata_core::cluster::{LevelCaps, Partition};
use strata_core::graph::csr::Csr;

/// Bound on polish sweeps: two passes catch the follow-up moves the first pass
/// unlocks without ballooning the wall clock.
/// Lifts per-SCC folder keys onto the coarsest chain level so seeding keeps
/// files from the same physical folder contiguous.
fn fold_affinity_to_top(chain: &[CoarseGraph], scc_keys: &[u32]) -> Vec<u32> {
    let Some(top) = chain.last() else {
        return Vec::new();
    };
    let mut tallies: Vec<BTreeMap<u32, u32>> = vec![BTreeMap::new(); top.graph.vertex_count()];
    for (scc, &key) in scc_keys.iter().enumerate() {
        let mut vertex = u32::try_from(scc).unwrap_or(u32::MAX);
        for level in chain.iter().skip(1) {
            vertex = level
                .fine_to_coarse
                .get(vertex as usize)
                .copied()
                .unwrap_or(vertex);
        }
        if let Some(tally) = tallies.get_mut(vertex as usize) {
            *tally.entry(key).or_insert(0) += 1;
        }
    }
    tallies
        .iter()
        .map(|tally| {
            tally
                .iter()
                .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
                .map_or(u32::MAX, |(key, _)| *key)
        })
        .collect()
}

/// Clusters a weighted quotient graph one level up (folders → domains, domains
/// → packages, …) with the same multilevel scheme the base level uses, minus
/// seed perturbation (the level is fully determined by the partition below it,
/// keeping assembly a pure function of the folder partition).
///
/// `affinity` keys each base vertex (a below-level container) by its dominant
/// home directory: the seed keeps same-home containers contiguous, so cap
/// boundaries fall *between* home directories and each domain/package comes out
/// home-coherent (a named `adapters`, `agent`, …) instead of an index-order
/// grab-bag that [`elect`] could name only through its lower rungs — a shared
/// prefix or a top-two join rather than one honest home. An empty slice — as
/// for the package-group level, which has no home key — restores the prior
/// neutral descending-layer order.
///
/// lean: refinement stays [`GainFn::cut_only`]. A cohesion gain here would not be
/// score-aligned — the objective's naming term scores only file/folder symbol
/// groups (`score`'s `cohesion_groups`), never domains — so it would bias the
/// search off the objective while, being far smaller than any integer cut gain,
/// never actually holding a folder that pure cut wants to move. The seed affinity
/// is the whole fix; a genuine cross-home coupling (a strictly-positive cut gain)
/// is still honoured, and the cross-home composite [`elect`] then joins for it
/// stays honest — named after its real origins, never a synthetic label.
pub(in crate::analyze) fn cluster_level(
    graph: &Csr,
    caps: &LevelCaps,
    level: SeedLevel,
    affinity: &[u32],
) -> Partition {
    let layers = tight_layers(graph);
    // each quotient vertex is one container of the level below, so capacity
    // weights are all one: the cap counts members directly.
    let unit_weights = vec![1_u32; graph.vertex_count()];
    let cap = level.cap(caps);
    let chain = coarsen_chain(graph, &layers, &unit_weights, cap.max(1));
    let Some(top) = chain.last() else {
        return Partition::from_assignment(Vec::new(), 0);
    };
    // lift the per-base-vertex home affinity onto the coarsest level so the seed
    // keeps same-home containers contiguous (empty affinity folds to the neutral
    // descending-layer order).
    let top_affinity = fold_affinity_to_top(&chain, affinity);
    let mut parts = seed(top, &top.layers, caps, level, &top_affinity);
    refine(
        top,
        &mut parts,
        &GainFn::cut_only(top.graph.vertex_count()),
        caps,
        level,
    );
    for window in chain.windows(2).rev() {
        let [fine, coarse] = window else {
            continue;
        };
        parts = coarse.project(&parts);
        refine(
            fine,
            &mut parts,
            &GainFn::cut_only(fine.graph.vertex_count()),
            caps,
            level,
        );
    }
    parts
}

/// Interns each base vertex's dominant home directory `homes[v]` into a seed
/// affinity ordinal, so `seed` keeps same-home containers contiguous and cap
/// boundaries fall between home directories. Equal keys share an ordinal; the
/// order the distinct keys are numbered is irrelevant (affinity only groups).
pub(in crate::analyze) fn home_affinity(homes: &[SmolStr]) -> Vec<u32> {
    let mut key_ids: BTreeMap<SmolStr, u32> = BTreeMap::new();
    homes
        .iter()
        .map(|home| {
            let next = u32::try_from(key_ids.len()).unwrap_or(u32::MAX);
            *key_ids.entry(home.clone()).or_insert(next)
        })
        .collect()
}
