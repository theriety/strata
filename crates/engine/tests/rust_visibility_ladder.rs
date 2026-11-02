//! End-to-end ladder-floor regressions for Rust restricted visibility (ADR-24).
//!
//! `pub(super)` is the narrowest spelling that compiles for an item used from a
//! module outside its own, even when the laminar tree places both modules in
//! one folder; the analysis must never advise narrowing it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Writes a throwaway crate holding `files` and returns its root.
fn write_crate(files: &[(&str, &str)]) -> Result<PathBuf, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "strata-ladder-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let manifest = "[package]\nname = \"ladder\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    for (path, contents) in files
        .iter()
        .chain(&[("Cargo.toml", manifest)])
        .map(|(path, contents)| (*path, *contents))
    {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| error.to_string())?;
    }
    Ok(root)
}

/// Returns every visibility advice line the analysis states for the crate.
fn visibility_advice(root: &Path) -> Result<Vec<String>, String> {
    let config = AnalyzeConfig::default();
    let snapshot = snapshot_from_root(root, &config).map_err(|error| error.to_string())?;
    let result = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?;
    Ok(json
        .lines()
        .filter(|line| line.contains("exported at"))
        .map(str::to_owned)
        .collect())
}

#[test]
fn should_not_advise_narrowing_a_pub_super_item_used_by_the_parent_definition_file()
-> Result<(), String> {
    // parent-definition-file case: `lines` lives in `render/report.rs` and is
    // used by `render.rs`, the file that also owns the `render/` folder.
    let root = write_crate(&[
        (
            "src/lib.rs",
            "mod render;\nfn go() -> usize {\n    render::run()\n}\n",
        ),
        (
            "src/render.rs",
            "mod report;\nmod tree;\npub(super) fn run() -> usize {\n    report::lines() + tree::depth()\n}\n",
        ),
        (
            "src/render/report.rs",
            "pub(super) fn lines() -> usize {\n    1\n}\n",
        ),
        (
            "src/render/tree.rs",
            "pub(super) fn depth() -> usize {\n    2\n}\n",
        ),
    ])?;
    let advice = visibility_advice(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(advice, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_not_advise_narrowing_a_pub_super_item_used_by_a_sibling_file() -> Result<(), String> {
    // sibling-file case: `settle` lives in `relocation/collision.rs` and is used
    // by `relocation/solver.rs`; the parent `relocation.rs` owns the folder.
    let root = write_crate(&[
        (
            "src/lib.rs",
            "mod relocation;\nfn go() -> usize {\n    relocation::run()\n}\n",
        ),
        (
            "src/relocation.rs",
            "mod collision;\nmod solver;\npub(super) fn run() -> usize {\n    solver::solve()\n}\n",
        ),
        (
            "src/relocation/collision.rs",
            "pub(super) fn settle() -> usize {\n    1\n}\n",
        ),
        (
            "src/relocation/solver.rs",
            "pub(super) fn solve() -> usize {\n    super::collision::settle()\n}\n",
        ),
    ])?;
    let advice = visibility_advice(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(advice, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_not_advise_narrowing_a_crate_root_child_item_used_by_the_crate_root() -> Result<(), String>
{
    // crate-root-child case: `parse` lives in `src/parse.rs` (which owns
    // `src/parse/`) and is used by `lib.rs`.
    let root = write_crate(&[
        (
            "src/lib.rs",
            "mod parse;\nfn go() -> usize {\n    parse::parse()\n}\n",
        ),
        (
            "src/parse.rs",
            "mod walk;\npub(super) fn parse() -> usize {\n    walk::step()\n}\n",
        ),
        (
            "src/parse/walk.rs",
            "pub(super) fn step() -> usize {\n    1\n}\n",
        ),
    ])?;
    let advice = visibility_advice(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(advice, Vec::<String>::new());
    Ok(())
}

#[test]
fn should_still_advise_narrowing_a_pub_super_item_used_only_inside_its_own_module()
-> Result<(), String> {
    // `helper` is `pub(super)` in `parse/walk.rs` yet only `walk.rs` uses it,
    // so private is expressible and strictly narrower.
    let root = write_crate(&[
        (
            "src/lib.rs",
            "mod parse;\nfn go() -> usize {\n    parse::parse()\n}\n",
        ),
        (
            "src/parse.rs",
            "mod walk;\npub(super) fn parse() -> usize {\n    walk::step()\n}\n",
        ),
        (
            "src/parse/walk.rs",
            "pub(super) fn step() -> usize {\n    helper()\n}\n\npub(super) fn helper() -> usize {\n    1\n}\n",
        ),
    ])?;
    let advice = visibility_advice(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert!(
        advice.iter().any(|line| line.contains("`helper`")),
        "helper must still be flagged: {advice:?}"
    );
    assert!(
        advice.iter().all(|line| !line.contains("`step`")),
        "step is used by its parent and must not be flagged: {advice:?}"
    );
    Ok(())
}
