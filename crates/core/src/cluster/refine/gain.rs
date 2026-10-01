//! Cohesion-aware gain model for refinement moves.
//!
//! Holds the per-node naming and path token sets with the alpha and beta mixing
//! coefficients, and scores a node's cohesion against a cluster by mean Jaccard
//! similarity.

use crate::cluster::{ClusterId, Partition};

/// The cohesion-aware gain model: per-node token sets plus the α / β mixing
/// coefficients that select anchored vs greenfield behaviour.
///
/// `naming_tokens[v]` are the case/underscore-split tokens of node `v`'s symbol
/// name; `path_tokens[v]` are the tokens of its current container path. Naming
/// cohesion (weighted by `alpha`) is always on; path cohesion (weighted by
/// `beta`) is greenfield-disabled by passing `beta = 0.0`. Cohesion between a
/// node and a cluster is the mean Jaccard similarity of the node's tokens
/// against the cluster's members' tokens.
#[derive(Debug, Clone)]
pub struct GainFn {
    /// Naming-token sets per node, each sorted and deduplicated.
    naming_tokens: Vec<Vec<u32>>,
    /// Path-token sets per node, each sorted and deduplicated.
    path_tokens: Vec<Vec<u32>>,
    /// Weight on naming-token cohesion (α).
    alpha: f32,
    /// Weight on path cohesion (β); zero in greenfield mode.
    beta: f32,
}

impl GainFn {
    /// Builds a gain model from interned per-node token sets and the mixing
    /// coefficients. Token vectors are sorted and deduplicated defensively so
    /// the Jaccard computation can assume set semantics.
    #[must_use]
    pub fn new(
        naming_tokens: Vec<Vec<u32>>,
        path_tokens: Vec<Vec<u32>>,
        alpha: f32,
        beta: f32,
    ) -> Self {
        let normalise = |mut sets: Vec<Vec<u32>>| {
            for set in &mut sets {
                set.sort_unstable();
                set.dedup();
            }
            sets
        };
        Self {
            naming_tokens: normalise(naming_tokens),
            path_tokens: normalise(path_tokens),
            alpha,
            beta,
        }
    }

    /// A cohesion-free gain model: gain reduces to the cut-weight delta alone.
    /// Useful for tests and for runs where cohesion is disabled.
    #[must_use]
    pub fn cut_only(node_count: usize) -> Self {
        Self {
            naming_tokens: vec![Vec::new(); node_count],
            path_tokens: vec![Vec::new(); node_count],
            alpha: 0.0,
            beta: 0.0,
        }
    }

    /// The cohesion bonus a node gains by being a member of `cluster`:
    /// `α · naming + β · path`.
    pub(super) fn bonus(&self, node: u32, cluster: ClusterId, parts: &Partition) -> f32 {
        let naming = cohesion(&self.naming_tokens, node, cluster, parts);
        let path = cohesion(&self.path_tokens, node, cluster, parts);
        self.alpha * naming + self.beta * path
    }
}

/// Mean Jaccard cohesion of `node`'s `tokens` against the members of `cluster`
/// (excluding `node` itself). Returns `0.0` for an empty cluster.
fn cohesion(tokens: &[Vec<u32>], node: u32, cluster: ClusterId, parts: &Partition) -> f32 {
    let Some(node_set) = tokens.get(node as usize) else {
        return 0.0;
    };
    let mut total = 0.0_f32;
    let mut peers = 0_u32;
    for (other, owner) in parts.assignment().iter().enumerate() {
        if *owner != cluster {
            continue;
        }
        let other_u32 = u32::try_from(other).unwrap_or(u32::MAX);
        if other_u32 == node {
            continue;
        }
        if let Some(other_set) = tokens.get(other) {
            total += jaccard(node_set, other_set);
            peers += 1;
        }
    }
    if peers == 0 {
        return 0.0;
    }
    // `peers` counts cluster members (bounded by the level cap, ≤ 15), well
    // within f32's exact-integer range; the precision lint is allowed for this
    // single averaging conversion.
    #[allow(clippy::cast_precision_loss)]
    let mean = total / peers as f32;
    mean
}

/// Jaccard similarity of two sorted, deduplicated token sets: `|A ∩ B| / |A ∪ B|`.
pub(super) fn jaccard(a: &[u32], b: &[u32]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut left = a.iter().copied().peekable();
    let mut right = b.iter().copied().peekable();
    let mut intersection = 0_usize;
    while let (Some(&x), Some(&y)) = (left.peek(), right.peek()) {
        match x.cmp(&y) {
            std::cmp::Ordering::Less => {
                left.next();
            }
            std::cmp::Ordering::Greater => {
                right.next();
            }
            std::cmp::Ordering::Equal => {
                intersection += 1;
                left.next();
                right.next();
            }
        }
    }
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    // Token-set cardinalities are tiny (per-symbol name fragments), so the
    // ratio of two small counts is exactly representable; the precision lint is
    // allowed here for the single unavoidable count-to-float conversion.
    #[allow(clippy::cast_precision_loss)]
    let ratio = intersection as f32 / union as f32;
    ratio
}
