//! `Node.public_api` and `Node.advice_exempt` for code the loader may drop
//! (ADR-25 rules 7 and 8).

use std::fs;
use std::path::Path;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, IrFragment, SourceFile};

fn fragment(fixture: &str, paths: &[&str]) -> Result<IrFragment, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
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

fn dropped() -> Result<IrFragment, String> {
    fragment(
        "dropped/app",
        &["src/lib.rs", "src/inner.rs", "src/main.rs", "src/shared.rs"],
    )
}

/// Which of `public_api` and `advice_exempt` a node carries.
fn flags(fragment: &IrFragment, name: &str) -> Result<(bool, bool), String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name == name)
        .map(|node| (node.public_api, node.advice_exempt))
        .ok_or_else(|| format!("missing node {name}"))
}

#[test]
fn should_exempt_the_target_of_a_dropped_pub_use_without_calling_it_public_api()
-> Result<(), String> {
    assert_eq!(flags(&dropped()?, "Exported")?, (false, true));
    Ok(())
}

#[test]
fn should_keep_a_file_shared_with_a_binary_a_library_file() -> Result<(), String> {
    assert!(flags(&dropped()?, "gated")?.0);
    Ok(())
}

fn exact() -> Result<IrFragment, String> {
    fragment(
        "globs/exact",
        &["src/lib.rs", "src/a.rs", "src/a/sub.rs", "src/b.rs"],
    )
}

#[test]
fn should_exempt_every_visibility_of_the_module_behind_a_dropped_glob_and_its_subtree()
-> Result<(), String> {
    let fragment = exact()?;

    assert!(flags(&fragment, "helper")?.1);
    assert!(flags(&fragment, "nested")?.1);
    Ok(())
}

#[test]
fn should_leave_a_module_the_dropped_glob_does_not_name_advisable() -> Result<(), String> {
    assert!(!flags(&exact()?, "other")?.1);
    Ok(())
}

#[test]
fn should_exempt_the_whole_crate_when_the_glob_names_an_inline_module() -> Result<(), String> {
    let fragment = fragment("globs/inline", &["src/lib.rs", "src/b.rs"])?;

    assert!(flags(&fragment, "inner")?.1);
    assert!(flags(&fragment, "other")?.1);
    Ok(())
}

#[test]
fn should_exempt_the_whole_crate_when_the_globbed_subtree_re_exports() -> Result<(), String> {
    let fragment = fragment(
        "globs/chain",
        &["src/lib.rs", "src/a.rs", "src/b.rs", "src/c.rs"],
    )?;

    assert!(flags(&fragment, "far")?.1);
    assert!(flags(&fragment, "other")?.1);
    Ok(())
}

/// Whether the fragment states `user` as naming `used` for advice only.
fn advises(fragment: &IrFragment, user: &str, used: &str) -> Result<bool, String> {
    let id = |name: &str| {
        fragment
            .nodes
            .iter()
            .find(|node| node.name == name)
            .map(|node| node.id)
            .ok_or_else(|| format!("missing node {name}"))
    };
    let (user, used) = (id(user)?, id(used)?);
    Ok(fragment
        .advisory_uses
        .iter()
        .any(|pair| pair.user == user && pair.used == used))
}

fn cfgs() -> Result<IrFragment, String> {
    fragment("cfgs/app", &["src/lib.rs", "src/a.rs", "src/b.rs"])
}

#[test]
fn should_state_a_cfg_gated_statement_as_an_advisory_use() -> Result<(), String> {
    let fragment = cfgs()?;

    assert!(advises(&fragment, "go", "helper")?);
    assert!(!advises(&fragment, "calm", "plain")?);
    Ok(())
}

#[test]
fn should_state_a_cfg_gated_field_as_an_advisory_use() -> Result<(), String> {
    let fragment = cfgs()?;

    assert!(advises(&fragment, "S", "T")?);
    assert!(!advises(&fragment, "V", "U")?);
    Ok(())
}

#[test]
fn should_state_a_file_level_inner_cfg_as_advisory_uses() -> Result<(), String> {
    let fragment = round5()?;

    assert!(advises(&fragment, "user", "helper")?);
    Ok(())
}

#[test]
fn should_state_the_names_of_a_module_level_macro_as_advisory_uses() -> Result<(), String> {
    let fragment = round5()?;

    assert!(advises(&fragment, "host", "H")?);
    assert!(advises(&fragment, "host", "go")?);
    Ok(())
}

#[test]
fn should_treat_a_cfg_attr_module_as_maybe_dropped() -> Result<(), String> {
    let fragment = round5()?;

    assert!(advises(&fragment, "via_attr", "via_attr_target")?);
    Ok(())
}

fn round5() -> Result<IrFragment, String> {
    fragment(
        "round5/app",
        &["src/lib.rs", "src/a.rs", "src/x.rs", "src/m.rs", "src/c.rs"],
    )
}

#[test]
fn should_treat_a_file_the_loader_never_loaded_as_dropped() -> Result<(), String> {
    let fragment = fragment("round6/app", &["src/lib.rs", "src/a.rs", "src/win.rs"])?;

    assert!(advises(&fragment, "win_user", "helper")?);
    assert!(flags(&fragment, "win_pub")?.1);
    Ok(())
}

#[test]
fn should_read_an_inner_attribute_inside_a_declaration() -> Result<(), String> {
    let fragment = round5()?;

    assert!(advises(&fragment, "inner_gated", "via_attr_target")?);
    Ok(())
}
