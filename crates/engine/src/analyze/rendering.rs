//! Rendering of analysis results into the text and structured report views.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ContainerTree, Node, Polarity, ScopeLevel};

use crate::error::StrataError;
use crate::result::{ContainerNode, Level, SymbolPlacement};

mod names;

#[cfg(test)]
mod tests;

use names::{folder_increment, increment_name, merge_sibling_folders, nest_folder_segments};

/// Renders a [`ContainerTree`] into the nested [`ContainerNode`] DTO, attaching
/// each file's symbols and production SLOC.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the tree has no root container.
pub(in crate::analyze) fn render_tree(
    tree: &ContainerTree,
    nodes: &[Node],
    placement: &dyn Fn(&Node) -> Option<ContainerId>,
    key_by_id: &BTreeMap<u32, SmolStr>,
) -> Result<ContainerNode, StrataError> {
    let containers = tree.containers();
    let mut children_by_parent: BTreeMap<u32, Vec<&Container>> = BTreeMap::new();
    let mut roots = Vec::new();
    for container in containers {
        match container.parent {
            Some(parent) => children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container),
            None => roots.push(container),
        }
    }

    let contents = file_contents_by_container(nodes, placement);

    // a forest with several roots is wrapped under a synthetic package group so
    // the DTO is always a single tree; a lone root is rendered directly.
    match roots.as_slice() {
        [] => Err(StrataError::SnapshotInvalid {
            source: strata_ir::SnapshotError::Serialization {
                reason: "container tree has no root".to_owned(),
            },
        }),
        [root] => Ok(render_node(
            root,
            &children_by_parent,
            &contents,
            RenderScope::default(),
            key_by_id,
        )),
        many => Ok(ContainerNode {
            name: "workspace".to_owned(),
            level: Level::PackageGroup,
            children: Some(
                many.iter()
                    .map(|root| {
                        render_node(
                            root,
                            &children_by_parent,
                            &contents,
                            RenderScope::default(),
                            key_by_id,
                        )
                    })
                    .collect(),
            ),
            symbols: None,
            production_sloc: None,
        }),
    }
}

/// The naming context a render walk threads from a parent to its children: the
/// parent's cumulative display name, the parent's undecorated *key*, and the
/// undecorated key of the nearest enclosing package (empty above the package
/// level). The package key backs the folder-increment fallback — a real
/// directory foreign to its elected domain still displays relative to its own
/// package rather than re-embedding the package segment as a fabricated folder.
#[derive(Clone, Copy, Default)]
pub(in crate::analyze) struct RenderScope<'tree> {
    /// The parent's cumulative display name (empty at the root).
    parent_name: &'tree str,
    /// The parent's undecorated key (empty at the root).
    parent_key: &'tree str,
    /// The undecorated key of the nearest enclosing package, or empty when no
    /// package has been descended yet.
    package_key: &'tree str,
}

/// Renders the container nodes `container` contributes to its parent's child
/// list, collapsing two kinds of redundant levels at the render boundary.
///
/// A synthetic bucket names no real directory — it exists only so the internal
/// tree stays strictly level-ascending over a root-level file — so it
/// contributes no node of its own: its children rise to sit directly under the
/// nearest real ancestor (a package's root files become siblings of its real
/// folders). A domain whose undecorated key repeats its package's key carries
/// no naming information of its own either — the elected label merely echoes
/// the level above — so it is suppressed the same way: its folders and files
/// hang directly under the package. The internal tree keeps both containers;
/// only the DTO drops them. Every other container contributes itself.
fn render_contributions(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    scope: RenderScope<'_>,
    key_by_id: &BTreeMap<u32, SmolStr>,
) -> Vec<ContainerNode> {
    let own_key = key_by_id
        .get(&container.id.0)
        .map_or(container.name.as_str(), SmolStr::as_str);
    let echoes_package = container.level == ScopeLevel::Domain && own_key == scope.package_key;
    if container.synthetic || echoes_package {
        return children_by_parent
            .get(&container.id.0)
            .map(|children| {
                children
                    .iter()
                    .flat_map(|child| {
                        render_contributions(child, children_by_parent, contents, scope, key_by_id)
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    vec![render_node(
        container,
        children_by_parent,
        contents,
        scope,
        key_by_id,
    )]
}

/// Recursively renders one container and its descendants.
///
/// Interior container names are *cumulative* path prefixes internally; the DTO
/// carries only each node's increment over its parent so a rendered tree never
/// repeats segments. Files keep their full path (their stable identity) and a
/// root keeps its own name. A folder's multi-segment increment is a real
/// relative directory path, so it expands into one nested folder node per
/// segment ([`nest_folder_segments`]); slash-named domains, packages, and
/// groups are elected labels and render whole.
///
/// A folder's increment strips its parent's *key* (`scope.parent_key`), not the
/// parent's rendered display name: a domain whose display label
/// `qualify_elected` decorated (`core (constellation-ts.core)`) is no longer a
/// prefix of the folder key, so stripping the label would leave the whole key to
/// re-embed as a fabricated directory chain. `key_by_id` supplies the
/// undecorated key of any decorated ancestor; every other container keys on its
/// own name, so the two coincide and the render is unchanged. A folder key
/// foreign to its domain falls back to stripping the enclosing *package* key
/// ([`folder_increment`]), so a cross-domain real directory displays relative
/// to its own package instead of re-embedding the package segment as a
/// fabricated directory.
fn render_node(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    scope: RenderScope<'_>,
    key_by_id: &BTreeMap<u32, SmolStr>,
) -> ContainerNode {
    if container.level == ScopeLevel::File {
        let file = contents.get(&container.id.0);
        let symbols = file.map(|file| file.symbols.clone()).unwrap_or_default();
        let production_sloc = file.map_or(0, |file| file.production_sloc);
        return ContainerNode {
            name: container.name.to_string(),
            level: Level::from(container.level),
            children: None,
            symbols: Some(symbols),
            production_sloc: Some(production_sloc),
        };
    }

    let own_key = key_by_id
        .get(&container.id.0)
        .map_or(container.name.as_str(), SmolStr::as_str);
    let child_scope = RenderScope {
        parent_name: &container.name,
        parent_key: own_key,
        // descending a package establishes the fallback key its folders strip
        // against; every other level threads the enclosing package unchanged.
        package_key: if container.level == ScopeLevel::Package {
            own_key
        } else {
            scope.package_key
        },
    };
    let children = children_by_parent
        .get(&container.id.0)
        .map(|children| {
            let rendered = children
                .iter()
                .flat_map(|child| {
                    render_contributions(
                        child,
                        children_by_parent,
                        contents,
                        child_scope,
                        key_by_id,
                    )
                })
                .collect();
            merge_sibling_folders(rendered)
        })
        .unwrap_or_default();

    let increment = if container.level == ScopeLevel::Folder {
        folder_increment(&container.name, scope.parent_key, scope.package_key)
    } else {
        increment_name(&container.name, scope.parent_name)
    };
    if container.level == ScopeLevel::Folder {
        return nest_folder_segments(&increment, children);
    }

    ContainerNode {
        name: increment,
        level: Level::from(container.level),
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

/// A file container's rendered contents: the symbols placed in it and the sum of
/// production SLOC over its production-polarity symbols.
struct FileContents {
    /// The symbol placements rendered for the file, in node order.
    symbols: Vec<SymbolPlacement>,
    /// True production SLOC: the sum of `effective_size` over the production
    /// nodes placed in the file.
    production_sloc: u32,
}

/// Groups symbol placements and sums production SLOC by the container each node
/// is placed in, using `placement` to map a node to its (current or candidate)
/// file container.
fn file_contents_by_container(
    nodes: &[Node],
    placement: &dyn Fn(&Node) -> Option<ContainerId>,
) -> BTreeMap<u32, FileContents> {
    let mut by_container: BTreeMap<u32, FileContents> = BTreeMap::new();
    for node in nodes {
        let Some(container) = placement(node) else {
            continue;
        };
        let entry = by_container
            .entry(container.0)
            .or_insert_with(|| FileContents {
                symbols: Vec::new(),
                production_sloc: 0,
            });
        entry.symbols.push(SymbolPlacement {
            name: node.name.to_string(),
            visibility: Level::from(node.visibility),
        });
        if node.polarity == Polarity::Production {
            entry.production_sloc = entry.production_sloc.saturating_add(node.effective_size);
        }
    }
    by_container
}
