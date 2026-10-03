//! Public API of library crates (ADR-25 rule 8).
//!
//! An item a library exports is used by crates strata cannot see, so advice
//! never narrows it. The set is asked of rust-analyzer instead of re-derived
//! from syntax: a module's scope seen from a module of *another* crate is
//! exactly the names an outside user can write, with glob and renamed
//! `pub use` already resolved and every `pub(..)` import filtered out.
//!
//! Anything the semantic view cannot settle counts as public API, because the
//! flag only ever silences advice: a `pub` declaration the loader may drop
//! (any cfg, ADR-25 rule 7) or sits in a file it never loaded is public unless
//! it belongs to a crate known not to be a library.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use ra_ap_hir::{Crate, HasVisibility as _, Module, ModuleDef, ScopeDef, Visibility, attach_db};
use ra_ap_ide::{RootDatabase, Semantics, TryToNav as _};
use smol_str::SmolStr;
use strata_ir::NodeId;

use super::assignment::NodeAssignment;
use super::database::Database;
use crate::parse::{ParsedFile, VisibilityKind};

/// Nodes of the workspace that belong to the public API of a library crate,
/// and the workspace files the loader reached.
pub(super) fn public_api_nodes(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    database: &Database,
    repository_root: &Path,
) -> (BTreeSet<NodeId>, HashSet<SmolStr>) {
    let semantics = Semantics::new(database.host.raw_database());
    let mut marker = Marker {
        semantics: &semantics,
        database,
        assignment,
        repository_root,
        found: BTreeSet::new(),
        loaded: HashSet::new(),
        non_library: HashSet::new(),
        library_files: HashSet::new(),
    };
    // type lowering (an impl's self type) needs the database attached.
    attach_db(semantics.db, || {
        let crates = Crate::all(semantics.db);
        for krate in &crates {
            let root = krate.root_module(semantics.db);
            if marker.path_of(ModuleDef::Module(root)).is_none() {
                continue;
            }
            let library = marker.is_library_root(root);
            marker.record_files(root, library);
            if library {
                let outsider = marker.outsider(&crates, *krate);
                marker.walk_crate(*krate, root, outsider);
            }
        }
    });
    let unseen = unseen_public_declarations(files, assignment, &marker);
    marker.found.extend(unseen);
    (marker.found, marker.loaded)
}

/// A `pub` declaration the semantic view cannot place: the loader may drop it
/// (non-empty advisory names) or never loaded its file. It counts as public
/// API unless its file belongs to a crate known not to be a library.
fn unseen_public_declarations(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    marker: &Marker,
) -> Vec<NodeId> {
    files
        .iter()
        .filter(|file| !marker.excluded(file.path.as_str()))
        .flat_map(|file| {
            let loaded = marker.loaded.contains(file.path.as_str());
            file.declarations
                .iter()
                .filter(move |declaration| {
                    declaration.visibility_kind == VisibilityKind::Public
                        && (!declaration.advisory_names.is_empty() || !loaded)
                })
                .filter_map(|declaration| assignment.node_at(&file.path, declaration.byte_start))
        })
        .collect()
}

/// Walks library crates and records the nodes their public API declares.
struct Marker<'a> {
    semantics: &'a Semantics<'a, RootDatabase>,
    database: &'a Database,
    assignment: &'a NodeAssignment,
    repository_root: &'a Path,
    found: BTreeSet<NodeId>,
    /// Workspace files the loader reached, in any target.
    loaded: HashSet<SmolStr>,
    /// Workspace files that belong to a binary, test or example target.
    non_library: HashSet<SmolStr>,
    /// Workspace files reached from a library target, even when a binary
    /// also compiles them (`#[path]` shared between both).
    library_files: HashSet<SmolStr>,
}

impl Marker<'_> {
    /// Whether a file belongs only to binary, test or example targets.
    fn excluded(&self, file: &str) -> bool {
        self.non_library.contains(file) && !self.library_files.contains(file)
    }

    /// Whether `root` is the root of a library target of the workspace.
    fn is_library_root(&self, root: Module) -> bool {
        let Some(target) = ModuleDef::Module(root).try_to_nav(self.semantics) else {
            return false;
        };
        self.database.is_lib_root(target.call_site().file_id)
    }

    /// The module of the crate an outside user is. A restricted (`pub(crate)`,
    /// `pub(in ..)`) item is `Visibility::Module(..)`, visible only from
    /// descendants of that module, so it is invisible from every module of any
    /// other crate and the choice cannot change the result. Prefer a workspace
    /// member, then any other crate (the sysroot); with no other crate `None`
    /// sees every name, which only over-suppresses.
    fn outsider(&self, crates: &[Crate], krate: Crate) -> Option<Module> {
        let db = self.semantics.db;
        crates
            .iter()
            .filter(|other| **other != krate)
            .map(|other| other.root_module(db))
            .min_by_key(|root| self.path_of(ModuleDef::Module(*root)).is_none())
    }

    /// Records the files of every module under `root` as loaded, and as
    /// non-library when the crate is not a library.
    fn record_files(&mut self, root: Module, library: bool) {
        let db = self.semantics.db;
        let mut stack = vec![root];
        while let Some(module) = stack.pop() {
            stack.extend(module.children(db));
            if let Some(path) = self.path_of(ModuleDef::Module(module)) {
                if library {
                    self.library_files.insert(path.clone());
                } else {
                    self.non_library.insert(path.clone());
                }
                self.loaded.insert(path);
            }
        }
    }

    /// The repository-relative file a definition is declared in.
    fn path_of(&self, def: ModuleDef) -> Option<SmolStr> {
        let target = def.try_to_nav(self.semantics)?;
        self.database
            .relative_path(target.call_site().file_id, self.repository_root)
    }

    /// Records the public items of one crate, then the inherent items of
    /// every public type, which live in impls anywhere in the crate.
    fn walk_crate(&mut self, krate: Crate, root: Module, outsider: Option<Module>) {
        let db = self.semantics.db;
        let mut public_types = HashSet::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(module) = stack.pop() {
            if !seen.insert(module) {
                continue;
            }
            for (_, entry) in module.scope(db, outsider) {
                let ScopeDef::ModuleDef(def) = entry else {
                    continue;
                };
                match def {
                    ModuleDef::Module(child) if child.krate(db) == krate => stack.push(child),
                    ModuleDef::Module(_) => {}
                    ModuleDef::Adt(_) => {
                        public_types.insert(def);
                        self.mark(def);
                    }
                    other => self.mark(other),
                }
            }
        }

        let mut modules = vec![root];
        while let Some(module) = modules.pop() {
            modules.extend(module.children(db));
            for implementation in module.impl_defs(db) {
                let owner = implementation.self_ty(db).as_adt().map(ModuleDef::Adt);
                if implementation.trait_(db).is_some()
                    || !owner.is_some_and(|owner| public_types.contains(&owner))
                {
                    continue;
                }
                for item in implementation.items(db) {
                    let def = ModuleDef::from(item);
                    if def.visibility(db) == Visibility::Public {
                        self.mark(def);
                    }
                }
            }
        }
    }

    /// Records the node a definition declares, when the workspace has one.
    fn mark(&mut self, def: ModuleDef) {
        let Some(target) = def.try_to_nav(self.semantics) else {
            return;
        };
        let target = target.call_site();
        let Some(path) = self
            .database
            .relative_path(target.file_id, self.repository_root)
        else {
            return;
        };
        // the name sits inside the declaration whatever its attributes cover.
        let at = target.focus_range.unwrap_or(target.full_range).start();
        if let Some(node) = self.assignment.node_at(&path, u32::from(at)) {
            self.found.insert(node);
        }
    }
}
