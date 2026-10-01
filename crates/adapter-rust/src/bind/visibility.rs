//! Restricted-visibility scope resolution.
//!
//! Resolves every `pub(in path)` declaration and re-export to the complete
//! analyzed module subtree it is visible within, emitting the IR visibility
//! sidecar. The syntactic and semantic child-module views must agree, otherwise
//! the scope is left unresolved.

mod ladder;

use std::collections::{BTreeSet, hash_map::Entry};
use std::path::Path;

use ra_ap_ide::{Semantics, TextSize};
use ra_ap_syntax::AstNode as _;
use smol_str::SmolStr;
use strata_ir::{ScopeRung, VisibilityScope};

use self::ladder::ScopeCache;
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
        cache: &mut ScopeCache,
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

        let chain = current.path_to_root(semantics.db);
        let mut rungs = Vec::with_capacity(chain.len());
        let mut declared = None;
        for module in chain {
            let scope = match cache.entry(module) {
                Entry::Occupied(entry) => entry.get().clone(),
                Entry::Vacant(entry) => entry
                    .insert(self.module_scope(&semantics, module, repository_root, parsed_paths))
                    .clone(),
            };
            if module == target {
                declared.clone_from(&scope);
            }
            rungs.push(scope);
        }
        let declared = declared?;
        // a ladder with an unresolved rung is dropped whole: a missing rung
        // could only hide a narrower spelling, so no floor is better.
        let expressible = rungs
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        Some(ResolvedScope {
            files: declared.files,
            definition_file: declared.definition_file,
            expressible: expressible
                .into_iter()
                .map(|rung| ScopeRung {
                    files: rung.files,
                    definition_file: rung.definition_file,
                })
                .collect(),
        })
    }
}

/// A resolved restricted-visibility module: its files and definition file.
struct ResolvedScope {
    files: Vec<SmolStr>,
    definition_file: Option<SmolStr>,
    expressible: Vec<ScopeRung>,
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
    let mut cache = ScopeCache::new();
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
                &mut cache,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: resolved.files,
                    definition_file: resolved.definition_file,
                    expressible: resolved.expressible,
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
                &mut cache,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: resolved.files,
                    definition_file: resolved.definition_file,
                    expressible: resolved.expressible,
                });
            }
        }
    }
    scopes
}
