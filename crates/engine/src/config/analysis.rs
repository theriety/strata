//! Run-selection settings: the CLI mode selector, profile names, and process-wide analysis options.

use serde::{Deserialize, Serialize};

/// CLI compatibility selector for the parameter profiles a run executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Execute only the anchored parameter profile.
    Anchored,
    /// Execute only the greenfield parameter profile.
    Greenfield,
    /// Execute both parameter profiles against one discovered snapshot.
    Both,
}

impl Mode {
    /// Returns whether this mode produces an anchored result.
    #[must_use]
    pub fn includes_anchored(self) -> bool {
        matches!(self, Self::Anchored | Self::Both)
    }

    /// Returns whether this mode produces a greenfield result.
    #[must_use]
    pub fn includes_greenfield(self) -> bool {
        matches!(self, Self::Greenfield | Self::Both)
    }
}

/// A named parameter profile available to an analysis run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileName {
    /// The profile whose defaults favor preserving today's layout.
    Anchored,
    /// The profile whose defaults ignore today's path and placement.
    Greenfield,
}

/// Process-wide analysis settings shared by every selected profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnalysisConfig {
    /// Parameter profiles to execute against the discovered snapshot.
    pub profiles: Vec<ProfileName>,
    /// Parallelism hint; `0` means all logical cores and never affects results.
    pub jobs: u32,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            profiles: vec![ProfileName::Anchored, ProfileName::Greenfield],
            jobs: 0,
        }
    }
}
