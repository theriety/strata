//! Module-scope construction for visibility ladders.
//!
//! Resolves a module to its complete analyzed subtree, checking that the
//! syntactic and semantic child-module views agree, and caches the result so
//! every rung of every ladder is resolved once per pass.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use ra_ap_hir::Module;
use ra_ap_ide::{RootDatabase, Semantics};
use ra_ap_syntax::{
    AstNode as _, SyntaxNode,
    ast::{self, HasModuleItem as _, HasName as _},
};
use smol_str::SmolStr;

use crate::bind::database::Database;

impl Database {
    /// Resolves one module to its complete analyzed subtree.
    pub(super) fn module_scope(
        &self,
        semantics: &Semantics<'_, RootDatabase>,
        target: Module,
        repository_root: &Path,
        parsed_paths: &BTreeSet<SmolStr>,
    ) -> Option<ModuleScope> {
        let mut files = BTreeSet::new();
        let mut definition_file = None;
        let mut pending = vec![target];
        while let Some(module) = pending.pop() {
            let definition = semantics.module_definition_node(module);
            let original = semantics.original_range_opt(&definition.value)?;
            let relative =
                self.relative_path(original.file_id.file_id(semantics.db), repository_root)?;
            if !parsed_paths.contains(&relative) {
                return None;
            }
            if module == target
                && module.parent(semantics.db).is_some()
                && ast::SourceFile::can_cast(definition.value.kind())
            {
                definition_file = Some(relative.clone());
            }

            let children = module.children(semantics.db).collect::<Vec<_>>();
            let mut semantic_names = children
                .iter()
                .map(|child| {
                    child
                        .name(semantics.db)
                        .map(|name| name.as_str().to_owned())
                })
                .collect::<Option<Vec<_>>>()?;
            let mut syntactic_names = direct_module_names(semantics, &definition.value)?;
            semantic_names.sort();
            syntactic_names.sort();
            if semantic_names != syntactic_names {
                return None;
            }

            files.insert(relative);
            pending.extend(children);
        }
        if files.is_empty() {
            return None;
        }
        // the definition file only counts when it owns a folder holding every
        // other file of the scope: `foo.rs` beside `foo/`, or `foo/mod.rs`.
        let definition_file = definition_file.filter(|definition| {
            let folder = match definition.strip_suffix("/mod.rs") {
                Some(directory) => format!("{directory}/"),
                None => format!("{}/", definition.trim_end_matches(".rs")),
            };
            files.len() > 1
                && files
                    .iter()
                    .all(|file| file == definition || file.starts_with(&folder))
        });
        Some(ModuleScope {
            files: files.into_iter().collect(),
            definition_file,
        })
    }
}

/// A module's analyzed subtree and its definition file.
#[derive(Clone)]
pub(super) struct ModuleScope {
    pub(super) files: Vec<SmolStr>,
    pub(super) definition_file: Option<SmolStr>,
}

/// Module scopes already resolved during one pass, shared across declarations.
pub(super) type ScopeCache = HashMap<Module, Option<ModuleScope>>;

/// Returns direct syntactic child-module names for a module definition.
fn direct_module_names(
    semantics: &Semantics<'_, RootDatabase>,
    node: &SyntaxNode,
) -> Option<Vec<String>> {
    if let Some(source) = ast::SourceFile::cast(node.clone()) {
        return module_item_names(semantics, source.items());
    }
    let module = ast::Module::cast(node.clone())?;
    module_item_names(semantics, module.item_list()?.items())
}

/// Collects module names from one syntactic module-item list.
fn module_item_names(
    semantics: &Semantics<'_, RootDatabase>,
    items: impl Iterator<Item = ast::Item>,
) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for item in items {
        if let ast::Item::Module(module) = item {
            // a module the analyzer does not define (`cfg`-disabled in any
            // form) is absent from the semantic children, so skip it here too.
            if semantics.to_module_def(&module).is_none() {
                continue;
            }
            let name = module.name()?.text().to_string();
            names.push(name.strip_prefix("r#").unwrap_or(&name).to_owned());
        }
    }
    Some(names)
}
