//! Move distance of a candidate tree and the folder-key primitive it shares with path cohesion.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{ContainerId, ContainerTree, ScopeLevel, Snapshot};

/// Computes the move distance of a candidate tree: the fraction of symbols whose
/// owning file changes real folder — or, since FIX08, whose effective placement
/// lands it in a DIFFERENT file than the one that houses it today.
///
/// A file's location is exactly its folder key — the path it would be moved to —
/// so the comparison reads folder keys on both sides and never composes a
/// root-to-leaf path. Labels above the folder are display, not location:
/// renaming a domain relocates nothing and must not register here. The second,
/// placement-aware rule prices symbol-grain relocation: `placement` maps each
/// node to the candidate file container it occupies, and a node whose placed
/// file carries another path has left its home even when its folder key is
/// unchanged. File-only layouts pass the assembly's own placement, which maps
/// every node to its current file's candidate id, so the extension changes
/// nothing for them — μ and β stop being blind to symbol moves without any
/// coefficient moving.
pub(in crate::analyze) fn move_distance(
    snapshot: &Snapshot,
    candidate: &ContainerTree,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
) -> f64 {
    let ir = snapshot.ir();
    let total = ir.nodes.len();
    if total == 0 {
        return 0.0;
    }
    let current_folder = folder_key_of_files(&ir.containers);
    let candidate_folder = folder_key_of_files(candidate);

    // match candidate files by the original file's path, which the assembly
    // preserves; the id → name map avoids a per-node linear scan.
    let current_name: BTreeMap<u32, &SmolStr> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    let candidate_file_name: BTreeMap<u32, &str> = candidate
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.as_str()))
        .collect();
    let moved = ir
        .nodes
        .iter()
        .filter(|node| {
            let Some(name) = current_name.get(&node.container.0) else {
                return false;
            };
            let Some(current) = current_folder.get(name.as_str()) else {
                return false;
            };
            // FIX08: a symbol whose effective placement sits in a file of
            // another path has relocated between files, whatever its folder key
            // did. Unplaced nodes fall through to the file-key rule alone.
            let left_file = placement(node.id.0).is_some_and(|file| {
                candidate_file_name
                    .get(&file.0)
                    .is_some_and(|placed| *placed != name.as_str())
            });
            left_file
                || candidate_folder
                    .get(name.as_str())
                    .is_none_or(|placed| placed != current)
        })
        .count();

    f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Maps each file container's path key to the folder key holding it directly.
///
/// This is the one primitive both location-sensitive terms read: a file's real
/// place is the folder key it sits under, so β (does the file still sit where it
/// already lives?) and μ (did the file leave?) ask the same question of the same
/// key space. Comparing keys rather than a raw-path prefix is what keeps
/// source-root transparency intact — one folder legitimately merges `src/x` with
/// `spec/x`, so it has no single raw directory to compare against.
pub(super) fn folder_key_of_files(tree: &ContainerTree) -> BTreeMap<&str, &str> {
    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    tree.containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|container| {
            let folder = name_of.get(&container.parent?.0)?;
            Some((container.name.as_str(), folder.as_str()))
        })
        .collect()
}
