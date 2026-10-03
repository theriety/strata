//! Out-of-line module discovery and cfg-gate propagation to child files.
//!
//! A file reached through `#[cfg(test)] mod x;` (and everything it declares in
//! turn) is compiled only under `cfg(test)`, though nothing inside it says so.
//! A file reached through a declaration the loader may drop is invisible to the
//! semantic loader, so its names become advisory uses (see [`super::advisory`]).

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use syn::Item;

use super::advisory::mark_all;
use super::globs::{module_paths, resolve_globs};
use super::metadata::{has_cfg_test, loader_may_drop};
use super::{Declaration, ParsedFile};

/// An out-of-line `mod name;` declaration found in a file.
pub(super) struct ModDecl {
    /// Directory segments contributed by enclosing inline modules.
    prefix: Vec<String>,
    /// The declared module name.
    name: String,
    /// The `#[path = ".."]` value, when present.
    path: Option<String>,
    /// Whether the declaration (or an enclosing inline module) is `cfg(test)`.
    cfg_test: bool,
    /// Whether the declaration (or an enclosing inline module) may be dropped by the loader.
    may_drop: bool,
}

/// A parsed file's module declarations and source text.
pub(super) struct Modules {
    /// The out-of-line module declarations.
    pub(super) decls: Vec<ModDecl>,
    /// The file contents, for name collection in such files.
    pub(super) source: String,
    /// Whether an inner `#![cfg(..)]` may make the loader drop the whole file.
    pub(super) file_gated: bool,
}

/// Collects the out-of-line module declarations among `items`.
pub(super) fn collect_mod_decls(items: &[Item]) -> Vec<ModDecl> {
    let mut found = Vec::new();
    collect(items, &[], (false, false), &mut found);
    found
}

/// Walks `items`, carrying the `(cfg_test, may_drop)` gates of enclosing modules.
fn collect(items: &[Item], prefix: &[String], gates: (bool, bool), out: &mut Vec<ModDecl>) {
    for item in items {
        let Item::Mod(item_mod) = item else { continue };
        let cfg_test = gates.0 || has_cfg_test(&item_mod.attrs);
        // a `cfg_attr` may add a `path` or another cfg we cannot evaluate.
        let may_drop = gates.1
            || loader_may_drop(&item_mod.attrs)
            || item_mod
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("cfg_attr"));
        let name = item_mod.ident.to_string();
        if let Some((_, nested)) = &item_mod.content {
            let mut inner = prefix.to_vec();
            inner.push(name);
            collect(nested, &inner, (cfg_test, may_drop), out);
        } else {
            out.push(ModDecl {
                prefix: prefix.to_vec(),
                name,
                path: path_attr(&item_mod.attrs),
                cfg_test,
                may_drop,
            });
        }
    }
}

/// The string value of a `#[path = ".."]` attribute.
fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(pair) = &attr.meta else {
            return None;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) = &pair.value
        else {
            return None;
        };
        pair.path.is_ident("path").then(|| text.value())
    })
}

/// Marks every declaration of a file reached through a `cfg(test)` module
/// declaration as `cfg(test)` code with no production SLOC, and records the
/// names used in files reached through a declaration the loader may drop.
pub(super) fn propagate_gates(parsed: &mut [(ParsedFile, Modules)]) {
    let known: HashSet<SmolStr> = parsed.iter().map(|(file, _)| file.path.clone()).collect();
    let children = resolve_children(parsed, &known);

    let test_files = reach(&children, |child| child.cfg_test);
    let mut dropped_files = reach(&children, |child| child.may_drop);
    let gated: Vec<SmolStr> = parsed
        .iter()
        .filter(|(_, modules)| modules.file_gated)
        .map(|(file, _)| file.path.clone())
        .collect();
    descend(&children, gated, &mut dropped_files);

    for (file, modules) in parsed.iter_mut() {
        if test_files.contains(&file.path) {
            file.declarations.iter_mut().for_each(mark_cfg_test);
            file.re_exports
                .iter_mut()
                .for_each(|re_export| re_export.cfg_test = true);
        }
        if dropped_files.contains(&file.path) {
            mark_all(file, &modules.source);
        }
    }
    let paths = module_paths(parsed, &children);
    resolve_globs(parsed, &paths);
}

/// Files gated by `select` on some declaration, plus all their descendants.
fn reach(children: &HashMap<SmolStr, Vec<Child>>, select: fn(&Child) -> bool) -> HashSet<SmolStr> {
    let mut found: HashSet<SmolStr> = HashSet::new();
    let seeds: Vec<SmolStr> = children
        .values()
        .flatten()
        .filter(|child| select(child))
        .map(|child| child.path.clone())
        .collect();
    descend(children, seeds, &mut found);
    found
}

/// Adds `seeds` and every file their module declarations reach to `found`.
fn descend(
    children: &HashMap<SmolStr, Vec<Child>>,
    seeds: Vec<SmolStr>,
    found: &mut HashSet<SmolStr>,
) {
    let mut pending: Vec<SmolStr> = seeds
        .into_iter()
        .filter(|path| found.insert(path.clone()))
        .collect();
    while let Some(path) = pending.pop() {
        for child in children.get(&path).into_iter().flatten() {
            if found.insert(child.path.clone()) {
                pending.push(child.path.clone());
            }
        }
    }
}

/// Resolves every file's children. A file loaded through `#[path]` owns its
/// directory, which in turn changes where its own children are found, so the
/// set of such files is grown until it is stable.
fn resolve_children(
    parsed: &[(ParsedFile, Modules)],
    known: &HashSet<SmolStr>,
) -> HashMap<SmolStr, Vec<Child>> {
    let mut path_loaded: HashSet<SmolStr> = HashSet::new();
    loop {
        let children: HashMap<SmolStr, Vec<Child>> = parsed
            .iter()
            .map(|(file, modules)| {
                let owns_dir = path_loaded.contains(&file.path);
                (
                    file.path.clone(),
                    child_files(&file.path, &modules.decls, known, owns_dir),
                )
            })
            .collect();
        let grown: HashSet<SmolStr> = children
            .values()
            .flatten()
            .filter(|child| child.via_path)
            .map(|child| child.path.clone())
            .collect();
        if grown.is_subset(&path_loaded) {
            return children;
        }
        path_loaded.extend(grown);
    }
}

fn mark_cfg_test(declaration: &mut Declaration) {
    declaration.cfg_test = true;
    declaration.sloc = 0;
}

/// A resolved child module file with the gate and origin of its declaration.
pub(super) struct Child {
    pub(super) path: SmolStr,
    /// Module path segments below the declaring file's module.
    pub(super) module: Vec<String>,
    cfg_test: bool,
    may_drop: bool,
    via_path: bool,
}

/// Whether a file is a cargo target root, whose children live beside it rather
/// than in a directory named after it: `mod.rs`, `src/lib.rs`, `src/main.rs`,
/// `src/bin/*.rs`, and the package-level `build.rs`, `tests/*.rs`,
/// `examples/*.rs` and `benches/*.rs`. A folder merely named `tests` or `bin`
/// deeper inside `src` holds ordinary modules.
fn is_dir_root(path: &str) -> bool {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let segments: Vec<&str> = dir
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let in_src = segments.contains(&"src");
    let folder = segments.last().copied().unwrap_or("");
    match file {
        "mod.rs" => true,
        "lib.rs" | "main.rs" => folder == "src" || segments.is_empty(),
        "build.rs" => !in_src,
        _ => {
            (folder == "bin" && segments.iter().rev().nth(1) == Some(&"src"))
                || (matches!(folder, "tests" | "examples" | "benches") && !in_src)
        }
    }
}

/// Resolves each declared child module to a known file, with its gates.
/// `owns_dir` is set for a file that was itself loaded through `#[path]`.
fn child_files(
    parent: &str,
    mods: &[ModDecl],
    known: &HashSet<SmolStr>,
    owns_dir: bool,
) -> Vec<Child> {
    let (dir, file) = parent.rsplit_once('/').unwrap_or(("", parent));
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let base = if owns_dir || is_dir_root(parent) {
        dir.to_string()
    } else {
        join(dir, stem)
    };
    mods.iter()
        .filter_map(|decl| {
            let candidates = if let Some(path) = &decl.path {
                let start = if decl.prefix.is_empty() { dir } else { &base };
                let inline = decl
                    .prefix
                    .iter()
                    .fold(start.to_string(), |acc, s| join(&acc, s));
                vec![normalize(&join(&inline, path))]
            } else {
                let root = decl
                    .prefix
                    .iter()
                    .fold(base.clone(), |acc, s| join(&acc, s));
                let module = join(&root, &decl.name);
                vec![format!("{module}.rs"), format!("{module}/mod.rs")]
            };
            candidates
                .into_iter()
                .map(SmolStr::new)
                .find(|candidate| known.contains(candidate))
                .map(|path| Child {
                    path,
                    module: decl
                        .prefix
                        .iter()
                        .cloned()
                        .chain(std::iter::once(decl.name.clone()))
                        .collect(),
                    cfg_test: decl.cfg_test,
                    may_drop: decl.may_drop,
                    via_path: decl.path.is_some(),
                })
        })
        .collect()
}

/// Resolves `.` and `..` segments of a relative path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    parts.join("/")
}

fn join(base: &str, segment: &str) -> String {
    if base.is_empty() {
        segment.to_string()
    } else {
        format!("{base}/{segment}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, path: Option<&str>) -> ModDecl {
        ModDecl {
            prefix: Vec::new(),
            name: name.to_string(),
            path: path.map(str::to_string),
            cfg_test: false,
            may_drop: false,
        }
    }

    fn resolve(parent: &str, decl: ModDecl, known: &[&str], owns_dir: bool) -> Vec<String> {
        let known: HashSet<SmolStr> = known.iter().map(|path| SmolStr::new(*path)).collect();
        child_files(parent, &[decl], &known, owns_dir)
            .into_iter()
            .map(|child| child.path.to_string())
            .collect()
    }

    #[test]
    fn should_treat_a_bin_target_as_a_directory_root() {
        let found = resolve(
            "src/bin/tool.rs",
            decl("util", None),
            &["src/bin/util.rs"],
            false,
        );

        assert_eq!(found, vec!["src/bin/util.rs"]);
    }

    #[test]
    fn should_treat_a_package_level_tests_file_as_a_directory_root() {
        let found = resolve(
            "tests/it.rs",
            decl("common", None),
            &["tests/common.rs"],
            false,
        );

        assert_eq!(found, vec!["tests/common.rs"]);
    }

    #[test]
    fn should_not_treat_a_module_under_a_src_tests_folder_as_a_root() {
        let found = resolve(
            "src/tests/x.rs",
            decl("y", None),
            &["src/tests/x/y.rs", "src/tests/y.rs"],
            false,
        );

        assert_eq!(found, vec!["src/tests/x/y.rs"]);
    }

    #[test]
    fn should_treat_a_plain_module_file_as_a_module_directory() {
        let found = resolve(
            "src/a.rs",
            decl("b", None),
            &["src/a/b.rs", "src/b.rs"],
            false,
        );

        assert_eq!(found, vec!["src/a/b.rs"]);
    }

    #[test]
    fn should_let_a_path_loaded_file_own_its_directory() {
        let known = ["src/a/b.rs", "src/b.rs"];

        assert_eq!(
            resolve("src/a.rs", decl("b", None), &known, true),
            vec!["src/b.rs"]
        );
    }

    #[test]
    fn should_resolve_a_path_attribute_against_the_declaring_files_directory() {
        let found = resolve(
            "src/a.rs",
            decl("x", Some("../shared/x.rs")),
            &["shared/x.rs", "src/shared/x.rs"],
            false,
        );

        assert_eq!(found, vec!["shared/x.rs"]);
    }
}
