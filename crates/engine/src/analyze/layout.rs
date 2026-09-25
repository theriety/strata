use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::coarsen::{CoarseGraph, coarsen_chain, tight_layers};
use strata_core::cluster::refine::{GainFn, refine};
use strata_core::cluster::seed::{SeedLevel, seed};
use strata_core::cluster::{ClusterId, LevelCaps, Partition};
use strata_core::condense::Condensation;
use strata_core::graph::csr::Csr;
use strata_ir::{Container, ContainerId, NodeId, ScopeLevel};

use crate::analyze::relocation::{FileInfo, ROOF_COHERENCE_FLOOR, RelocationIdentityGuard};
use crate::analyze::scoring::ContainerSpec;
use crate::narrate::{path_segments, physical_namespace, tokenize};

#[cfg(test)]
mod tests;

/// Renders a file's package-relative namespace (ADR-18) from its real path
/// and laminar home — the see-through source-root segments its folder key
/// omits (`src` for `crates/core/src/x.rs`).
pub(in crate::analyze) fn render_namespace(path: &str, home: &LaminarHome) -> SmolStr {
    let directory = path_segments(path.rsplit_once('/').map_or("", |(directory, _)| directory));
    let package = path_segments(&home.package);
    let folder = if home.synthetic {
        package.clone()
    } else {
        path_segments(&home.folder)
    };
    SmolStr::new(physical_namespace(&directory, &package, &folder).join("/"))
}

/// The laminar container tree's already-resolved folder, domain, and package
/// name keys for one file — full-prefix keys (`ai/adapters`, `ai`) with any
/// transparent source-root segment stripped and the package resolved to its
/// nearest manifest root.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::analyze) struct LaminarHome {
    /// The file's folder-level container name key.
    pub(in crate::analyze) folder: SmolStr,
    /// The file's domain-level container name key.
    pub(in crate::analyze) domain: SmolStr,
    /// The file's package-level container name key.
    pub(in crate::analyze) package: SmolStr,
    /// True when the folder-level container is the synthetic `workspace` bucket
    /// (a root-level file with no real directory of its own). Functionally
    /// determined by `folder`, so it never splits two otherwise-equal homes into
    /// distinct clusters; it carries the current tree's collapse marker across to
    /// the candidate folder so a greenfield render drops the bucket too.
    pub(in crate::analyze) synthetic: bool,
}

/// Reads the laminar folder/domain/package name keys for the file container
/// `file_container` by walking its ancestor chain in `by_id`. The laminar tree
/// (`build_laminar_tree`) already stripped a transparent leading source root and
/// resolved the nearest package root, so reusing its names keeps candidate
/// naming consistent with the current tree instead of re-deriving from raw paths
/// (which would surface `src` as a package/folder). Folders and package roots
/// nest, so the NEAREST ancestor at each level wins — deeper keys are the real
/// place. A level missing from the chain inherits the nearest broader key (a
/// file directly in its domain directory has that directory as its real
/// folder), and only a chain with no package at all falls back to the laminar
/// synthetic `workspace` bucket.
pub(in crate::analyze) fn laminar_home(
    by_id: &BTreeMap<u32, &Container>,
    file_container: u32,
) -> LaminarHome {
    let (mut folder, mut domain, mut package) = (None, None, None);
    let mut synthetic = false;
    let mut current = by_id.get(&file_container).copied();
    while let Some(container) = current {
        match container.level {
            ScopeLevel::Folder if folder.is_none() => {
                folder = Some(container.name.clone());
                synthetic = container.synthetic;
            }
            ScopeLevel::Domain if domain.is_none() => domain = Some(container.name.clone()),
            ScopeLevel::Package if package.is_none() => package = Some(container.name.clone()),
            _ => {}
        }
        current = container
            .parent
            .and_then(|parent| by_id.get(&parent.0).copied());
    }
    let package = package.unwrap_or_else(|| SmolStr::new("workspace"));
    let domain = domain.unwrap_or_else(|| package.clone());
    let folder = folder.unwrap_or_else(|| domain.clone());
    LaminarHome {
        folder,
        domain,
        package,
        synthetic,
    }
}

/// Bound on polish sweeps: two passes catch the follow-up moves the first pass
/// unlocks without ballooning the wall clock.
/// Lifts per-SCC folder keys onto the coarsest chain level so seeding keeps
/// files from the same physical folder contiguous.
pub(in crate::analyze) fn fold_affinity_to_top(
    chain: &[CoarseGraph],
    scc_keys: &[u32],
) -> Vec<u32> {
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

/// The member an SCC is placed by: the file with the largest production SLOC,
/// ties to the lexicographically smaller path.
pub(in crate::analyze) fn dominant_member<'a>(
    files: &'a [FileInfo],
    members: &[NodeId],
) -> Option<&'a FileInfo> {
    let mut dominant: Option<&FileInfo> = None;
    for member in members {
        let Some(file) = files.get(member.0 as usize) else {
            continue;
        };
        let better = dominant.is_none_or(|top| {
            file.production_sloc > top.production_sloc
                || (file.production_sloc == top.production_sloc && file.name < top.name)
        });
        if better {
            dominant = Some(file);
        }
    }
    dominant
}

/// Builds the real-directory folder partition: each file SCC lands in the
/// cluster of its dominant member's laminar home — the file with the largest
/// production SLOC, ties to the lexicographically smaller path (an SCC
/// spanning folders must co-cluster anyway, so it stays with its heaviest
/// member). Folders come from reality, not from clustering, so this partition
/// doubles as the identity layout. Clusters key on the full laminar location
/// (folder, domain, package), never the folder name alone: real directories
/// are already unique by their full-depth keys, and the full location keeps
/// same-named fallback buckets (`workspace`) of different packages apart.
/// Cluster ids are dense over the distinct locations in ascending order, and
/// the returned names carry each cluster's real directory key so emission
/// never re-elects folder names.
pub(in crate::analyze) fn real_dir_partition(
    files: &[FileInfo],
    condensation: &Condensation,
) -> (Partition, Vec<SmolStr>, Vec<bool>) {
    let fallback = || LaminarHome {
        folder: SmolStr::new("workspace"),
        domain: SmolStr::new("workspace"),
        package: SmolStr::new("workspace"),
        synthetic: false,
    };
    let keys: Vec<LaminarHome> = condensation
        .members
        .iter()
        .map(|members| {
            dominant_member(files, members).map_or_else(fallback, |file| file.home.clone())
        })
        .collect();
    let distinct: BTreeSet<LaminarHome> = keys.iter().cloned().collect();
    let cluster_of_key: BTreeMap<LaminarHome, u32> = distinct
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let assignment = keys
        .iter()
        .map(|key| ClusterId(cluster_of_key.get(key).copied().unwrap_or(0)))
        .collect();
    let names = qualify_folder_names(&distinct);
    // the synthetic marker rides in lockstep with `names`: both map the distinct
    // homes in the same iteration order, so cluster `i` names and marks the same
    // real location. It is folder-determined, so it never perturbs the clustering.
    let synthetic: Vec<bool> = distinct.iter().map(|home| home.synthetic).collect();
    (
        Partition::from_assignment(assignment, names.len()),
        names,
        synthetic,
    )
}

/// Relieves every over-capacity physical namespace with evidence-backed child
/// groups while preserving whole-SCC movement.
///
/// The split runs on priced connectivity between the folder's SCCs: two SCCs
/// end up in the same pile only when a positively-priced file edge joins them
/// ([`connected_piles`]), so each child group stays internally connected and the
/// quotient DAG never gains a cycle (whole SCCs always move together). Only
/// evidence-backed groups split off: a pile qualifies when its members are
/// actually joined (at least two SCCs or two files); unrelated singletons stay
/// glued to the original cluster, because inventing groups for unconnected
/// files would erase the merge pressure that cross-folder coupling otherwise
/// exerts on the search. A connected pile that alone exceeds the budget is
/// chunked contiguously along its own connectivity; a single oversized SCC
/// cannot be split at all and is left whole — the objective still prices it,
/// which is honest.
///
/// Pile 0 of each folder keeps the folder's original cluster id; every later
/// pile gets a fresh cluster id, a `{folder}/{token}` name that path-extends
/// its base folder at render time, and a `false` synthetic marker. Members'
/// `home.domain` keys stay untouched: the groups already sit in one domain —
/// their base folder's — and the slash in the label is what nests the group
/// under it, so no home rewriting can weld or split anything here. Returns the
/// files plus the relieved partition and its extended tables; the
/// identity entry must be cloned before this runs (the caller does).
pub(in crate::analyze) fn relieve_over_capacity(
    files: Vec<FileInfo>,
    condensation: &Condensation,
    graph: &Csr,
    base: &Partition,
    base_names: &[SmolStr],
    base_synthetic: &[bool],
    cap: u32,
) -> (Vec<FileInfo>, Partition, Vec<SmolStr>, Vec<bool>) {
    let mut assignment = base.assignment().to_vec();
    let mut names = base_names.to_vec();
    let mut synthetic = base_synthetic.to_vec();
    if cap == 0 {
        // a disabled budget binds nothing, so there is nothing to relieve.
        return (files, base.clone(), names, synthetic);
    }
    let base_count = base.cluster_count();
    let mut next_cluster = u64::from(u32::try_from(base_count).unwrap_or(u32::MAX));
    let mut used_labels: BTreeSet<SmolStr> = names.iter().cloned().collect();
    let direct_children = relief_child_entries(base, base_names, condensation, &files);
    for cluster_index in 0..base_count {
        if next_cluster > u64::from(u32::MAX) {
            break;
        }
        let cluster = ClusterId(u32::try_from(cluster_index).unwrap_or(u32::MAX));
        let sccs: Vec<u32> = (0..u32::try_from(condensation.members.len()).unwrap_or(u32::MAX))
            .filter(|scc| base.cluster_of(*scc) == Some(cluster))
            .collect();
        let base_name = base_names
            .get(cluster_index)
            .cloned()
            .unwrap_or_else(|| SmolStr::new("workspace"));
        let by_namespace = relief_sccs_by_namespace(&sccs, condensation, &files);
        let mut planned_groups = Vec::new();
        for (namespace, file_count_by_scc) in &by_namespace {
            let bound: u32 = file_count_by_scc.values().copied().sum();
            let child_entries = direct_children
                .get(&(cluster_index, namespace.clone()))
                .copied()
                .unwrap_or(0);
            if bound.saturating_add(child_entries) <= cap {
                continue;
            }
            let file_budget = cap.saturating_sub(child_entries).max(1);
            let namespace_sccs: Vec<u32> = file_count_by_scc.keys().copied().collect();
            let piles = connected_relief_piles(
                &namespace_sccs,
                condensation,
                graph,
                file_budget,
                file_count_by_scc,
            );
            planned_groups.extend(relief_move_groups(
                &piles,
                file_count_by_scc,
                bound,
                child_entries,
                cap,
            ));
        }
        for (ordinal, group) in merge_overlapping_relief_groups(planned_groups)
            .iter()
            .enumerate()
        {
            let token = elect_split_token(group, condensation, &files);
            let mut label = token.map_or_else(
                || format!("{base_name}/{}", ordinal + 1),
                |stem| format!("{base_name}/{stem}"),
            );
            if used_labels.contains(label.as_str()) {
                label = format!("{base_name}/{next_cluster}");
            }
            used_labels.insert(SmolStr::from(label.clone()));
            for &scc in group {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(SmolStr::from(label));
            synthetic.push(false);
            next_cluster += 1;
        }
    }
    (
        files,
        Partition::from_assignment(
            assignment,
            usize::try_from(next_cluster).unwrap_or(base_count),
        ),
        names,
        synthetic,
    )
}

pub(in crate::analyze) fn relief_child_entries(
    base: &Partition,
    names: &[SmolStr],
    condensation: &Condensation,
    files: &[FileInfo],
) -> BTreeMap<(usize, SmolStr), u32> {
    let mut entries = BTreeMap::new();
    for (child, child_name) in names.iter().enumerate().take(base.cluster_count()) {
        let Some((parent_name, _)) = child_name.rsplit_once('/') else {
            continue;
        };
        let Some(parent) = names.iter().position(|name| name.as_str() == parent_name) else {
            continue;
        };
        let child_cluster = ClusterId(u32::try_from(child).unwrap_or(u32::MAX));
        let namespaces: BTreeSet<SmolStr> = condensation
            .members
            .iter()
            .enumerate()
            .filter(|(scc, _)| {
                base.cluster_of(u32::try_from(*scc).unwrap_or(u32::MAX)) == Some(child_cluster)
            })
            .flat_map(|(_, members)| members)
            .filter_map(|member| files.get(member.0 as usize))
            .map(|file| file.namespace.clone())
            .collect();
        for namespace in namespaces {
            *entries.entry((parent, namespace)).or_default() += 1;
        }
    }
    entries
}

pub(in crate::analyze) fn relief_sccs_by_namespace(
    sccs: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> BTreeMap<SmolStr, BTreeMap<u32, u32>> {
    let mut grouped = BTreeMap::new();
    for &scc in sccs {
        for file in condensation
            .members
            .get(scc as usize)
            .into_iter()
            .flatten()
            .filter_map(|member| files.get(member.0 as usize))
        {
            let by_scc: &mut BTreeMap<u32, u32> =
                grouped.entry(file.namespace.clone()).or_default();
            let count = by_scc.entry(scc).or_default();
            *count = count.saturating_add(1);
        }
    }
    grouped
}

pub(in crate::analyze) fn connected_relief_piles(
    sccs: &[u32],
    condensation: &Condensation,
    graph: &Csr,
    file_budget: u32,
    file_count_by_scc: &BTreeMap<u32, u32>,
) -> Vec<Vec<u32>> {
    connected_piles(sccs, condensation, graph, file_budget, file_count_by_scc)
        .into_iter()
        .filter(|pile| {
            pile.len() >= 2
                || pile
                    .first()
                    .and_then(|scc| file_count_by_scc.get(scc))
                    .is_some_and(|&count| count >= 2)
        })
        .collect()
}

pub(in crate::analyze) fn relief_move_groups(
    piles: &[Vec<u32>],
    file_count_by_scc: &BTreeMap<u32, u32>,
    bound: u32,
    child_entries: u32,
    cap: u32,
) -> Vec<Vec<u32>> {
    if piles.is_empty() {
        return Vec::new();
    }
    let grouped_files: u32 = piles
        .iter()
        .flatten()
        .filter_map(|scc| file_count_by_scc.get(scc))
        .copied()
        .sum();
    let stationary = bound.saturating_sub(grouped_files);
    let available = usize::try_from(cap.saturating_sub(child_entries).saturating_sub(stationary))
        .unwrap_or(usize::MAX);
    if available == 0 {
        return Vec::new();
    }
    let first_files: u32 = piles
        .first()
        .into_iter()
        .flatten()
        .filter_map(|scc| file_count_by_scc.get(scc))
        .copied()
        .sum();
    let keep_first = stationary
        .saturating_add(first_files)
        .saturating_add(child_entries)
        .saturating_add(u32::try_from((piles.len() - 1).min(available)).unwrap_or(u32::MAX))
        <= cap;
    let moving = if keep_first {
        piles.get(1..).unwrap_or_default()
    } else {
        piles
    };
    let group_count = moving.len().min(available);
    let mut groups = vec![Vec::new(); group_count];
    for (index, pile) in moving.iter().enumerate() {
        let group = index.saturating_mul(group_count) / moving.len();
        if let Some(target) = groups.get_mut(group) {
            target.extend(pile.iter().copied());
        }
    }
    groups
}

pub(in crate::analyze) fn merge_overlapping_relief_groups(groups: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
    let mut merged: Vec<BTreeSet<u32>> = Vec::new();
    for group in groups {
        let mut current: BTreeSet<u32> = group.into_iter().collect();
        if current.is_empty() {
            continue;
        }
        let mut untouched = Vec::new();
        for existing in std::mem::take(&mut merged) {
            if current.is_disjoint(&existing) {
                untouched.push(existing);
            } else {
                current.extend(existing);
            }
        }
        merged = untouched;
        merged.push(current);
    }
    let mut result: Vec<Vec<u32>> = merged
        .into_iter()
        .map(|group| group.into_iter().collect())
        .collect();
    result.sort();
    result
}

/// Counts the files an SCC holds.
pub(in crate::analyze) fn scc_file_count(condensation: &Condensation, scc: u32) -> u32 {
    condensation.members.get(scc as usize).map_or(0, |members| {
        u32::try_from(members.len()).unwrap_or(u32::MAX)
    })
}

/// Follows `parent` links from `start` to its union-find root.
pub(in crate::analyze) fn union_root(parent: &BTreeMap<u32, u32>, start: u32) -> u32 {
    let mut node = start;
    while let Some(&up) = parent.get(&node) {
        if up == node {
            return node;
        }
        node = up;
    }
    start
}

/// Groups `sccs` into connectivity piles joined by positively-priced edges
/// between their member files: every connected component becomes one pile,
/// and a component that alone exceeds `cap` is chunked by breadth-first
/// traversal from its smallest SCC, closing a chunk just before the next SCC
/// would push it past the budget (a lone oversized SCC stays whole). No
/// packing happens across components — unrelated files must not be invented
/// into a shared half. Deterministic: CSR rows are ascending, union roots are
/// the smaller id, and traversal order follows ascending neighbor ids.
pub(in crate::analyze) fn connected_piles(
    sccs: &[u32],
    condensation: &Condensation,
    graph: &Csr,
    cap: u32,
    file_count_by_scc: &BTreeMap<u32, u32>,
) -> Vec<Vec<u32>> {
    let wanted: BTreeSet<u32> = sccs.iter().copied().collect();
    let mut parent: BTreeMap<u32, u32> = wanted.iter().map(|&scc| (scc, scc)).collect();
    let mut adjacency: BTreeMap<u32, BTreeSet<u32>> =
        wanted.iter().map(|&scc| (scc, BTreeSet::new())).collect();
    for &scc in sccs {
        let Some(members) = condensation.members.get(scc as usize) else {
            continue;
        };
        for member in members {
            let vertex = member.0;
            for (&neighbor, weight) in graph.neighbors(vertex).iter().zip(graph.weights(vertex)) {
                if *weight <= 0.0 {
                    // zero-priced edges never bind placement (FIX04 doctrine).
                    continue;
                }
                let Some(&other_scc) = condensation.membership.get(neighbor as usize) else {
                    continue;
                };
                let other_scc = other_scc.0;
                if !wanted.contains(&other_scc) || other_scc == scc {
                    continue;
                }
                adjacency.entry(scc).or_default().insert(other_scc);
                adjacency.entry(other_scc).or_default().insert(scc);
                let (a, b) = (union_root(&parent, scc), union_root(&parent, other_scc));
                if a != b {
                    let (keep, move_) = if a <= b { (a, b) } else { (b, a) };
                    parent.insert(move_, keep);
                }
            }
        }
    }

    // gather connected components, each ordered ascending by SCC id.
    let mut components: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &scc in sccs {
        components
            .entry(union_root(&parent, scc))
            .or_default()
            .push(scc);
    }

    let mut piles: Vec<Vec<u32>> = Vec::new();
    for members in components.values_mut() {
        members.sort_unstable();
        let total: u32 = members
            .iter()
            .filter_map(|scc| file_count_by_scc.get(scc))
            .copied()
            .sum();
        let chunks: Vec<Vec<u32>> = if total <= cap {
            vec![members.clone()]
        } else {
            let mut visited: BTreeSet<u32> = BTreeSet::new();
            let mut queue: std::collections::VecDeque<u32> =
                members.first().copied().into_iter().collect();
            let mut chunk: Vec<u32> = Vec::new();
            let mut chunk_size = 0_u32;
            let mut pieces: Vec<Vec<u32>> = Vec::new();
            while let Some(scc) = queue.pop_front() {
                if !visited.insert(scc) {
                    continue;
                }
                let scc_size = file_count_by_scc.get(&scc).copied().unwrap_or(0);
                if !chunk.is_empty() && chunk_size + scc_size > cap {
                    pieces.push(std::mem::take(&mut chunk));
                    chunk_size = 0;
                }
                chunk.push(scc);
                chunk_size += scc_size;
                for neighbor in adjacency.get(&scc).into_iter().flatten() {
                    if !visited.contains(neighbor) {
                        queue.push_back(*neighbor);
                    }
                }
            }
            if !chunk.is_empty() {
                pieces.push(chunk);
            }
            pieces
        };
        piles.extend(chunks);
    }
    piles
}

/// Elects a split-half suffix token from the dominant alphabetic basename stem
/// among the pile's files, weighted by production SLOC then file count, with
/// lexicographic order breaking exact ties. Returns `None` when no file has a
/// usable stem, falling back to numeric labels.
pub(in crate::analyze) fn elect_split_token(
    pile: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> Option<String> {
    #[derive(Clone, Copy, Default)]
    struct Tally {
        sloc: u64,
        count: u32,
    }
    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    for &scc in pile {
        let Some(members) = condensation.members.get(scc as usize) else {
            continue;
        };
        for member in members {
            let Some(file) = files.get(member.0 as usize) else {
                continue;
            };
            if let Some(token) = basename_stem(&file.name) {
                let entry = tally.entry(token).or_default();
                entry.sloc += u64::from(file.production_sloc);
                entry.count += 1;
            }
        }
    }
    tally
        .into_iter()
        .max_by(|a, b| {
            (a.1.sloc, a.1.count)
                .cmp(&(b.1.sloc, b.1.count))
                .then_with(|| b.0.cmp(&a.0))
        })
        .map(|(token, _)| token)
}

/// Extracts a file path's lowercase leading-alphabetic basename stem:
/// `hub/ingest_00.py` elects `ingest`, `emit_00.ts` elects `emit`. Numeric or
/// punctuation-only stems yield `None`.
pub(in crate::analyze) fn basename_stem(name: &str) -> Option<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split_once('.').map_or(basename, |(stem, _)| stem);
    let run: String = stem
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_lowercase();
    if run.is_empty() { None } else { Some(run) }
}

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

/// Fraction of `member_files` whose basename shares a token with the last `/`
/// segment of `container_name` — the engine-side twin of the eval harness's
/// alignment metric, so the synthesis trigger and the verdict measure the same
/// coherence and can never disagree about what "misnamed" means.
pub(in crate::analyze) fn roof_coherence(
    container_name: &str,
    residual: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> f64 {
    let last = container_name.rsplit('/').next().unwrap_or(container_name);
    let container_tokens = tokenize(last);
    let member_files: Vec<String> = residual
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .collect();
    let total = member_files.len();
    if total == 0 {
        return 1.0;
    }
    let aligned = member_files
        .iter()
        .filter(|file| {
            let base = file.rsplit('/').next().unwrap_or(file.as_str());
            let stem = base.split('.').next().unwrap_or(base);
            tokenize(stem)
                .iter()
                .any(|token| container_tokens.contains(token))
        })
        .count();
    // reason: member counts are corpus-sized; the f64 mantissa loses nothing
    #[allow(clippy::cast_precision_loss)]
    let sharing = aligned as f64 / total as f64;
    sharing
}

/// Collects the file paths inside one SCC, ascending.
pub(in crate::analyze) fn scc_file_names(
    condensation: &Condensation,
    files: &[FileInfo],
    scc: u32,
) -> Vec<String> {
    condensation
        .members
        .get(scc as usize)
        .into_iter()
        .flatten()
        .filter_map(|member| files.get(member.0 as usize))
        .map(|file| file.name.to_string())
        .collect()
}

/// Basename tokens of one file path, per the CONTRACT tokenization scope: the
/// basename minus extension only — the directory prefix never counts toward a
/// container's claim on a file.
pub(in crate::analyze) fn basename_tokens(name: &str) -> BTreeSet<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split('.').next().unwrap_or(basename);
    tokenize(stem)
}

/// Groups `sccs` into token-connected components: two SCCs join when any pair
/// of their files shares a basename token. This is naming evidence only — no
/// edge is priced or fabricated (D-46 holds: absence of priced edges is read
/// as separation evidence, and shared words group the separated). Deterministic:
/// ascending SCC pairs, union roots the smaller id, components emitted by
/// smallest member ascending, each sorted ascending.
pub(in crate::analyze) fn token_groups(
    sccs: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> Vec<Vec<u32>> {
    let tokens_of: BTreeMap<u32, BTreeSet<String>> = sccs
        .iter()
        .map(|&scc| {
            let tokens: BTreeSet<String> = scc_file_names(condensation, files, scc)
                .iter()
                .flat_map(|name| basename_tokens(name))
                .collect();
            (scc, tokens)
        })
        .collect();
    let mut parent: BTreeMap<u32, u32> = sccs.iter().map(|&scc| (scc, scc)).collect();
    for (index, &left) in sccs.iter().enumerate() {
        for &right in sccs.iter().skip(index + 1) {
            let joined = tokens_of.get(&left).is_some_and(|left_tokens| {
                tokens_of
                    .get(&right)
                    .is_some_and(|right_tokens| !left_tokens.is_disjoint(right_tokens))
            });
            if joined {
                let (a, b) = (union_root(&parent, left), union_root(&parent, right));
                if a != b {
                    let (keep, move_) = if a <= b { (a, b) } else { (b, a) };
                    parent.insert(move_, keep);
                }
            }
        }
    }
    let mut components: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &scc in sccs {
        components
            .entry(union_root(&parent, scc))
            .or_default()
            .push(scc);
    }
    let mut groups: Vec<Vec<u32>> = components.into_values().collect();
    for group in &mut groups {
        group.sort_unstable();
    }
    groups
}

/// Names one proposed place after its members: a token shared by EVERY file in
/// the group wins (`helpers/utils`); otherwise the two heaviest distinct
/// basename stems join (`helpers/charge-refund`) — the [`join_top_two`]
/// honesty, so a name covering two stems survives later absorption of a third
/// differently-named file without falling under half-aligned. The separator
/// between base and suffix is a slash — the label path-extends its base folder
/// at render time — while a joined stem pair stays dash-joined inside the last
/// segment. Collisions fall back to the numeric form, mirroring
/// [`relieve_over_capacity`]. Returns the label inserted into `used`.
pub(in crate::analyze) fn rebuild_label(
    base_name: &str,
    group: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
    used: &mut BTreeSet<SmolStr>,
    ordinal: u64,
) -> SmolStr {
    // per-file token sets, for the everyone-shares-it intersection.
    let per_file: Vec<BTreeSet<String>> = group
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .map(|name| basename_tokens(&name))
        .collect();
    let common: Option<String> = per_file
        .first()
        .map(|first| {
            per_file.iter().skip(1).fold(first.clone(), |held, set| {
                held.intersection(set).cloned().collect()
            })
        })
        // an all-digit token would mint a label indistinguishable from the
        // numeric fallback (`helpers/2024` vs `helpers/3`) and would never
        // align under the contract tokenizer, which drops digit tokens — skip
        // it and let the stem path or the fallback name the place.
        .and_then(|tokens| {
            tokens
                .into_iter()
                .find(|token| token.chars().any(char::is_alphabetic))
        });
    let candidate = if let Some(token) = common {
        format!("{base_name}/{token}")
    } else {
        // heaviest distinct stems by production SLOC then stem order.
        #[derive(Default)]
        struct Tally {
            sloc: u64,
        }
        let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
        for &scc in group {
            let Some(members) = condensation.members.get(scc as usize) else {
                continue;
            };
            for member in members {
                let Some(file) = files.get(member.0 as usize) else {
                    continue;
                };
                let Some(stem) = basename_stem(&file.name) else {
                    continue;
                };
                tally.entry(stem).or_default().sloc += u64::from(file.production_sloc);
            }
        }
        let mut ranked: Vec<(String, u64)> =
            tally.into_iter().map(|(stem, t)| (stem, t.sloc)).collect();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let stems: Vec<String> = ranked.into_iter().map(|(stem, _)| stem).take(2).collect();
        if stems.is_empty() {
            // no alphabetic stem anywhere in the group: nothing honest to
            // name it after — the numeric form is the only fit left.
            format!("{base_name}/{ordinal}")
        } else {
            format!("{base_name}/{}", stems.join("-"))
        }
    };
    let label = if used.contains(candidate.as_str()) {
        format!("{base_name}/{ordinal}")
    } else {
        candidate
    };
    used.insert(SmolStr::from(label.clone()));
    SmolStr::from(label)
}

/// Names each distinct real location by its folder key, qualifying key ties by
/// real location so distinct places never share a name: a folder key unique
/// among the distinct locations stays bare; a key shared across packages
/// qualifies as `{folder} ({package})`; a key shared within one package
/// qualifies as `{folder} ({package} {domain})`. Qualifiers dot their path
/// separators so display folding never splits a qualifier into path segments.
/// Distinct locations always differ in some coordinate, so the tiered names
/// are injective for every laminar-derived snapshot; only a hand-built folder
/// name that textually embeds another location's qualifier can still collide,
/// which the arena's numeric backstop absorbs.
pub(in crate::analyze) fn qualify_folder_names(distinct: &BTreeSet<LaminarHome>) -> Vec<SmolStr> {
    let mut folder_count: BTreeMap<&SmolStr, u32> = BTreeMap::new();
    let mut pair_count: BTreeMap<(&SmolStr, &SmolStr), u32> = BTreeMap::new();
    for home in distinct {
        *folder_count.entry(&home.folder).or_default() += 1;
        *pair_count.entry((&home.folder, &home.package)).or_default() += 1;
    }
    let dotted = |key: &SmolStr| key.replace('/', ".");
    distinct
        .iter()
        .map(|home| {
            let folder_ties = folder_count.get(&home.folder).copied().unwrap_or(0);
            let pair_ties = pair_count
                .get(&(&home.folder, &home.package))
                .copied()
                .unwrap_or(0);
            if folder_ties == 1 {
                home.folder.clone()
            } else if pair_ties == 1 {
                SmolStr::new(format!("{} ({})", home.folder, dotted(&home.package)))
            } else {
                SmolStr::new(format!(
                    "{} ({} {})",
                    home.folder,
                    dotted(&home.package),
                    dotted(&home.domain)
                ))
            }
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
    let cap = match level {
        SeedLevel::Folder => caps.folder,
        SeedLevel::Domain => caps.domain,
        SeedLevel::Package => caps.package,
        SeedLevel::PackageGroup => caps.package_group,
    };
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

/// Interns candidate containers with dense ids, keeping the sibling names taken
/// under each `(parent, level)` scope as a last-resort collision guard.
#[derive(Default)]
pub(in crate::analyze) struct ContainerArena {
    /// The containers interned so far, indexed by their dense id.
    pub(in crate::analyze) containers: Vec<Container>,
    /// The sibling names already taken under each `(parent, level)` scope.
    used: BTreeMap<(Option<u32>, ScopeLevel), BTreeSet<SmolStr>>,
}

impl ContainerArena {
    /// Interns `spec` under the next dense id and returns it.
    ///
    /// Every reachable naming path is injective by construction — folder names
    /// through `qualify_folder_names`, elected group/package/domain names
    /// through `qualify_elected` — so the numeric suffix below is a backstop
    /// for names that textually embed another sibling's qualifier: a real
    /// directory literally named like `http (ai)` colliding with a qualified
    /// twin (non-adversarial), or a hand-built snapshot reusing a qualified
    /// shape outright (adversarial). Neither arises from real elected paths.
    pub(in crate::analyze) fn push(&mut self, spec: ContainerSpec<'_>) -> ContainerId {
        let ContainerSpec {
            name,
            level,
            parent,
            synthetic,
        } = spec;
        let siblings = self
            .used
            .entry((parent.map(|parent| parent.0), level))
            .or_default();
        let mut unique = name.clone();
        let mut suffix = 2_u32;
        while siblings.contains(&unique) {
            unique = SmolStr::new(format!("{name}-{suffix}"));
            suffix = suffix.saturating_add(1);
        }
        siblings.insert(unique.clone());
        let id = ContainerId(u32::try_from(self.containers.len()).unwrap_or(u32::MAX));
        self.containers.push(Container {
            id,
            name: unique,
            level,
            parent,
            synthetic,
        });
        id
    }
}

/// Per-cluster directory election: each key holds its accumulated
/// (production SLOC, file count) vote.
pub(in crate::analyze) type NameTally = BTreeMap<u32, BTreeMap<SmolStr, (u64, u32)>>;

/// Adds one file's vote for `key` — production SLOC weighs first, file count
/// second, so test-only files cannot outvote the production home directory.
pub(in crate::analyze) fn vote(
    tally: &mut NameTally,
    cluster: u32,
    key: SmolStr,
    production_sloc: u32,
) {
    let (sloc, count) = tally.entry(cluster).or_default().entry(key).or_default();
    *sloc = sloc.saturating_add(u64::from(production_sloc));
    *count = count.saturating_add(1);
}

/// Returns the heaviest-weighted key in `tally`, ties broken by the
/// lexicographically smallest key so container naming is deterministic across
/// runs.
pub(in crate::analyze) fn plurality<V: Ord>(tally: &BTreeMap<SmolStr, V>) -> SmolStr {
    tally
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
        .map_or_else(|| SmolStr::new("workspace"), |(key, _)| key.clone())
}

/// True when a name is fit to serve as an elected identity: non-empty, not
/// all-digit at every `/`-segment (`2024`, `2024/2025`), and not tailed by a
/// `-<digits>` marker (`report-2`) — the shapes reserved for real directory
/// names and the arena's collision backstop, which a *suggested* container
/// name must never imitate.
pub(in crate::analyze) fn fit_for_election(name: &str) -> bool {
    let numeric = name
        .split('/')
        .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()));
    let suffixed = name
        .rsplit_once('-')
        .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()));
    !name.is_empty() && !numeric && !suffixed
}

/// Returns the plurality home when it holds a *strict* majority of the
/// cluster's weight and is fit to elect. Weight is production SLOC (as
/// `plurality` ranks), degrading to file count only for an all-test,
/// zero-SLOC cluster — a production home keeps its name despite companion
/// specs. Cross-multiplication keeps the test integer-exact, and demanding
/// `2 × win > total` sends a tied pair to the ladder's lower rungs instead
/// of crowning one side.
pub(in crate::analyze) fn strict_majority(
    tally: &BTreeMap<SmolStr, (u64, u32)>,
) -> Option<SmolStr> {
    let winner = plurality(tally);
    let (win_sloc, win_count) = tally.get(&winner).copied()?;
    let total_sloc = tally
        .values()
        .fold(0_u64, |total, &(sloc, _)| total.saturating_add(sloc));
    let majority = if total_sloc > 0 {
        win_sloc.saturating_mul(2) > total_sloc
    } else {
        let total_count = tally.values().fold(0_u64, |total, &(_, count)| {
            total.saturating_add(u64::from(count))
        });
        u64::from(win_count).saturating_mul(2) > total_count
    };
    (majority && fit_for_election(&winner)).then_some(winner)
}

/// Returns the longest `/`-segment prefix shared by every key in `tally` —
/// empty when the keys already diverge at their first segment.
pub(in crate::analyze) fn shared_prefix(tally: &BTreeMap<SmolStr, (u64, u32)>) -> SmolStr {
    let mut keys = tally.keys();
    let Some(first) = keys.next() else {
        return SmolStr::new("");
    };
    let mut prefix: Vec<&str> = first.split('/').collect();
    for key in keys {
        let shared = prefix
            .iter()
            .zip(key.split('/'))
            .take_while(|(held, segment)| **held == *segment)
            .count();
        prefix.truncate(shared);
    }
    SmolStr::new(prefix.join("/"))
}

/// Joins the cluster's two heaviest homes with `/` — ranked by production
/// SLOC then file count, the same vote order every other rung uses, ties by
/// key order — when at least two homes exist and the composite is fit to
/// elect. A balanced grab-bag with no shared prefix is honestly named after
/// both of its real origins, and a test-only spec dump outnumbering the
/// production homes in files can never lead the joined name.
///
/// Package-qualified homes share their leading segments; the second home is
/// relativized against the first before joining, so `ai/adapters` +
/// `ai/model` composes `ai/adapters/model`. A joined name is synthetic by
/// design — a proposed container not existing yet is the point — but it must
/// stay coherent: `ai/adapters/ai/model` re-embeds the shared root mid-path,
/// a nesting no human would ever write, and bakes in the very segment
/// repetition the display fold exists to prevent. This rung fires only when a
/// divergent third home has already emptied the all-member [`shared_prefix`]
/// consensus, so the pair's common ancestor alone would misclaim the cluster;
/// naming both heavy homes stays the more honest cover. When one home is the
/// other's ancestor, though, that ancestor covers both and elects alone.
pub(in crate::analyze) fn join_top_two(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut ranked: Vec<(&SmolStr, (u64, u32))> =
        tally.iter().map(|(key, &weight)| (key, weight)).collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let [(first, _), (second, _), ..] = ranked.as_slice() else {
        return None;
    };
    let shared = first
        .split('/')
        .zip(second.split('/'))
        .take_while(|(left, right)| left == right)
        .count();
    let remainder = second.split('/').skip(shared).collect::<Vec<_>>().join("/");
    let joined = if remainder.is_empty() {
        // `second` is an ancestor of `first`: the ancestor covers both homes.
        (*second).clone()
    } else if shared == first.split('/').count() {
        // `first` is an ancestor of `second`: same cover, other direction.
        (*first).clone()
    } else {
        SmolStr::new(format!("{first}/{remainder}"))
    };
    fit_for_election(&joined).then_some(joined)
}

/// Returns the heaviest non-numeric path token across the cluster's home
/// keys — weight accumulated as (production SLOC, file count), ties by the
/// lexicographically smaller token — skipping tokens unfit to elect. Rescues
/// a name when whole keys are numeric (`2024/2025`) but a real word survives
/// inside them.
pub(in crate::analyze) fn dominant_token(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut tokens: BTreeMap<&str, (u64, u32)> = BTreeMap::new();
    for (key, &(sloc, count)) in tally {
        for segment in key.split('/') {
            if segment.is_empty() || segment.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let (token_sloc, token_count) = tokens.entry(segment).or_default();
            *token_sloc = token_sloc.saturating_add(sloc);
            *token_count = token_count.saturating_add(count);
        }
    }
    let mut ranked: Vec<(&str, (u64, u32))> = tokens.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .map(|(token, _)| token)
        .find(|token| fit_for_election(token))
        .map(SmolStr::new)
}

/// Elects a cluster's directory name through a deterministic ladder that is
/// structurally incapable of yielding a synthetic label or a bare number —
/// the first fit rung wins:
///
/// 1. the strict-majority home ([`strict_majority`]);
/// 2. the longest home prefix every member shares ([`shared_prefix`]);
/// 3. the two heaviest homes joined ([`join_top_two`]);
/// 4. the dominant non-numeric path token ([`dominant_token`]);
/// 5. the first home key — wrapped with the cluster's dot-encoded `anchor`
///    folder when the key alone is unfit (`2024 (2024.x)`), so even an
///    all-numeric grab-bag renders as a real, non-numeric identity.
pub(in crate::analyze) fn elect(
    tally: &BTreeMap<SmolStr, (u64, u32)>,
    anchor: &SmolStr,
) -> SmolStr {
    if let Some(winner) = strict_majority(tally) {
        return winner;
    }
    let prefix = shared_prefix(tally);
    if !prefix.is_empty() && fit_for_election(&prefix) {
        return prefix;
    }
    if let Some(joined) = join_top_two(tally) {
        return joined;
    }
    if let Some(token) = dominant_token(tally) {
        return token;
    }
    let first = tally
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| SmolStr::new("workspace"));
    if fit_for_election(&first) {
        return first;
    }
    SmolStr::new(format!("{first} ({})", anchor.replace('/', ".")))
}

/// Keeps the lexicographically smallest anchor folder name seen per cluster.
pub(in crate::analyze) fn anchor_min(
    anchors: &mut BTreeMap<u32, SmolStr>,
    cluster: u32,
    name: &SmolStr,
) {
    anchors
        .entry(cluster)
        .and_modify(|held| {
            if *name < *held {
                *held = name.clone();
            }
        })
        .or_insert_with(|| name.clone());
}

/// Qualifies elected sibling names into injective ones: a raw name shared by
/// two clusters under one parent gains each cluster's dot-encoded anchor
/// folder — `app (pa.app.x)` — the same real-location style folder twins use,
/// so no reachable elected path ever needs the arena's numeric backstop.
/// Records `id`'s undecorated elected key when `qualify_elected` decorated its
/// display `name`, so the render boundary can strip a folder's increment against
/// the real key rather than the anchor-decorated label. An undecorated name (the
/// common, no-collision case) already matches the folder-key prefix, so it is
/// left out — keeping the map empty and the render byte-identical to before.
pub(in crate::analyze) fn record_undecorated_key(
    key_by_id: &mut BTreeMap<u32, SmolStr>,
    id: ContainerId,
    raw: &SmolStr,
    display: &SmolStr,
) {
    if raw != display {
        key_by_id.insert(id.0, raw.clone());
    }
}

pub(in crate::analyze) fn qualify_elected(
    raw: &BTreeMap<u32, (u32, SmolStr)>,
    anchors: &BTreeMap<u32, SmolStr>,
) -> BTreeMap<u32, SmolStr> {
    let mut sibling_count: BTreeMap<(u32, &SmolStr), u32> = BTreeMap::new();
    for (parent, name) in raw.values() {
        *sibling_count.entry((*parent, name)).or_default() += 1;
    }
    raw.iter()
        .map(|(&cluster, (parent, name))| {
            let colliding = sibling_count.get(&(*parent, name)).copied().unwrap_or(0) > 1;
            let name = if colliding {
                let anchor = anchors
                    .get(&cluster)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                SmolStr::new(format!("{name} ({})", anchor.replace('/', ".")))
            } else {
                name.clone()
            };
            (cluster, name)
        })
        .collect()
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
