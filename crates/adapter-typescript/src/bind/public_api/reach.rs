//! The items importable from a set of modules (ADR-29 rule 3).
//!
//! The walk starts at modules whose whole surface is importable and follows what
//! each one publishes: its own exports, `export { name as alias }` and default
//! clauses, and every `export ... from`, module by module. A request for one name
//! of a module looks the name up among that module's exports, re-export
//! bindings and clauses. Whatever the walk cannot place makes the reach
//! incomplete, which keeps the owning package open.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use unlace_ir::NodeId;

use super::super::ExportTable;
use super::super::resolution::{Resolver, normalize_join};
use crate::parse::{
    ExportClause, ExportLoad, ImportedName, ParsedModule, ReExport, ReExportBinding,
};

/// The module graph the reach is followed over.
pub(super) struct Graph<'graph> {
    modules: HashMap<&'graph str, &'graph ParsedModule>,
    resolver: &'graph Resolver,
    exports: &'graph ExportTable,
    local: &'graph ExportTable,
    /// The module that holds each node.
    module_of: HashMap<NodeId, &'graph SmolStr>,
    /// Names of the packages inside the tree.
    package_names: HashSet<SmolStr>,
}

/// The items reached from a set of modules.
pub(super) struct Reach {
    /// Every item an outside importer can get hold of.
    pub(super) nodes: HashSet<NodeId>,
    /// `false` when some link of the chain could not be followed, so the nodes
    /// may be fewer than the real surface.
    pub(super) complete: bool,
}

/// The state of one walk.
struct Walk<'walk> {
    /// Whether an unresolved relative specifier, given as the path it leads to
    /// from the importing file, may be left out because no closed package could
    /// be behind it.
    ignore_unresolved: &'walk dyn Fn(&str) -> bool,
    nodes: HashSet<NodeId>,
    complete: bool,
    /// Modules whose whole surface was walked, with or without the default export.
    surfaces: HashSet<(SmolStr, bool)>,
    /// The outcome of each name request: `false` while it is still being
    /// answered, so a cycle never counts as a find.
    names: HashMap<(SmolStr, SmolStr), bool>,
}

impl<'graph> Graph<'graph> {
    /// Indexes the bound modules and symbol tables.
    pub(super) fn new(
        modules: &'graph [ParsedModule],
        resolver: &'graph Resolver,
        tables: (&'graph ExportTable, &'graph ExportTable),
        package_names: HashSet<SmolStr>,
    ) -> Self {
        let (exports, local) = tables;
        let module_of = local
            .iter()
            .flat_map(|(path, names)| names.values().map(move |&node| (node, path)))
            .collect();
        Self {
            modules: modules
                .iter()
                .map(|module| (module.path.as_str(), module))
                .collect(),
            resolver,
            exports,
            local,
            module_of,
            package_names,
        }
    }

    /// The module that holds `node`.
    pub(super) fn module_of(&self, node: NodeId) -> Option<&'graph SmolStr> {
        self.module_of.get(&node).copied()
    }

    /// Walks everything the `modules` make importable, default exports included.
    ///
    /// `ignore_unresolved` says, for the path an unresolved relative specifier
    /// leads to, whether leaving it out is safe; a walk that cannot say passes
    /// `|_| false`.
    pub(super) fn reach_from<'seed>(
        &self,
        modules: impl IntoIterator<Item = &'seed SmolStr>,
        ignore_unresolved: &dyn Fn(&str) -> bool,
    ) -> Reach {
        let mut walk = Walk {
            ignore_unresolved,
            nodes: HashSet::new(),
            complete: true,
            surfaces: HashSet::new(),
            names: HashMap::new(),
        };
        for module in modules {
            self.walk_surface(&mut walk, module, true);
        }
        Reach {
            nodes: walk.nodes,
            complete: walk.complete,
        }
    }

    /// Walks every name `module` publishes; `with_default` says whether its
    /// default export is one of them (`export *` leaves it out).
    fn walk_surface(&self, walk: &mut Walk<'_>, module: &str, with_default: bool) {
        let Some(parsed) = self.modules.get(module) else {
            return;
        };
        if !walk.surfaces.insert((SmolStr::new(module), with_default)) {
            return;
        }
        walk.nodes.extend(
            self.exports
                .get(module)
                .into_iter()
                .flat_map(|names| names.values().copied()),
        );
        for re_export in &parsed.re_exports {
            self.walk_re_export(walk, parsed, re_export);
        }
        for clause in &parsed.export_clauses {
            if with_default || clause.exported != "default" {
                self.follow_clause(walk, parsed, clause);
            }
        }
        for load in &parsed.export_loads {
            if with_default || load.exported != "default" {
                self.follow_load(walk, parsed, load);
            }
        }
    }

    /// Walks everything one `export ... from` publishes.
    fn walk_re_export(&self, walk: &mut Walk<'_>, module: &ParsedModule, re_export: &ReExport) {
        let Some(source) = self.source_of(walk, module, &re_export.source) else {
            return;
        };
        if re_export.names.is_empty() {
            self.walk_surface(walk, &source, false);
            return;
        }
        for binding in &re_export.names {
            match binding {
                ReExportBinding::Namespace { .. } => self.walk_surface(walk, &source, true),
                ReExportBinding::Legacy(original) | ReExportBinding::Named { original, .. } => {
                    self.request_name(walk, &source, original);
                }
            }
        }
    }

    /// Asks for one definite name of `module`; the reach is incomplete when the
    /// module does not publish it.
    fn request_name(&self, walk: &mut Walk<'_>, module: &str, name: &str) {
        if !self.walk_name(walk, module, name) {
            walk.complete = false;
        }
    }

    /// Walks the nodes behind `name` in `module`, `true` when the module
    /// publishes it.
    fn walk_name(&self, walk: &mut Walk<'_>, module: &str, name: &str) -> bool {
        let key = (SmolStr::new(module), SmolStr::new(name));
        if let Some(&answer) = walk.names.get(&key) {
            return answer;
        }
        walk.names.insert(key.clone(), false);
        let Some(parsed) = self.modules.get(module) else {
            return false;
        };
        let mut found = false;
        if let Some(&node) = self.exports.get(module).and_then(|names| names.get(name)) {
            walk.nodes.insert(node);
            found = true;
        }
        for re_export in &parsed.re_exports {
            let hit = self.walk_name_in_re_export(walk, parsed, re_export, name);
            found |= hit;
        }
        for clause in parsed
            .export_clauses
            .iter()
            .filter(|clause| clause.exported == name)
        {
            self.follow_clause(walk, parsed, clause);
            found = true;
        }
        for load in parsed
            .export_loads
            .iter()
            .filter(|load| load.exported == name)
        {
            self.follow_load(walk, parsed, load);
            found = true;
        }
        walk.names.insert(key, found);
        found
    }

    /// Walks the part of one `export ... from` that publishes `name`, `true`
    /// when it may.
    fn walk_name_in_re_export(
        &self,
        walk: &mut Walk<'_>,
        module: &ParsedModule,
        re_export: &ReExport,
        name: &str,
    ) -> bool {
        if re_export.names.is_empty() {
            // `export *` leaves the default out and may or may not hold `name`.
            if name == "default" {
                return false;
            }
            let Some(source) = self.source_of(walk, module, &re_export.source) else {
                return false;
            };
            return self.walk_name(walk, &source, name);
        }
        let mut found = false;
        for binding in &re_export.names {
            match binding {
                ReExportBinding::Namespace { namespace } if namespace == name => {
                    if let Some(source) = self.source_of(walk, module, &re_export.source) {
                        self.walk_surface(walk, &source, true);
                    }
                    found = true;
                }
                ReExportBinding::Legacy(original) if original == name => {
                    self.walk_binding(walk, module, re_export, original);
                    found = true;
                }
                ReExportBinding::Named { original, exported } if exported == name => {
                    self.walk_binding(walk, module, re_export, original);
                    found = true;
                }
                ReExportBinding::Namespace { .. }
                | ReExportBinding::Legacy(_)
                | ReExportBinding::Named { .. } => {}
            }
        }
        found
    }

    /// Walks one named binding of an `export { original as ... } from`.
    fn walk_binding(
        &self,
        walk: &mut Walk<'_>,
        module: &ParsedModule,
        re_export: &ReExport,
        original: &str,
    ) {
        if let Some(source) = self.source_of(walk, module, &re_export.source) {
            self.request_name(walk, &source, original);
        }
    }

    /// Walks what an export clause hands out: a declaration of the module, or
    /// the export an import binds the name to.
    fn follow_clause(&self, walk: &mut Walk<'_>, module: &ParsedModule, clause: &ExportClause) {
        if let Some(&node) = self
            .local
            .get(&module.path)
            .and_then(|names| names.get(&clause.local))
        {
            walk.nodes.insert(node);
            return;
        }
        let bound = module.imports.iter().find_map(|import| {
            import
                .bindings
                .iter()
                .find(|binding| binding.local == clause.local)
                .map(|binding| (import, binding))
        });
        let Some((import, binding)) = bound else {
            // a mentioned name that is no binding of the module is a global or a
            // parameter; a named one that is missing cannot be placed.
            if !clause.mentioned {
                walk.complete = false;
            }
            return;
        };
        let Some(source) = self.source_of(walk, module, &import.source) else {
            return;
        };
        match &binding.imported {
            ImportedName::Named(imported) if imported != "default" => {
                self.request_name(walk, &source, imported);
            }
            ImportedName::Named(_) | ImportedName::Default => {
                self.request_name(walk, &source, "default");
            }
            ImportedName::Namespace => self.walk_surface(walk, &source, true),
        }
    }

    /// Walks the module an export loads wholesale; a target that is computed or
    /// is no module cannot be followed.
    fn follow_load(&self, walk: &mut Walk<'_>, module: &ParsedModule, load: &ExportLoad) {
        let Some(specifier) = &load.source else {
            walk.complete = false;
            return;
        };
        if let Some(source) = self.source_of(walk, module, specifier) {
            self.walk_surface(walk, &source, true);
        }
    }

    /// Resolves the module `specifier` leads to. A specifier that leads outside
    /// the tree exposes none of its items and yields `None`; one that should lead
    /// inside but does not resolve makes the reach incomplete.
    fn source_of(
        &self,
        walk: &mut Walk<'_>,
        module: &ParsedModule,
        specifier: &str,
    ) -> Option<SmolStr> {
        let resolved = self.resolver.resolve(&module.path, specifier);
        if resolved.is_none()
            && self.names_a_package_inside(specifier)
            && !Self::may_ignore(walk, module, specifier)
        {
            walk.complete = false;
        }
        resolved
    }

    /// Whether the walk may leave out an unresolved relative `specifier`.
    fn may_ignore(walk: &Walk<'_>, module: &ParsedModule, specifier: &str) -> bool {
        if !specifier.starts_with('.') {
            return false;
        }
        let directory = module.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        (walk.ignore_unresolved)(&normalize_join(directory, specifier))
    }

    /// Whether `specifier` points into the tree: a relative path, a `#` import,
    /// a configured alias, or the name of a package inside the tree.
    fn names_a_package_inside(&self, specifier: &str) -> bool {
        specifier.starts_with(['.', '#'])
            || self.resolver.names_an_alias(specifier)
            || self.package_names.contains(package_name(specifier))
    }
}

/// The package a bare specifier names: `@scope/pkg` or `pkg`, without a subpath.
fn package_name(specifier: &str) -> &str {
    let mut segments = specifier.splitn(3, '/');
    let first = segments.next().unwrap_or(specifier);
    if first.starts_with('@') {
        let end = first.len() + 1 + segments.next().map_or(0, str::len);
        specifier.get(..end).unwrap_or(specifier)
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_name_the_package_of_a_bare_specifier_without_its_subpath() {
        assert_eq!(package_name("zod"), "zod");
        assert_eq!(package_name("zod/v4"), "zod");
        assert_eq!(package_name("@acme/core"), "@acme/core");
        assert_eq!(package_name("@acme/core/sub/path"), "@acme/core");
    }
}
