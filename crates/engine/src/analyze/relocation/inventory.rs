//! Inventories the current tree's file containers and their physical homes.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::condense::Condensation;
use strata_ir::{Container, IntermediateRepresentation, Polarity, ScopeLevel};

use crate::analyze::layout::{LaminarHome, laminar_home, qualify_folder_names, render_namespace};
use crate::analyze::relocation::FileInfo;

/// Keeps physical homes separate from the dominant representative used to
/// search a cross-directory SCC as one atomic unit.
pub(super) fn cycle_homes(
    files: &[FileInfo],
    condensation: &Condensation,
) -> BTreeMap<u32, SmolStr> {
    let homes: BTreeSet<LaminarHome> = files.iter().map(|file| file.home.clone()).collect();
    let names = qualify_folder_names(&homes);
    let name_by_home: BTreeMap<_, _> = homes.into_iter().zip(names).collect();
    let mut retained = BTreeMap::new();
    for members in &condensation.members {
        let member_homes: BTreeSet<_> = members
            .iter()
            .filter_map(|member| files.get(member.0 as usize).map(|file| &file.home))
            .collect();
        if member_homes.len() > 1 {
            for member in members {
                if let Some(file) = files.get(member.0 as usize)
                    && let Some(name) = name_by_home.get(&file.home)
                {
                    retained.insert(member.0, name.clone());
                }
            }
        }
    }
    retained
}

/// Inventories the current tree's file containers in file-graph vertex order:
/// every file with its laminar home keys, ascending container id, paired with
/// the container-to-vertex index. Production SLOC is folded into each entry so
/// relief and narration can weigh files without re-walking the IR nodes.
pub(in crate::analyze) fn file_inventory(
    ir: &IntermediateRepresentation,
) -> (Vec<FileInfo>, BTreeMap<u32, u32>) {
    // index the laminar containers by id so each file can read its already-
    // resolved folder/domain/package name keys off its ancestor chain.
    let by_id: BTreeMap<u32, &Container> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut files: Vec<FileInfo> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| {
            let home = laminar_home(&by_id, container.id.0);
            FileInfo {
                container: container.id.0,
                name: container.name.clone(),
                production_sloc: 0,
                namespace: render_namespace(&container.name, &home),
                home,
            }
        })
        .collect();
    files.sort_by_key(|file| file.container);
    let index_of: BTreeMap<u32, u32> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.container, u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        let Some(&index) = index_of.get(&node.container.0) else {
            continue;
        };
        if let Some(file) = files.get_mut(index as usize) {
            file.production_sloc = file.production_sloc.saturating_add(node.effective_size);
        }
    }
    (files, index_of)
}
