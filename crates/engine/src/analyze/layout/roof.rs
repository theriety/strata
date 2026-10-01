//! Synthesizes the alternative start that rebuilds a misnamed real roof.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::Condensation;
use strata_core::graph::csr::Csr;

use crate::analyze::layout::{rebuild_label, roof_coherence, scc_file_count, token_groups};
use crate::analyze::relocation::{FileInfo, ROOF_COHERENCE_FLOOR, RelocationIdentityGuard};

/// FIX09 (naming-incoherence): synthesizes one alternative search start that
/// rebuilds a misnamed real roof into evidence-backed places, returning the
/// rebuilt partition — or `None` when no folder carries the signature, leaving
/// every downstream byte unchanged.
///
/// The signature, per real non-synthetic folder cluster: a group of at least
/// two zero-priced files (no incident edge prices above zero anywhere in the
/// graph — D-46 makes them structurally unanchored, hence invisible to every
/// pull-driven move) whose basenames share tokens pairwise-connectedly, hosted
/// alongside at least one other file. Those strangers are exactly the
/// population edge-driven relocation can never nominate, so without synthesis
/// the pool collapses onto layouts that keep them welded under a label that
/// describes someone else.
///
/// The rebuild follows the [`relieve_over_capacity`] pattern: whole SCCs move
/// (the quotient DAG never gains a cycle), each new place gets a fresh cluster
/// id and a `{folder}/…` name grounded in member names that path-extends its
/// base folder at render time; member `home.domain` keys stay untouched. Two
/// shapes emerge, all deterministic:
///
/// 1. stranger groups leave for places named after their own shared word;
/// 2. when what remains covers fewer than half its files under the original
///    roof's name ([`ROOF_COHERENCE_FLOOR`]), the ENTIRE residual becomes one
///    rebuilt place named after its own heaviest member stems — a roof that
///    misdescribes its residents is replaced wholesale, not subdivided on
///    evidence the graph cannot price. The joined-stem name stays honest even
///    if polish later absorbs another consumer into the place: two of three
///    differently-named members still clear the majority floor.
///
/// Anything else — coherent residuals, lone stragglers, quiet all-unbonded
/// folders — stays put: naming alone never tears a folder that carries no
/// mixed-population signature.
// One function because the trigger, the stranger regroup, and the wholesale
// rebuild share one pass over the base partition; splitting them would either
// duplicate the bond scan or thread four pieces of mutable state through
// helpers. The length is documentation and the two place-naming branches.
#[allow(clippy::too_many_lines)]
pub(in crate::analyze) fn synthesize_roof_rebuild(
    files: &[FileInfo],
    condensation: &Condensation,
    graph: &Csr,
    test_zone: &[bool],
    pinned_scc: &[bool],
    base: &Partition,
    names: &mut Vec<SmolStr>,
    synthetic: &mut Vec<bool>,
) -> Option<Partition> {
    let relocation_identity = RelocationIdentityGuard::new(files, condensation, base, base, None);
    // which file vertices carry priced company at all — the bond evidence the
    // trigger reads. Zero-priced edges stay in the graph but bind nothing
    // (D-46), so a vertex whose every incident edge prices zero is exactly the
    // "unnominatable" population this synthesis exists for.
    let mut bonded = vec![false; graph.vertex_count()];
    // CSR neighbors are valid vertex ids by construction — `Csr` admits only
    // in-range endpoints — so every slot below exists; `.get_mut` keeps the
    // bound explicit and the loop total.
    for vertex in 0..graph.vertex_count() {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        for (&neighbor, weight) in graph.neighbors(from).iter().zip(graph.weights(from)) {
            if *weight > 0.0 {
                if let Some(slot) = bonded.get_mut(vertex) {
                    *slot = true;
                }
                if let Some(slot) = bonded.get_mut(neighbor as usize) {
                    *slot = true;
                }
            }
        }
    }
    // an SCC is unanchored only when EVERY file inside is: whole SCCs move, so
    // partial bonds keep the component glued to its measured company. A member
    // id outside the bond table counts as bonded — absence of evidence of
    // unanchorage keeps the component put.
    let unbonded_scc = |scc: u32| -> bool {
        condensation
            .members
            .get(scc as usize)
            .is_some_and(|members| {
                members
                    .iter()
                    .all(|member| bonded.get(member.0 as usize).is_some_and(|slot| !*slot))
            })
    };

    let base_count = base.cluster_count();
    let mut assignment = base.assignment().to_vec();
    let mut used_labels: BTreeSet<SmolStr> = names.iter().cloned().collect();
    let mut next_cluster = u64::from(u32::try_from(names.len()).unwrap_or(u32::MAX));
    let mut fired = false;

    for cluster_index in 0..base_count {
        if next_cluster > u64::from(u32::MAX) {
            break;
        }
        if synthetic.get(cluster_index).copied().unwrap_or(false) {
            // the workspace bucket is an absence of structure, not a roof to
            // rebuild (the FIX05 doctrine).
            continue;
        }
        let cluster = ClusterId(u32::try_from(cluster_index).unwrap_or(u32::MAX));
        let sccs: Vec<u32> = (0..u32::try_from(condensation.members.len()).unwrap_or(u32::MAX))
            .filter(|&scc| base.cluster_of(scc) == Some(cluster))
            .collect();
        if sccs.is_empty() {
            continue;
        }

        // strangers: unanchored SCCs grouped by shared basename tokens, keeping
        // groups of at least two files — a lone stray earns no invented place.
        // FIX11: an all-test-zone SCC is the spec-twin population; zone edges
        // price zero so such an SCC always looks unanchored, and the roof
        // rebuild must never sweep it into an invented production place.
        let strangers: Vec<u32> = sccs
            .iter()
            .copied()
            .filter(|&scc| unbonded_scc(scc))
            .filter(|&scc| !pinned_scc.get(scc as usize).copied().unwrap_or(true))
            .filter(|&scc| {
                condensation
                    .members
                    .get(scc as usize)
                    .is_some_and(|members| {
                        members.iter().all(|member| {
                            !test_zone.get(member.0 as usize).copied().unwrap_or(false)
                        })
                    })
            })
            .collect();
        let groups: Vec<Vec<u32>> = token_groups(&strangers, condensation, files)
            .into_iter()
            .flat_map(|group| relocation_identity.collision_free_subgroups(&group))
            .filter(|group| {
                group
                    .iter()
                    .map(|&scc| scc_file_count(condensation, scc))
                    .sum::<u32>()
                    >= 2
            })
            .collect();
        if groups.is_empty() {
            continue;
        }
        let exiled: BTreeSet<u32> = groups.iter().flatten().copied().collect();
        let residual: Vec<u32> = sccs
            .iter()
            .copied()
            .filter(|scc| !exiled.contains(scc))
            .collect();
        let residual_files: u32 = residual
            .iter()
            .map(|&scc| scc_file_count(condensation, scc))
            .sum();
        let base_name = names
            .get(cluster_index)
            .cloned()
            .unwrap_or_else(|| SmolStr::new("workspace"));

        // strangers first, each a fresh cluster id.
        for group in &groups {
            let label = rebuild_label(
                &base_name,
                group,
                condensation,
                files,
                &mut used_labels,
                next_cluster,
            );
            for &scc in group {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(label);
            synthetic.push(false);
            next_cluster += 1;
            fired = true;
        }

        // then the residual roof: when its own name covers fewer than half of
        // what remains, replace the roof wholesale with one place named after
        // the residents themselves. A lone straggler keeps the original roof —
        // one file under any name is vacuously covered.
        if residual_files >= 2
            && residual
                .iter()
                .all(|scc| !pinned_scc.get(*scc as usize).copied().unwrap_or(true))
            && roof_coherence(&base_name, &residual, condensation, files) < ROOF_COHERENCE_FLOOR
        {
            let label = rebuild_label(
                &base_name,
                &residual,
                condensation,
                files,
                &mut used_labels,
                next_cluster,
            );
            for &scc in &residual {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(label);
            synthetic.push(false);
            next_cluster += 1;
            fired = true;
        }
    }

    fired.then(|| {
        Partition::from_assignment(
            assignment,
            usize::try_from(next_cluster).unwrap_or(base_count),
        )
    })
}
