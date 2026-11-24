//! Rust file-import facts from `mod` declarations, `use` items and qualified
//! paths (A4).

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use unlace_adapter_rust::RustAdapter;
use unlace_ir::{Adapter, FileImport, ImportKind, IrFragment, SourceFile};

const APP: &str = "crates/app/src";
const APP_FILES: [&str; 6] = [
    "crates/app/src/lib.rs",
    "crates/app/src/alpha.rs",
    "crates/app/src/alpha/helper.rs",
    "crates/app/src/beta/mod.rs",
    "crates/app/src/custom/gamma_impl.rs",
    "crates/app/src/inline_host/nested.rs",
];
const DEP_FILES: [&str; 2] = ["crates/dep/src/lib.rs", "crates/dep/src/shared.rs"];

fn fragment(fixture: &str, paths: &[&str]) -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/imports")
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

/// The app fixture bound once per file set: `full` adds the dependency crate.
fn app_fragment(full: bool) -> Result<&'static IrFragment, String> {
    static ALONE: OnceLock<Result<IrFragment, String>> = OnceLock::new();
    static FULL: OnceLock<Result<IrFragment, String>> = OnceLock::new();
    let (cell, extra): (_, &[&str]) = if full {
        (&FULL, &DEP_FILES)
    } else {
        (&ALONE, &[])
    };
    cell.get_or_init(|| {
        let paths: Vec<&str> = APP_FILES.iter().chain(extra).copied().collect();
        fragment("app", &paths)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `(importer, imported, specifier, line)` of every fact, with the app source
/// directory abbreviated to `~`.
fn summary(fragment: &IrFragment) -> Vec<(String, String, String, u32)> {
    let short = |path: &str| path.replace(APP, "~");
    fragment
        .imports
        .iter()
        .map(|import| {
            (
                short(&import.importer),
                short(&import.imported),
                import.specifier.to_string(),
                import.line,
            )
        })
        .collect()
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items
}

fn fact(
    importer: &str,
    imported: &str,
    specifier: &str,
    line: u32,
) -> (String, String, String, u32) {
    (
        importer.to_owned(),
        imported.to_owned(),
        specifier.to_owned(),
        line,
    )
}

/// The summary of the facts of `kind` only.
fn of_kind(fragment: &IrFragment, kind: ImportKind) -> Vec<(String, String, String, u32)> {
    let only = IrFragment {
        imports: fragment
            .imports
            .iter()
            .filter(|import| import.kind == kind)
            .cloned()
            .collect(),
        ..IrFragment::default()
    };
    summary(&only)
}

#[test]
fn should_state_a_module_declaration_for_each_mod_that_resolves_to_a_file() -> Result<(), String> {
    let modules = of_kind(app_fragment(false)?, ImportKind::ModuleDeclaration);

    assert_eq!(
        modules,
        vec![
            fact("~/alpha.rs", "~/alpha/helper.rs", "helper", 1),
            fact("~/lib.rs", "~/alpha.rs", "alpha", 1),
            fact("~/lib.rs", "~/beta/mod.rs", "beta", 2),
            // `#[path]` is honoured; the line is the `mod` keyword's, not the attribute's.
            fact("~/lib.rs", "~/custom/gamma_impl.rs", "gamma", 4),
            // a file module declared inside an inline module.
            fact("~/lib.rs", "~/inline_host/nested.rs", "nested", 7),
        ]
    );
    Ok(())
}

#[test]
fn should_state_every_use_form_as_a_value_import_of_the_defining_file() -> Result<(), String> {
    let fragment = app_fragment(true)?;
    let found = sorted(summary(fragment));

    assert_eq!(
        found,
        sorted(vec![
            fact("~/alpha.rs", "~/alpha/helper.rs", "helper", 1),
            fact("~/alpha/helper.rs", "~/alpha.rs", "super::*", 1),
            fact(
                "~/alpha/helper.rs",
                "~/beta/mod.rs",
                "super::super::beta::beta_fn",
                2
            ),
            fact("~/beta/mod.rs", "~/alpha.rs", "crate::alpha::Alpha", 1),
            fact("~/custom/gamma_impl.rs", "~/alpha.rs", "crate::alpha::*", 1),
            fact("~/lib.rs", "~/alpha.rs", "alpha", 1),
            fact("~/lib.rs", "~/beta/mod.rs", "beta", 2),
            fact("~/lib.rs", "~/custom/gamma_impl.rs", "gamma", 4),
            fact("~/lib.rs", "~/inline_host/nested.rs", "nested", 7),
            fact("~/lib.rs", "~/alpha.rs", "crate::alpha::Alpha", 10),
            fact(
                "~/lib.rs",
                "~/alpha/helper.rs",
                "crate::alpha::helper::Assist",
                11
            ),
            fact("~/lib.rs", "~/beta/mod.rs", "crate::beta::beta_fn", 12),
            fact(
                "~/lib.rs",
                "~/custom/gamma_impl.rs",
                "crate::gamma::gamma_fn",
                12
            ),
            fact(
                "~/lib.rs",
                "~/inline_host/nested.rs",
                "self::inline_host::nested::nested_fn",
                13
            ),
            fact("~/lib.rs", "~/alpha.rs", "crate::beta::Reexported", 14),
            fact(
                "~/lib.rs",
                "crates/dep/src/shared.rs",
                "imports_dep::shared::Shared",
                16
            ),
            fact(
                "crates/dep/src/lib.rs",
                "crates/dep/src/shared.rs",
                "shared",
                1
            ),
        ])
    );
    // every fact but the five `mod` declarations of the app crate and the one of
    // the dependency crate binds names.
    assert_eq!(
        fragment
            .imports
            .iter()
            .filter(|import| import.kind != ImportKind::Value)
            .count(),
        6
    );
    Ok(())
}

#[test]
fn should_state_nothing_for_an_external_crate_or_a_file_outside_the_analysed_set()
-> Result<(), String> {
    let found = summary(app_fragment(false)?);

    // `use std::collections::HashMap` (line 17), `use crate::omitted::Omitted`
    // (line 15, a file not passed in) and the dependency crate's file (not passed
    // in) produce no fact, and `mod omitted;` (line 5) none either.
    let lib_lines: Vec<u32> = sorted(
        found
            .iter()
            .filter(|(importer, ..)| importer == "~/lib.rs")
            .map(|(.., line)| *line)
            .collect(),
    );
    assert_eq!(lib_lines, vec![1, 2, 4, 7, 10, 11, 12, 12, 13, 14]);
    Ok(())
}

#[test]
fn should_state_a_path_through_another_analysed_workspace_crate_as_that_crates_file()
-> Result<(), String> {
    let with_dep = summary(app_fragment(true)?);
    let without_dep = summary(app_fragment(false)?);

    assert!(with_dep.contains(&fact(
        "~/lib.rs",
        "crates/dep/src/shared.rs",
        "imports_dep::shared::Shared",
        16
    )));
    assert!(with_dep.contains(&fact(
        "crates/dep/src/lib.rs",
        "crates/dep/src/shared.rs",
        "shared",
        1
    )));
    assert!(
        !without_dep
            .iter()
            .any(|(_, imported, ..)| imported.contains("dep"))
    );
    Ok(())
}

#[test]
fn should_state_one_fact_per_statement_per_target_file_even_for_a_group_reaching_two()
-> Result<(), String> {
    let found = summary(app_fragment(false)?);

    let line_11 = found.iter().filter(|(_, _, _, line)| *line == 11).count();
    let line_12: Vec<_> = found
        .iter()
        .filter(|(importer, _, _, line)| importer == "~/lib.rs" && *line == 12)
        .map(|(_, imported, ..)| imported.as_str())
        .collect();
    // `{Assist, assist}` reaches one file once; `{beta::beta_fn, gamma::gamma_fn}` two.
    assert_eq!(line_11, 1);
    assert_eq!(line_12, vec!["~/beta/mod.rs", "~/custom/gamma_impl.rs"]);
    Ok(())
}

#[test]
fn should_name_files_exactly_as_the_file_containers_do() -> Result<(), String> {
    let fragment = app_fragment(true)?;

    let containers: Vec<&str> = fragment
        .containers
        .iter()
        .map(|container| container.name.as_str())
        .collect();
    for FileImport {
        importer, imported, ..
    } in &fragment.imports
    {
        assert!(containers.contains(&importer.as_str()), "{importer}");
        assert!(containers.contains(&imported.as_str()), "{imported}");
    }
    Ok(())
}

#[test]
fn should_state_imports_in_a_stable_sorted_order() -> Result<(), String> {
    let first = app_fragment(true)?.imports.clone();
    let second = fragment(
        "app",
        &[APP_FILES.as_slice(), DEP_FILES.as_slice()].concat(),
    )?
    .imports;

    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted);
    assert_eq!(first, second);
    Ok(())
}

#[test]
fn should_claim_to_state_imports_when_every_non_external_import_resolves() -> Result<(), String> {
    let fragment = app_fragment(true)?;

    assert!(fragment.states_imports);
    Ok(())
}

#[test]
fn should_not_claim_to_state_imports_when_a_use_does_not_resolve() -> Result<(), String> {
    let fragment = fragment(
        "unresolved",
        &["src/lib.rs", "src/present.rs", "src/off.rs"],
    )?;

    // `use crate::off::Hidden` sits under a cfg the loader leaves off (the A4
    // failure class: a use that no semantic definition backs).
    assert!(!fragment.states_imports);
    assert!(
        fragment
            .imports
            .iter()
            .any(|import| import.specifier == "crate::present::Present")
    );
    Ok(())
}

const PATHS_APP: &str = "crates/app/src";
const PATHS_FILES: [&str; 6] = [
    "crates/app/src/lib.rs",
    "crates/app/src/a.rs",
    "crates/app/src/child.rs",
    "crates/app/src/t.rs",
    "crates/app/src/tests.rs",
    "crates/other/src/lib.rs",
];

/// The qualified-path fixture, bound once.
fn paths_fragment() -> Result<&'static IrFragment, String> {
    static PATHS: OnceLock<Result<IrFragment, String>> = OnceLock::new();
    PATHS
        .get_or_init(|| fragment("paths", &PATHS_FILES))
        .as_ref()
        .map_err(Clone::clone)
}

/// The facts of `kind` in the qualified-path fixture, with its app source
/// directory abbreviated to `~`.
fn paths_of_kind(kind: ImportKind) -> Result<Vec<(String, String, String, u32)>, String> {
    let found = of_kind(paths_fragment()?, kind);
    Ok(found
        .into_iter()
        .map(|(importer, imported, specifier, line)| {
            (
                importer.replace(PATHS_APP, "~"),
                imported.replace(PATHS_APP, "~"),
                specifier,
                line,
            )
        })
        .collect())
}

#[test]
fn should_state_a_module_declaration_for_a_cfg_test_mod_too() -> Result<(), String> {
    let modules = paths_of_kind(ImportKind::ModuleDeclaration)?;

    assert_eq!(
        modules,
        vec![
            fact("~/lib.rs", "~/a.rs", "a", 1),
            fact("~/lib.rs", "~/child.rs", "child", 2),
            fact("~/lib.rs", "~/t.rs", "t", 3),
            fact("~/lib.rs", "~/tests.rs", "tests", 5),
        ]
    );
    Ok(())
}

#[test]
fn should_state_a_value_import_for_each_qualified_path_into_another_file_of_the_crate()
-> Result<(), String> {
    let values = paths_of_kind(ImportKind::Value)?;

    assert_eq!(
        sorted(values),
        sorted(vec![
            // a single-segment `use child as kid`.
            fact("~/lib.rs", "~/child.rs", "child", 7),
            // `child::run()` with no `use`; the child's `use super::Config`
            // closes the cycle it opens.
            fact("~/lib.rs", "~/child.rs", "child", 12),
            fact("~/child.rs", "~/lib.rs", "super::Config", 1),
            // through the `kid` alias.
            fact("~/lib.rs", "~/child.rs", "kid", 13),
            // `crate::a::f()` with no `use`.
            fact("~/lib.rs", "~/a.rs", "crate::a", 14),
            // `a::f(); crate::a::g();` on one line: one fact, the first spelling.
            fact("~/lib.rs", "~/a.rs", "a", 18),
            // a parameter type and two patterns.
            fact("~/lib.rs", "~/a.rs", "a", 20),
            fact("~/lib.rs", "~/a.rs", "a", 22),
            fact("~/lib.rs", "~/a.rs", "a", 23),
            // a type position, `config: super::Config`.
            fact("~/t.rs", "~/lib.rs", "super", 4),
            // `impl crate::t::Tr for`.
            fact("~/a.rs", "~/t.rs", "crate::t", 12),
        ])
    );
    Ok(())
}

#[test]
fn should_state_nothing_for_std_another_crate_a_macro_body_the_own_file_or_test_code()
-> Result<(), String> {
    let fragment = paths_fragment()?;
    let values = paths_of_kind(ImportKind::Value)?;

    // `std::mem` (28), `paths_other::shared()` (29, another analysed crate),
    // `crate::t::shown()` in `println!` (30), `self::inner::here()` (34, its
    // own file), the inline `cfg(test)` module (43, 46) and `tests.rs`, reached
    // through `#[cfg(test)] mod tests;`, state no import.
    let lib_lines: Vec<u32> = values
        .iter()
        .filter(|(importer, ..)| importer == "~/lib.rs")
        .map(|(.., line)| *line)
        .collect();
    assert!(lib_lines.iter().all(|line| *line < 28), "{lib_lines:?}");
    assert!(
        !values.iter().any(|(importer, ..)| importer == "~/tests.rs"),
        "{values:?}"
    );
    assert!(
        !values
            .iter()
            .any(|(_, imported, ..)| imported.starts_with("crates/other")),
        "{values:?}"
    );
    // none of them is an unresolved import statement.
    assert!(fragment.states_imports);
    Ok(())
}
