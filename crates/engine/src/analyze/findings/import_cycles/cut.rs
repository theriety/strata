//! The minimum feedback arc set of one import-graph component, deterministic
//! by construction: integer weights, no time budget, and every tie broken on
//! the files' path order.
//!
//! A component of up to [`EXACT_LIMIT`] files is solved exactly by dynamic
//! programming over file subsets: a cut is the set of backward edges of some
//! linear order, and the cheapest order is built one file at a time. Ties keep
//! the cut with fewer imports, then the one whose `(importer, imported)` list
//! sorts first. Larger components take a greedy Eades–Lin–Smyth order whose
//! choices also fall to the lowest index, then restore every cut edge that no
//! longer closes a cycle. The order costs O((V+E)·log V) and the restore pass
//! O(c·(V+E)) for a cut of c imports, both on adjacency lists.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

/// The largest component solved exactly: 2^12 subsets.
pub(super) const EXACT_LIMIT: usize = 12;

/// The imports to remove, as local `(importer, imported)` indices ascending,
/// and whether the set is proven minimum.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Cut {
    /// The edges to remove, ascending.
    pub(super) edges: Vec<(usize, usize)>,
    /// `true` for the exact solve.
    pub(super) exact: bool,
}

/// Solves the component of `count` files whose edges carry `weights`.
pub(super) fn solve(count: usize, weights: &BTreeMap<(usize, usize), u64>) -> Cut {
    if count <= EXACT_LIMIT {
        Cut {
            edges: exact(count, weights),
            exact: true,
        }
    } else {
        Cut {
            edges: greedy(count, weights),
            exact: false,
        }
    }
}

/// One subset's cheapest prefix order, by its backward edges.
#[derive(Clone, Default)]
struct Prefix {
    cost: u64,
    cut: Vec<(usize, usize)>,
}

impl Prefix {
    /// The ordering key: cost, then import count, then the sorted edge list.
    /// Cost and count are additive, so the exact cut is optimal on both; the
    /// list only makes the choice deterministic, compared per subset as the
    /// cut grows, and is not a global lexicographic minimum.
    fn key(&self) -> (u64, usize, &[(usize, usize)]) {
        (self.cost, self.cut.len(), self.cut.as_slice())
    }
}

/// The exact cut: the cheapest backward-edge set over every linear order.
pub(super) fn exact(count: usize, weights: &BTreeMap<(usize, usize), u64>) -> Vec<(usize, usize)> {
    let full = (1_usize << count) - 1;
    let mut best: BTreeMap<usize, Prefix> = BTreeMap::new();
    best.insert(0, Prefix::default());
    for placed in 0..full {
        let Some(prefix) = best.get(&placed).cloned() else {
            continue;
        };
        for next in (0..count).filter(|next| placed & (1 << next) == 0) {
            // `next` sits after every placed file, so its edges into them run
            // backward and must be cut.
            let mut candidate = prefix.clone();
            for earlier in (0..count).filter(|earlier| placed & (1 << earlier) != 0) {
                if let Some(weight) = weights.get(&(next, earlier)) {
                    candidate.cost += weight;
                    candidate.cut.push((next, earlier));
                }
            }
            candidate.cut.sort_unstable();
            let grown = placed | (1 << next);
            if best
                .get(&grown)
                .is_none_or(|current| candidate.key() < current.key())
            {
                best.insert(grown, candidate);
            }
        }
    }
    best.remove(&full)
        .map(|prefix| prefix.cut)
        .unwrap_or_default()
}

/// The component's edges as adjacency lists over edge indices.
///
/// Edges are numbered in ascending `(importer, imported)` order, so each
/// list is ascending by neighbour and every walk visits files in path order.
/// Self-edges are left out: they never order two files.
struct Adjacency {
    /// Each edge as `(importer, imported, weight)`, ascending.
    edges: Vec<(usize, usize, u64)>,
    /// The edges leaving each file.
    outgoing: Vec<Vec<usize>>,
    /// The edges entering each file.
    incoming: Vec<Vec<usize>>,
}

impl Adjacency {
    fn new(count: usize, weights: &BTreeMap<(usize, usize), u64>) -> Self {
        let edges: Vec<(usize, usize, u64)> = weights
            .iter()
            .filter(|((importer, imported), _)| {
                importer != imported && *importer < count && *imported < count
            })
            .map(|(&(importer, imported), &weight)| (importer, imported, weight))
            .collect();
        let mut outgoing = vec![Vec::new(); count];
        let mut incoming = vec![Vec::new(); count];
        for (index, &(importer, imported, _)) in edges.iter().enumerate() {
            if let Some(list) = outgoing.get_mut(importer) {
                list.push(index);
            }
            if let Some(list) = incoming.get_mut(imported) {
                list.push(index);
            }
        }
        Self {
            edges,
            outgoing,
            incoming,
        }
    }
}

/// The heuristic cut: the backward edges of a greedy order, then every cut
/// edge restored that closes no cycle, heaviest first.
///
/// The kept edges stay acyclic throughout: they start as the forward edges of
/// a linear order, and an edge is restored only when it closes no cycle. So
/// restoring `importer -> imported` is safe exactly when `imported` does not
/// already reach `importer`, which one depth-first walk answers. The pass
/// costs O(c·(V+E)) for a cut of c edges.
pub(super) fn greedy(count: usize, weights: &BTreeMap<(usize, usize), u64>) -> Vec<(usize, usize)> {
    let graph = Adjacency::new(count, weights);
    let mut position = vec![0_usize; count];
    for (rank, file) in greedy_order(&graph).into_iter().enumerate() {
        if let Some(slot) = position.get_mut(file) {
            *slot = rank;
        }
    }
    let rank = |file: usize| position.get(file).copied().unwrap_or_default();
    let mut in_cut: Vec<bool> = graph
        .edges
        .iter()
        .map(|&(importer, imported, _)| rank(importer) > rank(imported))
        .collect();
    let mut restore: Vec<usize> = (0..graph.edges.len())
        .filter(|&edge| in_cut.get(edge).copied().unwrap_or_default())
        .collect();
    // heaviest first, then the edge order, which is (importer, imported).
    restore.sort_by_key(|&edge| {
        (
            Reverse(graph.edges.get(edge).map(|&(_, _, weight)| weight)),
            edge,
        )
    });
    let mut walk = Walk::new(count);
    for edge in restore {
        let Some(&(importer, imported, _)) = graph.edges.get(edge) else {
            continue;
        };
        if !walk.reaches(&graph, &in_cut, imported, importer)
            && let Some(flag) = in_cut.get_mut(edge)
        {
            *flag = false;
        }
    }
    graph
        .edges
        .iter()
        .zip(&in_cut)
        .filter(|(_, cut)| **cut)
        .map(|(&(importer, imported, _), _)| (importer, imported))
        .collect()
}

/// A reusable depth-first walk over the kept edges. Each walk stamps the
/// files it visits, so no walk clears the previous one's marks.
struct Walk {
    /// The walk that last visited each file.
    visited: Vec<u32>,
    /// The current walk's stamp.
    stamp: u32,
    /// The files still to expand.
    stack: Vec<usize>,
}

impl Walk {
    fn new(count: usize) -> Self {
        Self {
            visited: vec![0; count],
            stamp: 0,
            stack: Vec::new(),
        }
    }

    /// Whether `from` reaches `to` over the edges not in the cut.
    fn reaches(&mut self, graph: &Adjacency, in_cut: &[bool], from: usize, to: usize) -> bool {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.visited.fill(0);
            self.stamp = 1;
        }
        self.stack.clear();
        self.stack.push(from);
        if let Some(mark) = self.visited.get_mut(from) {
            *mark = self.stamp;
        }
        while let Some(file) = self.stack.pop() {
            if file == to {
                return true;
            }
            for &edge in graph.outgoing.get(file).into_iter().flatten() {
                if in_cut.get(edge).copied().unwrap_or(true) {
                    continue;
                }
                let Some(&(_, next, _)) = graph.edges.get(edge) else {
                    continue;
                };
                if let Some(mark) = self.visited.get_mut(next)
                    && *mark != self.stamp
                {
                    *mark = self.stamp;
                    self.stack.push(next);
                }
            }
        }
        false
    }
}

/// The files not yet placed, bucketed for an Eades–Lin–Smyth order.
///
/// Every remaining file sits in one bucket keyed by its outgoing-minus-incoming
/// weight within the remaining files; a file sending no weight is also a sink,
/// and one receiving none a source. Each bucket is an ordered set, so every
/// choice falls to the lowest index in O(log V).
struct Buckets {
    outgoing: Vec<u64>,
    incoming: Vec<u64>,
    placed: Vec<bool>,
    by_delta: BTreeMap<i128, BTreeSet<usize>>,
    sinks: BTreeSet<usize>,
    sources: BTreeSet<usize>,
}

impl Buckets {
    fn new(graph: &Adjacency) -> Self {
        let count = graph.outgoing.len();
        let mut outgoing = vec![0_u64; count];
        let mut incoming = vec![0_u64; count];
        for &(importer, imported, weight) in &graph.edges {
            if let Some(sent) = outgoing.get_mut(importer) {
                *sent = sent.saturating_add(weight);
            }
            if let Some(received) = incoming.get_mut(imported) {
                *received = received.saturating_add(weight);
            }
        }
        let mut buckets = Self {
            outgoing,
            incoming,
            placed: vec![false; count],
            by_delta: BTreeMap::new(),
            sinks: BTreeSet::new(),
            sources: BTreeSet::new(),
        };
        for file in 0..count {
            buckets.file_in(file);
        }
        buckets
    }

    fn delta(&self, file: usize) -> i128 {
        let sent = self.outgoing.get(file).copied().unwrap_or_default();
        let received = self.incoming.get(file).copied().unwrap_or_default();
        i128::from(sent) - i128::from(received)
    }

    /// Files `file` under its current weights.
    fn file_in(&mut self, file: usize) {
        self.by_delta
            .entry(self.delta(file))
            .or_default()
            .insert(file);
        if self.outgoing.get(file).copied().unwrap_or_default() == 0 {
            self.sinks.insert(file);
        }
        if self.incoming.get(file).copied().unwrap_or_default() == 0 {
            self.sources.insert(file);
        }
    }

    /// Takes `file` out of every bucket it sits in.
    fn file_out(&mut self, file: usize) {
        let delta = self.delta(file);
        if let Some(bucket) = self.by_delta.get_mut(&delta) {
            bucket.remove(&file);
            if bucket.is_empty() {
                self.by_delta.remove(&delta);
            }
        }
        self.sinks.remove(&file);
        self.sources.remove(&file);
    }

    /// The next file: the lowest sink, else the lowest source, else the
    /// lowest file of the largest outgoing-minus-incoming weight. `true`
    /// marks a sink, which goes to the back of the order.
    fn next(&self) -> Option<(usize, bool)> {
        if let Some(&sink) = self.sinks.first() {
            return Some((sink, true));
        }
        if let Some(&source) = self.sources.first() {
            return Some((source, false));
        }
        let (_, bucket) = self.by_delta.last_key_value()?;
        bucket.first().map(|&file| (file, false))
    }

    /// Places `file`, withdrawing its weight from its remaining neighbours.
    fn place(&mut self, graph: &Adjacency, file: usize) {
        self.file_out(file);
        if let Some(flag) = self.placed.get_mut(file) {
            *flag = true;
        }
        for &edge in graph.outgoing.get(file).into_iter().flatten() {
            if let Some(&(_, imported, weight)) = graph.edges.get(edge) {
                self.withdraw(imported, weight, false);
            }
        }
        for &edge in graph.incoming.get(file).into_iter().flatten() {
            if let Some(&(importer, _, weight)) = graph.edges.get(edge) {
                self.withdraw(importer, weight, true);
            }
        }
    }

    /// Removes `weight` from what an unplaced `file` sends or receives.
    fn withdraw(&mut self, file: usize, weight: u64, sent: bool) {
        if self.placed.get(file).copied().unwrap_or(true) {
            return;
        }
        self.file_out(file);
        let side = if sent {
            self.outgoing.get_mut(file)
        } else {
            self.incoming.get_mut(file)
        };
        if let Some(total) = side {
            *total = total.saturating_sub(weight);
        }
        self.file_in(file);
    }
}

/// An Eades–Lin–Smyth order: sinks to the back, sources to the front, and
/// otherwise the file with the largest outgoing-minus-incoming weight next,
/// each choice falling to the lowest index. Each placement updates only the
/// placed file's neighbours, so the order costs O((V+E)·log V).
fn greedy_order(graph: &Adjacency) -> Vec<usize> {
    let mut buckets = Buckets::new(graph);
    let mut front: Vec<usize> = Vec::new();
    let mut back: Vec<usize> = Vec::new();
    while let Some((file, sink)) = buckets.next() {
        buckets.place(graph, file);
        if sink {
            back.push(file);
        } else {
            front.push(file);
        }
    }
    front.extend(back.into_iter().rev());
    front
}
