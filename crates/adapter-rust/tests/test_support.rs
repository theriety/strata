//! Test-support crate detection: unpublished library crates no workspace crate ships.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, SourceFile};

/// Writes a workspace of `crates` (name, manifest extras, has a binary) and
/// returns the roots the adapter states for it.
fn roots(crates: &[(&str, &str, bool)]) -> Result<Vec<String>, String> {
    roots_in("", crates)
}

/// Like [`roots`], with `workspace_extras` appended to the root manifest.
fn roots_in(workspace_extras: &str, crates: &[(&str, &str, bool)]) -> Result<Vec<String>, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "strata-support-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let members: Vec<String> = crates
        .iter()
        .map(|(name, _, _)| format!("\"{name}\""))
        .collect();
    let mut written = vec![(
        "Cargo.toml".to_owned(),
        format!(
            "[workspace]\nresolver = \"2\"\nmembers = [{}]\n{workspace_extras}\n",
            members.join(", ")
        ),
    )];
    let mut sources = Vec::new();
    for (name, extras, binary) in crates {
        written.push((
            format!("{name}/Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extras}\n"
            ),
        ));
        let (entry, body) = if *binary {
            (format!("{name}/src/main.rs"), "fn main() {}\n")
        } else {
            (format!("{name}/src/lib.rs"), "pub fn item() {}\n")
        };
        written.push((entry.clone(), body.to_owned()));
        sources.push(SourceFile {
            path: entry.as_str().into(),
            contents: body.to_owned(),
        });
    }
    for (path, contents) in &written {
        let target: PathBuf = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| error.to_string())?;
    }
    let adapter = RustAdapter::new(root.join("Cargo.toml"));
    let outcome = adapter
        .parse(&sources)
        .and_then(|trees| adapter.bind(trees))
        .map(|fragment| fragment.test_support_roots)
        .map_err(|error| error.to_string());
    let _ = fs::remove_dir_all(&root);
    Ok(outcome?.into_iter().map(|dir| dir.to_string()).collect())
}

#[test]
fn should_mark_an_unpublished_library_no_member_ships_as_test_support() -> Result<(), String> {
    let found = roots(&[("app", "", false), ("harness", "publish = false", false)])?;
    assert_eq!(found, ["harness"]);
    Ok(())
}

#[test]
fn should_not_mark_a_crate_shipped_through_dependencies_or_build_dependencies() -> Result<(), String>
{
    let found = roots(&[
        (
            "app",
            "publish = false\n[dependencies]\nshared = { path = \"../shared\" }\n[build-dependencies]\nbuilt = { path = \"../built\" }",
            false,
        ),
        ("shared", "publish = false", false),
        ("built", "publish = false", false),
    ])?;
    assert_eq!(found, ["app"]);
    Ok(())
}

#[test]
fn should_still_mark_a_crate_only_dev_dependencies_use() -> Result<(), String> {
    let found = roots(&[
        (
            "app",
            "[dev-dependencies]\nharness = { path = \"../harness\" }",
            false,
        ),
        ("harness", "publish = false", false),
    ])?;
    assert_eq!(found, ["harness"]);
    Ok(())
}

#[test]
fn should_not_mark_published_crates_binaries_or_a_lone_crate() -> Result<(), String> {
    let found = roots(&[
        ("app", "", false),
        ("tool", "publish = false", true),
        ("lib", "", false),
    ])?;
    assert!(found.is_empty(), "{found:?}");
    let lone = roots(&[("only", "publish = false", false)])?;
    assert!(lone.is_empty(), "{lone:?}");
    Ok(())
}

#[test]
fn should_flag_nothing_when_every_member_would_be_a_harness() -> Result<(), String> {
    let found = roots(&[
        ("one", "publish = false", false),
        ("two", "publish = false", false),
    ])?;
    assert!(found.is_empty(), "{found:?}");
    Ok(())
}

#[test]
fn should_read_publish_inherited_from_the_workspace_package() -> Result<(), String> {
    let found = roots_in(
        "[workspace.package]\npublish = false",
        &[
            ("app", "", false),
            ("harness", "publish.workspace = true", false),
        ],
    )?;
    assert_eq!(found, ["harness"]);
    let published = roots_in(
        "[workspace.package]\npublish = true",
        &[
            ("app", "", false),
            ("harness", "publish.workspace = true", false),
        ],
    )?;
    assert!(published.is_empty(), "{published:?}");
    Ok(())
}
