//! Scope-ladder projection (ADR-24): resolves adapter-stated visibility scopes
//! onto the final laminar container tree.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::{
    ContainerId, ContainerTree, Edge, EdgeKind, Node, NodeId, ScopeLadder, ScopeLevel,
    VisibilityScope,
};

use super::reexport::{name_lookup, re_export_targets, resolve_re_export};

/// Projects complete visibility sidecars onto the final laminar tree and
/// returns the expressible-scope ladders they state.
pub(super) fn apply_visibility_scopes(
    nodes: &mut [Node],
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    edges: &[Edge],
    visibility_scopes: &[VisibilityScope],
) -> Vec<ScopeLadder> {
    // a rung is a real spelling for a declaration only if it reaches every file
    // that uses it; the file containers of each declaration's consumers decide.
    // the set is read the way `derive_visibility` reads it, after re-export
    // flattening: a consumer of a re-export node is a consumer of its original.
    let node_containers: HashMap<NodeId, ContainerId> =
        nodes.iter().map(|node| (node.id, node.container)).collect();
    let re_export_target = re_export_targets(edges);
    let name_of = name_lookup(nodes);
    let mut consumers: HashMap<NodeId, HashSet<ContainerId>> = HashMap::new();
    for edge in edges {
        if let Some(container) = node_containers.get(&edge.source) {
            let target = if edge.kind == EdgeKind::ReExport {
                edge.target
            } else {
                resolve_re_export(edge.target, &re_export_target, &name_of).unwrap_or(edge.target)
            };
            consumers.entry(target).or_default().insert(*container);
        }
    }
    let path_of: HashMap<ContainerId, &SmolStr> =
        files.iter().map(|(path, id)| (*id, path)).collect();

    let mut scopes_by_node: HashMap<NodeId, Vec<&VisibilityScope>> = HashMap::new();
    for visibility_scope in visibility_scopes {
        scopes_by_node
            .entry(visibility_scope.node)
            .or_default()
            .push(visibility_scope);
    }

    let mut ladders = Vec::new();
    for node in nodes {
        let Some([visibility_scope]) = scopes_by_node.get(&node.id).map(Vec::as_slice) else {
            continue;
        };
        let Some(owner) = tree.containers().get(node.container.0 as usize) else {
            continue;
        };
        if owner.id != node.container || owner.level != ScopeLevel::File {
            continue;
        }

        let Some(level) = scope_level(
            tree,
            files,
            node.container,
            &visibility_scope.files,
            visibility_scope.definition_file.as_ref(),
        ) else {
            continue;
        };
        node.visibility = level;

        // a consumer no rung lists (a `cfg(test)` module the analyzer never
        // defined, a file outside the module tree) cannot be judged by the
        // ladder, so it is left out of the rung check rather than wiping the
        // ladder. `derive_visibility` still counts it in the derived need, so
        // the floored level is never below that need. An item whose only
        // unplaced consumer sits in a folder whose rung shares the need's level
        // label may still be flagged, as it would be with no ladder.
        let used_from: Option<HashSet<ContainerId>> = consumers.get(&node.id).map(|containers| {
            containers
                .iter()
                .copied()
                .filter(|consumer| {
                    path_of.get(consumer).is_some_and(|path| {
                        visibility_scope
                            .expressible
                            .iter()
                            .any(|rung| rung.files.binary_search(path).is_ok())
                    })
                })
                .collect()
        });
        // all-or-nothing: one rung that cannot project drops the whole ladder
        // (a gap could only hide a narrower spelling); rungs that project but
        // miss a placed consumer are skipped individually.
        let mut levels = visibility_scope
            .expressible
            .iter()
            .map(|rung| {
                rung_reaches(
                    tree,
                    files,
                    &path_of,
                    node.container,
                    used_from.as_ref(),
                    rung,
                )
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(covered, reaches)| reaches.then_some(covered))
            .collect::<Vec<_>>();
        levels.sort();
        levels.dedup();
        if !levels.is_empty() {
            ladders.push(ScopeLadder {
                node: node.id,
                levels,
            });
        }
    }
    ladders
}

/// Projects one rung to its level, or `None` when it cannot project.
///
/// The flag is `false` when the rung leaves a consumer file out: such a rung can
/// share its level with the need yet cover another folder, so it is not a
/// spelling for this declaration.
fn rung_reaches(
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    path_of: &HashMap<ContainerId, &SmolStr>,
    home: ContainerId,
    used_from: Option<&HashSet<ContainerId>>,
    rung: &strata_ir::ScopeRung,
) -> Option<(ScopeLevel, bool)> {
    let level = scope_level(
        tree,
        files,
        home,
        &rung.files,
        rung.definition_file.as_ref(),
    )?;
    // `scope_level` verified `rung.files` is strictly sorted.
    let reaches = used_from.is_none_or(|containers| {
        containers.iter().all(|consumer| {
            *consumer == home
                || path_of
                    .get(consumer)
                    .is_some_and(|path| rung.files.binary_search(path).is_ok())
        })
    });
    Some((level, reaches))
}

/// Resolves one stated scope to its level on the final tree, or `None` when the
/// scope is unsorted, empty, or names a file the tree does not hold.
fn scope_level(
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    home: ContainerId,
    scope_files: &[SmolStr],
    definition_file: Option<&SmolStr>,
) -> Option<ScopeLevel> {
    if scope_files.is_empty()
        || !scope_files.windows(2).all(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_some_and(|(left, right)| left < right)
        })
    {
        return None;
    }

    // a `dir/foo.rs` definition file sits beside the `dir/foo/` folder it
    // owns; the module's own level is that folder, so the definition file
    // is left out of the common-ancestor walk. Dropping it is deliberate
    // per R2 (pub(super)/pub(in) resolve to the module's own folder), and
    // the adapter states it: it is never guessed from file names, so a
    // miss widens instead of narrowing.
    // a definition file outside the stated scope is not trusted: keeping
    // it in the walk can only widen.
    let definition = definition_file.filter(|path| scope_files.contains(path));
    let definition_container = definition.and_then(|path| files.get(path).copied());

    let mut containers = Vec::with_capacity(scope_files.len() + 1);
    if definition_container != Some(home) {
        containers.push(home);
    }
    for path in scope_files {
        let container = files.get(path).copied()?;
        if Some(path) != definition {
            containers.push(container);
        }
    }
    let level = common_ancestor_level(tree, &containers)?;
    // a lone remaining file still lives in the module's own folder.
    Some(if definition.is_some() && level == ScopeLevel::File {
        ScopeLevel::Folder
    } else {
        level
    })
}

/// Returns the deepest actual ancestor shared by every supplied container.
fn common_ancestor_level(tree: &ContainerTree, containers: &[ContainerId]) -> Option<ScopeLevel> {
    let mut containers = containers.iter().copied();
    let mut common = ancestor_chain(tree, containers.next()?)?;
    for container in containers {
        let ancestors = ancestor_chain(tree, container)?;
        common.retain(|candidate| ancestors.contains(candidate));
    }
    let common_id = common.first()?;
    let common_container = tree.containers().get(common_id.0 as usize)?;
    (common_container.id == *common_id).then_some(common_container.level)
}

/// Returns one container's validated leaf-to-root ancestry.
fn ancestor_chain(tree: &ContainerTree, start: ContainerId) -> Option<Vec<ContainerId>> {
    let mut chain = Vec::new();
    let mut current = Some(start);
    while let Some(id) = current {
        if chain.len() >= tree.containers().len() || chain.contains(&id) {
            return None;
        }
        let container = tree.containers().get(id.0 as usize)?;
        if container.id != id {
            return None;
        }
        chain.push(id);
        current = container.parent;
    }
    Some(chain)
}
