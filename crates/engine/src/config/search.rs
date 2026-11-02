//! Capacity caps, the solver budget, diversification, and test-detection settings.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use strata_core::shatter::SolverLimits;

/// The per-level capacity caps.
///
/// `file` is a production-SLOC cap; the rest are member-count caps (files per
/// folder, folders per domain, and so on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CapacityConfig {
    /// Production SLOC per file.
    pub file: u32,
    /// File nodes per folder.
    pub folder: u32,
    /// Folders per domain.
    pub domain: u32,
    /// Domains per package.
    pub package: u32,
    /// Packages per package group.
    #[serde(rename = "package-group")]
    pub package_group: u32,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            file: 250,
            folder: 20,
            domain: 16,
            package: 15,
            package_group: 12,
        }
    }
}

/// The MFAS solver budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SolverConfig {
    /// Maximum SCC size that attempts the exact ILP.
    #[serde(rename = "ilp-threshold")]
    pub ilp_threshold: u32,
    /// Per-SCC exact-solve budget, in seconds, before the heuristic fallback.
    #[serde(rename = "timeout-seconds")]
    pub timeout_seconds: u64,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            ilp_threshold: 300,
            timeout_seconds: 60,
        }
    }
}

impl SolverConfig {
    /// Converts the config into the solver's [`SolverLimits`].
    #[must_use]
    pub fn limits(&self) -> SolverLimits {
        SolverLimits {
            ilp_threshold: self.ilp_threshold as usize,
            timeout: Duration::from_secs(self.timeout_seconds),
        }
    }
}

/// The multi-start diversification parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiversityConfig {
    /// Restart pool multiplier: the pool size is this times `k`.
    #[serde(rename = "seeds-per-candidate")]
    pub seeds_per_candidate: u32,
    /// Maximum relative score gap for a candidate to remain in the keep-band.
    #[serde(rename = "score-tolerance")]
    pub score_tolerance: f64,
    /// Minimum pairwise variation-of-information between returned candidates.
    #[serde(rename = "min-distance")]
    pub min_distance: f64,
}

impl Default for DiversityConfig {
    fn default() -> Self {
        Self {
            seeds_per_candidate: 10,
            score_tolerance: 0.05,
            min_distance: 0.05,
        }
    }
}

/// Settings governing how test files are detected and capped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TestsConfig {
    /// File cap (in production SLOC) for test-support files; test-case files are
    /// exempt.
    #[serde(rename = "helper-cap")]
    pub helper_cap: u32,
    /// Extra glob patterns marking test files beyond the built-in detection.
    ///
    /// Patterns use [`glob::Pattern`] syntax against repo-relative paths; a
    /// pattern containing `/` matches the whole path while a bare pattern
    /// matches the file name alone, so `*.spec.*` applies repo-wide and
    /// `apps/web/__tests__/**` stays scoped. A file matching any pattern is
    /// treated as a test file for the clustering tie-cut and subject-following
    /// passes regardless of its language's own detection.
    pub patterns: Vec<String>,
    /// Whether the built-in per-language detection participates alongside
    /// `patterns`. Disabling it makes only `patterns` decide; with an empty
    /// pattern list the tie-cut becomes fully inert.
    pub builtins: bool,
}

impl Default for TestsConfig {
    fn default() -> Self {
        Self {
            helper_cap: 250,
            patterns: Vec::new(),
            builtins: true,
        }
    }
}
