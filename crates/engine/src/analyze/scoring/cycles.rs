//! Cycle mass of a dependency graph, used as a component-wise admission budget.

use strata_core::condense::condense;
use strata_core::graph::csr::Csr;

/// cycle mass used as a component-wise admission budget
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::analyze) struct CycleCounts {
    pub(in crate::analyze) vertices: usize,
    pub(in crate::analyze) edges: usize,
}

impl CycleCounts {
    pub(in crate::analyze) fn from_graph(graph: &Csr) -> Self {
        let condensation = condense(graph);
        let cyclic: Vec<bool> = condensation
            .members
            .iter()
            .map(|members| members.len() > 1)
            .collect();
        let vertices = condensation
            .members
            .iter()
            .filter(|members| members.len() > 1)
            .map(Vec::len)
            .sum();
        let mut edges = 0;
        for source in 0..graph.vertex_count() {
            let Ok(source) = u32::try_from(source) else {
                continue;
            };
            let Some(&component) = condensation.membership.get(source as usize) else {
                continue;
            };
            if !cyclic.get(component.0 as usize).copied().unwrap_or(false) {
                continue;
            }
            edges += graph
                .neighbors(source)
                .iter()
                .filter(|&&target| {
                    condensation
                        .membership
                        .get(target as usize)
                        .is_some_and(|owner| *owner == component)
                })
                .count();
        }
        Self { vertices, edges }
    }

    pub(in crate::analyze) fn exceeds(self, baseline: Self) -> bool {
        self.vertices > baseline.vertices || self.edges > baseline.edges
    }
}
