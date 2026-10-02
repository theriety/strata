//! Regression coverage for semantic targets that must not become name collisions.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, EdgeKind, Hardness, IrFragment, NodeId, SourceFile};

fn bind_fixture(paths: &[&str]) -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resolution/app");
    let files = paths
        .iter()
        .map(|path| {
            fs::read_to_string(root.join(path))
                .map(|contents| SourceFile {
                    path: (*path).into(),
                    contents,
                })
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let adapter = RustAdapter::new(root.join("Cargo.toml"));
    let trees = adapter.parse(&files).map_err(|error| error.to_string())?;
    adapter.bind(trees).map_err(|error| error.to_string())
}

fn node_id(fragment: &IrFragment, name: &str) -> Result<NodeId, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name == name)
        .map(|node| node.id)
        .ok_or_else(|| format!("missing node {name}: {:?}", fragment.nodes))
}

fn fragment() -> Result<&'static IrFragment, String> {
    static FRAGMENT: OnceLock<Result<IrFragment, String>> = OnceLock::new();
    FRAGMENT
        .get_or_init(|| bind_fixture(&["src/lib.rs", "src/render.rs", "src/unindexed.rs"]))
        .as_ref()
        .map_err(Clone::clone)
}

#[test]
fn should_not_replace_an_external_target_with_a_local_name_collision() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "external_caller")?;
    let wrong_target = node_id(fragment, "external_value")?;

    let preserved_target = node_id(fragment, "local_value")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == wrong_target)
            && fragment.edges.iter().any(|edge| edge.source == source
                && edge.target == preserved_target
                && edge.kind == EdgeKind::Call
                && edge.hardness == Hardness::Hard),
        "external calls must not bind to local helpers: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_bind_an_unresolved_receiver_to_a_free_function() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "unresolved_caller")?;
    let wrong_target = node_id(fragment, "first")?;

    let preserved_target = node_id(fragment, "local_value")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == wrong_target)
            && fragment.edges.iter().any(|edge| edge.source == source
                && edge.target == preserved_target
                && edge.kind == EdgeKind::Call
                && edge.hardness == Hardness::Hard),
        "receiver calls must not bind by name: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_bind_a_macro_receiver_to_a_free_function() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "macro_caller")?;
    let wrong_target = node_id(fragment, "first")?;

    let preserved_target = node_id(fragment, "local_value")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == wrong_target)
            && fragment.edges.iter().any(|edge| edge.source == source
                && edge.target == preserved_target
                && edge.kind == EdgeKind::Call
                && edge.hardness == Hardness::Hard),
        "macro receiver calls must not bind by name: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_replace_an_omitted_semantic_target_with_a_local_collision() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "omitted_caller")?;
    let wrong_target = node_id(fragment, "omitted_value")?;

    let preserved_target = node_id(fragment, "local_value")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == wrong_target)
            && fragment.edges.iter().any(|edge| edge.source == source
                && edge.target == preserved_target
                && edge.kind == EdgeKind::Call
                && edge.hardness == Hardness::Hard),
        "excluded semantic targets must not bind elsewhere: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_preserve_exact_local_free_and_method_targets() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "local_caller")?;
    let free = node_id(fragment, "local_value")?;
    let method = node_id(fragment, "measure")?;
    let mut targets: Vec<_> = fragment
        .edges
        .iter()
        .filter(|edge| {
            edge.source == source && edge.kind == EdgeKind::Call && edge.hardness == Hardness::Hard
        })
        .map(|edge| edge.target)
        .collect();
    targets.sort();

    assert_eq!(targets, vec![free, method]);
    Ok(())
}

#[test]
fn should_preserve_nonreceiver_fallback_for_unindexed_source() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "unindexed_caller")?;
    let target = node_id(fragment, "local_value")?;

    assert!(
        fragment.edges.iter().any(|edge| edge.source == source
            && edge.target == target
            && edge.kind == EdgeKind::Call
            && edge.hardness == Hardness::Soft),
        "unresolved free calls retain conservative fallback: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_count_an_associated_call_as_a_reference_to_its_type() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "associated_caller")?;
    let built = fragment
        .nodes
        .iter()
        .find(|node| node.name == "Built" && node.kind == strata_ir::NodeKind::Type)
        .map(|node| node.id)
        .ok_or("missing Built type")?;

    assert!(
        fragment.edges.iter().any(|edge| edge.source == source
            && edge.target == built
            && edge.kind == EdgeKind::TypeReference),
        "`Built::new()` must count as a reference to `Built`: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_count_a_module_qualifier_as_a_type_reference() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "module_caller")?;
    let report = node_id(fragment, "report")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.kind == EdgeKind::TypeReference),
        "`render::report()` names a module, not a type: {:?}",
        fragment.edges
    );
    assert!(
        fragment.edges.iter().any(|edge| edge.source == source
            && edge.target == report
            && edge.kind == EdgeKind::Call),
        "the call itself must still bind: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_name_fall_back_a_qualifier_onto_a_non_type() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "unindexed_module_caller")?;
    let render = node_id(fragment, "render")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == render),
        "an unresolved `render::` qualifier must not bind to the function `render`: {:?}",
        fragment.edges
    );
    Ok(())
}

fn type_id(fragment: &IrFragment, name: &str) -> Result<NodeId, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name == name && node.kind == strata_ir::NodeKind::Type)
        .map(|node| node.id)
        .ok_or_else(|| format!("missing type {name}: {:?}", fragment.nodes))
}

fn references(fragment: &IrFragment, source: NodeId, target: NodeId) -> bool {
    fragment.edges.iter().any(|edge| {
        edge.source == source && edge.target == target && edge.kind == EdgeKind::TypeReference
    })
}

#[test]
fn should_not_name_fall_back_a_qualifier_under_a_foreign_path_root() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "unindexed_foreign_qualifier_caller")?;
    let error = type_id(fragment, "Error")?;

    assert!(
        !references(fragment, source, error),
        "`std::io::Error::other` is rooted outside the workspace and must not bind to the workspace `Error`: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_name_fall_back_a_qualifier_under_a_workspace_path_root() -> Result<(), String> {
    let fragment = fragment()?;
    let crate_rooted = node_id(fragment, "unindexed_crate_qualifier_caller")?;
    let module_rooted = node_id(fragment, "unindexed_module_qualifier_caller")?;
    let built = type_id(fragment, "Built")?;
    let frame = type_id(fragment, "Frame")?;

    assert!(
        references(fragment, crate_rooted, built),
        "`crate::Built::new()` is rooted in the workspace and keeps its fallback: {:?}",
        fragment.edges
    );
    assert!(
        references(fragment, module_rooted, frame),
        "`render::Frame::new()` is rooted in a workspace module and keeps its fallback: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_count_a_function_used_as_a_value_as_a_hard_call() -> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "value_use_caller")?;
    let target = node_id(fragment, "double_value")?;

    assert!(
        fragment.edges.iter().any(|edge| edge.source == source
            && edge.target == target
            && edge.kind == EdgeKind::Call
            && edge.hardness == Hardness::Hard),
        "a fn passed as a value is a hard call: {:?}",
        fragment.edges
    );
    Ok(())
}

#[test]
fn should_not_name_fall_back_a_bare_function_value_rust_analyzer_cannot_resolve()
-> Result<(), String> {
    let fragment = fragment()?;
    let source = node_id(fragment, "unindexed_value_caller")?;
    let target = node_id(fragment, "double_value")?;

    assert!(
        !fragment
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == target),
        "a bare value never binds by name: {:?}",
        fragment.edges
    );
    Ok(())
}
