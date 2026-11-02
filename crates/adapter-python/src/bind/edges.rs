//! Dependency-edge emission: imports, inheritance, calls, annotations, and
//! value references resolved through the import and same-module tables.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::{Edge, EdgeKind, Hardness, NodeId};

use super::dynamic::emit_dynamic;
use super::resolver::{Resolver, resolve_imports, resolve_star_imports};
use super::{CONFIDENCE_STATIC, ExportTable, push_edge};
use crate::parse::{Declaration, ParsedModule};

/// Emits all dependency edges for the bound module set.
pub(super) fn emit_edges(
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
    let imported = resolve_imports(module, resolver, exports);
    let star_targets = resolve_star_imports(module, resolver, exports);

    for declaration in &module.declarations {
        let Some(&source) = module_local.and_then(|table| table.get(&declaration.name)) else {
            continue;
        };
        emit_inheritance(declaration, source, &imported, module_local, edges);
        let called = emit_calls(declaration, source, &imported, module_local, edges);
        emit_annotations(declaration, source, &imported, module_local, edges);
        emit_references(declaration, source, &imported, module_local, &called, edges);
        emit_dynamic(
            declaration,
            source,
            &imported,
            &star_targets,
            resolver,
            exports,
            edges,
        );
    }
}

/// Emits inheritance edges (class bases) for one declaration.
fn emit_inheritance(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    for base in &declaration.bases {
        if let Some(target) = lookup(base, imported, module_local) {
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

/// Emits call edges for identifiers a declaration invokes.
///
/// A called name is resolved first through an imported binding, then a
/// same-module declaration. Returns the set of resolved call targets so neither
/// [`emit_references`] nor [`emit_annotations`] re-links the same target.
fn emit_calls(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) -> HashSet<NodeId> {
    let mut called: HashSet<NodeId> = HashSet::new();
    for name in &declaration.called {
        let Some(target) = lookup(name, imported, module_local) else {
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

/// Emits soft type-reference edges for a declaration's annotation references.
fn emit_annotations(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    let bases: HashSet<&SmolStr> = declaration.bases.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.annotations {
        if bases.contains(name) {
            continue;
        }
        if let Some(target) = lookup(name, imported, module_local)
            && seen.insert(target)
        {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::TypeReference,
                Hardness::Soft,
                CONFIDENCE_STATIC,
            );
        }
    }
}

/// Emits hard value-import edges for names a declaration references.
///
/// These edges model **dependency**, not the import list: a name resolved
/// inside the declaration's own module is as real a dependency as one pulled
/// across a module boundary, and the engine relocates symbols against exactly
/// that graph. So a referenced name is resolved first through the imports, then
/// through the same-module declarations — the order [`emit_calls`] already
/// uses.
///
/// `called` carries the targets already linked by [`emit_calls`]; a referenced
/// name that was also invoked is skipped here so it surfaces only as a call. A
/// declaration naming itself depends on nothing and emits no edge.
fn emit_references(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    called: &HashSet<NodeId>,
    edges: &mut Vec<Edge>,
) {
    let bases: HashSet<&SmolStr> = declaration.bases.iter().collect();
    let annotations: HashSet<&SmolStr> = declaration.annotations.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.referenced {
        if bases.contains(name) || annotations.contains(name) {
            continue;
        }
        let Some(target) = imported
            .get(name)
            .copied()
            .or_else(|| module_local.and_then(|table| table.get(name)).copied())
        else {
            continue;
        };
        if target == source || called.contains(&target) || !seen.insert(target) {
            continue;
        }
        push_edge(
            edges,
            source,
            target,
            EdgeKind::ValueImport,
            Hardness::Hard,
            CONFIDENCE_STATIC,
        );
    }
}

/// Resolves a referenced name to a node: imported binding first, then local.
fn lookup(
    name: &SmolStr,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
) -> Option<NodeId> {
    imported
        .get(name)
        .copied()
        .or_else(|| module_local.and_then(|table| table.get(name)).copied())
}
