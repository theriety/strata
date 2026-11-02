//! Which files a dropped glob `use` may import from (ADR-25 rule 7).
//!
//! A glob brings in every name visible to its file, whatever the visibility.
//! The path before it is resolved through the module tree, and the files of
//! that module and its whole subtree are the candidates. Anything not exact
//! widens to every file of the crate: an unresolved path, an inline module, an
//! external crate, or a subtree holding a `use` that is not private, since
//! names flow onward through it.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;

use super::ParsedFile;
use super::modules::{Child, Modules};

/// File path -> (crate root file, module path segments below the root).
pub(super) type ModulePaths = HashMap<SmolStr, (SmolStr, Vec<String>)>;

/// Each file's crate root and module path below it, through the module tree.
/// A file no declaration reaches is its own root.
pub(super) fn module_paths(
    parsed: &[(ParsedFile, Modules)],
    children: &HashMap<SmolStr, Vec<Child>>,
) -> ModulePaths {
    let reached: HashSet<&SmolStr> = children
        .values()
        .flatten()
        .map(|child| &child.path)
        .collect();
    let mut found = ModulePaths::new();
    for (file, _) in parsed {
        if reached.contains(&file.path) {
            continue;
        }
        found.insert(file.path.clone(), (file.path.clone(), Vec::new()));
        let mut pending = vec![file.path.clone()];
        while let Some(path) = pending.pop() {
            let Some((root, module)) = found.get(&path).cloned() else {
                continue;
            };
            for child in children.get(&path).into_iter().flatten() {
                if !found.contains_key(&child.path) {
                    let nested = module.iter().chain(&child.module).cloned().collect();
                    found.insert(child.path.clone(), (root.clone(), nested));
                    pending.push(child.path.clone());
                }
            }
        }
    }
    found
}

/// Fills `dropped_glob_files` of every file that has a dropped glob.
pub(super) fn resolve_globs(parsed: &mut [(ParsedFile, Modules)], paths: &ModulePaths) {
    let pub_use: HashSet<SmolStr> = parsed
        .iter()
        .filter(|(file, _)| file.pub_use)
        .map(|(file, _)| file.path.clone())
        .collect();
    for (file, _) in parsed.iter_mut() {
        let Some((root, here)) = paths.get(&file.path) else {
            continue;
        };
        let mut targets: Vec<SmolStr> = Vec::new();
        for glob in &file.dropped_globs {
            let scope = target_module(glob, here)
                .filter(|module| paths.values().any(|(r, m)| r == root && m == module));
            let subtree = scope.map(|module| {
                paths
                    .iter()
                    .filter(|(_, (r, m))| r == root && m.starts_with(&module))
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            });
            match subtree {
                Some(files) if !files.iter().any(|path| pub_use.contains(path)) => {
                    targets.extend(files);
                }
                _ => targets.extend(
                    paths
                        .iter()
                        .filter(|(_, (r, _))| r == root)
                        .map(|(path, _)| path.clone()),
                ),
            }
        }
        targets.sort();
        targets.dedup();
        file.dropped_glob_files = targets;
    }
}

/// The module a glob path names, from `crate`, `self` or `super` only.
fn target_module(glob: &str, here: &[String]) -> Option<Vec<String>> {
    let mut segments = glob.split("::").filter(|segment| !segment.is_empty());
    let mut module: Vec<String> = match segments.next()? {
        "crate" => Vec::new(),
        "self" => here.to_vec(),
        "super" => {
            let mut parent = here.to_vec();
            parent.pop()?;
            parent
        }
        _ => return None,
    };
    for segment in segments {
        match segment {
            "super" => {
                module.pop()?;
            }
            "self" => {}
            name => module.push(name.to_string()),
        }
    }
    Some(module)
}
