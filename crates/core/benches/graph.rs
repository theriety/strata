//! Release benchmark for the graph foundation: CSR construction, Tarjan
//! condensation, and longest-path layering over a 100k-node snapshot.
//!
//! Run with `cargo bench -p strata-core`. The synthetic graph is a chain of
//! 1,000-node cycles (100 cycles) linked head-to-tail, exercising both the SCC
//! collapse path and a deep condensation DAG.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use smol_str::SmolStr;
use strata_core::condense::condense;
use strata_core::graph::csr::{HardnessFilter, build_csr};
use strata_core::layer::layer;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IntermediateRepresentation,
    Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};

/// Number of nodes in each cycle of the synthetic graph.
const CYCLE_SIZE: u32 = 1_000;
/// Number of cycles chained together.
const CYCLE_COUNT: u32 = 100;

/// Builds the 100k-node snapshot: `CYCLE_COUNT` cycles of `CYCLE_SIZE` nodes,
/// each cycle linked forward into the next. Returns `None` only if assembly of
/// the hand-built graph fails, which it does not for this input.
fn snapshot() -> Option<Snapshot> {
    let node_count = CYCLE_SIZE * CYCLE_COUNT;

    let nodes: Vec<Node> = (0..node_count)
        .map(|id| Node {
            id: NodeId(id),
            name: SmolStr::new(format!("n{id}")),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        })
        .collect();

    let mut edges: Vec<Edge> = Vec::new();
    for cycle in 0..CYCLE_COUNT {
        let base = cycle * CYCLE_SIZE;
        for offset in 0..CYCLE_SIZE {
            let source = base + offset;
            let target = base + (offset + 1) % CYCLE_SIZE;
            edges.push(hard_edge(source, target));
        }
        if cycle + 1 < CYCLE_COUNT {
            edges.push(hard_edge(base, base + CYCLE_SIZE));
        }
    }

    let tree = ContainerTree::new(vec![Container {
        id: ContainerId(0),
        name: SmolStr::new("root"),
        level: ScopeLevel::File,
        parent: None,
        synthetic: false,
    }]);
    let ir = IntermediateRepresentation::new(nodes, edges, tree);

    Snapshot::assemble(ir).ok()
}

/// A single hard call edge.
fn hard_edge(source: u32, target: u32) -> Edge {
    Edge {
        source: NodeId(source),
        target: NodeId(target),
        kind: EdgeKind::Call,
        hardness: Hardness::Hard,
        confidence: 1.0,
    }
}

/// Benchmarks the full graph foundation pipeline on the 100k-node snapshot.
fn bench_pipeline(criterion: &mut Criterion) {
    let Some(snapshot) = snapshot() else {
        return;
    };

    criterion.bench_function("build_csr_condense_layer_100k", |bencher| {
        bencher.iter(|| {
            let views = build_csr(black_box(&snapshot), HardnessFilter::HardOnly);
            let condensation = condense(&views.forward);
            let layers = layer(&condensation);
            black_box(layers);
        });
    });
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
