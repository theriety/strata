//! The `strata.toml` schema, its built-in defaults, and validation.
//!
//! [`AnalyzeConfig`] mirrors `strata.toml` exactly: every key is optional and
//! every default matches a config-less CLI run (FR / the reference tables). The
//! config is the only knob surface — coefficients, caps, edge weights, the solver
//! budget, and diversification parameters all flow from here into the pure
//! [`analyze`] pass, so a deserialized config plus a snapshot fully determines a
//! result.
//!
//! Parsing is two-staged: [`toml`] rejects unknown keys and type mismatches at
//! deserialization (the structs `deny_unknown_fields`), then [`AnalyzeConfig::validate`]
//! range-checks every value and attributes a dotted key path on failure. Both
//! stages raise [`StrataError::ConfigInvalid`].
//!
//! [`analyze`]: fn@crate::analyze
//! [`StrataError::ConfigInvalid`]: crate::error::StrataError::ConfigInvalid

mod adapters;
mod analysis;
mod load;
mod mirror;
mod objective;
mod profile;
mod qualification;
mod relocation;
mod search;
#[cfg(test)]
mod tests;
mod validate;

pub use adapters::AdaptersConfig;
pub use analysis::{AnalysisConfig, Mode, ProfileName};
pub use load::load_config;
pub(crate) use mirror::{MirrorCaptures, MirrorTemplate};
pub use objective::{ObjectiveConfig, WeightsConfig};
pub use profile::{ProfileConfig, ProfilesConfig};
pub use qualification::{QualificationConfig, QualificationWeightsConfig};
pub(crate) use relocation::builtin_test_mirror_rules;
pub use relocation::{RelocationConfig, TestMirrorRule, TestMirroringConfig};
pub use search::{CapacityConfig, DiversityConfig, SolverConfig, TestsConfig};

use serde::{Deserialize, Serialize};

/// The full `strata.toml` schema with built-in defaults.
///
/// Every field is optional in the file and defaults to a config-less CLI run.
/// [`AnalyzeConfig::default`] is exactly the no-config baseline, so an embedder
/// who never touches a file still gets the shipped behaviour.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnalyzeConfig {
    /// Enabled adapters and source-discovery globs.
    pub adapters: AdaptersConfig,
    /// Process-wide execution settings and selected parameter profiles.
    pub analysis: AnalysisConfig,
    /// Complete independently configurable analysis parameter profiles.
    pub profiles: ProfilesConfig,
}

impl AnalyzeConfig {
    /// Replaces the selected parameter profiles using the CLI compatibility selector.
    pub fn select_mode(&mut self, mode: Mode) {
        self.analysis.profiles = match mode {
            Mode::Anchored => vec![ProfileName::Anchored],
            Mode::Greenfield => vec![ProfileName::Greenfield],
            Mode::Both => vec![ProfileName::Anchored, ProfileName::Greenfield],
        };
    }

    /// Applies a generic candidate-count override to every selected profile.
    pub fn override_candidates(&mut self, candidates: u32) {
        if self.analysis.profiles.contains(&ProfileName::Anchored) {
            self.profiles.anchored.candidates = candidates;
        }
        if self.analysis.profiles.contains(&ProfileName::Greenfield) {
            self.profiles.greenfield.candidates = candidates;
        }
    }

    /// Applies a generic seed override to every selected profile.
    pub fn override_seed(&mut self, seed: u64) {
        if self.analysis.profiles.contains(&ProfileName::Anchored) {
            self.profiles.anchored.seed = seed;
        }
        if self.analysis.profiles.contains(&ProfileName::Greenfield) {
            self.profiles.greenfield.seed = seed;
        }
    }

    /// Lifts the package wall for every profile in this run, whatever each
    /// profile's `allow-cross-package-moves` says (the CLI
    /// `--allow-cross-package-moves` flag).
    pub const fn lift_package_wall(&mut self) {
        self.profiles.anchored.relocation.allow_cross_package_moves = true;
        self.profiles
            .greenfield
            .relocation
            .allow_cross_package_moves = true;
    }

    /// Returns the configuration for a named parameter profile.
    #[must_use]
    pub const fn profile(&self, name: ProfileName) -> &ProfileConfig {
        match name {
            ProfileName::Anchored => &self.profiles.anchored,
            ProfileName::Greenfield => &self.profiles.greenfield,
        }
    }
}
