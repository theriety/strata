//! The objective function J(T) and its per-candidate breakdown.
//!
//! One score must rank wildly different laminar trees, and the same solver must
//! serve both incremental (anchored) and greenfield restructuring. Strata's
//! objective is the single scalar
//!
//! ```text
//! J(T) = sum_e w(e) c(e) h(lca_T(e))   // cut, weighted by crossing height
//!        + lambda * imbalance(T)        // sibling size imbalance
//!        - alpha  * naming(T)           // symbol-token cohesion
//!        - beta   * path(T)             // current-path cohesion (anchored)
//!        + mu     * d(T, T0)            // move distance from the current tree
//! ```
//!
//! minimised over candidate trees. The two modes differ only in coefficients:
//! greenfield zeroes `mu` and `beta` so the current layout cannot leak back in
//! through path similarity, while anchored keeps both positive so candidates stay
//! reachable from today's structure. There is no mode-specific code path — a mode
//! is purely a [`Coefficients`] preset.
//!
//! Cohesion terms (`naming`, `path`) enter with a minus sign because more
//! cohesion is *better*, i.e. lowers the objective. Every term is surfaced
//! separately in the [`ScoreBreakdown`] so a reader sees *why* a tree ranks where
//! it does. Hard constraints — acyclicity, polarity, capacity — never appear
//! here: they are vetoes enforced upstream, never penalties traded against score.

use std::collections::BTreeSet;

use strata_ir::{EdgeKind, ScopeLevel};

/// The coefficients of the objective, fixed per mode.
///
/// `lambda`, `alpha`, and `beta` weight the imbalance, naming, and path terms;
/// `mu` weights the anchoring (move-distance) term. A greenfield preset sets
/// `mu` and `beta` to zero; an anchored preset keeps them positive. The values
/// are not configurable in v1 — only the mode chooses between presets.
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
        }
    }

    /// The greenfield preset: an unbiased ideal. `mu = 0` and `beta = 0` so the
    /// current layout cannot leak back in through anchoring or path similarity;
    /// symbol-token cohesion (`alpha`) and imbalance (`lambda`) remain.
    #[must_use]
    pub const fn greenfield() -> Self {
        Self {
            lambda: 1.0,
            alpha: 1.0,
            beta: 0.0,
            mu: 0.0,
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
}

/// Per-candidate score decomposition, reported in the DTO so users see *why* a
/// tree ranks where it does.
///
/// `total` is the sum of all five terms — the value of J(T) being minimised.
/// `naming` and `path` are already negated (their minus sign folded in), so the
/// breakdown sums to `total` directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreBreakdown {
    /// `sum_e w(e) c(e) h(lca_T(e))` — the height-weighted cut cost.
    pub cut: f64,
    /// `lambda * imbalance(T)` — the sibling size-imbalance penalty.
    pub imbalance: f64,
    /// `-alpha * naming(T)` — the (negated) symbol-token cohesion bonus.
    pub naming: f64,
    /// `-beta * path(T)` — the (negated) current-path cohesion bonus.
    pub path: f64,
    /// `mu * d(T, T0)` — the move-distance anchoring penalty.
    pub anchor: f64,
    /// The full objective `J(T)`, the sum of the five terms above.
    pub total: f64,
}

/// Scores a candidate tree under the given coefficients and per-kind edge
/// weights, returning the full breakdown of J(T).
///
/// Each term is computed independently from the candidate's pre-extracted views:
/// the cut cost sums `w(e) c(e) h(level)` over edges priced by the `[weights]`
/// table, imbalance sums the squared coefficient of variation of every
/// container's child sizes, naming and path are the (negated) cohesion bonuses,
/// and anchor is `mu` times the move distance. Hard constraints are not scored
/// here.
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

    ScoreBreakdown {
        cut,
        imbalance,
        naming,
        path,
        anchor,
        total: cut + imbalance + naming + path + anchor,
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
        ScopeLevel::PackageGroup => 16.0,
    }
}

/// Sums the height-weighted cut cost `sum_e w(e) c(e) h(lca_T(e))` over the
/// candidate's edges, pricing `w(e)` from the configured `[weights]` table.
fn cut_cost(edges: &[ScoredEdge], weights: &KindWeights) -> f64 {
    edges
        .iter()
        .map(|edge| {
            weights.edge_weight(edge.kind, edge.confidence) * height_penalty(edge.lca_level)
        })
        .sum()
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

        // 1.5 * 0.5 * 8 = 6.0
        assert!(close(breakdown.cut, 6.0));
        assert!(close(breakdown.total, 6.0));
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
            + breakdown.anchor;
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
