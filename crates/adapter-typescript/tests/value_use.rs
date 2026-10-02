//! Pins that a function passed as a value already counts as a dependency.

use smol_str::SmolStr;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{Adapter, Hardness, IrFragment, NodeId, SourceFile};

fn bind_inline(contents: &str) -> Result<IrFragment, String> {
    let adapter = TypeScriptAdapter::new(".");
    let files = [SourceFile {
        path: SmolStr::new("src/values.ts"),
        contents: contents.to_string(),
    }];
    let trees = adapter
        .parse(&files)
        .map_err(|error| format!("inline source did not parse: {error}"))?;

    adapter
        .bind(trees)
        .map_err(|error| format!("inline source did not bind: {error}"))
}

fn node_id(fragment: &IrFragment, name: &str) -> Result<NodeId, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name == name)
        .map_or_else(|| Err(format!("expected node {name}")), |node| Ok(node.id))
}

#[test]
fn should_count_a_function_passed_as_a_value_as_a_hard_edge() -> Result<(), String> {
    let fragment = bind_inline(
        "export function double(value: number): number { return value * 2; }\n\
         export function run(items: number[]): number[] { return items.map(double); }\n",
    )?;
    let source = node_id(&fragment, "run")?;
    let target = node_id(&fragment, "double")?;

    let hard_edge = fragment.edges.iter().any(|edge| {
        edge.source == source && edge.target == target && edge.hardness == Hardness::Hard
    });

    assert!(
        hard_edge,
        "a fn passed as a value is a hard edge: {:?}",
        fragment.edges
    );
    Ok(())
}
