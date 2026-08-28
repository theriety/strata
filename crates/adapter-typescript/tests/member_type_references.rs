use smol_str::SmolStr;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{Adapter, EdgeKind, Hardness, IrFragment, NodeId, SourceFile};

fn bind_inline(contents: &str) -> Result<IrFragment, String> {
    let adapter = TypeScriptAdapter::new(".");
    let files = [SourceFile {
        path: SmolStr::new("src/types.ts"),
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

fn edges_from(
    fragment: &IrFragment,
    source: &str,
) -> Result<Vec<(SmolStr, EdgeKind, Hardness)>, String> {
    let source = node_id(fragment, source)?;

    fragment
        .edges
        .iter()
        .filter(|edge| edge.source == source)
        .map(|edge| {
            fragment
                .nodes
                .iter()
                .find(|node| node.id == edge.target)
                .map_or_else(
                    || Err(format!("expected edge target {:?}", edge.target)),
                    |node| Ok((node.name.clone(), edge.kind, edge.hardness)),
                )
        })
        .collect()
}

#[test]
fn should_emit_one_soft_type_reference_for_an_interface_property() -> Result<(), String> {
    let fragment = bind_inline("type Local = string; interface Request { value: Local; }")?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("Local"),
            EdgeKind::TypeReference,
            Hardness::Soft
        )]
    );
    Ok(())
}

#[test]
fn should_emit_one_soft_type_reference_for_a_type_literal_alias() -> Result<(), String> {
    let fragment = bind_inline("interface Local {} type Request = { value: Local };")?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("Local"),
            EdgeKind::TypeReference,
            Hardness::Soft
        )]
    );
    Ok(())
}

#[test]
fn should_not_treat_an_interface_property_key_as_a_reference() -> Result<(), String> {
    let fragment = bind_inline("type Value = string; interface Request { Value: number; }")?;

    assert!(edges_from(&fragment, "Request")?.is_empty());
    Ok(())
}

#[test]
fn should_not_treat_a_type_literal_property_key_as_a_reference() -> Result<(), String> {
    let fragment = bind_inline("interface Value {} type Request = { Value: number };")?;

    assert!(edges_from(&fragment, "Request")?.is_empty());
    Ok(())
}

#[test]
fn should_collect_a_computed_interface_property_expression() -> Result<(), String> {
    let fragment = bind_inline(
        "const MEMBER_KEY = 'value' as const; interface Request { [MEMBER_KEY]: number; }",
    )?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("MEMBER_KEY"),
            EdgeKind::ValueImport,
            Hardness::Hard,
        )]
    );
    Ok(())
}

#[test]
fn should_not_treat_a_mapped_type_binding_as_a_reference() -> Result<(), String> {
    let fragment = bind_inline(
        "type KeySet = 'value'; type Key = string; type Request = { [Key in KeySet]: number };",
    )?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("KeySet"),
            EdgeKind::TypeReference,
            Hardness::Soft,
        )]
    );
    Ok(())
}

#[test]
fn should_not_resolve_an_interface_type_parameter_to_an_outer_type() -> Result<(), String> {
    let fragment = bind_inline("type Item = string; interface Box<Item> { value: Item; }")?;

    assert!(edges_from(&fragment, "Box")?.is_empty());
    Ok(())
}

#[test]
fn should_not_resolve_an_alias_type_parameter_to_an_outer_type() -> Result<(), String> {
    let fragment = bind_inline("type Item = string; type Box<Item> = { value: Item };")?;

    assert!(edges_from(&fragment, "Box")?.is_empty());
    Ok(())
}

#[test]
fn should_collect_only_the_root_of_a_qualified_type_name() -> Result<(), String> {
    let fragment = bind_inline(
        "enum Namespace { Member } interface Member {} type Request = Namespace.Member;",
    )?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("Namespace"),
            EdgeKind::TypeReference,
            Hardness::Soft,
        )]
    );
    Ok(())
}

#[test]
fn should_preserve_nested_mapped_and_function_type_binder_scopes() -> Result<(), String> {
    let fragment = bind_inline(
        "type Key = string; type Value = string; type KeySet = 'key'; interface Annotation {} type Request = { [Key in KeySet]: <Value extends Annotation>(input: Value) => Key };",
    )?;
    let mut edges = edges_from(&fragment, "Request")?;
    edges.sort();

    assert_eq!(
        edges,
        vec![
            (
                SmolStr::new("Annotation"),
                EdgeKind::TypeReference,
                Hardness::Soft,
            ),
            (
                SmolStr::new("KeySet"),
                EdgeKind::TypeReference,
                Hardness::Soft,
            ),
        ]
    );
    Ok(())
}

#[test]
fn should_preserve_conditional_infer_binder_scope_and_constraint() -> Result<(), String> {
    let fragment = bind_inline(
        "type Subject = string; type Candidate = string; interface Constraint {} type Request<Subject> = Subject extends infer Candidate extends Constraint ? Candidate : never;",
    )?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(
            SmolStr::new("Constraint"),
            EdgeKind::TypeReference,
            Hardness::Soft,
        )]
    );
    Ok(())
}

#[test]
fn should_not_treat_a_labeled_tuple_element_name_as_a_reference() -> Result<(), String> {
    let fragment = bind_inline("type Label = string; type Request = [Label: number];")?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(edges, Vec::new());
    Ok(())
}

#[test]
fn should_not_resolve_a_type_predicate_parameter_to_a_module_value() -> Result<(), String> {
    let fragment =
        bind_inline("const value = 1; type Guard = (value: unknown) => value is string;")?;
    let edges = edges_from(&fragment, "Guard")?;

    assert_eq!(edges, Vec::new());
    Ok(())
}

#[test]
fn should_collect_a_mapped_constraint_before_its_same_named_binding() -> Result<(), String> {
    let fragment = bind_inline("type Key = 'x'; type Request = { [Key in Key]: number };")?;
    let edges = edges_from(&fragment, "Request")?;

    assert_eq!(
        edges,
        vec![(SmolStr::new("Key"), EdgeKind::TypeReference, Hardness::Soft,)]
    );
    Ok(())
}

#[test]
fn should_traverse_nested_generic_union_optional_and_array_member_types() -> Result<(), String> {
    let fragment = bind_inline(
        "type Leaf = string; type Branch = number; interface Tree { value?: Array<Promise<Leaf | Branch[]>>; }",
    )?;
    let edges = edges_from(&fragment, "Tree")?;
    let mut edges = edges;
    edges.sort();

    assert_eq!(
        edges,
        vec![
            (
                SmolStr::new("Branch"),
                EdgeKind::TypeReference,
                Hardness::Soft,
            ),
            (
                SmolStr::new("Leaf"),
                EdgeKind::TypeReference,
                Hardness::Soft,
            ),
        ]
    );
    Ok(())
}

#[test]
fn should_not_emit_a_self_edge_for_a_recursive_member_type() -> Result<(), String> {
    let fragment = bind_inline("interface Tree { child?: Tree; }")?;

    assert!(edges_from(&fragment, "Tree")?.is_empty());
    Ok(())
}

#[test]
fn should_emit_interface_inheritance_without_a_duplicate_type_reference() -> Result<(), String> {
    let fragment = bind_inline("interface Base {} interface Child extends Base {}")?;
    let edges = edges_from(&fragment, "Child")?;

    assert_eq!(
        edges,
        vec![(SmolStr::new("Base"), EdgeKind::Inheritance, Hardness::Hard)]
    );
    Ok(())
}
