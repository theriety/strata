//! Regression coverage for restricted-visibility sidecars beside `cfg`-disabled modules.

use std::fs;
use std::path::Path;

use smol_str::SmolStr;
use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, IrFragment, SourceFile};

/// Parses and binds the `scope/app` fixture, whose `parse` module holds
/// `cfg`-disabled children beside a doc-commented `pub(super)` item.
///
/// The fixture relies on the loader leaving `cfg(test)` off: `tests`,
/// `extra_tests` and the feature-gated `gated` module have no semantic module,
/// so a scope surviving beside them proves the syntactic and semantic child
/// lists are compared after skipping them.
fn bind_fixture() -> Result<IrFragment, String> {
    bind_fixture_files(&[
        "src/lib.rs",
        "src/parse.rs",
        "src/parse/refs.rs",
        "src/parse/tests.rs",
    ])
}

/// Parses and binds the `scope/app` fixture restricted to the given files.
fn bind_fixture_files(paths: &[&str]) -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope/app");
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

#[test]
fn should_emit_a_scope_for_a_pub_super_item_beside_cfg_disabled_modules() -> Result<(), String> {
    // also covers the feature-gated `gated` module: exactly one scope remains.
    let fragment = bind_fixture()?;
    assert_eq!(fragment.visibility_scopes.len(), 1);
    let scoped: Vec<&str> = fragment
        .visibility_scopes
        .iter()
        .flat_map(|scope| scope.files.iter().map(SmolStr::as_str))
        .collect();
    assert_eq!(
        scoped,
        ["src/parse.rs", "src/parse/refs.rs"],
        "scopes: {:?}",
        fragment.visibility_scopes
    );
    Ok(())
}

#[test]
fn should_emit_a_scope_for_an_item_whose_span_starts_on_a_doc_comment() -> Result<(), String> {
    // `refs::helper` begins with a `///` line, so its recorded offset lands on
    // trivia; the lookup must still resolve the enclosing module.
    let fragment = bind_fixture()?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no visibility scope emitted")?;
    let node = fragment
        .nodes
        .iter()
        .find(|node| node.id == scope.node)
        .ok_or("scope node missing from fragment")?;
    assert_eq!(node.name.as_str(), "helper");
    Ok(())
}

#[test]
fn should_state_the_definition_file_for_a_pub_super_scope() -> Result<(), String> {
    let fragment = bind_fixture()?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no pub(super) scope emitted")?;
    // `helper` is `pub(super)` inside `refs`, so the target is `parse`, defined
    // in `src/parse.rs` beside the `src/parse/` folder.
    assert_eq!(scope.definition_file.as_deref(), Some("src/parse.rs"));
    Ok(())
}

#[test]
fn should_not_state_a_definition_file_for_a_crate_root_scope() -> Result<(), String> {
    // `pub(super)` in a top-level module targets the crate root, the same
    // crate-wide scope a `pub(crate)` item has; `pub(crate)` itself never
    // reaches the sidecar (the parser classifies it as `VisibilityKind::Crate`).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope/crate_scope");
    let paths = ["src/lib.rs", "src/inner.rs"];
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
    let fragment = adapter.bind(trees).map_err(|error| error.to_string())?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no crate-root scope emitted")?;
    assert_eq!(scope.files, ["src/inner.rs", "src/lib.rs"]);
    assert_eq!(scope.definition_file, None);
    Ok(())
}

#[test]
fn should_not_state_a_definition_file_for_a_crate_root_that_owns_a_matching_folder()
-> Result<(), String> {
    // `src/lib.rs` pulls `src/lib/x.rs` in via `#[path]`, so the folder filter
    // (`src/lib/`) would pass; only the crate-root guard keeps the definition
    // file unstated.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope/crate_folder");
    let paths = ["src/lib.rs", "src/lib/x.rs"];
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
    let fragment = adapter.bind(trees).map_err(|error| error.to_string())?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no crate-root scope emitted")?;
    assert_eq!(scope.files, ["src/lib.rs", "src/lib/x.rs"]);
    assert_eq!(scope.definition_file, None);
    Ok(())
}

#[test]
fn should_state_no_rung_between_a_crate_root_child_and_the_crate() -> Result<(), String> {
    // `inner::item` is `pub(super)` under the crate root: the only spellings
    // are private (`src/inner.rs`) and the crate itself, so the ladder has no
    // rung narrower than the crate that still reaches `src/lib.rs`.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope/crate_scope");
    let paths = ["src/lib.rs", "src/inner.rs"];
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
    let fragment = adapter.bind(trees).map_err(|error| error.to_string())?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no crate-root scope emitted")?;
    let rungs: Vec<Vec<&str>> = scope
        .expressible
        .iter()
        .map(|rung| rung.files.iter().map(SmolStr::as_str).collect())
        .collect();
    assert_eq!(
        rungs,
        [vec!["src/inner.rs"], vec!["src/inner.rs", "src/lib.rs"]]
    );
    Ok(())
}

#[test]
fn should_state_every_enclosing_module_as_a_rung_for_a_nested_pub_super_item() -> Result<(), String>
{
    // `refs::helper` is `pub(super)`: private (`refs`), `parse`, crate root.
    let fragment = bind_fixture()?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no pub(super) scope emitted")?;
    let sizes: Vec<usize> = scope
        .expressible
        .iter()
        .map(|rung| rung.files.len())
        .collect();
    assert_eq!(sizes, [1, 2, 3]);
    Ok(())
}

#[test]
fn should_state_no_ladder_when_an_enclosing_rung_is_not_analyzed() -> Result<(), String> {
    // without `src/lib.rs` among the analyzed files the crate-root rung cannot
    // be resolved, so the whole ladder is dropped rather than left with a gap.
    let fragment = bind_fixture_files(&["src/parse.rs", "src/parse/refs.rs"])?;
    let scope = fragment
        .visibility_scopes
        .first()
        .ok_or("no pub(super) scope emitted")?;
    assert!(scope.expressible.is_empty());
    Ok(())
}
