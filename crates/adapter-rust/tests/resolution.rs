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
        .get_or_init(|| bind_fixture(&["src/lib.rs", "src/unindexed.rs"]))
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
