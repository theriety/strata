use smol_str::SmolStr;
use strata_core::condense::Condensation;
use strata_core::condense::SccId;
use strata_core::graph::csr::Csr;
use strata_ir::{
    Affinity, Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness,
    IntermediateRepresentation, Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};

use crate::config::{AnalyzeConfig, ProfileConfig};
use crate::result::{
    ContainerNode, CurrentStanding, Level, ProfileCurrent, ProfileResult, ScoreBreakdown,
};

pub(in crate::analyze) fn empty_profile_result() -> ProfileResult {
    ProfileResult {
        parameters: ProfileConfig::default(),
        current: ProfileCurrent {
            score: 0.0,
            score_breakdown: ScoreBreakdown {
                cut: 0.0,
                imbalance: 0.0,
                naming: 0.0,
                path: 0.0,
                anchor: 0.0,
                capacity: 0.0,
                dependency_only: 0.0,
                companion_separation: 0.0,
            },
            unique_findings: Vec::new(),
            standing: CurrentStanding::Outscored,
            capacity_breaks: 0,
        },
        candidates: Vec::new(),
        pairwise_distance: Vec::new(),
        solution_space_converged: false,
    }
}

/// Builds a symbol node owning a container.
pub(in crate::analyze) fn node(id: u32, name: &str, container: u32, polarity: Polarity) -> Node {
    Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity,
        container: ContainerId(container),
        visibility: ScopeLevel::File,
        effective_size: 1,
    }
}

pub(in crate::analyze) fn sloc_node(
    id: u32,
    name: &str,
    container: ContainerId,
    sloc: u32,
) -> Node {
    Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container,
        visibility: ScopeLevel::File,
        effective_size: sloc,
    }
}

/// Builds a hard call edge.
pub(in crate::analyze) fn edge(source: u32, target: u32) -> Edge {
    Edge {
        source: NodeId(source),
        target: NodeId(target),
        kind: EdgeKind::Call,
        hardness: Hardness::Hard,
        confidence: 1.0,
    }
}

/// Builds a soft type-reference edge (weak priced pull, `0.3`).
pub(in crate::analyze) fn type_ref(source: u32, target: u32) -> Edge {
    Edge {
        kind: EdgeKind::TypeReference,
        hardness: Hardness::Soft,
        ..edge(source, target)
    }
}

/// Builds a hard inheritance edge (the heaviest priced pull, `1.5`).
pub(in crate::analyze) fn inherits(source: u32, target: u32) -> Edge {
    Edge {
        kind: EdgeKind::Inheritance,
        ..edge(source, target)
    }
}

/// Builds a zero-priced re-export edge (the barrel-file shape).
pub(in crate::analyze) fn reexport(source: u32, target: u32) -> Edge {
    Edge {
        kind: EdgeKind::ReExport,
        ..edge(source, target)
    }
}

/// Builds a container at a level under an optional parent.
pub(in crate::analyze) fn container(
    id: u32,
    name: &str,
    level: ScopeLevel,
    parent: Option<u32>,
) -> Container {
    Container {
        id: ContainerId(id),
        name: SmolStr::new(name),
        level,
        parent: parent.map(ContainerId),
        synthetic: false,
    }
}

/// Builds a synthetic `workspace`-bucket container (domain or folder) — the
/// empty-scope node the render collapses.
pub(in crate::analyze) fn synthetic_container(
    id: u32,
    name: &str,
    level: ScopeLevel,
    parent: Option<u32>,
) -> Container {
    Container {
        synthetic: true,
        ..container(id, name, level, parent)
    }
}

/// Assembles a snapshot from parts, panicking loudly with the assemble
/// error — a broken test fixture must surface, never hide behind a
/// minimal fallback.
#[allow(clippy::panic)] // loud failure is the point of this test helper
pub(in crate::analyze) fn snapshot(
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    containers: Vec<Container>,
) -> Snapshot {
    snapshot_with_affinities(nodes, edges, Vec::new(), containers)
}

#[allow(clippy::panic)] // loud failure is the point of this test helper
pub(in crate::analyze) fn snapshot_with_affinities(
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    affinities: Vec<Affinity>,
    containers: Vec<Container>,
) -> Snapshot {
    let mut ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
    ir.affinities = affinities;
    Snapshot::assemble(ir)
        .unwrap_or_else(|error| panic!("test snapshot failed to assemble: {error}"))
}

/// Builds a nested-package move whose narration depends on physical file facts.
pub(in crate::analyze) fn nested_package_fact_snapshot(
    is_test_pair: bool,
) -> (Snapshot, ContainerTree) {
    let moved_name = if is_test_pair {
        "workspace/addon/source/base/unit.spec.ts"
    } else {
        "workspace/addon/source/base/worker.ts"
    };
    let resident_name = if is_test_pair {
        "workspace/addon/source/target/unit.ts"
    } else {
        "workspace/addon/source/target/anchor.ts"
    };
    let moved_polarity = if is_test_pair {
        Polarity::TestCase
    } else {
        Polarity::Production
    };
    let containers = vec![
        container(0, "workspace/addon", ScopeLevel::PackageGroup, None),
        container(1, "workspace/addon", ScopeLevel::Package, Some(0)),
        container(2, "workspace/addon/source", ScopeLevel::Domain, Some(1)),
        container(
            3,
            "workspace/addon/source/base",
            ScopeLevel::Folder,
            Some(2),
        ),
        container(
            4,
            "workspace/addon/source/target",
            ScopeLevel::Folder,
            Some(2),
        ),
        container(5, moved_name, ScopeLevel::File, Some(3)),
        container(6, resident_name, ScopeLevel::File, Some(4)),
    ];
    let candidate = ContainerTree::new(vec![
        container(0, "workspace/addon", ScopeLevel::PackageGroup, None),
        container(1, "workspace/addon", ScopeLevel::Package, Some(0)),
        container(2, "workspace/addon/source", ScopeLevel::Domain, Some(1)),
        container(
            3,
            "workspace/addon/source/base",
            ScopeLevel::Folder,
            Some(2),
        ),
        container(
            4,
            "workspace/addon/source/target",
            ScopeLevel::Folder,
            Some(2),
        ),
        container(5, moved_name, ScopeLevel::File, Some(4)),
        container(6, resident_name, ScopeLevel::File, Some(4)),
    ]);
    (
        snapshot(
            vec![
                node(0, "moved", 5, moved_polarity),
                node(1, "resident", 6, Polarity::Production),
            ],
            vec![edge(0, 1)],
            containers,
        ),
        candidate,
    )
}

/// Collects every container's name, level, and rendered production SLOC.
pub(in crate::analyze) fn collect_tree(
    node: &ContainerNode,
    names: &mut Vec<String>,
    sloc: &mut Vec<u32>,
    levels: &mut Vec<Level>,
) {
    names.push(node.name.clone());
    levels.push(node.level);
    if let Some(value) = node.production_sloc {
        sloc.push(value);
    }
    for child in node.children.iter().flatten() {
        collect_tree(child, names, sloc, levels);
    }
}

/// Builds a config requesting `k` candidates in both modes.
pub(in crate::analyze) fn config_with_k(k: u32) -> AnalyzeConfig {
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.candidates = k;
    config.profiles.greenfield.candidates = k;
    config
}

/// Builds a file node with `sloc` production SLOC.
pub(in crate::analyze) fn file(name: &str, sloc: u32) -> ContainerNode {
    ContainerNode {
        name: name.to_owned(),
        level: Level::File,
        children: None,
        symbols: Some(Vec::new()),
        production_sloc: Some(sloc),
    }
}

/// Builds a folder node holding `children`.
pub(in crate::analyze) fn folder(name: &str, children: Vec<ContainerNode>) -> ContainerNode {
    interior(name, Level::Folder, children)
}

/// Builds an interior node at `level` holding `children`.
pub(in crate::analyze) fn interior(
    name: &str,
    level: Level,
    children: Vec<ContainerNode>,
) -> ContainerNode {
    ContainerNode {
        name: name.to_owned(),
        level,
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

/// Returns the node's only child, or `None` when it has zero or several.
pub(in crate::analyze) fn only_child(node: ContainerNode) -> Option<ContainerNode> {
    node.children.and_then(|children| {
        if children.len() == 1 {
            children.into_iter().next()
        } else {
            None
        }
    })
}

/// Builds an all-singleton condensation over `file_count` files.
pub(in crate::analyze) fn singleton_condensation(file_count: usize) -> Condensation {
    Condensation {
        dag: Csr::from_sorted_edges(file_count, &[]),
        membership: (0..file_count)
            .map(|index| SccId(u32::try_from(index).unwrap_or(u32::MAX)))
            .collect(),
        members: (0..file_count)
            .map(|index| vec![NodeId(u32::try_from(index).unwrap_or(u32::MAX))])
            .collect(),
    }
}
