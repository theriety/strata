//! FIX09 diagnostic (temporary): dumps the naming-drift IR graph — every node's
//! container and every edge with its kind — so synthesis pricing is measurable
//! instead of inferred.

#![allow(clippy::print_stdout)] // diagnostic binary; the workspace deny targets library code

use strata_engine::config::{AnalyzeConfig, Mode};
use strata_engine::snapshot_from_root;
use strata_eval::{eval_fixture_root, load_target};

fn main() {
    if let Err(error) = run() {
        println!("probe failed: {error}");
    }
}

fn run() -> Result<(), String> {
    let spec = load_target("naming-drift").map_err(|error| format!("naming-drift: {error}"))?;
    let mut config = AnalyzeConfig::default();
    config.analysis.mode = Mode::Both;
    config.analysis.candidates = spec.run.candidates;
    config.analysis.seed = spec.run.seed;
    let root = eval_fixture_root("naming-drift");
    let snapshot =
        snapshot_from_root(&root, &config).map_err(|error| format!("naming-drift: {error}"))?;
    let ir = snapshot.ir();

    let name_of: std::collections::BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();
    println!("== nodes (id, kind, polarity, container, name) ==");
    for node in &ir.nodes {
        println!(
            "  {:>3} {:?} {:?} in {} :: {}",
            node.id.0,
            node.kind,
            node.polarity,
            name_of.get(&node.container.0).map_or("?", String::as_str),
            node.name
        );
    }
    println!("== edges (source -> target, hardness) ==");
    for edge in &ir.edges {
        println!(
            "  {} -> {} ({:?})",
            edge.source.0, edge.target.0, edge.hardness
        );
    }
    Ok(())
}
