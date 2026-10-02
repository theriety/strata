//! Config views the scorer reads: the profile and capacity sources and the level caps.

use strata_core::cluster::LevelCaps;

use crate::config::{AnalyzeConfig, CapacityConfig, ProfileConfig};

pub(in crate::analyze) trait ProfileSource {
    fn profile(&self) -> &ProfileConfig;
}

impl ProfileSource for ProfileConfig {
    fn profile(&self) -> &ProfileConfig {
        self
    }
}

impl ProfileSource for AnalyzeConfig {
    fn profile(&self) -> &ProfileConfig {
        &self.profiles.anchored
    }
}

pub(in crate::analyze) trait CapacitySource {
    fn capacity(&self) -> &CapacityConfig;
}

impl CapacitySource for CapacityConfig {
    fn capacity(&self) -> &CapacityConfig {
        self
    }
}

impl CapacitySource for AnalyzeConfig {
    fn capacity(&self) -> &CapacityConfig {
        &self.profiles.anchored.capacity
    }
}

/// Extracts the per-level member caps from the engine config.
pub(in crate::analyze) fn level_caps(source: &impl CapacitySource) -> LevelCaps {
    let capacity = source.capacity();
    LevelCaps {
        folder: capacity.folder,
        domain: capacity.domain,
        package: capacity.package,
        package_group: capacity.package_group,
    }
}
