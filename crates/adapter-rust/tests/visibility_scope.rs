//! Regression coverage for restricted-visibility sidecars beside `cfg`-disabled modules.

use std::fs;
use std::path::Path;

use smol_str::SmolStr;
use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, IrFragment, SourceFile, VisibilityScope};

/// Parses and binds the `scope/app` fixture, whose `parse` module holds
/// `cfg`-disabled children beside a doc-commented `pub(super)` item.
///
/// The loader enables `cfg(test)`: `tests` is a live child module, while
/// `prod_only` (`cfg(not(test))`) and the feature-gated `gated` module have no
/// semantic module, so a scope surviving beside them proves the syntactic and
/// semantic child lists are compared after skipping them.
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
    bind_scope_fixture("app", paths)
}

/// Parses and binds one `scope/<fixture>` crate restricted to the given files.
fn bind_scope_fixture(fixture: &str, paths: &[&str]) -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scope")
        .join(fixture);
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

/// The scope statement of the declaration called `name`.
fn scope_named<'a>(fragment: &'a IrFragment, name: &str) -> Result<&'a VisibilityScope, String> {
    let node = fragment
        .nodes
        .iter()
        .find(|node| node.name.as_str() == name)
        .ok_or_else(|| format!("no node named {name}"))?;
    fragment
        .visibility_scopes
        .iter()
        .find(|scope| scope.node == node.id)
        .ok_or_else(|| format!("no scope stated for {name}"))
}

/// The files of each rung of `scope`.
fn rung_files(scope: &VisibilityScope) -> Vec<Vec<&str>> {
    scope
        .expressible
        .iter()
        .map(|rung| rung.files.iter().map(SmolStr::as_str).collect())
        .collect()
}

#[test]
fn should_emit_a_scope_for_a_pub_super_item_beside_cfg_disabled_modules() -> Result<(), String> {
    // also covers the feature-gated `gated` module: the restricted scope remains,
    // and the live `cfg(test)` child `tests` belongs to the `parse` subtree.
    let fragment = bind_fixture()?;
    let scope = scope_named(&fragment, "helper")?;
    let scoped: Vec<&str> = scope.files.iter().map(SmolStr::as_str).collect();
    assert_eq!(
        scoped,
        ["src/parse.rs", "src/parse/refs.rs", "src/parse/tests.rs"],
        "scope: {scope:?}"
    );
    assert_eq!(scope.declared_rung, Some(1));
    Ok(())
}

#[test]
fn should_emit_a_scope_for_an_item_whose_span_starts_on_a_doc_comment() -> Result<(), String> {
    // `refs::helper` begins with a `///` line, so its recorded offset lands on
    // trivia; the lookup must still resolve the enclosing module.
    let fragment = bind_fixture()?;
    let scope = scope_named(&fragment, "helper")?;
    assert!(scope.declared_rung.is_some() && !scope.expressible.is_empty());
    Ok(())
}

#[test]
fn should_state_the_definition_file_for_a_pub_super_scope() -> Result<(), String> {
    let fragment = bind_fixture()?;
    // `helper` is `pub(super)` inside `refs`, so the target is `parse`, defined
    // in `src/parse.rs` beside the `src/parse/` folder.
    let scope = scope_named(&fragment, "helper")?;
    assert_eq!(scope.definition_file.as_deref(), Some("src/parse.rs"));
    Ok(())
}

#[test]
fn should_not_state_a_definition_file_for_a_crate_root_scope() -> Result<(), String> {
    // `pub(super)` in a top-level module targets the crate root, the same
    // crate-wide scope a `pub(crate)` item has.
    let fragment = bind_scope_fixture("crate_scope", &["src/lib.rs", "src/inner.rs"])?;
    let scope = scope_named(&fragment, "go")?;
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
    let fragment = bind_scope_fixture("crate_folder", &["src/lib.rs", "src/lib/x.rs"])?;
    let scope = scope_named(&fragment, "go")?;
    assert_eq!(scope.files, ["src/lib.rs", "src/lib/x.rs"]);
    assert_eq!(scope.definition_file, None);
    Ok(())
}

#[test]
fn should_state_no_rung_between_a_crate_root_child_and_the_crate() -> Result<(), String> {
    // `inner::go` is `pub(super)` under the crate root: the only spellings are
    // private (`src/inner.rs`) and the crate itself.
    let fragment = bind_scope_fixture("crate_scope", &["src/lib.rs", "src/inner.rs"])?;
    let scope = scope_named(&fragment, "go")?;
    assert_eq!(
        rung_files(scope),
        [vec!["src/inner.rs"], vec!["src/inner.rs", "src/lib.rs"]]
    );
    assert_eq!(scope.declared_rung, Some(1));
    Ok(())
}

#[test]
fn should_state_every_enclosing_module_as_a_rung_for_a_nested_pub_super_item() -> Result<(), String>
{
    // `refs::helper` is `pub(super)`: private (`refs`), `parse`, crate root.
    let fragment = bind_fixture()?;
    let sizes: Vec<usize> = scope_named(&fragment, "helper")?
        .expressible
        .iter()
        .map(|rung| rung.files.len())
        .collect();
    assert_eq!(sizes, [1, 3, 4]);
    Ok(())
}

#[test]
fn should_spell_each_rung_the_way_rust_writes_it() -> Result<(), String> {
    let fragment = bind_fixture()?;
    let spellings: Vec<Option<&str>> = scope_named(&fragment, "helper")?
        .expressible
        .iter()
        .map(|rung| rung.spelling.as_deref())
        .collect();
    assert_eq!(
        spellings,
        [
            Some("private (no `pub`)"),
            Some("`pub(super)`"),
            Some("`pub(crate)`")
        ]
    );
    Ok(())
}

#[test]
fn should_state_a_scope_for_a_plain_pub_item_without_restating_its_files() -> Result<(), String> {
    // `run` is plain `pub`: its declared rung is the crate root, and no `files`
    // label is stated, so the engine keeps the level the parser declared.
    let fragment = bind_fixture()?;
    let scope = scope_named(&fragment, "run")?;
    assert_eq!(scope.files, Vec::<SmolStr>::new());
    assert_eq!(
        scope.declared_rung,
        u32::try_from(scope.expressible.len().saturating_sub(1)).ok()
    );
    Ok(())
}

#[test]
fn should_leave_an_unanalyzed_crate_rung_out_and_keep_the_declared_rung() -> Result<(), String> {
    // without `src/lib.rs` among the analyzed files the crate-root rung cannot
    // be resolved; it is dropped, and `parse` stays the declared rung.
    let fragment =
        bind_fixture_files(&["src/parse.rs", "src/parse/refs.rs", "src/parse/tests.rs"])?;
    let scope = scope_named(&fragment, "helper")?;
    assert_eq!(scope.expressible.len(), 2);
    assert_eq!(scope.declared_rung, Some(1));
    Ok(())
}

#[test]
fn should_state_an_unresolved_scope_with_no_declared_rung() -> Result<(), String> {
    // `go` in `parse` is plain `pub`, so its declared rung is the crate root;
    // without `src/lib.rs` that rung cannot resolve, and the entry is stated
    // unresolved rather than omitted.
    let fragment =
        bind_fixture_files(&["src/parse.rs", "src/parse/refs.rs", "src/parse/tests.rs"])?;
    let scope = scope_named(&fragment, "go")?;
    assert_eq!(scope.declared_rung, None);
    assert_eq!(scope.files, Vec::<SmolStr>::new());
    Ok(())
}
