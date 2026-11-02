//! Fixture-based integration tests for the Rust adapter.
//!
//! The fixture cargo workspace under `tests/fixtures/workspace` (a leaf `util`
//! crate, a middle `core` crate, and a top `app` crate) is parsed and bound
//! through the real `ra_ap_*` semantic database, then the emitted nodes and
//! edges are asserted in a deterministic, canonical order — so any drift in
//! extraction, cross-crate resolution, polarity, or SLOC is caught.

use std::fs;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;
use strata_adapter_rust::RustAdapter;
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
}

/// A canonical, comparable view of one emitted edge (by symbol names).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeView {
    source: String,
    target: String,
    kind: EdgeKindView,
    hard: bool,
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

/// The fixture workspace root (the directory holding its `Cargo.toml`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace")
}

/// Recursively collects `.rs` files under `dir`, relative to `root`.
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
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
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

/// Loads the fixture workspace as a sorted list of source files.
fn load_fixture() -> (PathBuf, Vec<SourceFile>) {
    let root = workspace_root();
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    (root, files)
}

/// Builds the fragment, then canonical node and edge views keyed by symbol name.
fn build_views() -> Result<(Vec<NodeView>, Vec<EdgeView>, IntermediateRepresentation), String> {
    let (root, files) = load_fixture();
    if files.is_empty() {
        return Err("fixture workspace is empty".to_string());
    }
    let manifest = root.join("Cargo.toml");
    let adapter = RustAdapter::new(manifest);
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
        })
        .collect();
    edges.sort();
    edges.dedup();

    let ir = IntermediateRepresentation::new(
        fragment.nodes.clone(),
        fragment.edges.clone(),
        ContainerTree::new(fragment.containers.clone()),
    );
    Ok((nodes, edges, ir))
}

#[test]
fn should_extract_cross_crate_declarations_as_nodes() -> Result<(), String> {
    let (nodes, _edges, _ir) = build_views()?;

    // Every top-level declaration of the three fixture crates surfaces as a
    // production node with the expected symbol/type classification.
    let has = |file: &str, name: &str, kind: KindView| {
        nodes.iter().any(|node| {
            node.file == file
                && node.name == name
                && node.kind == kind
                && node.polarity == PolarityView::Production
        })
    };

    assert!(has("crates/util/src/lib.rs", "Measure", KindView::Type));
    assert!(has("crates/util/src/lib.rs", "Summarize", KindView::Type));
    assert!(has("crates/core/src/lib.rs", "Report", KindView::Type));
    assert!(has(
        "crates/core/src/lib.rs",
        "impl Summarize for Report",
        KindView::Type
    ));
    assert!(has("crates/app/src/lib.rs", "describe", KindView::Symbol));
    assert!(has(
        "crates/app/src/lib.rs",
        "combined_total",
        KindView::Symbol
    ));
    Ok(())
}

#[test]
fn should_resolve_a_cross_crate_trait_impl_as_inheritance() -> Result<(), String> {
    let (_nodes, edges, _ir) = build_views()?;

    // `impl Summarize for Report` in `core` resolves to the `Summarize` trait
    // declared in `util`: a hard inheritance edge from the block, named by its
    // header, to `Summarize`.
    let inheritance = edges.iter().any(|edge| {
        edge.kind == EdgeKindView::Inheritance
            && edge.source == "impl Summarize for Report"
            && edge.target == "Summarize"
            && edge.hard
    });
    assert!(
        inheritance,
        "expected a hard `impl Summarize for Report` -> Summarize inheritance edge, got {edges:#?}"
    );
    Ok(())
}

#[test]
fn should_resolve_a_cross_crate_method_call_as_a_call_edge() -> Result<(), String> {
    let (_nodes, edges, _ir) = build_views()?;

    // `report.summarize()` in `app::describe` resolves cross-crate to the
    // `summarize` method implemented in `core`.
    let call = edges
        .iter()
        .any(|edge| edge.kind == EdgeKindView::Call && edge.source == "describe" && edge.hard);
    assert!(
        call,
        "expected a hard call edge out of describe, got {edges:#?}"
    );
    Ok(())
}

#[test]
fn should_resolve_a_trait_method_call_to_the_whole_impl_block() -> Result<(), String> {
    let (nodes, edges, _ir) = build_views()?;

    // a trait impl cannot be split across files, so `summarize` is not its own
    // node: `report.summarize()` lands on the `impl Summarize for Report` block.
    assert!(
        !nodes.iter().any(|node| node.name == "summarize"),
        "a trait impl method is not a standalone node, got {nodes:#?}"
    );
    let call = edges.iter().any(|edge| {
        edge.kind == EdgeKindView::Call
            && edge.source == "describe"
            && edge.target == "impl Summarize for Report"
    });
    assert!(
        call,
        "expected describe -> `impl Summarize for Report` call edge, got {edges:#?}"
    );
    Ok(())
}

#[test]
fn should_emit_a_re_export_edge_for_a_pub_use() -> Result<(), String> {
    let (_nodes, edges, _ir) = build_views()?;

    // `pub use fixture_util::Measure as ReMeasure;` in `app` re-exports the
    // leaf-crate `Measure` type: a re-export edge from the barrel name to it.
    let re_export = edges.iter().any(|edge| {
        edge.kind == EdgeKindView::ReExport
            && edge.source == "ReMeasure"
            && edge.target == "Measure"
    });
    assert!(
        re_export,
        "expected a ReMeasure -> Measure re-export edge, got {edges:#?}"
    );
    Ok(())
}

#[test]
fn should_classify_a_shared_test_utility_as_test_support() -> Result<(), String> {
    let (nodes, _edges, _ir) = build_views()?;

    // `sample_report` is a `#[cfg(test)]` helper used only by a test case, so it
    // is test support; the `#[test]` function itself is a test case.
    let support = nodes
        .iter()
        .any(|node| node.name == "sample_report" && node.polarity == PolarityView::TestSupport);
    let test_case = nodes.iter().any(|node| {
        node.name == "report_doubles_its_measure" && node.polarity == PolarityView::TestCase
    });
    assert!(
        support,
        "expected sample_report to be test support, got {nodes:#?}"
    );
    assert!(
        test_case,
        "expected report_doubles_its_measure to be a test case, got {nodes:#?}"
    );
    Ok(())
}

#[test]
fn should_emit_at_most_one_edge_per_source_target_kind() -> Result<(), String> {
    let (_nodes, _edges, ir) = build_views()?;

    // Binding deduplicates edges, so the same dependency cited several ways (a
    // call plus a qualified-path reference, say) collapses to a single edge.
    let rank = |kind: EdgeKind| match kind {
        EdgeKind::ValueImport => 0u8,
        EdgeKind::TypeReference => 1,
        EdgeKind::Inheritance => 2,
        EdgeKind::Call => 3,
        EdgeKind::ReExport => 4,
    };
    let mut keys: Vec<(u32, u32, u8)> = ir
        .edges
        .iter()
        .map(|edge| (edge.source.0, edge.target.0, rank(edge.kind)))
        .collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        total,
        keys.len(),
        "expected no duplicate (source, target, kind) edges after dedup"
    );
    Ok(())
}

#[test]
fn should_assemble_into_a_valid_snapshot() -> Result<(), String> {
    let (_nodes, _edges, ir) = build_views()?;

    Snapshot::assemble(ir).map_err(|error| format!("snapshot assembly failed: {error}"))?;
    Ok(())
}

#[test]
fn should_surface_a_syntax_error_rather_than_skipping_the_file() {
    let files = vec![SourceFile {
        path: SmolStr::new("crates/app/src/broken.rs"),
        contents: "pub fn oops( {\n".to_string(),
    }];
    let adapter = RustAdapter::new(workspace_root().join("Cargo.toml"));

    let result = adapter.parse(&files);

    assert!(
        matches!(result, Err(strata_ir::AdapterError::Parse { ref path, .. }) if path == "crates/app/src/broken.rs"),
        "expected a Parse error pointing at the broken file, got {result:?}"
    );
}

#[test]
fn should_report_a_bind_failure_for_a_missing_manifest() -> Result<(), String> {
    let (_root, files) = load_fixture();
    if files.is_empty() {
        return Err("fixture workspace is empty".to_string());
    }
    // Anchor the adapter at a manifest that does not exist; loading the cargo
    // workspace must surface as an AdapterError::Bind, not a panic.
    let adapter = RustAdapter::new(workspace_root().join("does-not-exist/Cargo.toml"));
    let trees = adapter
        .parse(&files)
        .map_err(|error| format!("parse failed: {error}"))?;

    let result = adapter.bind(trees);

    assert!(
        matches!(result, Err(strata_ir::AdapterError::Bind { .. })),
        "expected a Bind error for a missing manifest, got {result:?}"
    );
    Ok(())
}
