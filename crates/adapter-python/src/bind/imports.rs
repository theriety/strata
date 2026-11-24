//! File-import facts: which analysed file each load-time import statement loads.
//!
//! The facts come from [`ParsedModule::import_sites`], so an import under
//! `if TYPE_CHECKING:` or inside a function body (a lazy import) never reaches
//! here. Each statement resolves through the same dotted-name table the symbol
//! edges use. A statement ends in one of four ways:
//!
//! - it resolves to analysed files, and one fact is emitted per file;
//! - it names a package outside the analysed set (external), and nothing is
//!   emitted;
//! - it names a namespace package (a folder with no `__init__.py`), which loads
//!   no file, and nothing is emitted;
//! - it names something inside the analysed set that no file answers to
//!   (unresolved). Nothing is emitted, and the adapter then cannot state that
//!   it listed every import, so `states_imports` stays `false`.

use std::collections::HashMap;

use smol_str::SmolStr;
use unlace_ir::{FileImport, ImportKind};

use super::resolver::Resolver;
use crate::parse::{Import, ImportSite, ParsedModule};

/// The import facts of a module set and whether they are complete.
pub(super) struct ImportFacts {
    /// The facts, sorted so emission is deterministic.
    pub(super) imports: Vec<FileImport>,
    /// `true` when every non-external import statement resolved or named a
    /// namespace package.
    pub(super) complete: bool,
}

/// How one name or module of an import statement resolved.
enum Outcome {
    /// The statement loads this analysed file.
    File(SmolStr),
    /// Outside the analysed set, or a namespace package: no file, no failure.
    NoFile,
    /// Inside the analysed set but answered by no file.
    Unresolved,
}

/// Emits the file-import facts for `modules`.
pub(super) fn emit_imports(modules: &[ParsedModule], resolver: &Resolver) -> ImportFacts {
    let by_path: HashMap<&str, &ParsedModule> = modules
        .iter()
        .map(|module| (module.path.as_str(), module))
        .collect();
    let mut imports = Vec::new();
    let mut complete = true;
    for module in modules {
        for site in &module.import_sites {
            for (specifier, outcome) in outcomes(module, site, resolver, &by_path) {
                match outcome {
                    Outcome::File(imported) if imported != module.path => {
                        imports.push(fact(module, site, imported, specifier));
                    }
                    Outcome::Unresolved => complete = false,
                    Outcome::File(_) | Outcome::NoFile => {}
                }
            }
        }
    }
    imports.sort();
    imports.dedup();
    ImportFacts { imports, complete }
}

/// Builds the value-import fact for one statement and target file.
fn fact(
    module: &ParsedModule,
    site: &ImportSite,
    imported: SmolStr,
    specifier: SmolStr,
) -> FileImport {
    FileImport {
        importer: module.path.clone(),
        imported,
        kind: ImportKind::Value,
        specifier,
        line: site.line,
    }
}

/// The module text of a `from` statement as written: the leading dots, then the
/// dotted name.
fn from_specifier(import: &Import) -> SmolStr {
    let dots = ".".repeat(usize::try_from(import.level).unwrap_or(0));
    SmolStr::new(format!("{dots}{}", import.module))
}

/// A resolved module or name, with the specifier its fact would carry.
type Resolved = (SmolStr, Outcome);

/// Resolves every module or name one statement loads.
fn outcomes(
    module: &ParsedModule,
    site: &ImportSite,
    resolver: &Resolver,
    by_path: &HashMap<&str, &ParsedModule>,
) -> Vec<Resolved> {
    if site.import.level == 0 && site.import.module.is_empty() {
        plain_outcomes(&site.import, resolver)
    } else {
        from_outcomes(module, &site.import, resolver, by_path)
    }
}

/// Resolves `import a.b, c`: each target is a dotted module name.
fn plain_outcomes(import: &Import, resolver: &Resolver) -> Vec<Resolved> {
    import
        .targets
        .iter()
        .map(|target| (target.clone(), resolve_dotted(target, resolver)))
        .collect()
}

/// Resolves a dotted module name: a file, an external or namespace package, or
/// nothing at all.
fn resolve_dotted(dotted: &str, resolver: &Resolver) -> Outcome {
    if let Some(file) = resolver.module_of_dotted(&SmolStr::new(dotted)) {
        Outcome::File(file.clone())
    } else if !resolver.is_first_party(dotted) || resolver.has_descendants(dotted) {
        Outcome::NoFile
    } else {
        Outcome::Unresolved
    }
}

/// Resolves `from m import a, b` and `from m import *`.
///
/// A name that is a module of package `m` (and not a name `m` binds itself)
/// resolves to that module's file. Any other name loads `m` itself.
fn from_outcomes(
    module: &ParsedModule,
    import: &Import,
    resolver: &Resolver,
    by_path: &HashMap<&str, &ParsedModule>,
) -> Vec<Resolved> {
    let written = from_specifier(import);
    let Some(dotted) = resolver.absolute_dotted(&module.path, import) else {
        return vec![(written, Outcome::Unresolved)];
    };
    if import.level == 0 && !resolver.is_first_party(&dotted) {
        return vec![(written, Outcome::NoFile)];
    }
    let package = resolver.module_of_dotted(&SmolStr::new(&dotted));
    if import.star {
        let outcome = package.map_or_else(|| resolve_dotted(&dotted, resolver), file);
        return vec![(written, outcome)];
    }
    import
        .targets
        .iter()
        .map(|name| {
            let outcome = name_outcome(&dotted, name, package, resolver, by_path);
            (written.clone(), outcome)
        })
        .collect()
}

/// Wraps a resolved file path as an outcome.
fn file(path: &SmolStr) -> Outcome {
    Outcome::File(path.clone())
}

/// Resolves one imported `name` of the package `dotted`.
fn name_outcome(
    dotted: &str,
    name: &str,
    package: Option<&SmolStr>,
    resolver: &Resolver,
    by_path: &HashMap<&str, &ParsedModule>,
) -> Outcome {
    let bound_by_package = package
        .and_then(|path| by_path.get(path.as_str()))
        .is_some_and(|parsed| binds(parsed, name));
    let submodule = if dotted.is_empty() {
        SmolStr::new(name)
    } else {
        SmolStr::new(format!("{dotted}.{name}"))
    };
    match (resolver.module_of_dotted(&submodule), package) {
        (Some(path), _) if !bound_by_package => file(path),
        (_, Some(path)) => file(path),
        (_, None) if resolver.has_descendants(dotted) => Outcome::NoFile,
        (_, None) => Outcome::Unresolved,
    }
}

/// Whether `module` itself defines or imports `name` at module level.
fn binds(module: &ParsedModule, name: &str) -> bool {
    module
        .declarations
        .iter()
        .any(|declaration| declaration.name == name)
        || module
            .imports
            .iter()
            .chain(&module.conditional_imports)
            .any(|import| import.names.iter().any(|bound| bound == name))
}
