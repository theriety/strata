//! Connectivity piles over file SCCs and the tokens that name their split halves.

use std::collections::{BTreeMap, BTreeSet};

use strata_core::condense::Condensation;
use strata_core::graph::csr::Csr;

use crate::analyze::relocation::FileInfo;

/// Counts the files an SCC holds.
pub(super) fn scc_file_count(condensation: &Condensation, scc: u32) -> u32 {
    condensation.members.get(scc as usize).map_or(0, |members| {
        u32::try_from(members.len()).unwrap_or(u32::MAX)
    })
}

/// Follows `parent` links from `start` to its union-find root.
pub(super) fn union_root(parent: &BTreeMap<u32, u32>, start: u32) -> u32 {
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
pub(super) fn connected_piles(
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
pub(super) fn elect_split_token(
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
pub(super) fn basename_stem(name: &str) -> Option<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split_once('.').map_or(basename, |(stem, _)| stem);
    let run: String = stem
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_lowercase();
    if run.is_empty() { None } else { Some(run) }
}
