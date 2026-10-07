//! Turning a placement into the objective.

use strata_core::score::{
    Candidate, Coefficients, KindWeights, ScoreBreakdown, ScoredEdge, score_with_naming,
    weighted_naming,
};
use strata_ir::ContainerId;

use super::ScoringContext;
use super::placed::Placed;

impl ScoringContext<'_> {
    /// Prices a placement function end to end: its dense form, its move
    /// distance, and its score.
    pub(in crate::analyze) fn score_placement(
        &self,
        placement: &dyn Fn(u32) -> Option<ContainerId>,
        coefficients: &Coefficients,
        weights: &KindWeights,
    ) -> ScoreBreakdown {
        let placed = self.lookups.place(placement);
        let distance = self.move_distance(&placed);
        self.score(&placed, distance, coefficients, weights)
    }

    /// Production SLOC landing in each container id under `placed`.
    fn file_sloc(&self, placed: &Placed) -> Vec<u32> {
        let mut file_sloc = vec![0_u32; self.slots.max(placed.slots)];
        for &(id, size) in &self.lookups.production {
            if let Some(file) = placed.file(id)
                && let Some(cell) = file_sloc.get_mut(file.0 as usize)
            {
                *cell = cell.saturating_add(size);
            }
        }
        file_sloc
    }

    /// Scores `placed` under the given coefficients, with `distance` as its
    /// move-distance term.
    pub(in crate::analyze) fn score(
        &self,
        placed: &Placed,
        distance: f64,
        coefficients: &Coefficients,
        weights: &KindWeights,
    ) -> ScoreBreakdown {
        let lookups = self.lookups;
        let edges = lookups
            .edges
            .iter()
            .filter_map(|edge| {
                let source = placed.file(edge.source)?;
                let target = placed.file(edge.target)?;
                Some(ScoredEdge {
                    kind: edge.kind,
                    confidence: edge.confidence,
                    affinity: edge.affinity,
                    lca_level: self.lca_level(source, target),
                })
            })
            .collect();

        let file_sloc = self.file_sloc(placed);
        let sloc_of = |container: usize| file_sloc.get(container).copied().unwrap_or(0);

        let mut group_sloc = vec![0_u32; self.group_cohesions.len()];
        let mut matched = 0_u64;
        let mut total = 0_u64;
        for file in &self.cohesion_files {
            let sloc = sloc_of(file.container);
            if let Some(cell) = group_sloc.get_mut(file.group) {
                *cell = cell.saturating_add(sloc);
            }
            total = total.saturating_add(u64::from(sloc));
            if file.matches_real {
                matched = matched.saturating_add(u64::from(sloc));
            }
        }
        let path_cohesion = if total == 0 {
            // no production SLOC lands anywhere, so no file demonstrably left
            // its folder: credit the layout in full.
            1.0
        } else {
            // reason: sloc totals fit u32 sums; the f64 mantissa loses nothing material
            #[allow(clippy::cast_precision_loss)]
            let ratio = matched as f64 / total as f64;
            ratio
        };
        let naming = weighted_naming(
            group_sloc
                .iter()
                .copied()
                .zip(self.group_cohesions.iter().copied()),
        );

        let candidate = Candidate {
            edges,
            containers: self.container_sizes(&file_sloc),
            cohesion_groups: Vec::new(),
            path_cohesion,
            move_distance: distance,
            capacity_pressure: self.pressure.total(&file_sloc),
            dependency_only_relocations: self.dependency_only_relocations(placed),
            companion_separations: self.companion_separations(placed),
        };
        score_with_naming(&candidate, naming, coefficients, weights)
    }

    /// Production declarations that left their pass-start file for one holding
    /// a dependency of theirs and no consumer.
    fn dependency_only_relocations(&self, placed: &Placed) -> u32 {
        self.lookups
            .dependency_nodes
            .iter()
            .filter(|facts| {
                let Some(destination) = placed
                    .file(facts.node)
                    .and_then(|file| self.pass_start.get(file.0 as usize).copied().flatten())
                else {
                    return false;
                };
                destination != facts.container
                    && facts.outgoing.binary_search(&destination).is_ok()
                    && facts.incoming.binary_search(&destination).is_err()
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Companions not placed in the pass-start file of their owner.
    fn companion_separations(&self, placed: &Placed) -> u32 {
        self.lookups
            .companions
            .iter()
            .filter(|facts| {
                placed
                    .file(facts.companion)
                    .and_then(|file| self.pass_start.get(file.0 as usize).copied().flatten())
                    != Some(facts.owner_container)
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
}
