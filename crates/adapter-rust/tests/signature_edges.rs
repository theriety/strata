//! Signature edges and exposures (ADR-25): trait bounds are dependencies, and a
//! signature type is told apart from a body-only use.

use std::fs;

use strata_adapter_rust::RustAdapter;
use strata_ir::{Adapter, EdgeKind, IrFragment, SourceFile};

const SOURCE: &str = "pub trait Bound {}\n\
pub struct Held;\n\
pub struct Local;\n\
pub fn generic<T: Bound>() {}\n\
pub fn object(_value: &dyn Bound) {}\n\
pub fn returns() -> Held {\n    Held\n}\n\
pub fn body_only() {\n    let _value: Local = Local;\n}\n\
fn private_signature(_held: Held) {}\n";

/// Binds `SOURCE` as a one-file crate.
fn bind() -> Result<IrFragment, String> {
    let root = std::env::temp_dir().join(format!("strata-signature-edges-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    let manifest = "[package]\nname = \"sig\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    fs::write(root.join("Cargo.toml"), manifest).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), SOURCE).map_err(|error| error.to_string())?;
    let files = [SourceFile {
        path: "src/lib.rs".into(),
        contents: SOURCE.to_owned(),
    }];
    let adapter = RustAdapter::new(root.join("Cargo.toml"));
    let trees = adapter.parse(&files).map_err(|error| error.to_string())?;
    let fragment = adapter.bind(trees).map_err(|error| error.to_string());
    let _ = fs::remove_dir_all(&root);
    fragment
}

fn id(fragment: &IrFragment, name: &str) -> Result<strata_ir::NodeId, String> {
    fragment
        .nodes
        .iter()
        .find(|node| node.name.as_str() == name)
        .map(|node| node.id)
        .ok_or_else(|| format!("no node named {name}"))
}

#[test]
fn should_bind_an_edge_from_a_generic_bound_and_a_trait_object() -> Result<(), String> {
    let fragment = bind()?;
    let bound = id(&fragment, "Bound")?;
    for caller in ["generic", "object"] {
        let source = id(&fragment, caller)?;
        assert!(
            fragment.edges.iter().any(|edge| edge.source == source
                && edge.target == bound
                && edge.kind == EdgeKind::TypeReference),
            "{caller} must depend on Bound: {:?}",
            fragment.edges
        );
    }
    Ok(())
}

#[test]
fn should_expose_a_signature_type_but_not_a_body_only_one() -> Result<(), String> {
    let fragment = bind()?;
    let exposes = |from: &str, to: &str| -> Result<bool, String> {
        let (exposer, exposed) = (id(&fragment, from)?, id(&fragment, to)?);
        Ok(fragment
            .exposures
            .iter()
            .any(|e| e.exposer == exposer && e.exposed == exposed && e.reach))
    };
    assert!(exposes("generic", "Bound")?);
    assert!(exposes("returns", "Held")?);
    assert!(!exposes("body_only", "Local")?);
    // a private fn states no reach of its own, so its signature exposes nothing.
    assert!(!exposes("private_signature", "Held")?);
    Ok(())
}
