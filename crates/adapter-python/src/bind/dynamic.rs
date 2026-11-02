//! Low-confidence edges for dynamic constructs and star imports.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{Edge, EdgeKind, Hardness, NodeId};

use super::resolver::Resolver;
use super::{CONFIDENCE_DYNAMIC, ExportTable, is_public, push_edge};
use crate::parse::{Declaration, DynamicRef};

/// Emits low-confidence edges for dynamic constructs and star imports.
///
/// `getattr(obj, "x")` and `importlib.import_module("m")` cannot be resolved to
/// a precise target; a star import pulls in an unknown subset of the target's
/// surface. Each becomes an honest edge below full confidence rather than a
/// silent drop. A `getattr` on an imported binding links to that binding; an
/// `importlib.import_module` of a string-literal module fans out over that
/// module's public surface; a star import does the same over its target.
pub(super) fn emit_dynamic(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    star_targets: &[NodeId],
    resolver: &Resolver,
    exports: &ExportTable,
    edges: &mut Vec<Edge>,
) {
    for dynamic in &declaration.dynamic {
        match dynamic {
            DynamicRef::GetAttr(base) => {
                if let Some(&target) = imported.get(base) {
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
            DynamicRef::ImportModule(dotted) => {
                for target in import_module_targets(dotted, resolver, exports) {
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
    }
    for &target in star_targets {
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

/// Resolves a string-literal `importlib.import_module` target to its public
/// surface nodes.
///
/// The dotted argument is matched against the known module set; an unknown
/// module yields no edge (the import is genuinely unresolvable). A resolved
/// module fans out over its declared `__all__` surface when present, else every
/// non-underscore export — the same surface a star import would pull in.
fn import_module_targets(
    dotted: &SmolStr,
    resolver: &Resolver,
    exports: &ExportTable,
) -> Vec<NodeId> {
    let Some(module) = resolver.module_of_dotted(dotted) else {
        return Vec::new();
    };
    let Some(target_exports) = exports.get(module) else {
        return Vec::new();
    };
    let surface = resolver.public_surface(module);
    let mut targets: Vec<NodeId> = target_exports
        .iter()
        .filter(|(name, _)| {
            surface
                .as_ref()
                .map_or_else(|| is_public(name), |all| all.contains(*name))
        })
        .map(|(_, &node)| node)
        .collect();
    targets.sort_unstable();
    targets
}
