//! Coverage for `#[cfg(test)]` code being loaded and resolved semantically.

use std::fs;
use std::path::Path;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, Hardness, IrFragment, Node, Polarity, SourceFile};

const FILES: [&str; 5] = [
    "src/lib.rs",
    "src/a.rs",
    "src/a/checks.rs",
    "src/b.rs",
    "src/c.rs",
];

fn fragment() -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cfg_test/app");
    let files = FILES
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

fn node<'a>(fragment: &'a IrFragment, name: &str) -> Result<&'a Node, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name == name)
        .ok_or_else(|| format!("missing node {name}"))
}

fn assert_semantic_edge(fragment: &IrFragment, source: &str, target: &str) -> Result<(), String> {
    let (source, target) = (node(fragment, source)?.id, node(fragment, target)?.id);
    let found = fragment.edges.iter().any(|edge| {
        edge.source == source
            && edge.target == target
            && edge.confidence >= 1.0
            && edge.hardness == Hardness::Hard
    });
    if found {
        Ok(())
    } else {
        Err(format!("no semantic edge: {:?}", fragment.edges))
    }
}

#[test]
fn should_resolve_an_out_of_line_cfg_test_module_semantically() -> Result<(), String> {
    let fragment = fragment()?;

    assert_semantic_edge(&fragment, "out_of_line_support", "helper")
}

#[test]
fn should_mark_out_of_line_cfg_test_module_declarations_as_test_code() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(
        node(&fragment, "out_of_line_support")?.polarity,
        Polarity::TestSupport
    );
    assert_eq!(
        node(&fragment, "out_of_line_case")?.polarity,
        Polarity::TestCase
    );
    assert_eq!(
        node(&fragment, "unreferenced_support")?.polarity,
        Polarity::TestSupport
    );
    assert_eq!(node(&fragment, "helper")?.polarity, Polarity::Production);
    Ok(())
}

#[test]
fn should_resolve_an_inline_cfg_test_module_semantically() -> Result<(), String> {
    let fragment = fragment()?;

    assert_semantic_edge(&fragment, "inline_case", "shared")
}
