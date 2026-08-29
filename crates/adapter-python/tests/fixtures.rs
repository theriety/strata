//! Fixture-based integration tests for the Python adapter.
//!
//! The fixture repo under `tests/fixtures/sample` is parsed and bound, then the
//! emitted nodes, edges, and containers are asserted in a deterministic,
//! canonical order — fragment-for-fragment — so any drift in extraction,
//! resolution, polarity, or SLOC is caught.

use std::fs;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;
use strata_adapter_python::PythonAdapter;
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

/// Recursively collects `.py` files under `dir`, relative to `root`.
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
        if path.extension().and_then(|ext| ext.to_str()) != Some("py") {
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
    let adapter = PythonAdapter::new(root);
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
            file: "app.py".to_string(),
            name: "load".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 1,
        },
        NodeView {
            file: "app.py".to_string(),
            name: "run".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 2,
        },
        // The `geometry/__init__.py` barrel re-exports four names; each binding
        // is materialized as its own zero-SLOC node (typed per the original).
        NodeView {
            file: "geometry/__init__.py".to_string(),
            name: "Dimensions".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "geometry/__init__.py".to_string(),
            name: "Rectangle".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "geometry/__init__.py".to_string(),
            name: "Shape".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "geometry/__init__.py".to_string(),
            name: "describe".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 0,
        },
        NodeView {
            file: "geometry/rectangle.py".to_string(),
            name: "Rectangle".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 4,
        },
        NodeView {
            file: "geometry/rectangle.py".to_string(),
            name: "describe".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::Production,
            sloc: 1,
        },
        NodeView {
            file: "geometry/shape.py".to_string(),
            name: "Dimensions".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 3,
        },
        NodeView {
            file: "geometry/shape.py".to_string(),
            name: "Shape".to_string(),
            kind: KindView::Type,
            polarity: PolarityView::Production,
            sloc: 2,
        },
        NodeView {
            file: "tests/conftest.py".to_string(),
            name: "make_dimensions".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::TestSupport,
            sloc: 1,
        },
        NodeView {
            file: "tests/test_app.py".to_string(),
            name: "test_run_describes_a_rectangle".to_string(),
            kind: KindView::Symbol,
            polarity: PolarityView::TestCase,
            sloc: 2,
        },
    ];

    assert_eq!(nodes, expected);
    Ok(())
}

#[test]
fn should_emit_the_exact_typed_edge_set() -> Result<(), String> {
    let (_nodes, edges, _ir) = build_views()?;
    assert_eq!(edges, expected_edges());
    Ok(())
}

/// Builds an [`EdgeView`] from its source/target names, kind, and confidence.
fn edge(source: &str, target: &str, kind: EdgeKindView, hard: bool, confidence: f64) -> EdgeView {
    EdgeView {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        hard,
        confidence_bits: confidence_bits(confidence),
    }
}

/// The exact, sorted edge set the sample package should bind to.
fn expected_edges() -> Vec<EdgeView> {
    use EdgeKindView::{Call, Inheritance, ReExport, TypeReference, ValueImport};

    let hard = 1.0;
    vec![
        // Barrel re-export edges from `geometry/__init__.py`: each barrel
        // binding (source) links soft to the original declaration (target).
        edge("Dimensions", "Dimensions", ReExport, false, hard),
        edge("Rectangle", "Dimensions", TypeReference, false, hard),
        edge("Rectangle", "Rectangle", ReExport, false, hard),
        edge("Rectangle", "Shape", Inheritance, true, hard),
        edge("Shape", "Shape", ReExport, false, hard),
        edge("describe", "Shape", TypeReference, false, hard),
        edge("describe", "describe", ReExport, false, hard),
        // `load` calls `importlib.import_module("geometry.shape")`: an honest
        // low-confidence value-import edge fans out over that module's public
        // surface (`Dimensions`, `Shape`) rather than dropping the dependency.
        edge("load", "Dimensions", ValueImport, true, 0.5),
        edge("load", "Shape", ValueImport, true, 0.5),
        edge("make_dimensions", "Dimensions", TypeReference, false, hard),
        edge("make_dimensions", "Dimensions", Call, true, hard),
        edge("run", "Rectangle", Call, true, hard),
        edge("run", "describe", Call, true, hard),
        edge(
            "test_run_describes_a_rectangle",
            "make_dimensions",
            Call,
            true,
            hard,
        ),
        edge("test_run_describes_a_rectangle", "run", Call, true, hard),
    ]
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
        path: SmolStr::new("pkg/broken.py"),
        contents: "def oops(:\n".to_string(),
    }];
    let adapter = PythonAdapter::new(".");

    let result = adapter.parse(&files);

    assert!(
        matches!(result, Err(strata_ir::AdapterError::Parse { ref path, .. }) if path == "pkg/broken.py"),
        "expected a Parse error pointing at the broken file, got {result:?}"
    );
}
