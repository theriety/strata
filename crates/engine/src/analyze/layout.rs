//! Layout: the real-directory start, capacity relief, and the naming ladder
//! that turns a folder partition into a five-level candidate tree.

#[cfg(test)]
mod tests;

mod arena;
mod election;
mod home;
mod levels;
mod piles;
mod real_dirs;
mod relief;
mod roof;
mod roof_naming;

pub(in crate::analyze) use arena::{
    ContainerArena, anchor_min, qualify_elected, record_undecorated_key,
};
pub(in crate::analyze) use election::{NameTally, elect, plurality, vote};
pub(in crate::analyze) use home::{
    LaminarHome, laminar_home, qualify_folder_names, render_namespace,
};
pub(in crate::analyze) use levels::{cluster_level, home_affinity};
use piles::{basename_stem, connected_piles, elect_split_token, scc_file_count, union_root};
pub(in crate::analyze) use real_dirs::{dominant_member, real_dir_partition};
pub(in crate::analyze) use relief::relieve_over_capacity;
#[cfg(test)]
use relief::{relief_child_entries, relief_sccs_by_namespace};
pub(in crate::analyze) use roof::synthesize_roof_rebuild;
use roof_naming::roof_coherence;
pub(in crate::analyze) use roof_naming::{rebuild_label, token_groups};
