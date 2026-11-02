//! Restricted-visibility scope resolution.
//!
//! Resolves every `pub(in path)` declaration and re-export to the complete
//! analyzed module subtree it is visible within, emitting the IR visibility
//! sidecar. The syntactic and semantic child-module views must agree, otherwise
//! the scope is left unresolved.

use std::collections::BTreeSet;
use std::path::Path;

use ra_ap_ide::{RootDatabase, Semantics, TextSize};
use ra_ap_syntax::{
    AstNode as _, SyntaxNode,
    ast::{self, HasModuleItem as _, HasName as _},
};
use smol_str::SmolStr;
use strata_ir::VisibilityScope;

use super::assignment::NodeAssignment;
use super::database::Database;
use crate::parse::{ParsedFile, VisibilityKind};

impl Database {
    /// Resolves a restricted path to the complete analyzed module subtree.
    fn visibility_files(
        &self,
        path: &str,
        offset: u32,
        visibility_path: &str,
        repository_root: &Path,
        parsed_paths: &BTreeSet<SmolStr>,
    ) -> Option<ResolvedScope> {
        let file_id = self.file_id_for(path, repository_root)?;
        let semantics = Semantics::new(self.host.raw_database());
        if semantics.file_to_module_defs(file_id).count() != 1 {
            return None;
        }

        let parsed = semantics.parse_guess_edition(file_id);
        let recorded = TextSize::new(offset);
        if !parsed.syntax().text_range().contains(recorded) {
            return None;
        }
        let mut token = parsed.syntax().token_at_offset(recorded).right_biased()?;
        // a declaration's recorded start may sit on its leading doc comment;
        // the module scope is the same, so move on to the first real token.
        while token.kind().is_trivia() {
            token = token.next_token()?;
        }
        let position = token.text_range().start();
        let scope_node = token.parent()?;
        let current = semantics.scope_at_offset(&scope_node, position)?.module();
        let mut target = current;
        for (index, segment) in visibility_path.split("::").enumerate() {
            match segment {
                "crate" if index == 0 => target = current.crate_root(semantics.db),
                "self" if index == 0 => {}
                "super" => target = target.parent(semantics.db)?,
                "crate" | "self" | "" => return None,
                name => {
                    let expected = name.strip_prefix("r#").unwrap_or(name);
                    let child = {
                        let mut matches = target.children(semantics.db).filter(|child| {
                            child
                                .name(semantics.db)
                                .is_some_and(|child_name| child_name.as_str() == expected)
                        });
                        let only = matches.next()?;
                        if matches.next().is_some() {
                            return None;
                        }
                        only
                    };
                    target = child;
                }
            }
        }
        if !current.path_to_root(semantics.db).contains(&target) {
            return None;
        }

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
            let mut syntactic_names = direct_module_names(&semantics, &definition.value)?;
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
        Some(ResolvedScope {
            files: files.into_iter().collect(),
            definition_file,
        })
    }
}

/// A resolved restricted-visibility module: its files and definition file.
struct ResolvedScope {
    files: Vec<SmolStr>,
    definition_file: Option<SmolStr>,
}

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

/// Resolves every restricted declaration and re-export into an IR sidecar.
pub(super) fn resolve_visibility_scopes(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    database: &Database,
    repository_root: &Path,
) -> Vec<VisibilityScope> {
    let parsed_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let mut scopes = Vec::new();
    for file in files {
        for declaration in &file.declarations {
            let Some((node, visibility_path)) = assignment
                .node_at(&file.path, declaration.byte_start)
                .zip(declaration.visibility_path.as_deref())
                .filter(|_| declaration.visibility_kind == VisibilityKind::Restricted)
            else {
                continue;
            };
            if let Some(resolved) = database.visibility_files(
                &file.path,
                declaration.byte_start,
                visibility_path,
                repository_root,
                &parsed_paths,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: resolved.files,
                    definition_file: resolved.definition_file,
                });
            }
        }
        for re_export in &file.re_exports {
            let Some((node, visibility_path)) = assignment
                .re_export_node_at(&file.path, re_export.offset)
                .zip(re_export.visibility_path.as_deref())
                .filter(|_| re_export.visibility_kind == VisibilityKind::Restricted)
            else {
                continue;
            };
            if let Some(resolved) = database.visibility_files(
                &file.path,
                re_export.offset,
                visibility_path,
                repository_root,
                &parsed_paths,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: resolved.files,
                    definition_file: resolved.definition_file,
                });
            }
        }
    }
    scopes
}
