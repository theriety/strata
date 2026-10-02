//! The real-directory folder partition every non-identity search starts from.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::Condensation;
use strata_ir::NodeId;

use crate::analyze::layout::{LaminarHome, qualify_folder_names};
use crate::analyze::relocation::FileInfo;

/// The member an SCC is placed by: the file with the largest production SLOC,
/// ties to the lexicographically smaller path.
pub(in crate::analyze) fn dominant_member<'a>(
    files: &'a [FileInfo],
    members: &[NodeId],
) -> Option<&'a FileInfo> {
    let mut dominant: Option<&FileInfo> = None;
    for member in members {
        let Some(file) = files.get(member.0 as usize) else {
            continue;
        };
        let better = dominant.is_none_or(|top| {
            file.production_sloc > top.production_sloc
                || (file.production_sloc == top.production_sloc && file.name < top.name)
        });
        if better {
            dominant = Some(file);
        }
    }
    dominant
}

/// Builds the real-directory folder partition: each file SCC lands in the
/// cluster of its dominant member's laminar home — the file with the largest
/// production SLOC, ties to the lexicographically smaller path (an SCC
/// spanning folders must co-cluster anyway, so it stays with its heaviest
/// member). Folders come from reality, not from clustering, so this partition
/// doubles as the identity layout. Clusters key on the full laminar location
/// (folder, domain, package), never the folder name alone: real directories
/// are already unique by their full-depth keys, and the full location keeps
/// same-named fallback buckets (`workspace`) of different packages apart.
/// Cluster ids are dense over the distinct locations in ascending order, and
/// the returned names carry each cluster's real directory key so emission
/// never re-elects folder names.
pub(in crate::analyze) fn real_dir_partition(
    files: &[FileInfo],
    condensation: &Condensation,
) -> (Partition, Vec<SmolStr>, Vec<bool>) {
    let fallback = || LaminarHome {
        folder: SmolStr::new("workspace"),
        domain: SmolStr::new("workspace"),
        package: SmolStr::new("workspace"),
        synthetic: false,
    };
    let keys: Vec<LaminarHome> = condensation
        .members
        .iter()
        .map(|members| {
            dominant_member(files, members).map_or_else(fallback, |file| file.home.clone())
        })
        .collect();
    let distinct: BTreeSet<LaminarHome> = keys.iter().cloned().collect();
    let cluster_of_key: BTreeMap<LaminarHome, u32> = distinct
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let assignment = keys
        .iter()
        .map(|key| ClusterId(cluster_of_key.get(key).copied().unwrap_or(0)))
        .collect();
    let names = qualify_folder_names(&distinct);
    // the synthetic marker rides in lockstep with `names`: both map the distinct
    // homes in the same iteration order, so cluster `i` names and marks the same
    // real location. It is folder-determined, so it never perturbs the clustering.
    let synthetic: Vec<bool> = distinct.iter().map(|home| home.synthetic).collect();
    (
        Partition::from_assignment(assignment, names.len()),
        names,
        synthetic,
    )
}
