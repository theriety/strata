//! Import resolution: dotted module names, relative levels, and `__all__`.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use unlace_ir::NodeId;

use super::{ExportTable, is_star_importable};
use crate::parse::{DunderAllForm, Import, ParsedModule};

/// Returns `true` if `path` is a package initializer (`__init__.py`).
///
/// Stubs take no part in dependency edges, so `__init__.pyi` is not one.
pub(super) fn is_package_init(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == "__init__.py"
}

/// Resolves Python import statements to canonical module paths.
pub(super) struct Resolver {
    /// Dotted module name -> source file path.
    by_dotted: HashMap<SmolStr, SmolStr>,
    /// Source file path -> its dotted module name.
    dotted_of: HashMap<SmolStr, SmolStr>,
    /// Source file path -> the names its `__all__` lists, when it lists any.
    surfaces: HashMap<SmolStr, Surface>,
}

/// The names a module lists in `__all__`.
struct Surface {
    /// The names read from the module's `__all__` assignments.
    listed: HashSet<SmolStr>,
    /// Whether they are the whole list; a partial or computed `__all__` closes
    /// nothing.
    closed: bool,
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
            let closed = module.dunder_all_form == DunderAllForm::Literal;
            if closed || !module.dunder_all.is_empty() {
                surfaces.insert(
                    module.path.clone(),
                    Surface {
                        listed: module.dunder_all.iter().cloned().collect(),
                        closed,
                    },
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
        // an empty relative name is the root package (the analysed folder holds
        // an `__init__.py`); an empty absolute name names nothing.
        if dotted.is_empty() && import.level == 0 {
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
        let mut segments: Vec<&str> = importer_dotted
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect();
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

    /// Whether `from module import *` pulls `name` in.
    ///
    /// A closed `__all__` is the whole surface. An open one (built or changed in
    /// a way the parser cannot read) adds its listed names to the non-underscore
    /// names; with no `__all__` the surface is the non-underscore names.
    pub(super) fn star_includes(&self, module: &SmolStr, name: &str) -> bool {
        match self.surfaces.get(module) {
            Some(surface) if surface.closed => surface.listed.contains(name),
            Some(surface) => surface.listed.contains(name) || is_star_importable(name),
            None => is_star_importable(name),
        }
    }

    /// Resolves a dotted module name to its source file path, if known.
    pub(super) fn module_of_dotted(&self, dotted: &SmolStr) -> Option<&SmolStr> {
        self.by_dotted.get(dotted)
    }
}

/// Converts a source file path to its dotted Python module name.
///
/// `pkg/sub/mod.py` becomes `pkg.sub.mod`; `pkg/sub/__init__.py` becomes
/// `pkg.sub` (the package the initializer represents); a root `__init__.py` is
/// the unnamed root package, `""`. A stub keeps its extension, so no import
/// resolves to it.
fn dotted_name(path: &str) -> SmolStr {
    let without_extension = path.strip_suffix(".py").unwrap_or(path);
    let trimmed = if path == "__init__.py" {
        ""
    } else {
        without_extension
            .strip_suffix("/__init__")
            .unwrap_or(without_extension)
    };
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
        for (name, &node) in target_exports {
            if resolver.star_includes(&target_module, name) {
                targets.push(node);
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::{Resolver, dotted_name, is_package_init};
    use crate::parse::{DunderAllForm, Import, ParsedModule};

    /// Builds an empty module summary at `path`.
    fn module_at(path: &str) -> ParsedModule {
        ParsedModule {
            path: SmolStr::new(path),
            declarations: Vec::new(),
            imports: Vec::new(),
            conditional_imports: Vec::new(),
            lazy_loading: false,
            dunder_all: Vec::new(),
            dunder_all_form: DunderAllForm::Absent,
        }
    }

    /// Builds an absolute `from <module> import x` statement.
    fn absolute(module: &str) -> Import {
        Import {
            module: SmolStr::new(module),
            level: 0,
            names: vec![SmolStr::new("x")],
            targets: vec![SmolStr::new("x")],
            star: false,
            aliases: Vec::new(),
        }
    }

    /// Builds a relative `from <dots><module> import x` statement.
    fn relative(level: u32, module: &str) -> Import {
        Import {
            level,
            ..absolute(module)
        }
    }

    #[test]
    fn should_convert_a_module_path_to_a_dotted_name() {
        assert_eq!(dotted_name("pkg/sub/mod.py"), SmolStr::new("pkg.sub.mod"));
        assert_eq!(dotted_name("pkg/sub/__init__.py"), SmolStr::new("pkg.sub"));
        assert_eq!(dotted_name("mod.py"), SmolStr::new("mod"));
    }

    #[test]
    fn should_name_a_root_package_initializer_after_the_unnamed_root() {
        assert_eq!(dotted_name("__init__.py"), SmolStr::new(""));
        assert_eq!(dotted_name("pkg/__init__.py"), SmolStr::new("pkg"));
    }

    #[test]
    fn should_leave_stubs_out_of_the_dependency_edge_rules() {
        // dependency edges treat a stub as an unrelated file, as they always
        // did: it takes no part in dotted names and is no package initializer.
        assert_eq!(dotted_name("pkg/mod.pyi"), SmolStr::new("pkg.mod.pyi"));
        assert_eq!(dotted_name("__init__.pyi"), SmolStr::new("__init__.pyi"));
        assert_eq!(
            dotted_name("pkg/__init__.pyi"),
            SmolStr::new("pkg.__init__.pyi")
        );
        assert!(!is_package_init("pkg/__init__.pyi"));
        let resolver = Resolver::new(&[module_at("pkg/_native.pyi")]);
        assert_eq!(resolver.resolve("app.py", &absolute("pkg._native")), None);
    }

    #[test]
    fn should_resolve_a_relative_import_of_a_root_package_initializer() {
        let resolver = Resolver::new(&[module_at("__init__.py"), module_at("_impl.py")]);

        assert_eq!(
            resolver.resolve("__init__.py", &relative(1, "_impl")),
            Some(SmolStr::new("_impl.py"))
        );
        assert_eq!(
            resolver.resolve("_impl.py", &relative(1, "")),
            Some(SmolStr::new("__init__.py"))
        );
    }
}
