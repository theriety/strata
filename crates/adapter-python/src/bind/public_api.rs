//! Which items code outside the repository can import (ADR-29, Python rules 5
//! and 6).
//!
//! Python cannot block a deep import, so the surface is a matter of convention.
//! An item is private when its name, its module or any package on its import
//! path starts with `_`, or when its module lists an `__all__` that leaves it
//! out. A name that a public module imports, and so makes available, is public
//! together with everything it leads to, whichever module declares it; a module
//! a public module binds under its own name (`from . import _impl as impl`) is
//! public the same way, item by item. Every doubtful case stays public: an
//! `__all__` the parser cannot read closes nothing, a dunder such as
//! `__version__` is metadata rather than an internal name, and a package whose
//! `__init__.py` sets up lazy loading (a module-level `__getattr__`, or a
//! replacement of itself in `sys.modules`) can expose anything below it.
//!
//! The modules are named by [`names::ModuleNames`], which sees stubs; dependency
//! edges never use it.

mod names;

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use unlace_ir::{Node, NodeId};

use super::polarity::{is_support_path, is_test_path};
use super::{ExportTable, is_star_importable};
use crate::parse::{DunderAllForm, Import, ParsedModule};
use names::{ModuleNames, split_path, stem_of};

/// For each module, the nodes each of its imported names leads to.
type Reached = HashMap<SmolStr, HashMap<SmolStr, HashSet<NodeId>>>;

/// Sets `public_api` on every node.
///
/// A node is public when its own module makes its name public, or when a public
/// module imports a name that leads to it.
pub(super) fn mark_public_api(nodes: &mut [Node], modules: &[ParsedModule], local: &ExportTable) {
    let packages = Packages::new(modules);
    let reached = reached_nodes(&imports_of(modules), local);

    let mut public: HashSet<NodeId> = HashSet::new();
    for module in modules {
        let is_public = |name: &str| is_public_in(module, name, &packages);
        let own = local.get(&module.path).into_iter().flatten();
        public.extend(own.filter(|&(name, _)| is_public(name)).map(|(_, &id)| id));
        let imported = reached.get(&module.path).into_iter().flatten();
        for (_, leads_to) in imported.filter(|&(name, _)| is_public(name)) {
            public.extend(leads_to);
        }
    }

    for node in nodes {
        node.public_api = public.contains(&node.id);
    }
}

/// Whether rule 5 makes `name` public in `module`.
fn is_public_in(module: &ParsedModule, name: &str, packages: &Packages) -> bool {
    !is_internal(name) && !packages.is_internal_path(&module.path) && is_listed(module, name)
}

/// Whether a name, module or package is internal: it starts with `_` and is not
/// a dunder such as `__version__`, `__about__` or `__init__`.
fn is_internal(segment: &str) -> bool {
    segment.starts_with('_') && !is_dunder(segment)
}

/// Whether `name` is a dunder: `__` on both sides of at least one character.
fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

/// Whether the module's `__all__` leaves `name` in: always, unless the module
/// lists a closed `__all__` that omits it. A dunder is never left out.
fn is_listed(module: &ParsedModule, name: &str) -> bool {
    match module.dunder_all_form {
        DunderAllForm::Literal => {
            is_dunder(name) || module.dunder_all.iter().any(|listed| listed == name)
        }
        DunderAllForm::Absent | DunderAllForm::Dynamic => true,
    }
}

/// The package directories of the tree, which fix each module's import path.
struct Packages {
    /// Directories that hold an `__init__.py` or `__init__.pyi`.
    with_init: HashSet<String>,
    /// The subset whose initializer sets up lazy loading.
    lazy: HashSet<String>,
    /// Whether the analysed folder itself holds an initializer, making it the
    /// outermost package.
    root_is_package: bool,
    /// Whether that root initializer sets up lazy loading.
    root_is_lazy: bool,
}

impl Packages {
    /// Collects the package directories of `modules`.
    fn new(modules: &[ParsedModule]) -> Self {
        let mut packages = Self {
            with_init: HashSet::new(),
            lazy: HashSet::new(),
            root_is_package: false,
            root_is_lazy: false,
        };
        for module in modules {
            let (directory, file) = split_path(&module.path);
            if stem_of(file) != "__init__" {
                continue;
            }
            if directory.is_empty() {
                packages.root_is_package = true;
                packages.root_is_lazy |= module.lazy_loading;
                continue;
            }
            packages.with_init.insert(directory.to_owned());
            if module.lazy_loading {
                packages.lazy.insert(directory.to_owned());
            }
        }
        packages
    }

    /// Whether a module or package on the import path of `path` is internal.
    ///
    /// The import path starts at the outermost directory that holds an
    /// `__init__.py`: folders above it (`src/`, `_legacy/`) are not part of any
    /// dotted name, and when the analysed folder is itself a package every folder
    /// below it counts. A module outside every package counts only by its own
    /// name, since where its folder sits on `sys.path` cannot be told. Below a
    /// package whose `__init__.py` sets up lazy loading, only the package itself
    /// counts: what lies below it may be exposed lazily.
    fn is_internal_path(&self, path: &str) -> bool {
        if self.root_is_lazy {
            return false;
        }
        let (directory, file) = split_path(path);
        let mut prefix = String::new();
        let mut inside_package = self.root_is_package;
        for part in directory.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            inside_package = inside_package || self.with_init.contains(&prefix);
            if !inside_package {
                continue;
            }
            if is_internal(part) {
                return true;
            }
            if self.lazy.contains(&prefix) {
                return false;
            }
        }
        is_internal(stem_of(file))
    }
}

/// Whether `path` is test code, which exposes nothing: a `tests` or `test`
/// folder anywhere on the path, a `test_*` or `*_test` module, or `conftest`,
/// matched on whole segments (`contests` is not a test folder), and the polarity
/// rules agree. A shipped package such as `django.test` passes the first check
/// but not the second, so its imports still count.
fn is_test_code(path: &str) -> bool {
    is_test_named(path) && (is_test_path(path) || is_support_path(path))
}

/// Whether the folders and file name of `path` say test code.
fn is_test_named(path: &str) -> bool {
    let (directory, file) = split_path(path);
    let stem = stem_of(file);
    stem == "conftest"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || directory
            .split('/')
            .any(|part| part == "tests" || part == "test")
}

/// One import statement of a module that can make names available, with
/// everything it refers to already resolved.
struct ResolvedImport<'tree> {
    /// The module that issues the import.
    importer: &'tree SmolStr,
    /// The statement.
    import: &'tree Import,
    /// The modules the statement imports from.
    sources: Vec<&'tree ParsedModule>,
    /// The modules the statement binds whole, by the name it binds them to.
    bound_modules: Vec<(SmolStr, &'tree ParsedModule)>,
}

/// Resolves the imports of every module once, test code left out: it exposes
/// nothing, so its imports are not followed.
fn imports_of(modules: &[ParsedModule]) -> Vec<ResolvedImport<'_>> {
    let names = ModuleNames::new(modules);
    let by_path: HashMap<&str, &ParsedModule> = modules
        .iter()
        .map(|module| (module.path.as_str(), module))
        .collect();
    let modules_at = |paths: Vec<SmolStr>| -> Vec<&ParsedModule> {
        paths
            .iter()
            .filter_map(|path| by_path.get(path.as_str()).copied())
            .collect()
    };
    let mut resolved = Vec::new();
    for module in modules.iter().filter(|module| !is_test_code(&module.path)) {
        for import in module.imports.iter().chain(&module.conditional_imports) {
            let sources = modules_at(names.sources(&module.path, import));
            let mut bound_modules = Vec::new();
            if import.star {
                // a star import binds names, never a module.
            } else if import.module.is_empty() && import.level == 0 {
                // `import pkg._impl as impl` binds the module; without `as` it
                // binds the top package, which is not what the statement names.
                for (bound, dotted) in import.names.iter().zip(&import.targets) {
                    if import.aliases.contains(bound) {
                        for found in modules_at(names.absolute(dotted)) {
                            bound_modules.push((bound.clone(), found));
                        }
                    }
                }
            } else {
                for (bound, original) in import.names.iter().zip(&import.targets) {
                    for found in modules_at(names.children(&module.path, import, original)) {
                        bound_modules.push((bound.clone(), found));
                    }
                }
            }
            resolved.push(ResolvedImport {
                importer: &module.path,
                import,
                sources,
                bound_modules,
            });
        }
    }
    resolved
}

/// Follows every import to a fixed point: for each name a module binds by
/// importing, the nodes it leads to (the imported module's own node for the
/// name, whatever that name leads to in turn, and for a module bound whole, the
/// items it lists).
fn reached_nodes(imports: &[ResolvedImport<'_>], local: &ExportTable) -> Reached {
    let mut reached = Reached::new();
    loop {
        let mut changed = false;
        for statement in imports {
            let mut found: Vec<(SmolStr, Vec<NodeId>)> = Vec::new();
            for source in &statement.sources {
                for (bound, original) in imported_names(statement.import, source, local, &reached) {
                    found.push((bound, leads_to(&source.path, &original, local, &reached)));
                }
            }
            for (bound, module) in &statement.bound_modules {
                let items = star_names(module, local, &reached)
                    .iter()
                    .flat_map(|name| leads_to(&module.path, name, local, &reached))
                    .collect();
                found.push((bound.clone(), items));
            }
            for (bound, leads) in found {
                let known = reached
                    .entry(statement.importer.clone())
                    .or_default()
                    .entry(bound)
                    .or_default();
                for node in leads {
                    changed |= known.insert(node);
                }
            }
        }
        if !changed {
            return reached;
        }
    }
}

/// The `(bound name, name in the imported module)` pairs of one import.
fn imported_names(
    import: &Import,
    source: &ParsedModule,
    local: &ExportTable,
    reached: &Reached,
) -> Vec<(SmolStr, SmolStr)> {
    if import.star {
        return star_names(source, local, reached)
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect();
    }
    import
        .names
        .iter()
        .cloned()
        .zip(import.targets.iter().cloned())
        .collect()
}

/// The names `from module import *` pulls in: those a closed `__all__` lists,
/// else every name that does not start with `_` plus any an open `__all__`
/// lists.
fn star_names(module: &ParsedModule, local: &ExportTable, reached: &Reached) -> Vec<SmolStr> {
    if module.dunder_all_form == DunderAllForm::Literal {
        return module.dunder_all.clone();
    }
    let declared = local.get(&module.path).into_iter().flatten();
    let imported = reached.get(&module.path).into_iter().flatten();
    declared
        .map(|(name, _)| name)
        .chain(imported.map(|(name, _)| name))
        .filter(|name| is_star_importable(name))
        .chain(&module.dunder_all)
        .cloned()
        .collect()
}

/// The nodes `name` stands for in `module`: its own node, if the module
/// declares or re-exports it, and everything it was imported from.
fn leads_to(
    module: &SmolStr,
    name: &SmolStr,
    local: &ExportTable,
    reached: &Reached,
) -> Vec<NodeId> {
    let own = local
        .get(module)
        .and_then(|names| names.get(name))
        .into_iter()
        .copied();
    let imported = reached
        .get(module)
        .and_then(|names| names.get(name))
        .into_iter()
        .flatten()
        .copied();
    own.chain(imported).collect()
}
