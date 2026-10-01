//! Range checks for a deserialized [`AnalyzeConfig`], attributing dotted key paths.

mod ceilings;
mod policy;

use crate::error::StrataError;

use super::{AnalyzeConfig, ProfileConfig};

use ceilings::{
    CANDIDATES_CEILING, ILP_THRESHOLD_CEILING, JOBS_CEILING, RESTART_POOL_CEILING,
    SEEDS_PER_CANDIDATE_CEILING, at_least_one_finite, at_most, non_negative_finite, positive,
    within_ceiling,
};

impl AnalyzeConfig {
    /// Range-checks every value, attributing a dotted TOML key path on failure.
    ///
    /// Caps must be positive (a zero cap admits nothing), and the folder, domain,
    /// and package caps must not exceed the built-in `CAP_CEILING` (256) — beyond
    /// it a cap never fires, so an untrusted `strata.toml` could silently disable
    /// capacity enforcement while inflating cap-driven working sets.
    ///
    /// The knobs that size the search carry ceilings for the same reason: the
    /// parallelism hint reaches the thread-pool builder directly, and the seed
    /// multiplier, candidate count, and their product bound the restart pool,
    /// which has no fallback path once it is running. The solver's exact-ILP
    /// cutover is bounded because its worst case is exponential. Together these
    /// keep a `strata.toml` committed by an untrusted contributor from turning a
    /// routine analysis into a denial of service against the machine running it.
    /// Coefficients, weights, tolerances, and distances must be finite and
    /// non-negative. The first violation found is returned.
    ///
    /// # Errors
    ///
    /// Returns [`StrataError::ConfigInvalid`] naming the offending key and its
    /// expected range.
    pub fn validate(&self) -> Result<(), StrataError> {
        // `jobs` takes no floor: zero is the documented "use every logical core"
        // sentinel. It still takes a ceiling, because the value is handed
        // straight to the thread-pool builder.
        at_most("analysis.jobs", self.analysis.jobs, JOBS_CEILING)?;
        if self.analysis.profiles.is_empty() {
            return Err(StrataError::ConfigInvalid {
                key: Some("analysis.profiles".to_owned()),
                reason: "at least one parameter profile must be selected".to_owned(),
            });
        }
        let mut selected = self.analysis.profiles.clone();
        selected.sort_unstable();
        selected.dedup();
        if selected.len() != self.analysis.profiles.len() {
            return Err(StrataError::ConfigInvalid {
                key: Some("analysis.profiles".to_owned()),
                reason: "parameter profile names must be unique".to_owned(),
            });
        }

        Self::validate_profile("profiles.anchored", &self.profiles.anchored)?;
        Self::validate_profile("profiles.greenfield", &self.profiles.greenfield)?;

        if self.adapters.languages.is_empty() {
            return Err(StrataError::ConfigInvalid {
                key: Some("adapters.languages".to_owned()),
                reason: "at least one language must be enabled".to_owned(),
            });
        }
        for language in &self.adapters.languages {
            if !matches!(language.as_str(), "typescript" | "rust" | "python") {
                return Err(StrataError::ConfigInvalid {
                    key: Some("adapters.languages".to_owned()),
                    reason: format!(
                        "unknown language `{language}`; expected typescript, rust, or python"
                    ),
                });
            }
        }

        Ok(())
    }

    fn validate_profile(prefix: &str, profile: &ProfileConfig) -> Result<(), StrataError> {
        let key = |suffix: &str| format!("{prefix}.{suffix}");
        positive(&key("capacity.file"), profile.capacity.file)?;
        positive(&key("capacity.folder"), profile.capacity.folder)?;
        positive(&key("capacity.domain"), profile.capacity.domain)?;
        positive(&key("capacity.package"), profile.capacity.package)?;
        positive(
            &key("capacity.package-group"),
            profile.capacity.package_group,
        )?;
        within_ceiling(&key("capacity.folder"), profile.capacity.folder)?;
        within_ceiling(&key("capacity.domain"), profile.capacity.domain)?;
        within_ceiling(&key("capacity.package"), profile.capacity.package)?;
        positive(&key("tests.helper-cap"), profile.tests.helper_cap)?;
        for (index, pattern) in profile.tests.patterns.iter().enumerate() {
            if pattern.is_empty() {
                return Err(StrataError::ConfigInvalid {
                    key: Some(format!("{prefix}.tests.patterns[{index}]")),
                    reason: "a pattern must not be empty".to_owned(),
                });
            }
            glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                key: Some(format!("{prefix}.tests.patterns[{index}]")),
                reason: error.to_string(),
            })?;
        }
        Self::validate_relocation(prefix, &profile.relocation)?;
        Self::validate_qualification(prefix, &profile.qualification)?;
        positive(&key("solver.ilp-threshold"), profile.solver.ilp_threshold)?;
        at_most(
            &key("solver.ilp-threshold"),
            profile.solver.ilp_threshold,
            ILP_THRESHOLD_CEILING,
        )?;
        positive(&key("candidates"), profile.candidates)?;
        at_most(&key("candidates"), profile.candidates, CANDIDATES_CEILING)?;
        non_negative_finite(&key("objective.imbalance"), profile.objective.imbalance)?;
        non_negative_finite(&key("objective.naming"), profile.objective.naming)?;
        non_negative_finite(&key("objective.path"), profile.objective.path)?;
        non_negative_finite(&key("objective.anchor"), profile.objective.anchor)?;
        non_negative_finite(&key("objective.capacity"), profile.objective.capacity)?;
        non_negative_finite(
            &key("objective.dependency-only"),
            profile.objective.dependency_only,
        )?;
        non_negative_finite(
            &key("objective.companion-separation"),
            profile.objective.companion_separation,
        )?;
        non_negative_finite(&key("weights.value-import"), profile.weights.value_import)?;
        non_negative_finite(&key("weights.inheritance"), profile.weights.inheritance)?;
        non_negative_finite(&key("weights.call"), profile.weights.call)?;
        non_negative_finite(
            &key("weights.type-reference"),
            profile.weights.type_reference,
        )?;
        non_negative_finite(&key("weights.re-export"), profile.weights.re_export)?;
        at_least_one_finite(
            &key("weights.same-file-symbol"),
            profile.weights.same_file_symbol,
        )?;
        at_least_one_finite(
            &key("weights.same-file-type"),
            profile.weights.same_file_type,
        )?;
        non_negative_finite(
            &key("diversity.score-tolerance"),
            profile.diversity.score_tolerance,
        )?;
        non_negative_finite(
            &key("diversity.min-distance"),
            profile.diversity.min_distance,
        )?;
        positive(
            &key("diversity.seeds-per-candidate"),
            profile.diversity.seeds_per_candidate,
        )?;
        at_most(
            &key("diversity.seeds-per-candidate"),
            profile.diversity.seeds_per_candidate,
            SEEDS_PER_CANDIDATE_CEILING,
        )?;
        at_most(
            &format!("{prefix}.candidates * {prefix}.diversity.seeds-per-candidate"),
            profile
                .candidates
                .saturating_mul(profile.diversity.seeds_per_candidate),
            RESTART_POOL_CEILING,
        )
    }
}
