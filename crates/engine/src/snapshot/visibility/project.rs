//! Projects a stated scope's files onto the final laminar tree.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

/// Resolves one stated scope to its level on the final tree, or `None` when the
/// scope is unsorted, empty, or names a file the tree does not hold.
pub(super) fn scope_level(
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
