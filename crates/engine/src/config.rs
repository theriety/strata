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

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use strata_core::score::{Coefficients, KindWeights};
use strata_core::shatter::SolverLimits;

use crate::error::StrataError;

/// The restructuring mode a run targets.
///
/// A mode is purely an objective-coefficient preset (there is no mode-specific
/// algorithm); [`Mode::Both`] requests both presets in one pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Stay close to the current layout: every objective term active.
    Anchored,
    /// Propose an unbiased ideal: the move-distance and path terms vanish.
    Greenfield,
    /// Produce both the anchored and greenfield results in one run.
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

/// The enabled language adapters and the globs that bound source discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdaptersConfig {
    /// Enabled adapters, by language name.
    pub languages: Vec<String>,
    /// Source-inclusion globs, relative to the analysis root.
    pub include: Vec<String>,
    /// Source-exclusion globs, applied after inclusion.
    pub exclude: Vec<String>,
    /// Directory names treated as transparent when deriving container levels, so
    /// a source file and its test share a domain/folder. One leading source-root
    /// segment below each package root is stripped (`src/adapters/x` and
    /// `spec/adapters/x` both resolve to the `adapters` domain).
    pub source_roots: Vec<String>,
}

impl Default for AdaptersConfig {
    fn default() -> Self {
        Self {
            languages: vec![
                "typescript".to_owned(),
                "rust".to_owned(),
                "python".to_owned(),
            ],
            include: vec!["**/*".to_owned()],
            exclude: vec![
                "**/.git/**".to_owned(),
                "**/node_modules/**".to_owned(),
                "**/target/**".to_owned(),
                "**/.venv/**".to_owned(),
            ],
            source_roots: vec![
                "src".to_owned(),
                "spec".to_owned(),
                "test".to_owned(),
                "tests".to_owned(),
                "lib".to_owned(),
                "dist".to_owned(),
                "__tests__".to_owned(),
            ],
        }
    }
}

/// Run-level analysis settings: which modes, how many candidates, the seed, and
/// the parallelism hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AnalysisConfig {
    /// Which restructuring mode(s) to produce.
    pub mode: Mode,
    /// Number of diverse candidates to return per mode (`k`).
    pub candidates: u32,
    /// The deterministic base seed.
    pub seed: u64,
    /// Parallelism hint; `0` means all logical cores and never affects results.
    pub jobs: u32,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Both,
            candidates: 3,
            seed: 42,
            jobs: 0,
        }
    }
}

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

/// The objective coefficients (the `J(T)` weights).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObjectiveConfig {
    /// `lambda`: the sibling size-imbalance penalty weight.
    pub imbalance: f64,
    /// `alpha`: the naming-token cohesion bonus weight.
    pub naming: f64,
    /// `beta`: the current-path cohesion bonus weight (anchored only).
    pub path: f64,
    /// `mu`: the move-distance anchoring penalty weight (anchored only).
    pub anchor: f64,
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        Self {
            imbalance: 0.1,
            naming: 0.3,
            path: 0.2,
            anchor: 1.0,
        }
    }
}

impl ObjectiveConfig {
    /// Converts the config into the anchored-mode [`Coefficients`].
    #[must_use]
    pub const fn anchored(&self) -> Coefficients {
        Coefficients {
            lambda: self.imbalance,
            alpha: self.naming,
            beta: self.path,
            mu: self.anchor,
        }
    }

    /// Converts the config into the greenfield-mode [`Coefficients`].
    ///
    /// Greenfield is layout-blind (AD-2): the current-path bonus and anchoring
    /// penalty are forced to zero regardless of what the config says.
    #[must_use]
    pub const fn greenfield(&self) -> Coefficients {
        Coefficients {
            lambda: self.imbalance,
            alpha: self.naming,
            beta: 0.0,
            mu: 0.0,
        }
    }
}

/// The per-kind edge weights used when cutting dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeightsConfig {
    /// Weight of a runtime value import.
    #[serde(rename = "value-import")]
    pub value_import: f64,
    /// Weight of a subtype / implements relationship.
    pub inheritance: f64,
    /// Weight of a direct call.
    pub call: f64,
    /// Weight of a type reference.
    #[serde(rename = "type-reference")]
    pub type_reference: f64,
    /// Weight of a re-export (zero — flattened during normalization).
    #[serde(rename = "re-export")]
    pub re_export: f64,
}

impl Default for WeightsConfig {
    fn default() -> Self {
        Self {
            value_import: 1.0,
            inheritance: 1.5,
            call: 1.0,
            type_reference: 0.3,
            re_export: 0.0,
        }
    }
}

impl WeightsConfig {
    /// Converts the config into the scorer's [`KindWeights`] table.
    #[must_use]
    pub const fn kind_weights(&self) -> KindWeights {
        KindWeights {
            value_import: self.value_import,
            inheritance: self.inheritance,
            call: self.call,
            type_reference: self.type_reference,
            re_export: self.re_export,
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

/// Settings governing how test files are capped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TestsConfig {
    /// File cap (in production SLOC) for test-support files; test-case files are
    /// exempt.
    #[serde(rename = "helper-cap")]
    pub helper_cap: u32,
}

impl Default for TestsConfig {
    fn default() -> Self {
        Self { helper_cap: 250 }
    }
}

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
    /// Run-level analysis settings (mode, candidate count, seed).
    pub analysis: AnalysisConfig,
    /// The per-level capacity caps.
    pub capacity: CapacityConfig,
    /// The objective coefficients.
    pub objective: ObjectiveConfig,
    /// The per-kind edge weights.
    pub weights: WeightsConfig,
    /// The MFAS solver budget.
    pub solver: SolverConfig,
    /// The diversification parameters.
    pub diversity: DiversityConfig,
    /// Test-file capping settings.
    pub tests: TestsConfig,
}

impl AnalyzeConfig {
    /// Range-checks every value, attributing a dotted TOML key path on failure.
    ///
    /// Caps must be positive (a zero cap admits nothing), and the folder, domain,
    /// and package caps must not exceed the built-in `CAP_CEILING` (256) — beyond
    /// it a cap never fires, so an untrusted `strata.toml` could silently disable
    /// capacity enforcement while inflating cap-driven working sets. The seed
    /// multiplier and
    /// candidate count are bounded so the restart pool stays finite; coefficients,
    /// weights, tolerances, and distances must be finite and non-negative. The
    /// first violation found is returned.
    ///
    /// # Errors
    ///
    /// Returns [`StrataError::ConfigInvalid`] naming the offending key and its
    /// expected range.
    pub fn validate(&self) -> Result<(), StrataError> {
        positive("capacity.file", self.capacity.file)?;
        positive("capacity.folder", self.capacity.folder)?;
        positive("capacity.domain", self.capacity.domain)?;
        positive("capacity.package", self.capacity.package)?;
        positive("capacity.package-group", self.capacity.package_group)?;
        within_ceiling("capacity.folder", self.capacity.folder)?;
        within_ceiling("capacity.domain", self.capacity.domain)?;
        within_ceiling("capacity.package", self.capacity.package)?;
        positive("tests.helper-cap", self.tests.helper_cap)?;
        positive("solver.ilp-threshold", self.solver.ilp_threshold)?;

        non_negative_finite("objective.imbalance", self.objective.imbalance)?;
        non_negative_finite("objective.naming", self.objective.naming)?;
        non_negative_finite("objective.path", self.objective.path)?;
        non_negative_finite("objective.anchor", self.objective.anchor)?;

        non_negative_finite("weights.value-import", self.weights.value_import)?;
        non_negative_finite("weights.inheritance", self.weights.inheritance)?;
        non_negative_finite("weights.call", self.weights.call)?;
        non_negative_finite("weights.type-reference", self.weights.type_reference)?;
        non_negative_finite("weights.re-export", self.weights.re_export)?;

        non_negative_finite("diversity.score-tolerance", self.diversity.score_tolerance)?;
        non_negative_finite("diversity.min-distance", self.diversity.min_distance)?;
        positive(
            "diversity.seeds-per-candidate",
            self.diversity.seeds_per_candidate,
        )?;

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
}

/// Parses and validates a `strata.toml` from `path`, applying defaults to every
/// omitted key.
///
/// Unknown keys, type mismatches, and out-of-range values all surface as
/// [`StrataError::ConfigInvalid`]; a missing or unreadable file surfaces as
/// [`StrataError::InputUnreadable`]. Callers that want the no-config baseline
/// should use [`AnalyzeConfig::default`] rather than this function.
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when the file cannot be read and
/// [`StrataError::ConfigInvalid`] when it cannot be parsed or fails validation.
pub fn load_config(path: impl AsRef<Path>) -> Result<AnalyzeConfig, StrataError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|error| StrataError::InputUnreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let config: AnalyzeConfig =
        toml::from_str(&text).map_err(|error| StrataError::ConfigInvalid {
            key: None,
            reason: error.message().to_owned(),
        })?;
    config.validate()?;
    Ok(config)
}

/// The inclusive ceiling for the folder, domain, and package member-count caps.
///
/// The shipped defaults (20/16/15) sit an order of magnitude below this bound, so
/// every legitimate configuration fits comfortably; a cap beyond it can never fire
/// on a sanely sized container, which would turn capacity enforcement into a no-op
/// and hand an untrusted `strata.toml` a lever over cap-driven working sets.
const CAP_CEILING: u32 = 256;

/// Returns `Ok` when `value` is at most [`CAP_CEILING`], else a `ConfigInvalid`
/// naming `key`, the ceiling, and the offending value.
fn within_ceiling(key: &str, value: u32) -> Result<(), StrataError> {
    if value <= CAP_CEILING {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: format!("must be at most {CAP_CEILING}, got {value}"),
        })
    }
}

/// Returns `Ok` when `value` is at least one, else a `ConfigInvalid` naming `key`.
fn positive(key: &str, value: u32) -> Result<(), StrataError> {
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
fn non_negative_finite(key: &str, value: f64) -> Result<(), StrataError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "must be a finite, non-negative number".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_default_to_the_config_less_baseline() {
        let config = AnalyzeConfig::default();

        assert_eq!(config.analysis.mode, Mode::Both);
        assert_eq!(config.analysis.candidates, 3);
        assert_eq!(config.analysis.seed, 42);
        assert_eq!(config.capacity.file, 250);
        assert_eq!(config.capacity.folder, 20);
        assert_eq!(config.capacity.domain, 16);
        assert_eq!(config.capacity.package, 15);
        assert_eq!(config.solver.ilp_threshold, 300);
        assert_eq!(config.diversity.seeds_per_candidate, 10);
    }

    #[test]
    fn should_parse_a_partial_toml_filling_omitted_keys_with_defaults() {
        let toml = "[analysis]\nmode = \"anchored\"\ncandidates = 5\n";

        let config: AnalyzeConfig =
            toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

        assert_eq!(config.analysis.mode, Mode::Anchored);
        assert_eq!(config.analysis.candidates, 5);
        // an omitted key keeps its default.
        assert_eq!(config.capacity.file, 250);
    }

    #[test]
    fn should_round_trip_renamed_kebab_keys() {
        let toml = "[capacity]\npackage-group = 7\n[weights]\nvalue-import = 2.0\n";

        let config: AnalyzeConfig =
            toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

        assert_eq!(config.capacity.package_group, 7);
        assert!((config.weights.value_import - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn should_reject_an_unknown_key() {
        let toml = "[analysis]\nnonsense = true\n";

        let result: Result<AnalyzeConfig, _> = toml::from_str(toml);

        assert!(result.is_err());
    }

    #[test]
    fn should_reject_a_zero_cap_with_its_key_path() {
        let config = AnalyzeConfig {
            capacity: CapacityConfig {
                file: 0,
                ..CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "capacity.file"
        ));
    }

    #[test]
    fn should_reject_a_folder_cap_above_the_ceiling() {
        let config = AnalyzeConfig {
            capacity: CapacityConfig {
                folder: 257,
                ..CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "capacity.folder" && reason.contains("256") && reason.contains("257")
        ));
    }

    #[test]
    fn should_reject_a_domain_cap_above_the_ceiling() {
        let config = AnalyzeConfig {
            capacity: CapacityConfig {
                domain: 300,
                ..CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "capacity.domain" && reason.contains("256") && reason.contains("300")
        ));
    }

    #[test]
    fn should_reject_a_package_cap_above_the_ceiling() {
        // u32::MAX is the classic hostile value: the error must format it, not wrap.
        let config = AnalyzeConfig {
            capacity: CapacityConfig {
                package: u32::MAX,
                ..CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "capacity.package" && reason.contains("256") && reason.contains("4294967295")
        ));
    }

    #[test]
    fn should_accept_caps_at_the_ceiling() {
        // 256 is inclusive: the bound rejects only what lies beyond it.
        let config = AnalyzeConfig {
            capacity: CapacityConfig {
                folder: 256,
                domain: 256,
                package: 256,
                ..CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn should_reject_a_negative_coefficient() {
        let config = AnalyzeConfig {
            objective: ObjectiveConfig {
                naming: -1.0,
                ..ObjectiveConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "objective.naming"
        ));
    }

    #[test]
    fn should_reject_an_unknown_language() {
        let config = AnalyzeConfig {
            adapters: AdaptersConfig {
                languages: vec!["cobol".to_owned()],
                ..AdaptersConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "adapters.languages"
        ));
    }

    #[test]
    fn should_validate_the_default_config() {
        assert!(AnalyzeConfig::default().validate().is_ok());
    }

    #[test]
    fn should_report_modes_that_a_mode_includes() {
        assert!(Mode::Anchored.includes_anchored());
        assert!(!Mode::Anchored.includes_greenfield());
        assert!(Mode::Both.includes_anchored());
        assert!(Mode::Both.includes_greenfield());
        assert!(Mode::Greenfield.includes_greenfield());
    }

    #[test]
    fn should_convert_solver_config_into_limits() {
        let limits = SolverConfig::default().limits();

        assert_eq!(limits.ilp_threshold, 300);
        assert_eq!(limits.timeout, Duration::from_mins(1));
    }

    #[test]
    fn should_map_objective_config_onto_anchored_coefficients() {
        let objective = ObjectiveConfig {
            imbalance: 0.4,
            naming: 0.5,
            path: 0.6,
            anchor: 0.7,
        };

        let coefficients = objective.anchored();

        assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
        assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
        assert!((coefficients.beta - 0.6).abs() < f64::EPSILON);
        assert!((coefficients.mu - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn should_zero_path_and_anchor_in_greenfield_coefficients() {
        // greenfield is layout-blind: beta and mu are forced to zero even when
        // the config sets them.
        let objective = ObjectiveConfig {
            imbalance: 0.4,
            naming: 0.5,
            path: 0.6,
            anchor: 0.7,
        };

        let coefficients = objective.greenfield();

        assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
        assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
        assert!(coefficients.beta.abs() < f64::EPSILON);
        assert!(coefficients.mu.abs() < f64::EPSILON);
    }

    #[test]
    fn should_map_weights_config_onto_kind_weights() {
        let weights = WeightsConfig {
            value_import: 2.0,
            inheritance: 3.0,
            call: 4.0,
            type_reference: 5.0,
            re_export: 6.0,
        };

        let table = weights.kind_weights();

        assert!((table.value_import - 2.0).abs() < f64::EPSILON);
        assert!((table.inheritance - 3.0).abs() < f64::EPSILON);
        assert!((table.call - 4.0).abs() < f64::EPSILON);
        assert!((table.type_reference - 5.0).abs() < f64::EPSILON);
        assert!((table.re_export - 6.0).abs() < f64::EPSILON);
    }
}
