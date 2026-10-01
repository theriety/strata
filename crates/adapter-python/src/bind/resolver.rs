//! Import resolution: dotted module names, relative levels, and `__all__`.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::NodeId;

use super::{ExportTable, is_public};
use crate::parse::{Import, ParsedModule};

/// Returns `true` if `path` is a package initializer (`__init__.py`).
pub(super) fn is_package_init(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == "__init__.py"
}

/// Resolves Python import statements to canonical module paths.
pub(super) struct Resolver {
    /// Dotted module name -> source file path.
    by_dotted: HashMap<SmolStr, SmolStr>,
    /// Source file path -> its dotted module name.
    dotted_of: HashMap<SmolStr, SmolStr>,
    /// Source file path -> its `__all__` public surface, when declared.
    surfaces: HashMap<SmolStr, HashSet<SmolStr>>,
}

impl Resolver {
    /// Builds a resolver over the known module set.
    pub(super) fn new(modules: &[ParsedModule]) -> Self {
        let mut by_dotted = HashMap::new();
        let mut dotted_of = HashMap::new();
        let mut surfaces = HashMap::new();
        for module in modules {
            let dotted = dotted_name(&module.path);
            by_dotted.insert(dotted.clone(), module.path.clone());
            dotted_of.insert(module.path.clone(), dotted);
            if !module.dunder_all.is_empty() {
                surfaces.insert(
                    module.path.clone(),
                    module.dunder_all.iter().cloned().collect(),
                );
            }
        }
        Self {
            by_dotted,
            dotted_of,
            surfaces,
        }
    }

    /// Resolves an import statement issued from `importer` to a module path.
    ///
    /// Absolute imports resolve against the dotted-name table; relative imports
    /// resolve their leading-dot level against the importer's package.
    pub(super) fn resolve(&self, importer: &str, import: &Import) -> Option<SmolStr> {
        let dotted = if import.level == 0 {
            import.module.to_string()
        } else {
            self.relative_dotted(importer, import)?
        };
        if dotted.is_empty() {
            return None;
        }
        self.by_dotted.get(dotted.as_str()).cloned()
    }

    /// Computes the absolute dotted name a relative import refers to.
    ///
    /// Level 1 (`from . import x`) is the importer's own package; each extra dot
    /// ascends one package. The `module` suffix, when present, is appended.
    fn relative_dotted(&self, importer: &str, import: &Import) -> Option<String> {
        let importer_dotted = self.dotted_of.get(importer)?;
        let mut segments: Vec<&str> = importer_dotted.split('.').collect();
        // A non-package module drops its own final segment to reach its package;
        // an `__init__.py` already names its package, so it keeps every segment.
        if !is_package_init(importer) {
            segments.pop();
        }
        // Each dot beyond the first ascends one further package level.
        for _ in 1..import.level {
            segments.pop()?;
        }
        if !import.module.is_empty() {
            segments.extend(import.module.split('.'));
        }
        Some(segments.join("."))
    }

    /// Returns the declared `__all__` surface of a module, if any.
    pub(super) fn public_surface(&self, module: &SmolStr) -> Option<&HashSet<SmolStr>> {
        self.surfaces.get(module)
    }

    /// Resolves a dotted module name to its source file path, if known.
    pub(super) fn module_of_dotted(&self, dotted: &SmolStr) -> Option<&SmolStr> {
        self.by_dotted.get(dotted)
    }
}

/// Converts a source file path to its dotted Python module name.
///
/// `pkg/sub/mod.py` becomes `pkg.sub.mod`; `pkg/sub/__init__.py` becomes
/// `pkg.sub` (the package the initializer represents).
fn dotted_name(path: &str) -> SmolStr {
    let without_extension = path.strip_suffix(".py").unwrap_or(path);
    let trimmed = without_extension
        .strip_suffix("/__init__")
        .unwrap_or(without_extension);
    SmolStr::new(trimmed.replace('/', "."))
}

/// Maps each locally imported name to its resolved target node.
pub(super) fn resolve_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> HashMap<SmolStr, NodeId> {
    let mut imported: HashMap<SmolStr, NodeId> = HashMap::new();
    for import in &module.imports {
        if import.star {
            continue;
        }
        let Some(target_module) = resolver.resolve(&module.path, import) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        // `names` is the locally bound name; `targets` the original export name.
        for (bound, original) in import.names.iter().zip(&import.targets) {
            if let Some(&target) = target_exports.get(original) {
                imported.insert(bound.clone(), target);
            }
        }
    }
    imported
}

/// Resolves the exported nodes reachable through each `from m import *` in a
/// module, fanning out over the target's public surface (its `__all__` when
/// declared, else every non-underscore export).
pub(super) fn resolve_star_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> Vec<NodeId> {
    let mut targets = Vec::new();
    for import in &module.imports {
        if !import.star {
            continue;
        }
        let Some(target_module) = resolver.resolve(&module.path, import) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        let surface = resolver.public_surface(&target_module);
        for (name, &node) in target_exports {
            let included = surface
                .as_ref()
                .map_or_else(|| is_public(name), |all| all.contains(name));
            if included {
                targets.push(node);
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::dotted_name;

    #[test]
    fn should_convert_a_module_path_to_a_dotted_name() {
        assert_eq!(dotted_name("pkg/sub/mod.py"), SmolStr::new("pkg.sub.mod"));
        assert_eq!(dotted_name("pkg/sub/__init__.py"), SmolStr::new("pkg.sub"));
        assert_eq!(dotted_name("mod.py"), SmolStr::new("mod"));
    }
}
