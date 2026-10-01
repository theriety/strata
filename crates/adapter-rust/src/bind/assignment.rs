//! Node assignment and location indexing.
//!
//! Assigns a dense node id to every declaration and re-export, builds the IR
//! nodes with their base polarity and visibility, and indexes them by source
//! location and name so resolved targets map back to nodes.

use std::collections::HashMap;
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{ContainerId, Node, NodeId, NodeKind, Polarity, ScopeLevel};

use super::containers::ContainerBuilder;
use crate::parse::{DeclKind, Declaration, ParsedFile, ReExport, VisibilityKind};

/// Assigns a dense node id to every declaration and indexes them by location so
/// resolved definition offsets can be mapped back to nodes.
pub(super) struct NodeAssignment {
    /// Nodes in assignment order; the index equals the [`NodeId`] value.
    nodes: Vec<Node>,
    /// Per-file declaration intervals, sorted by start, for offset lookup.
    intervals: HashMap<SmolStr, Vec<Interval>>,
    /// Declaration name -> the node(s) declaring it, for the name-based
    /// resolution fallback. A name mapping to exactly one node is resolvable;
    /// an ambiguous name (two or more) is left unresolved to stay deterministic.
    by_name: HashMap<SmolStr, Vec<NodeId>>,
    /// Barrel-local nodes materialized for `pub use` re-exports, each carrying
    /// the source location whose `goto_definition` finds the original symbol.
    pub(super) re_export_links: Vec<ReExportLink>,
}

/// A materialized `pub use` re-export: the barrel-local node and the source
/// location [`crate::bind`] resolves to find the re-exported original.
pub(super) struct ReExportLink {
    /// The node created in the re-exporting file for the re-exported name.
    pub(super) source: NodeId,
    /// Repository-relative path of the file that holds the `pub use`.
    pub(super) path: SmolStr,
    /// Byte offset of the re-exported leaf identifier, for `goto_definition`.
    pub(super) offset: u32,
}

/// One declaration's byte interval and the node it was assigned.
struct Interval {
    /// Inclusive lower byte bound.
    start: u32,
    /// Exclusive upper byte bound.
    end: u32,
    /// The node assigned to the declaration.
    node: NodeId,
}

impl NodeAssignment {
    /// Builds the node set and location index from the parsed files.
    pub(super) fn build(files: &[ParsedFile], root: &Path) -> Self {
        let containers = ContainerBuilder::build(files, root);
        let mut nodes = Vec::new();
        let mut intervals: HashMap<SmolStr, Vec<Interval>> = HashMap::new();
        let mut by_name: HashMap<SmolStr, Vec<NodeId>> = HashMap::new();
        let mut re_export_links = Vec::new();

        for file in files {
            let container = containers.file_of(&file.path);
            let file_intervals = intervals.entry(file.path.clone()).or_default();
            for declaration in &file.declarations {
                let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                nodes.push(node_for(declaration, id, container));
                file_intervals.push(Interval {
                    start: declaration.byte_start,
                    end: declaration.byte_end,
                    node: id,
                });
                by_name
                    .entry(declaration.name.clone())
                    .or_default()
                    .push(id);
            }
        }
        for file_intervals in intervals.values_mut() {
            file_intervals.sort_by_key(|interval| interval.start);
        }

        // Barrel nodes for `pub use` re-exports come after all declarations so
        // declaration node ids stay dense and offset lookups are unaffected.
        for file in files {
            let container = containers.file_of(&file.path);
            for re_export in &file.re_exports {
                let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                nodes.push(re_export_node(re_export, id, container));
                re_export_links.push(ReExportLink {
                    source: id,
                    path: file.path.clone(),
                    offset: re_export.offset,
                });
            }
        }

        Self {
            nodes,
            intervals,
            by_name,
            re_export_links,
        }
    }

    /// Returns the node whose declaration interval contains `offset` in `path`.
    ///
    /// When intervals nest (a method inside an impl, say) the innermost — the
    /// one with the latest start that still contains the offset — wins, so a
    /// resolution lands on the most specific declaration.
    pub(super) fn node_at(&self, path: &str, offset: u32) -> Option<NodeId> {
        let file_intervals = self.intervals.get(path)?;
        file_intervals
            .iter()
            .filter(|interval| interval.start <= offset && offset < interval.end)
            .max_by_key(|interval| interval.start)
            .map(|interval| interval.node)
    }

    /// Returns the barrel node materialized at one re-export source location.
    pub(super) fn re_export_node_at(&self, path: &str, offset: u32) -> Option<NodeId> {
        let mut matches = self
            .re_export_links
            .iter()
            .filter(|link| link.path == path && link.offset == offset)
            .map(|link| link.source);
        let node = matches.next()?;
        matches.next().is_none().then_some(node)
    }

    /// Returns the kind of the node assigned `id`.
    pub(super) fn kind_of(&self, id: NodeId) -> Option<NodeKind> {
        let index = usize::try_from(id.0).ok()?;
        self.nodes.get(index).map(|node| node.kind)
    }

    /// Whether a source file in the same crate as `from` declares a module
    /// called `name`: the file stem, or the directory holding a `mod.rs`. A
    /// same-named file in another crate cannot be what `from` refers to.
    pub(super) fn names_a_module(&self, from: &str, name: &str) -> bool {
        let crate_src = crate_src_dir(from);
        self.intervals.keys().any(|file| {
            if crate_src_dir(file) != crate_src {
                return false;
            }
            let file = Path::new(file.as_str());
            let stem = file.file_stem().and_then(|stem| stem.to_str());
            let module = match stem {
                Some("mod") => file
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|folder| folder.to_str()),
                other => other,
            };
            module == Some(name)
        })
    }

    /// Returns the single in-workspace declaration named `name`, or `None` when
    /// no declaration or more than one carries that name. Ambiguous names are
    /// deliberately left unresolved so the fallback never invents an edge.
    pub(super) fn unique_node_named(&self, name: &str) -> Option<NodeId> {
        match self.by_name.get(name)?.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Consumes the assignment, yielding the assembled node set.
    pub(super) fn into_nodes(self) -> Vec<Node> {
        self.nodes
    }
}

/// The repository-relative prefix up to a path's first `src` directory, which
/// identifies the crate owning a source file; a path with no `src` directory
/// yields the empty prefix and so shares one scope with every other such path.
/// The scope is the crate's `src` prefix, not the module path: two files in
/// different modules of one crate still count as the same scope.
fn crate_src_dir(path: &str) -> &str {
    path.split_once("/src/")
        .map_or("", |(crate_dir, _)| crate_dir)
}

/// Builds an IR [`Node`] for a declaration, fixing its base polarity.
///
/// A `#[test]`-attributed function is a [`Polarity::TestCase`] — the entry point
/// of a test. Any other `#[cfg(test)]` item is a shared test utility,
/// [`Polarity::TestSupport`]; everything else starts as [`Polarity::Production`].
/// [`classify_polarity`] refines this with cross-node reachability afterwards.
/// Test-zoned nodes carry zero production SLOC.
fn node_for(declaration: &Declaration, id: NodeId, container: ContainerId) -> Node {
    let polarity = if declaration.test_case {
        Polarity::TestCase
    } else if declaration.cfg_test {
        Polarity::TestSupport
    } else {
        Polarity::Production
    };
    Node {
        id,
        name: declaration.name.clone(),
        kind: match declaration.kind {
            DeclKind::Symbol => NodeKind::Symbol,
            DeclKind::Type => NodeKind::Type,
        },
        polarity,
        container,
        visibility: declared_visibility(declaration.visibility_kind, declaration.exported),
        effective_size: declaration.sloc,
        re_export: false,
    }
}

/// Builds the barrel-local IR [`Node`] for a `pub use` re-export.
///
/// The node carries the re-exported name within the re-exporting file's
/// container, is `pub`-visible (a re-export is always part of the export
/// surface), and has zero size — it owns no source of its own.
fn re_export_node(re_export: &ReExport, id: NodeId, container: ContainerId) -> Node {
    Node {
        id,
        name: re_export.name.clone(),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container,
        visibility: declared_visibility(re_export.visibility_kind, true),
        effective_size: 0,
        re_export: true,
    }
}

/// Maps source visibility to the conservative pre-merge IR scope.
fn declared_visibility(kind: VisibilityKind, legacy_exported: bool) -> ScopeLevel {
    match kind {
        VisibilityKind::Legacy if legacy_exported => ScopeLevel::Package,
        VisibilityKind::Legacy | VisibilityKind::Inherited => ScopeLevel::File,
        VisibilityKind::Public | VisibilityKind::Crate | VisibilityKind::Restricted => {
            ScopeLevel::Package
        }
    }
}
