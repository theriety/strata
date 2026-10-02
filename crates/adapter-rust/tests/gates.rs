//! Coverage for compound cfg predicates, `#[path]` modules and `cfg(not(test))` code.

use std::fs;
use std::path::Path;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, IrFragment, Node, Polarity, SourceFile};

const FILES: [&str; 5] = [
    "src/lib.rs",
    "src/all_tests.rs",
    "src/any_mixed.rs",
    "src/support/cases.rs",
    "src/real.rs",
];

fn fragment() -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gates/app");
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

fn has_edge(fragment: &IrFragment, source: &str, target: &str) -> Result<bool, String> {
    let (source, target) = (node(fragment, source)?.id, node(fragment, target)?.id);
    Ok(fragment
        .edges
        .iter()
        .any(|edge| edge.source == source && edge.target == target))
}

#[test]
fn should_mark_a_module_gated_by_all_test_and_more_as_test_code() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(
        node(&fragment, "all_support")?.polarity,
        Polarity::TestSupport
    );
    Ok(())
}

#[test]
fn should_keep_a_module_gated_by_any_test_or_unix_as_production() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(node(&fragment, "mixed")?.polarity, Polarity::Production);
    Ok(())
}

#[test]
fn should_follow_a_path_attribute_to_a_cfg_test_module_file() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(
        node(&fragment, "path_support")?.polarity,
        Polarity::TestSupport
    );
    assert!(has_edge(&fragment, "path_support", "shared")?);
    Ok(())
}

#[test]
fn should_keep_production_nodes_and_sloc_of_a_cfg_not_test_file() -> Result<(), String> {
    let fragment = fragment()?;

    let drive = node(&fragment, "drive")?;
    assert_eq!(drive.polarity, Polarity::Production);
    assert!(drive.effective_size > 0);
    Ok(())
}
