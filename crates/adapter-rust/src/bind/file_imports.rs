//! File-import facts stated from `mod` declarations, `use` items and qualified
//! paths (E3).
//!
//! - `mod x;` is an [`ImportKind::ModuleDeclaration`] from the declaring file to
//!   the module file, resolved through the module tree at parse time (`x.rs`,
//!   `x/mod.rs`, or the `#[path]` target). It states ownership in the module
//!   tree, not a use of the child's items; an inline `mod x { .. }` states
//!   nothing.
//! - A `use` leaf is resolved through the semantic database at the leaf's
//!   offset, so it lands on the file that defines the named item, exactly the
//!   way a value-import edge does. A glob resolves through the module before
//!   it. One fact per `use` statement per target file: a grouped `use` that
//!   reaches two files states two facts with the same line.
//! - A qualified path outside a `use` tree (`child::run()`, `crate::a::f()`,
//!   `field: super::Config`) is a [`ImportKind::Value`] import when it starts
//!   with a module. Its segments resolve left to right through the same
//!   database: the module prefix is the leading run that names modules, and the
//!   fact lands where the equivalent `use` would, on the file that defines the
//!   first item after the prefix (or on the prefix's last module when that item
//!   does not resolve). The specifier is the prefix as written (`child`,
//!   `crate::a`, `super`), the line the one the path starts on. Only a file of
//!   the importer's own crate is a target: a path into another crate, the
//!   sysroot or a file outside the analysed set, a path that starts with an
//!   item (`Report::new()`, whose `use` states the import), and one whose first
//!   segment does not resolve state nothing, and never make the fragment stop
//!   claiming to state its imports, since they are not import statements.
//!   A `use` into another analysed workspace crate does state that crate's
//!   file (below); a qualified path does not. The asymmetry cannot hide a
//!   cycle: Cargo forbids cyclic crate dependencies, so no import cycle spans
//!   two crates, and the cross-crate `use` facts only serve as located imports.
//! - A leaf into an external crate, the sysroot, or a file outside the analysed
//!   set states nothing. Workspace crates are not external: a path through
//!   another analysed crate resolves to that crate's file like any other.
//! - Facts are deduplicated per (importer, imported, line), and a target in the
//!   importer's own file states nothing.
//!
//! Test-only code (`cfg(test)` items, inline test modules, `#[test]` functions,
//! files reached through a `cfg(test)` `mod`) and macro token trees state
//! nothing; parsing leaves them out (see [`crate::parse`]).
//!
//! A `use` leaf the database cannot resolve at all (an item the loader never
//! saw, under a `cfg` that is off, or produced by a macro) is left out, and the
//! fragment then does not claim to state its imports.

use std::collections::BTreeSet;
use std::path::Path;

use ra_ap_ide::{Crate, FileId};
use smol_str::SmolStr;
use unlace_ir::{FileImport, ImportKind};

use super::database::{Database, ResolvedTarget};
use crate::parse::{ParsedFile, QualifiedPath};

/// The import facts of `files`, sorted and deduplicated, and whether every
/// `use` that is not external resolved (the fragment then states its imports).
pub(super) fn file_imports(
    files: &[ParsedFile],
    database: &Database,
    workspace_root: &Path,
) -> (Vec<FileImport>, bool) {
    let present: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let loaded = database.loaded_files(&present, workspace_root);
    let mut imports = Vec::new();
    let mut seen: BTreeSet<(SmolStr, SmolStr, u32)> = BTreeSet::new();
    let mut complete = true;
    for file in files {
        imports.extend(
            file.mod_imports
                .iter()
                .filter(|module| module.imported != file.path)
                .map(|module| FileImport {
                    importer: file.path.clone(),
                    imported: module.imported.clone(),
                    kind: ImportKind::ModuleDeclaration,
                    specifier: module.name.clone(),
                    line: module.line,
                }),
        );
        let mut push = |imported: SmolStr, specifier: SmolStr, line: u32| {
            // one import per target file per line, the first in source order
            // naming the specifier.
            let first = seen.insert((file.path.clone(), imported.clone(), line));
            if imported != file.path && present.contains(imported.as_str()) && first {
                imports.push(FileImport {
                    importer: file.path.clone(),
                    imported,
                    kind: ImportKind::Value,
                    specifier,
                    line,
                });
            }
        };
        for statement in &file.uses {
            for leaf in &statement.leaves {
                match database.resolve(&file.path, leaf.offset, workspace_root) {
                    None => complete = false,
                    Some(ResolvedTarget::External) => {}
                    Some(ResolvedTarget::Workspace { path, .. }) => {
                        push(path, SmolStr::new(&leaf.path), statement.line);
                    }
                }
            }
        }
        let Some((file_id, crates)) = loaded.get(&file.path) else {
            continue;
        };
        for path in &file.paths {
            let reached = path_target(path, *file_id, database, workspace_root);
            if let Some((imported, specifier)) = reached
                && shares_crate(crates, loaded.get(&imported))
            {
                push(imported, specifier, path.line);
            }
        }
    }
    imports.sort();
    (imports, complete)
}

/// The file a qualified path reaches through its module prefix, with that
/// prefix as written, or `None` when the path starts with an item, leaves the
/// workspace, or its first segment does not resolve.
fn path_target(
    path: &QualifiedPath,
    file_id: FileId,
    database: &Database,
    workspace_root: &Path,
) -> Option<(SmolStr, SmolStr)> {
    let mut module = None;
    let mut depth = 0;
    for segment in &path.segments {
        match database.resolve_in(file_id, segment.offset, workspace_root) {
            Some(ResolvedTarget::Workspace {
                path: target,
                module: is_module,
                crate_root,
                ..
            }) if is_module || crate_root => {
                module = Some(target);
                depth += 1;
            }
            Some(ResolvedTarget::Workspace { path: target, .. }) => {
                return (depth > 0).then(|| (target, prefix_text(path, depth)));
            }
            Some(ResolvedTarget::External) => return None,
            // an item the database cannot resolve (behind an unexpanded
            // macro, say) is still reached through its module.
            None => break,
        }
    }
    module.map(|target| (target, prefix_text(path, depth)))
}

/// The first `depth` segments of `path` as written, `::`-joined.
fn prefix_text(path: &QualifiedPath, depth: usize) -> SmolStr {
    let names: Vec<&str> = path
        .segments
        .iter()
        .take(depth)
        .map(|segment| segment.name.as_str())
        .collect();
    let lead = if path.absolute { "::" } else { "" };
    SmolStr::new(format!("{lead}{}", names.join("::")))
}

/// Whether the target file belongs to one of the importer's `crates`.
fn shares_crate(crates: &[Crate], target: Option<&(FileId, Vec<Crate>)>) -> bool {
    target.is_some_and(|(_, owners)| owners.iter().any(|owner| crates.contains(owner)))
}
