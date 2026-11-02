//! Visibility scope resolution (ADR-24, ADR-25).
//!
//! Resolves every declaration and re-export that is `pub`, `pub(crate)` or
//! `pub(in path)` to the ladder of modules it could be spelled at, and marks the
//! rung it declares today, emitting the IR visibility sidecar. The syntactic and
//! semantic child-module views must agree, otherwise that rung is left out; a
//! declaration whose own rung cannot be resolved is still stated, with no
//! declared rung, so the engine stays silent for it.

mod ladder;

use std::collections::{BTreeSet, hash_map::Entry};
use std::path::Path;

use ra_ap_hir::Module;
use ra_ap_ide::{Semantics, TextSize};
use ra_ap_syntax::AstNode as _;
use smol_str::SmolStr;
use strata_ir::{NodeId, ScopeRung, VisibilityScope};

use self::ladder::ScopeCache;
use super::assignment::NodeAssignment;
use super::database::Database;
use crate::parse::{ParsedFile, VisibilityKind};

/// Where a declaration's visibility reaches, as written in source.
enum Declared<'a> {
    /// `pub(in path)`, `pub(super)` or `pub(self)`.
    Path(Option<&'a str>),
    /// `pub` or `pub(crate)`: the crate root.
    Crate,
}

impl<'a> Declared<'a> {
    /// The declared form of a visibility, or `None` for a private declaration.
    fn of(kind: VisibilityKind, path: Option<&'a str>) -> Option<Self> {
        match kind {
            VisibilityKind::Restricted => Some(Self::Path(path)),
            VisibilityKind::Public | VisibilityKind::Crate => Some(Self::Crate),
            VisibilityKind::Legacy | VisibilityKind::Inherited => None,
        }
    }
}

impl Database {
    /// Resolves a declaration's scope ladder; a failure leaves it unresolved.
    fn visibility_scope(
        &self,
        node: NodeId,
        path: &str,
        offset: u32,
        declared: &Declared<'_>,
        repository_root: &Path,
        parsed_paths: &BTreeSet<SmolStr>,
        cache: &mut ScopeCache,
    ) -> VisibilityScope {
        let unresolved = VisibilityScope {
            node,
            files: Vec::new(),
            definition_file: None,
            expressible: Vec::new(),
            declared_rung: None,
        };
        self.resolve_ladder(
            node,
            path,
            offset,
            declared,
            repository_root,
            parsed_paths,
            cache,
        )
        .unwrap_or(unresolved)
    }

    /// Resolves the module the declaration sits in, its target module, and every
    /// rung between them and the crate root.
    fn resolve_ladder(
        &self,
        node: NodeId,
        path: &str,
        offset: u32,
        declared: &Declared<'_>,
        repository_root: &Path,
        parsed_paths: &BTreeSet<SmolStr>,
        cache: &mut ScopeCache,
    ) -> Option<VisibilityScope> {
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
        let target = match declared {
            Declared::Crate => current.crate_root(semantics.db),
            Declared::Path(restricted) => restricted_target(&semantics, current, (*restricted)?)?,
        };

        let chain = current.path_to_root(semantics.db);
        if !chain.contains(&target) || is_inline(&semantics, current) {
            // an item inside an inline `mod m { .. }` shares its file with the
            // parent, so no file set can tell the two scopes apart: stay silent.
            return None;
        }
        // an unresolved rung is left out: it could only hide a narrower
        // spelling, and the engine still never advises one it cannot see.
        let mut expressible = Vec::with_capacity(chain.len());
        let mut declared_rung = None;
        let mut declared_scope = None;
        for (index, module) in chain.iter().copied().enumerate() {
            let scope = match cache.entry(module) {
                Entry::Occupied(entry) => entry.get().clone(),
                Entry::Vacant(entry) => entry
                    .insert(self.module_scope(&semantics, module, repository_root, parsed_paths))
                    .clone(),
            };
            let Some(scope) = scope else { continue };
            // an inline module has the file set of its parent, so it is never a
            // rung of its own; its spelling stays in the chain's index.
            if is_inline(&semantics, module) {
                continue;
            }
            if module == target {
                declared_rung = u32::try_from(expressible.len()).ok();
                declared_scope = Some(scope.clone());
            }
            expressible.push(ScopeRung {
                files: scope.files,
                definition_file: scope.definition_file,
                spelling: ladder::spelling(&semantics, &chain, index),
            });
        }
        // only a restricted declaration moves its declared level to the module.
        let (files, definition_file) = match (declared, declared_scope) {
            (Declared::Path(_), Some(scope)) => (scope.files, scope.definition_file),
            _ => (Vec::new(), None),
        };
        Some(VisibilityScope {
            node,
            files,
            definition_file,
            expressible,
            declared_rung,
        })
    }
}

/// Whether a module is written inline (`mod m { .. }`) rather than as a file.
fn is_inline(semantics: &Semantics<'_, ra_ap_ide::RootDatabase>, module: Module) -> bool {
    module.parent(semantics.db).is_some()
        && !ra_ap_syntax::ast::SourceFile::can_cast(
            semantics.module_definition_node(module).value.kind(),
        )
}

/// Walks a `pub(in path)` target from the declaring module.
fn restricted_target(
    semantics: &Semantics<'_, ra_ap_ide::RootDatabase>,
    current: Module,
    visibility_path: &str,
) -> Option<Module> {
    let mut target = current;
    for (index, segment) in visibility_path.split("::").enumerate() {
        match segment {
            "crate" if index == 0 => target = current.crate_root(semantics.db),
            "self" if index == 0 => {}
            "super" => target = target.parent(semantics.db)?,
            "crate" | "self" | "" => return None,
            name => {
                let expected = name.strip_prefix("r#").unwrap_or(name);
                let mut matches = target.children(semantics.db).filter(|child| {
                    child
                        .name(semantics.db)
                        .is_some_and(|child_name| child_name.as_str() == expected)
                });
                let only = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                target = only;
            }
        }
    }
    Some(target)
}

/// States the scope ladder of every `pub`-family declaration and re-export.
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
        let declarations = file.declarations.iter().map(|declaration| {
            (
                assignment.node_at(&file.path, declaration.byte_start),
                declaration.byte_start,
                Declared::of(
                    declaration.visibility_kind,
                    declaration.visibility_path.as_deref(),
                ),
            )
        });
        let re_exports = file.re_exports.iter().map(|re_export| {
            (
                assignment.re_export_node_at(&file.path, re_export.offset),
                re_export.offset,
                Declared::of(
                    re_export.visibility_kind,
                    re_export.visibility_path.as_deref(),
                ),
            )
        });
        for (node, offset, declared) in declarations.chain(re_exports) {
            let Some((node, declared)) = node.zip(declared) else {
                continue;
            };
            scopes.push(database.visibility_scope(
                node,
                &file.path,
                offset,
                &declared,
                repository_root,
                &parsed_paths,
                &mut cache,
            ));
        }
    }
    scopes
}
