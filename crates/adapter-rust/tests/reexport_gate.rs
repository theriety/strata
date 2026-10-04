//! Coverage for re-exports gated by `cfg(test)` being test code, not production.

use std::fs;
use std::path::Path;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, EdgeKind, IrFragment, Node, Polarity, SourceFile};

const FILES: [&str; 5] = [
    "src/lib.rs",
    "src/facade.rs",
    "src/origin.rs",
    "src/support.rs",
    "src/leak.rs",
];

fn fragment() -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reexport_gate/app");
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

fn re_export<'a>(fragment: &'a IrFragment, name: &str) -> Result<&'a Node, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.re_export && node.name == name)
        .ok_or_else(|| format!("missing re-export node {name}"))
}

#[test]
fn should_treat_a_cfg_test_attributed_re_export_as_test_support() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(
        re_export(&fragment, "attr_item")?.polarity,
        Polarity::TestSupport
    );
    Ok(())
}

#[test]
fn should_treat_a_re_export_inside_an_inline_cfg_test_module_as_test_support() -> Result<(), String>
{
    let fragment = fragment()?;

    assert_eq!(
        re_export(&fragment, "inline_item")?.polarity,
        Polarity::TestSupport
    );
    Ok(())
}

#[test]
fn should_treat_a_re_export_in_an_out_of_line_cfg_test_file_as_test_support() -> Result<(), String>
{
    let fragment = fragment()?;

    assert_eq!(
        re_export(&fragment, "file_item")?.polarity,
        Polarity::TestSupport
    );
    Ok(())
}

#[test]
fn should_keep_an_ungated_re_export_production() -> Result<(), String> {
    let fragment = fragment()?;

    assert_eq!(re_export(&fragment, "prod")?.polarity, Polarity::Production);
    Ok(())
}

#[test]
fn should_keep_an_ungated_re_export_of_test_only_code_as_a_production_to_test_edge()
-> Result<(), String> {
    let fragment = fragment()?;
    let leaked = re_export(&fragment, "leaked")?;
    let target = fragment
        .nodes
        .iter()
        .find(|node| !node.re_export && node.name == "attr_item")
        .ok_or("missing attr_item")?;

    assert_eq!(leaked.polarity, Polarity::Production);
    assert_eq!(target.polarity, Polarity::TestSupport);
    assert!(
        fragment
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::ReExport
                && edge.source == leaked.id
                && edge.target == target.id),
        "edges: {:?}",
        fragment.edges
    );
    Ok(())
}
