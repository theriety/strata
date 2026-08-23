//! The objective function J(T) and its per-candidate breakdown.
//!
//! One score must rank wildly different laminar trees, and the same solver must
//! serve both incremental (anchored) and greenfield restructuring. Strata's
//! objective is the single scalar
//!
//! ```text
//! J(T) = meancross(T)                  // normalized cut, in 0..1 (`cut_cost`)
//!      + lambda * imbalance(T)         // sibling size imbalance
//!      - alpha  * naming(T)            // symbol-token cohesion
//!      - beta   * path(T)              // current-path cohesion (anchored)
//!      + mu     * d(T, T0)             // move distance from the current tree
//!      + gamma  * cap(T)               // scoped over-capacity binding
//! ```
//!
//! minimised over candidate trees. Every term is a dimensionless ratio on a
//! comparable scale — cut is a weighted mean crossing height over its own
//! edge-weight mass, imbalance sums squared CVs, naming/path/anchor are shares
//! of the token/layout populations, and capacity sums over-cap shares against
//! the folder budget — so the coefficients act as relative weights rather than
//! unit conversions between incommensurable units. Without that normalization
//! the raw cut sum grows with repository size while every other term stays
//! bounded, and the bounded terms drown (D-37).
//!
//! The two modes differ only in coefficients: greenfield zeroes `mu` and `beta`
//! so the current layout cannot leak back in through path similarity, while
//! anchored keeps both positive so candidates stay reachable from today's
//! structure. There is no mode-specific code path — a mode is purely a
//! [`Coefficients`] preset.
//!
//! Cohesion terms (`naming`, `path`) enter with a minus sign because more
//! cohesion is *better*, i.e. lowers the objective. Every term is surfaced
//! separately in the [`ScoreBreakdown`] so a reader sees *why* a tree ranks where
//! it does. Acyclicity and polarity stay hard vetoes enforced upstream, never
//! penalties. Capacity is priced here (FIX03): a layout that binds more files
//! than the folder budget at any scoped container pays `gamma` per over-cap
//! share, so a relieving proposal can outsore the current tree on J itself and
//! the "improvement ≥ 0" invariant survives exposing relief that moves far. The
//! move-level veto (a folder never accepts more than the cap) still holds in the
//! solver; the term prices whole-layout binding, it does not trade against the
//! veto.

use std::collections::BTreeSet;

use strata_ir::{EdgeKind, ScopeLevel};

/// The coefficients of the objective, fixed per mode.
///
/// `lambda`, `alpha`, and `beta` weight the imbalance, naming, and path terms;
/// `mu` weights the anchoring (move-distance) term; `gamma` weights the scoped
/// over-capacity binding term (FIX03). A greenfield preset sets `mu` and `beta`
/// to zero; an anchored preset keeps them positive. Capacity binds in both
/// modes: relief is owed regardless of how far from today's layout it sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    /// Weight of the sibling size-imbalance penalty.
    pub lambda: f64,
    /// Weight of the symbol-token naming-cohesion bonus.
    pub alpha: f64,
    /// Weight of the current-path cohesion bonus (anchored mode only).
    pub beta: f64,
    /// Weight of the move-distance anchoring penalty (anchored mode only).
    pub mu: f64,
    /// Weight of the scoped over-capacity binding penalty (both modes).
    pub gamma: f64,
}

impl Coefficients {
    /// The anchored preset: every term active, so candidates stay close to the
    /// current tree (`mu > 0`) and reward keeping today's directory groupings
    /// (`beta > 0`).
    #[must_use]
    pub const fn anchored() -> Self {
        Self {
            lambda: 1.0,
            alpha: 1.0,
            beta: 1.0,
            mu: 1.0,
            gamma: 4.0,
        }
    }

    /// The greenfield preset: an unbiased ideal. `mu = 0` and `beta = 0` so the
    /// current layout cannot leak back in through anchoring or path similarity;
    /// symbol-token cohesion (`alpha`), imbalance (`lambda`), and capacity
    /// (`gamma`) remain.
    #[must_use]
    pub const fn greenfield() -> Self {
        Self {
            lambda: 1.0,
            alpha: 1.0,
            beta: 0.0,
            mu: 0.0,
            gamma: 4.0,
        }
    }
}

/// Per-kind base edge weights `w(e)` — the `[weights]` config surface.
///
/// The default table matches a config-less run; a custom table flows from
/// `strata.toml` into every consumer that prices an edge: the
/// CSR builder, the cut term, MFAS break weights, and narration pull weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KindWeights {
    /// Weight of a runtime value import.
    pub value_import: f64,
    /// Weight of a subtype / implements relationship.
    pub inheritance: f64,
    /// Weight of a direct call.
    pub call: f64,
    /// Weight of a type reference.
    pub type_reference: f64,
    /// Weight of a re-export (zero by default — flattened during normalization).
    pub re_export: f64,
}

impl Default for KindWeights {
    fn default() -> Self {
        Self {
            value_import: 1.0,
            inheritance: 1.5,
            call: 1.0,
            type_reference: 0.3,
            re_export: 0.0,
        }
    }
}

impl KindWeights {
    /// Returns the base weight `w(e)` for an edge of `kind`.
    #[must_use]
    pub fn weight_of(&self, kind: EdgeKind) -> f64 {
        match kind {
            EdgeKind::ValueImport => self.value_import,
            EdgeKind::Inheritance => self.inheritance,
            EdgeKind::Call => self.call,
            EdgeKind::TypeReference => self.type_reference,
            EdgeKind::ReExport => self.re_export,
        }
    }

    /// Returns the weight of a single edge: kind weight × binder confidence.
    #[must_use]
    pub fn edge_weight(&self, kind: EdgeKind, confidence: f64) -> f64 {
        self.weight_of(kind) * confidence
    }
}

/// A scored dependency edge of a candidate: its kind-weight, binder confidence,
/// and the level at which it crosses in the candidate tree.
///
/// `lca_level` is the level of the lowest common ancestor of the edge's endpoints
/// in the candidate tree — the height at which the dependency leaves a shared
/// container. An edge whose endpoints share a file crosses at [`ScopeLevel::File`]
/// (cheapest); one spanning two packages crosses at [`ScopeLevel::Package`] or
/// higher (most expensive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoredEdge {
    /// The dependency kind, which fixes the base weight `w(e)`.
    pub kind: EdgeKind,
    /// The binder confidence `c(e)` in `0.0..=1.0`.
    pub confidence: f64,
    /// The level of the endpoints' lowest common ancestor in the candidate tree.
    pub lca_level: ScopeLevel,
}

/// One container of a candidate, described by the production-SLOC subtree sizes
/// of its direct children — the input to the imbalance term.
///
/// A container with fewer than two children contributes no imbalance (there is
/// nothing to be imbalanced against), so only the multi-child containers need be
/// listed, though listing all is harmless.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSizes {
    /// The production-SLOC subtree size of each direct child container.
    pub child_sizes: Vec<u32>,
}

/// A group of symbols sharing one container (a file or folder), described by each
/// member's naming-token set — the input to the naming-cohesion term.
///
/// Tokens are the case- and underscore-split pieces of a symbol's name, lowered;
/// directory-path tokens are deliberately excluded so greenfield candidates stay
/// invariant under folder renames. The group's cohesion is the mean pairwise
/// Jaccard similarity of its members' token sets.
#[derive(Debug, Clone, PartialEq)]
pub struct CohesionGroup {
    /// The production SLOC of the group, which size-weights its cohesion.
    pub production_sloc: u32,
    /// Each member symbol's lower-cased naming-token set.
    pub members: Vec<BTreeSet<String>>,
}

/// A candidate laminar tree, reduced to exactly the views the objective scores.
///
/// Every field is a pre-extracted projection of the full tree: the scored edges
/// with their crossing levels, the per-container child sizes for imbalance, the
/// file/folder symbol groups for naming, the path-cohesion ratio for anchored
/// mode, and the move distance from the current tree. Mirroring [`pack::FolderView`],
/// scoring owns only the data it consumes, never the whole tree.
///
/// [`pack::FolderView`]: crate::pack::FolderView
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The dependency edges, each with its crossing level in this tree.
    pub edges: Vec<ScoredEdge>,
    /// Every container's direct-child subtree sizes, for the imbalance term.
    pub containers: Vec<ContainerSizes>,
    /// The file- and folder-level symbol groups, for the naming term.
    pub cohesion_groups: Vec<CohesionGroup>,
    /// Fraction of co-foldered symbol pairs that also shared a directory in T0,
    /// in `0.0..=1.0`; the path term (anchored mode only).
    pub path_cohesion: f64,
    /// Fraction of symbols whose container path differs from T0, in `0.0..=1.0`;
    /// the normalised move distance `d(T, T0)`.
    pub move_distance: f64,
    /// Sum over the candidate's scoped containers (folders and domains) of the
    /// share their transitively-bound file count exceeds the folder budget —
    /// `Σ max(0, bound − budget) / budget`. Zero when every scoped container
    /// binds within the budget; one full breach unit per container at twice the
    /// budget.
    pub capacity_pressure: f64,
}

/// Per-candidate score decomposition, reported in the DTO so users see *why* a
/// tree ranks where it does.
///
/// `total` is the sum of all six terms — the value of J(T) being minimised.
/// `naming` and `path` are already negated (their minus sign folded in), so the
/// breakdown sums to `total` directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreBreakdown {
    /// The normalized cut term in `0.0..=1.0`: the edge-weight-share-weighted
    /// mean crossing height over the candidate's edges (`cut_cost`).
    pub cut: f64,
    /// `lambda * imbalance(T)` — the sibling size-imbalance penalty.
    pub imbalance: f64,
    /// `-alpha * naming(T)` — the (negated) symbol-token cohesion bonus.
    pub naming: f64,
    /// `-beta * path(T)` — the (negated) current-path cohesion bonus.
    pub path: f64,
    /// `mu * d(T, T0)` — the move-distance anchoring penalty.
    pub anchor: f64,
    /// `gamma * cap(T)` — the scoped over-capacity binding penalty (FIX03).
    pub capacity: f64,
    /// The full objective `J(T)`, the sum of the six terms above.
    pub total: f64,
}

/// Scores a candidate tree under the given coefficients and per-kind edge
/// weights, returning the full breakdown of J(T).
///
/// Each term is computed independently from the candidate's pre-extracted views:
/// cut is the edge-weight-share-weighted mean crossing height normalized into
/// `0.0..=1.0`, imbalance sums the squared coefficient of variation of every
/// container's child sizes, naming and path are the (negated) cohesion bonuses,
/// anchor is `mu` times the move distance, and capacity is `gamma` times the
/// pre-extracted scoped binding pressure. Acyclicity and polarity stay hard
/// vetoes enforced upstream.
#[must_use]
pub fn score(
    candidate: &Candidate,
    coefficients: &Coefficients,
    weights: &KindWeights,
) -> ScoreBreakdown {
    let cut = cut_cost(&candidate.edges, weights);
    let imbalance = coefficients.lambda * imbalance(&candidate.containers);
    let naming = -coefficients.alpha * naming(&candidate.cohesion_groups);
    let path = -coefficients.beta * candidate.path_cohesion;
    let anchor = coefficients.mu * candidate.move_distance;
    let capacity = coefficients.gamma * candidate.capacity_pressure;

    ScoreBreakdown {
        cut,
        imbalance,
        naming,
        path,
        anchor,
        capacity,
        total: cut + imbalance + naming + path + anchor + capacity,
    }
}

/// Returns the height penalty `h(level)`: cutting inside a file is cheap, across a
/// package group expensive. The penalty doubles per level (file 1, folder 2,
/// domain 4, package 8, package group 16) and is monotone in LCA height, so a
/// cut that crosses higher in the tree always costs more.
fn height_penalty(level: ScopeLevel) -> f64 {
    match level {
        ScopeLevel::File => 1.0,
        ScopeLevel::Folder => 2.0,
        ScopeLevel::Domain => 4.0,
        ScopeLevel::Package => 8.0,
        ScopeLevel::PackageGroup => MAX_CROSSING_HEIGHT,
    }
}

/// The tallest crossing a laminar tree can price — `height_penalty` of the
/// package-group scope, and the denominator ceiling that keeps the normalized
/// cut term in `0.0..=1.0`.
const MAX_CROSSING_HEIGHT: f64 = 16.0;

/// Returns the normalized cut term: the edge-weight-share-weighted mean
/// crossing height of the candidate's edges, scaled by the maximum height
/// penalty into `0.0..=1.0`.
///
/// `sum_e w(e) c(e) h(lca_T(e)) / (MAX_CROSSING_HEIGHT * sum_e w(e) c(e))`,
/// pricing `w(e)` from the configured `[weights]` table. Dividing by the
/// candidate's own edge-weight mass is what makes the term dimensionless and
/// repository-size independent: the mass is invariant across the candidates of
/// one repository (the same edges change only where they cross), so intra-repo
/// ranking by cut alone is preserved while the term stops drowning every
/// bounded reality term as repositories grow (D-37). An edge-free candidate
/// has nothing to cross and scores zero.
fn cut_cost(edges: &[ScoredEdge], weights: &KindWeights) -> f64 {
    let mut weighted_height = 0.0;
    let mut total_weight = 0.0;
    for edge in edges {
        let weight = weights.edge_weight(edge.kind, edge.confidence);
        weighted_height += weight * height_penalty(edge.lca_level);
        total_weight += weight;
    }
    if total_weight == 0.0 {
        return 0.0;
    }
    weighted_height / (MAX_CROSSING_HEIGHT * total_weight)
}

/// Sums the squared coefficient of variation of each container's child subtree
/// sizes — higher when a container's children differ wildly in size.
///
/// A container with fewer than two children, or one whose children are all empty,
/// contributes nothing (its mean is zero or it has no spread to measure).
fn imbalance(containers: &[ContainerSizes]) -> f64 {
    containers
        .iter()
        .map(|container| squared_cv(&container.child_sizes))
        .sum()
}

/// Returns the squared coefficient of variation (variance / mean²) of `sizes`, or
/// zero when there are fewer than two values or the mean is zero.
fn squared_cv(sizes: &[u32]) -> f64 {
    let count = sizes.len();
    if count < 2 {
        return 0.0;
    }
    let count_f = f64::from(u32::try_from(count).unwrap_or(u32::MAX));
    let sum: f64 = sizes.iter().map(|&size| f64::from(size)).sum();
    let mean = sum / count_f;
    if mean == 0.0 {
        return 0.0;
    }
    let variance: f64 = sizes
        .iter()
        .map(|&size| {
            let delta = f64::from(size) - mean;
            delta * delta
        })
        .sum::<f64>()
        / count_f;
    variance / (mean * mean)
}

/// Returns the size-weighted mean of each group's naming cohesion — the average,
/// across all file/folder groups, of their mean pairwise Jaccard token
/// similarity, weighted by production SLOC.
///
/// A group with fewer than two members has no pairs and contributes no cohesion;
/// when every group's weight is zero the result is zero.
fn naming(groups: &[CohesionGroup]) -> f64 {
    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;
    for group in groups {
        let weight = f64::from(group.production_sloc);
        weighted_sum += weight * group_cohesion(&group.members);
        total_weight += weight;
    }
    if total_weight == 0.0 {
        0.0
    } else {
        weighted_sum / total_weight
    }
}

/// Returns the mean pairwise Jaccard similarity of a group's member token sets,
/// or zero when the group has fewer than two members.
fn group_cohesion(members: &[BTreeSet<String>]) -> f64 {
    let count = members.len();
    if count < 2 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut pairs = 0_u32;
    for left in 0..count {
        for right in (left + 1)..count {
            let (Some(a), Some(b)) = (members.get(left), members.get(right)) else {
                continue;
            };
            sum += jaccard(a, b);
            pairs = pairs.saturating_add(1);
        }
    }
    if pairs == 0 {
        0.0
    } else {
        sum / f64::from(pairs)
    }
}

/// Returns the Jaccard similarity of two token sets, or zero when both are empty.
fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        f64::from(u32::try_from(intersection).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(union).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a token set from string slices.
    fn tokens(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|&token| token.to_owned()).collect()
    }

    /// Returns whether two scores are equal within floating-point tolerance.
    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < f64::EPSILON
    }

    /// Builds an empty candidate that scores to all-zero under any coefficients.
    fn empty_candidate() -> Candidate {
        Candidate {
            edges: Vec::new(),
            containers: Vec::new(),
            cohesion_groups: Vec::new(),
            path_cohesion: 0.0,
            move_distance: 0.0,
            capacity_pressure: 0.0,
        }
    }

    #[test]
    fn should_weight_a_cut_by_kind_confidence_and_height() {
        // one inheritance edge (w=1.5), confidence 0.5, crossing a package (h=8).
        let candidate = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::Inheritance,
                confidence: 0.5,
                lca_level: ScopeLevel::Package,
            }],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        // With one edge the weighted mean crossing height is just its own:
        // 1.5 * 0.5 * 8 / (16 * 1.5 * 0.5) = 0.5.
        assert!(close(breakdown.cut, 0.5));
        assert!(close(breakdown.total, 0.5));
    }

    #[test]
    fn should_keep_the_cut_term_scale_free_under_edge_mass() {
        // D-37 regression guard: the normalized cut term is a mean over the
        // candidate's own edge-weight mass, so growing the repository — more
        // files, more edges at the same crossing profile — must not inflate it.
        let single = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::Call,
                confidence: 1.0,
                lca_level: ScopeLevel::Domain,
            }],
            ..empty_candidate()
        };
        let mut grown = single.clone();
        for _ in 0..99 {
            grown.edges.push(ScoredEdge {
                kind: EdgeKind::Call,
                confidence: 1.0,
                lca_level: ScopeLevel::Domain,
            });
        }

        let small = score(
            &single,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );
        let large = score(&grown, &Coefficients::greenfield(), &KindWeights::default());

        assert!(
            close(small.cut, large.cut),
            "cut must be mass-invariant: single {} vs hundred-fold {}",
            small.cut,
            large.cut
        );
        assert!(
            (0.0..=1.0).contains(&large.cut),
            "normalized cut must stay in 0..1, got {}",
            large.cut
        );
    }

    #[test]
    fn should_charge_nothing_for_a_re_export_cut() {
        let candidate = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::ReExport,
                confidence: 1.0,
                lca_level: ScopeLevel::PackageGroup,
            }],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.cut, 0.0));
    }

    #[test]
    fn should_charge_more_for_a_higher_crossing() {
        let in_file = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::Call,
                confidence: 1.0,
                lca_level: ScopeLevel::File,
            }],
            ..empty_candidate()
        };
        let across_packages = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::Call,
                confidence: 1.0,
                lca_level: ScopeLevel::Package,
            }],
            ..empty_candidate()
        };

        let cheap = score(
            &in_file,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        )
        .cut;
        let dear = score(
            &across_packages,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        )
        .cut;

        assert!(dear > cheap);
    }

    #[test]
    fn should_penalise_sibling_size_imbalance() {
        // children of wildly different sizes -> positive imbalance.
        let candidate = Candidate {
            containers: vec![ContainerSizes {
                child_sizes: vec![1, 99],
            }],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(breakdown.imbalance > 0.0);
    }

    #[test]
    fn should_not_penalise_balanced_or_lonely_containers() {
        let candidate = Candidate {
            containers: vec![
                ContainerSizes {
                    child_sizes: vec![50, 50],
                },
                ContainerSizes {
                    child_sizes: vec![10],
                },
            ],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.imbalance, 0.0));
    }

    #[test]
    fn should_reward_naming_cohesion_as_a_negative_term() {
        // two symbols sharing every token -> Jaccard 1.0 -> naming term negative.
        let candidate = Candidate {
            cohesion_groups: vec![CohesionGroup {
                production_sloc: 10,
                members: vec![tokens(&["user", "service"]), tokens(&["user", "service"])],
            }],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.naming, -1.0));
    }

    #[test]
    fn should_size_weight_naming_across_groups() {
        // a large fully-cohesive group and an equally large disjoint group:
        // weighted mean cohesion is 0.5, so the naming term is -0.5.
        let candidate = Candidate {
            cohesion_groups: vec![
                CohesionGroup {
                    production_sloc: 10,
                    members: vec![tokens(&["a"]), tokens(&["a"])],
                },
                CohesionGroup {
                    production_sloc: 10,
                    members: vec![tokens(&["b"]), tokens(&["c"])],
                },
            ],
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.naming, -0.5));
    }

    #[test]
    fn should_apply_the_path_term_only_under_anchored_coefficients() {
        let candidate = Candidate {
            path_cohesion: 0.8,
            ..empty_candidate()
        };

        let greenfield = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );
        let anchored = score(
            &candidate,
            &Coefficients::anchored(),
            &KindWeights::default(),
        );

        // beta = 0 in greenfield, so the path term vanishes; anchored negates it.
        assert!(close(greenfield.path, 0.0));
        assert!(close(anchored.path, -0.8));
    }

    #[test]
    fn should_apply_the_anchor_term_only_under_anchored_coefficients() {
        let candidate = Candidate {
            move_distance: 0.25,
            ..empty_candidate()
        };

        let greenfield = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );
        let anchored = score(
            &candidate,
            &Coefficients::anchored(),
            &KindWeights::default(),
        );

        assert!(close(greenfield.anchor, 0.0));
        assert!(close(anchored.anchor, 0.25));
    }

    #[test]
    fn should_price_capacity_binding_in_both_modes() {
        // FIX03: a container binding twice the folder budget pays one full
        // pressure unit; `gamma` prices it identically in either mode.
        let candidate = Candidate {
            capacity_pressure: 1.0,
            ..empty_candidate()
        };

        let greenfield = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );
        let anchored = score(
            &candidate,
            &Coefficients::anchored(),
            &KindWeights::default(),
        );

        assert!(close(greenfield.capacity, 4.0));
        assert!(close(anchored.capacity, 4.0));
        assert!(close(greenfield.total, 4.0));
    }

    #[test]
    fn should_charge_nothing_for_within_budget_binding() {
        let candidate = Candidate {
            capacity_pressure: 0.0,
            ..empty_candidate()
        };

        let breakdown = score(
            &candidate,
            &Coefficients::greenfield(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.capacity, 0.0));
    }

    #[test]
    fn should_sum_every_term_into_the_total() {
        let candidate = Candidate {
            edges: vec![ScoredEdge {
                kind: EdgeKind::ValueImport,
                confidence: 1.0,
                lca_level: ScopeLevel::Folder,
            }],
            containers: vec![ContainerSizes {
                child_sizes: vec![1, 9],
            }],
            cohesion_groups: vec![CohesionGroup {
                production_sloc: 4,
                members: vec![tokens(&["x"]), tokens(&["x"])],
            }],
            path_cohesion: 0.5,
            move_distance: 0.2,
            capacity_pressure: 0.3,
        };
        let breakdown = score(
            &candidate,
            &Coefficients::anchored(),
            &KindWeights::default(),
        );

        let expected = breakdown.cut
            + breakdown.imbalance
            + breakdown.naming
            + breakdown.path
            + breakdown.anchor
            + breakdown.capacity;
        assert!(close(breakdown.total, expected));
    }

    #[test]
    fn should_score_an_empty_candidate_to_zero() {
        let breakdown = score(
            &empty_candidate(),
            &Coefficients::anchored(),
            &KindWeights::default(),
        );

        assert!(close(breakdown.total, 0.0));
    }

    #[test]
    fn should_default_kind_weights_to_the_config_less_table() {
        let weights = KindWeights::default();

        for (kind, expected) in [
            (EdgeKind::ValueImport, 1.0),
            (EdgeKind::Inheritance, 1.5),
            (EdgeKind::Call, 1.0),
            (EdgeKind::TypeReference, 0.3),
            (EdgeKind::ReExport, 0.0),
        ] {
            assert!(close(weights.weight_of(kind), expected));
        }
    }

    #[test]
    fn should_scale_the_edge_weight_by_confidence() {
        let weights = KindWeights::default();

        // inheritance 1.5 × confidence 0.5 = 0.75.
        assert!(close(weights.edge_weight(EdgeKind::Inheritance, 0.5), 0.75));
    }
}
