//! Search-size ceilings and the numeric range predicates shared by config validation.

use crate::error::StrataError;

/// The inclusive ceiling for the folder, domain, and package member-count caps.
///
/// The shipped defaults (20/16/15) sit an order of magnitude below this bound, so
/// every legitimate configuration fits comfortably; a cap beyond it can never fire
/// on a sanely sized container, which would turn capacity enforcement into a no-op
/// and hand an untrusted `strata.toml` a lever over cap-driven working sets.
const CAP_CEILING: u32 = 256;

/// The inclusive ceiling for `analysis.jobs`, the Rayon pool size.
///
/// `jobs` flows into `ThreadPoolBuilder::num_threads`, which tries to spawn that
/// many OS threads before it can fail. At roughly 8 MiB of stack apiece an
/// unbounded value exhausts memory long before the graceful fallback is reached,
/// so the ceiling keeps a hostile `strata.toml` from turning one `analyze` run
/// into a fork bomb. It sits far above any real machine's core count, so no
/// legitimate parallelism hint is refused.
pub(super) const JOBS_CEILING: u32 = 1_024;

/// The inclusive ceiling for `analysis.candidates` (`k`).
pub(super) const CANDIDATES_CEILING: u32 = 64;

/// The inclusive ceiling for `diversity.seeds-per-candidate`.
pub(super) const SEEDS_PER_CANDIDATE_CEILING: u32 = 64;

/// The inclusive ceiling on the whole restart pool, `candidates *
/// seeds-per-candidate`.
///
/// Each pooled seed runs the full assemble, score, and narrate pipeline with no
/// fallback path, so the product — not either factor alone — is what bounds the
/// work. Two individually legal values can still multiply into an effectively
/// unbounded search, which is why this is checked separately.
pub(super) const RESTART_POOL_CEILING: u32 = 512;

/// The inclusive ceiling for `solver.ilp-threshold`.
///
/// The threshold sizes the exact MFAS solver's branch-and-bound cutover, whose
/// worst case is exponential in the size of the cyclic component handed to it.
pub(super) const ILP_THRESHOLD_CEILING: u32 = 4_096;

/// Returns `Ok` when `value` is at most `ceiling`, else a `ConfigInvalid` naming
/// `key`, the ceiling, and the offending value.
pub(super) fn at_most(key: &str, value: u32, ceiling: u32) -> Result<(), StrataError> {
    if value <= ceiling {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: format!("must be at most {ceiling}, got {value}"),
        })
    }
}

/// Returns `Ok` when `value` is at most [`CAP_CEILING`], else a `ConfigInvalid`
/// naming `key`, the ceiling, and the offending value.
pub(super) fn within_ceiling(key: &str, value: u32) -> Result<(), StrataError> {
    at_most(key, value, CAP_CEILING)
}

/// Returns `Ok` when `value` is at least one, else a `ConfigInvalid` naming `key`.
pub(super) fn positive(key: &str, value: u32) -> Result<(), StrataError> {
    if value >= 1 {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "must be at least 1".to_owned(),
        })
    }
}

/// Returns `Ok` when `value` is finite and non-negative, else a `ConfigInvalid`
/// naming `key`.
pub(super) fn non_negative_finite(key: &str, value: f64) -> Result<(), StrataError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "must be a finite, non-negative number".to_owned(),
        })
    }
}

pub(super) fn unit_interval(key: &str, value: f64) -> Result<(), StrataError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "must be a finite number between 0 and 1".to_owned(),
        })
    }
}

/// Returns `Ok` when `value` is finite and at least one.
pub(super) fn at_least_one_finite(key: &str, value: f64) -> Result<(), StrataError> {
    if value.is_finite() && value >= 1.0 {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "must be a finite number at least 1".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AnalysisConfig, AnalyzeConfig, DiversityConfig, ProfileConfig, ProfilesConfig, SolverConfig,
    };

    #[test]
    fn should_reject_a_job_count_above_the_ceiling() {
        let config = AnalyzeConfig {
            analysis: AnalysisConfig {
                jobs: JOBS_CEILING + 1,
                ..AnalysisConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "analysis.jobs"
        ));
    }

    #[test]
    fn should_reject_a_candidate_count_above_the_ceiling() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    candidates: CANDIDATES_CEILING + 1,
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.candidates"
        ));
    }

    #[test]
    fn should_reject_a_seed_multiplier_above_the_ceiling() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    diversity: DiversityConfig {
                        seeds_per_candidate: SEEDS_PER_CANDIDATE_CEILING + 1,
                        ..DiversityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. })
                if key == "profiles.anchored.diversity.seeds-per-candidate"
        ));
    }

    #[test]
    fn should_reject_an_ilp_threshold_above_the_ceiling() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    solver: SolverConfig {
                        ilp_threshold: ILP_THRESHOLD_CEILING + 1,
                        ..SolverConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. })
                if key == "profiles.anchored.solver.ilp-threshold"
        ));
    }
}
