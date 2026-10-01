//! Dependency-edge emission: inheritance, calls, references, dynamic imports.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::{Edge, EdgeKind, Hardness, NodeId};

use crate::parse::{DeclarationKind, ParsedModule};

use super::super::ExportTable;
use super::super::resolution::{Resolver, resolve_imports};
use super::CONFIDENCE_STATIC;

/// Confidence assigned to a dynamic `import('...')` edge.
const CONFIDENCE_DYNAMIC: f64 = 0.5;

/// Emits all dependency edges for the bound module set.
pub(in crate::bind) fn emit_edges(
    modules: &[ParsedModule],
    resolver: &Resolver,
    exports: &ExportTable,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for module in modules {
        emit_module_edges(module, resolver, exports, local, &mut edges);
    }
    edges
}

/// Emits every dependency edge originating in a single module.
fn emit_module_edges(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    let module_local = local.get(&module.path);
    let local_types: HashMap<SmolStr, bool> = module
        .declarations
        .iter()
        .map(|declaration| {
            (
                declaration.name.clone(),
                declaration.kind == DeclarationKind::Type,
            )
        })
        .collect();
    let imported = resolve_imports(module, resolver, exports);

    for declaration in &module.declarations {
        let Some(&source) = module_local.and_then(|table| table.get(&declaration.name)) else {
            continue;
        };
        emit_inheritance(declaration, source, &imported, module_local, edges);
        let called = emit_calls(declaration, source, &imported, module_local, edges);
        emit_references(
            declaration,
            source,
            &imported,
            module_local,
            &local_types,
            &called,
            edges,
        );
        emit_dynamic_imports(declaration, source, &module.path, resolver, exports, edges);
    }
}

/// Emits inheritance edges (`extends` / `implements`) for one declaration.
fn emit_inheritance(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    for supertype in &declaration.supertypes {
        if let Some(target) = imported
            .get(supertype)
            .map(|(target, _)| *target)
            .or_else(|| module_local.and_then(|table| table.get(supertype)).copied())
        {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::Inheritance,
                Hardness::Hard,
                CONFIDENCE_STATIC,
            );
        }
    }
}

/// Emits call edges for identifiers a declaration invokes (call or `new`).
///
/// A called name is resolved through the symbol tables — first an imported
/// binding, then a same-module declaration — to the node it denotes; the
/// invocation produces a hard [`EdgeKind::Call`]. Returns the set of resolved
/// call targets so [`emit_references`] does not also emit a value-import edge
/// for the same target (a call is the more specific relationship).
fn emit_calls(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) -> HashSet<NodeId> {
    let mut called: HashSet<NodeId> = HashSet::new();
    for name in &declaration.called {
        let Some(target) = imported
            .get(name)
            .map(|(target, _)| *target)
            .or_else(|| module_local.and_then(|table| table.get(name)).copied())
        else {
            continue;
        };
        if !called.insert(target) {
            continue;
        }
        push_edge(
            edges,
            source,
            target,
            EdgeKind::Call,
            Hardness::Hard,
            CONFIDENCE_STATIC,
        );
    }
    called
}

/// Emits value-import / type-reference edges for names a declaration references.
///
/// These edges model **dependency**, not the import list: a name resolved
/// inside the declaration's own module is as real a dependency as one pulled
/// across a module boundary, and the engine relocates symbols against exactly
/// that graph. So a referenced name is resolved first through the imports, then
/// through the same-module declarations — the order [`emit_calls`] already
/// uses. The edge kind follows the *target*: a type target is a soft
/// [`EdgeKind::TypeReference`], anything else a hard [`EdgeKind::ValueImport`].
///
/// `called` carries the targets already linked by [`emit_calls`]; a referenced
/// name that was also invoked is skipped here so it surfaces only as a call. A
/// declaration naming itself depends on nothing and emits no edge.
fn emit_references(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    local_types: &HashMap<SmolStr, bool>,
    called: &HashSet<NodeId>,
    edges: &mut Vec<Edge>,
) {
    // Supertypes already produced inheritance edges; skip them here so an
    // `implements Shape` clause does not also surface as a value/type edge.
    let supertypes: HashSet<&SmolStr> = declaration.supertypes.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.referenced {
        if supertypes.contains(name) {
            continue;
        }
        let resolved = imported.get(name).copied().or_else(|| {
            let target = module_local.and_then(|table| table.get(name)).copied()?;
            Some((target, local_types.get(name).copied().unwrap_or(false)))
        });
        let Some((target, is_type)) = resolved else {
            continue;
        };
        if target == source || called.contains(&target) || !seen.insert(target) {
            continue;
        }
        let (kind, hardness) = if is_type {
            (EdgeKind::TypeReference, Hardness::Soft)
        } else {
            (EdgeKind::ValueImport, Hardness::Hard)
        };
        push_edge(edges, source, target, kind, hardness, CONFIDENCE_STATIC);
    }
}

/// Emits low-confidence value-import edges for a declaration's dynamic imports.
///
/// The specific symbol pulled from a dynamic `import('...')` is unknowable
/// statically, so every exported symbol of the resolved module receives an edge.
fn emit_dynamic_imports(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    importer: &SmolStr,
    resolver: &Resolver,
    exports: &ExportTable,
    edges: &mut Vec<Edge>,
) {
    for specifier in &declaration.dynamic_imports {
        let Some(target_module) = resolver.resolve(importer, specifier) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        for &target in target_exports.values() {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::ValueImport,
                Hardness::Hard,
                CONFIDENCE_DYNAMIC,
            );
        }
    }
}

/// Appends an edge, skipping self-loops which carry no dependency information.
pub(super) fn push_edge(
    edges: &mut Vec<Edge>,
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    hardness: Hardness,
    confidence: f64,
) {
    if source == target {
        return;
    }
    edges.push(Edge {
        source,
        target,
        kind,
        hardness,
        confidence,
    });
}
