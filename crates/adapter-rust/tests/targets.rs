//! Coverage for integration tests, examples and benches as users of the library.
//!
//! These tests pass without the `cfg(test)` loading fix: they are coverage of
//! target handling, not a regression guard for it.

use std::fs;
use std::path::Path;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, IrFragment, Node, Polarity, SourceFile};

const FILES: [&str; 4] = [
    "src/lib.rs",
    "tests/it.rs",
    "examples/demo.rs",
    "benches/speed.rs",
];

fn fragment() -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/targets/app");
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

fn assert_used(fragment: &IrFragment, user: &str, used: &str) -> Result<(), String> {
    let (user, used) = (node(fragment, user)?.id, node(fragment, used)?.id);
    if fragment
        .edges
        .iter()
        .any(|edge| edge.source == user && edge.target == used)
    {
        Ok(())
    } else {
        Err(format!("no edge: {:?}", fragment.edges))
    }
}

#[test]
fn should_count_an_integration_test_as_a_user_of_a_public_item() -> Result<(), String> {
    let fragment = fragment()?;

    assert_used(&fragment, "integration_case", "used_by_integration_test")?;
    assert_eq!(
        node(&fragment, "integration_case")?.polarity,
        Polarity::TestCase
    );
    Ok(())
}

#[test]
fn should_count_an_example_as_a_user_of_a_public_item() -> Result<(), String> {
    assert_used(&fragment()?, "run_example", "used_by_example")
}

#[test]
fn should_count_a_bench_as_a_user_of_a_public_item() -> Result<(), String> {
    assert_used(&fragment()?, "run_bench", "used_by_bench")
}
