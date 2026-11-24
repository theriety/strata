//! File-import facts: the imports that make one analysed file load another.
//!
//! A fact is emitted per top-level static import or re-export that resolves to
//! an analysed file, through the same resolver the symbol edges use. Type-only
//! statements never reach this pass (the parser leaves them out), and a dynamic
//! `import()` is deliberately not a fact: it loads lazily, after both modules
//! have finished loading, so it cannot form a load-time cycle.

use unlace_ir::FileImport;

use crate::parse::ParsedModule;

use super::resolution::Resolver;

/// Emits one [`FileImport`] per resolvable module load, sorted for determinism.
///
/// Unresolved specifiers (packages, files outside the analysed set) and a file
/// importing itself are left out.
pub(super) fn emit_file_imports(modules: &[ParsedModule], resolver: &Resolver) -> Vec<FileImport> {
    let mut imports: Vec<FileImport> = modules
        .iter()
        .flat_map(|module| {
            module.module_loads.iter().filter_map(|load| {
                let imported = resolver.resolve_load(&module.path, &load.source)?;
                (imported != module.path).then(|| FileImport {
                    importer: module.path.clone(),
                    imported,
                    kind: load.kind,
                    specifier: load.source.clone(),
                    line: load.line,
                })
            })
        })
        .collect();
    imports.sort();
    imports
}
