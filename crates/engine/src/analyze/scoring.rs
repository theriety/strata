//! Scoring of the current layout and of candidate trees against the core objective.
//!
//! Each child owns one job: the config views, the current-layout score, the
//! candidate view handed to the scorer, and the placement-derived inputs it reads.

use smol_str::SmolStr;
use strata_ir::{ContainerId, ScopeLevel};

mod candidate;
mod capacity;
mod current;
mod cycles;
mod distance;
mod inputs;
mod relocation_counts;
mod sources;

#[cfg(test)]
mod tests;

pub(in crate::analyze) use candidate::score_candidate;
#[cfg(test)]
pub(in crate::analyze) use capacity::{binding_pressure, physical_binding_pressure};
#[cfg(test)]
pub(in crate::analyze) use current::score_current;
pub(in crate::analyze) use current::{score_current_with_affinity, score_current_with_overlay};
pub(in crate::analyze) use cycles::CycleCounts;
pub(in crate::analyze) use distance::move_distance;
#[cfg(test)]
use inputs::cohesion_inputs;
#[cfg(test)]
pub(in crate::analyze) use relocation_counts::{
    companion_separations, dependency_only_relocations,
};
pub(in crate::analyze) use sources::{CapacitySource, ProfileSource, level_caps};

/// The description of one container to intern: the name key it carries, the
/// level it sits at, and its parent.
#[derive(Clone, Copy)]
pub(in crate::analyze) struct ContainerSpec<'name> {
    /// The name key the container's cluster carries.
    pub(in crate::analyze) name: &'name SmolStr,
    /// The level the container sits at.
    pub(in crate::analyze) level: ScopeLevel,
    /// The container's parent, or `None` at the root.
    pub(in crate::analyze) parent: Option<ContainerId>,
    /// True for the synthetic `workspace` folder bucket the render collapses.
    /// Only a root-file folder is ever synthetic; every upper level is false.
    pub(in crate::analyze) synthetic: bool,
}
