//! Fixture-based integration tests for the TypeScript adapter.
//!
//! The fixture repo under `tests/fixtures/sample` is parsed and bound, then the
//! emitted nodes, edges, and containers are asserted in a deterministic,
//! canonical order — fragment-for-fragment — so any drift in extraction,
//! resolution, polarity, or SLOC is caught.

use std::fs;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{
    Adapter, ContainerTree, EdgeKind, Hardness, IntermediateRepresentation, NodeKind, Polarity,
    ScopeLevel, Snapshot, SourceFile,
};

/// A canonical, comparable view of one emitted node.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NodeView {
    file: String,
    name: String,
    kind: KindView,
    polarity: PolarityView,
    sloc: u32,
}

/// A canonical, comparable view of one emitted edge (by symbol names).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeView {
    source: String,
    target: String,
    kind: EdgeKindView,
    hard: bool,
    confidence_bits: u64,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum KindView {
    Symbol,
    Type,
    FileBody,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PolarityView {
    Production,
    TestCase,
    TestSupport,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKindView {
    ValueImport,
    TypeReference,
    Inheritance,
    Call,
    ReExport,
}

fn kind_view(kind: NodeKind) -> KindView {
    match kind {
        NodeKind::Symbol => KindView::Symbol,
        NodeKind::Type => KindView::Type,
        NodeKind::FileBody => KindView::FileBody,
    }
}

fn polarity_view(polarity: Polarity) -> PolarityView {
    match polarity {
        Polarity::Production => PolarityView::Production,
        Polarity::TestCase => PolarityView::TestCase,
        Polarity::TestSupport => PolarityView::TestSupport,
    }
}

fn edge_kind_view(kind: EdgeKind) -> EdgeKindView {
    match kind {
        EdgeKind::ValueImport => EdgeKindView::ValueImport,
        EdgeKind::TypeReference => EdgeKindView::TypeReference,
        EdgeKind::Inheritance => EdgeKindView::Inheritance,
        EdgeKind::Call => EdgeKindView::Call,
        EdgeKind::ReExport => EdgeKindView::ReExport,
    }
}

/// Recursively collects `.ts` / `.tsx` files under `dir`, relative to `root`.
///
/// Unreadable entries are skipped rather than panicking; the caller asserts the
/// resulting set is non-empty, which fails loudly if the fixture is missing.
fn collect(dir: &Path, root: &Path, out: &mut Vec<SourceFile>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, out);
            continue;
        }
        if !matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("ts" | "tsx")
        ) {
            continue;
        }
        let (Ok(relative), Ok(contents)) = (path.strip_prefix(root), fs::read_to_string(&path))
        else {
            continue;
        };
        out.push(SourceFile {
            path: SmolStr::new(relative.to_string_lossy().replace('\\', "/")),
            contents,
        });
    }
}

/// Loads the sample fixture repo as a sorted list of source files.
fn load_sample() -> (PathBuf, Vec<SourceFile>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    (root, files)
}

/// Builds the fragment, then canonical node and edge views keyed by symbol name.
///
/// Returns `Err` with a human-readable reason on any IO, parse, or bind failure
/// so callers can assert success and surface the cause.
fn build_views() -> Result<(Vec<NodeView>, Vec<EdgeView>, IntermediateRepresentation), String> {
    let (root, files) = load_sample();
    assert!(!files.is_empty(), "fixture repo is empty");
    let adapter = TypeScriptAdapter::new(root);
    let trees = adapter
        .parse(&files)
        .map_err(|error| format!("parse failed: {error}"))?;
    let fragment = adapter
        .bind(trees)
        .map_err(|error| format!("bind failed: {error}"))?;

    let container_name = |id: strata_ir::ContainerId| -> String {
        fragment
            .containers
            .iter()
            .find(|container| container.id == id && container.level == ScopeLevel::File)
            .map_or_else(|| "?".to_string(), |container| container.name.to_string())
    };
    let node_name = |id: strata_ir::NodeId| -> String {
        fragment
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| "?".to_string(), |node| node.name.to_string())
    };

    let mut nodes: Vec<NodeView> = fragment
        .nodes
        .iter()
        .map(|node| NodeView {
            file: container_name(node.container),
            name: node.name.to_string(),
            kind: kind_view(node.kind),
            polarity: polarity_view(node.polarity),
            sloc: node.effective_size,
        })
        .collect();
    nodes.sort();

    let mut edges: Vec<EdgeView> = fragment
        .edges
        .iter()
        .map(|edge| EdgeView {
            source: node_name(edge.source),
            target: node_name(edge.target),
            kind: edge_kind_view(edge.kind),
            hard: edge.hardness == Hardness::Hard,
            confidence_bits: edge.confidence.to_bits(),
        })
        .collect();
    edges.sort();

    let ir = IntermediateRepresentation::new(
        fragment.nodes.clone(),
        fragment.edges.clone(),
        ContainerTree::new(fragment.containers.clone()),
    );
    Ok((nodes, edges, ir))
}

/// The raw bit pattern of a confidence, for exact, hashable comparison.
fn confidence_bits(confidence: f64) -> u64 {
    confidence.to_bits()
}

#[test]
fn should_emit_the_exact_node_set_with_polarity_and_sloc() -> Result<(), String> {
    let (nodes, _edges, _ir) = build_views()?;

    let expected = vec![
        NodeView {
            file: "src/__tests__/app.spec.ts".to_string(),
            name: "shouldRun".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::TestCase,
            sloc: 3,
        },
        NodeView {
            file: "src/__tests__/support.ts".to_string(),
            name: "makeRect".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::TestCase,
            sloc: 3,
        },
        NodeView {
            file: "src/app.ts".to_string(),
            name: "lazy".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 4,
        },
        NodeView {
            file: "src/app.ts".to_string(),
            name: "run".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 4,
        },
        // The barrel `src/geometry/index.ts` re-exports four names; each binding
        // is materialized as its own zero-SLOC node (typed per `export type`).
        NodeView {
            file: "src/geometry/index.ts".to_string(),
            name: "Dimensions".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "src/geometry/index.ts".to_string(),
            name: "Rectangle".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "src/geometry/index.ts".to_string(),
            name: "Shape".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "src/geometry/index.ts".to_string(),
            name: "describe".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "src/geometry/rectangle.ts".to_string(),
            name: "Rectangle".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 6,
        },
        NodeView {
            file: "src/geometry/rectangle.ts".to_string(),
            name: "describe".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 3,
        },
        NodeView {
            file: "src/geometry/shape.ts".to_string(),
            name: "Dimensions".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 4,
        },
        NodeView {
            file: "src/geometry/shape.ts".to_string(),
            name: "Shape".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 3,
        },
    ];

    assert_eq!(nodes, expected);
    Ok(())
}

#[test]
fn should_emit_the_exact_typed_edge_set() -> Result<(), String> {
    let (_nodes, edges, _ir) = build_views()?;

    let hard = confidence_bits(1.0);
    let dynamic = confidence_bits(0.5);
    let expected = vec![
        // Barrel re-export edges from `src/geometry/index.ts`: each barrel
        // binding (source) links soft to the original declaration (target).
        EdgeView {
            source: "Dimensions".to_string(),
            target: "Dimensions".to_string(),
            kind: EdgeKindView::ReExport,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "Rectangle".to_string(),
            target: "Dimensions".to_string(),
            kind: EdgeKindView::TypeReference,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "Rectangle".to_string(),
            target: "Rectangle".to_string(),
            kind: EdgeKindView::ReExport,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "Rectangle".to_string(),
            target: "Shape".to_string(),
            kind: EdgeKindView::Inheritance,
            hard: true,
            confidence_bits: hard,
        },
        EdgeView {
            source: "Shape".to_string(),
            target: "Shape".to_string(),
            kind: EdgeKindView::ReExport,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "describe".to_string(),
            target: "Dimensions".to_string(),
            kind: EdgeKindView::TypeReference,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "describe".to_string(),
            target: "describe".to_string(),
            kind: EdgeKindView::ReExport,
            hard: false,
            confidence_bits: hard,
        },
        EdgeView {
            source: "lazy".to_string(),
            target: "Rectangle".to_string(),
            kind: EdgeKindView::ValueImport,
            hard: true,
            confidence_bits: dynamic,
        },
        EdgeView {
            source: "lazy".to_string(),
            target: "describe".to_string(),
            kind: EdgeKindView::ValueImport,
            hard: true,
            confidence_bits: dynamic,
        },
        EdgeView {
            source: "makeRect".to_string(),
            target: "Rectangle".to_string(),
            kind: EdgeKindView::Call,
            hard: true,
            confidence_bits: hard,
        },
        EdgeView {
            source: "run".to_string(),
            target: "Rectangle".to_string(),
            kind: EdgeKindView::Call,
            hard: true,
            confidence_bits: hard,
        },
        EdgeView {
            source: "shouldRun".to_string(),
            target: "makeRect".to_string(),
            kind: EdgeKindView::Call,
            hard: true,
            confidence_bits: hard,
        },
        EdgeView {
            source: "shouldRun".to_string(),
            target: "run".to_string(),
            kind: EdgeKindView::Call,
            hard: true,
            confidence_bits: hard,
        },
    ];

    assert_eq!(edges, expected);
    Ok(())
}

#[test]
fn should_assemble_into_a_valid_snapshot() -> Result<(), String> {
    let (_nodes, _edges, ir) = build_views()?;

    assert!(Snapshot::assemble(ir).is_ok());
    Ok(())
}

#[test]
fn should_surface_a_syntax_error_rather_than_skipping_the_file() {
    let files = vec![SourceFile {
        path: SmolStr::new("src/broken.ts"),
        contents: "export function oops(  {\n".to_string(),
    }];
    let adapter = TypeScriptAdapter::new(".");

    let result = adapter.parse(&files);

    assert!(
        matches!(result, Err(strata_ir::AdapterError::Parse { ref path, .. }) if path == "src/broken.ts"),
        "expected a Parse error pointing at the broken file, got {result:?}"
    );
}
