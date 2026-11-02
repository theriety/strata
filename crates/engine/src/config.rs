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

mod mirror;

pub(crate) use mirror::{MirrorCaptures, MirrorTemplate};
use mirror::{mirror_templates_overlap, validate_mirror_template};

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
    /// `spec/adapters/x` both resolve to the `adapters` domain). Spelled
    /// `source-roots` like every other key; `source_roots` remains accepted.
    #[serde(rename = "source-roots", alias = "source_roots")]
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
    /// `gamma`: the scoped over-capacity binding penalty weight (FIX03). Both
    /// modes price it: a layout binding more files than the folder budget at
    /// any folder or domain pays `gamma` per over-cap share, so relief can win
    /// on J instead of relying on selection alone.
    pub capacity: f64,
    /// Fixed charge for relocating a declaration into a file that contains one
    /// of its dependencies but none of its consumers at pass start.
    #[serde(rename = "dependency-only")]
    pub dependency_only: f64,
    /// Fixed charge while a companion type remains outside its owner file.
    #[serde(rename = "companion-separation")]
    pub companion_separation: f64,
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        Self {
            imbalance: 0.1,
            naming: 0.3,
            path: 0.2,
            anchor: 1.0,
            capacity: 4.0,
            dependency_only: 0.05,
            companion_separation: 0.05,
        }
    }
}

impl ObjectiveConfig {
    /// Converts this profile's objective values into scorer coefficients.
    #[must_use]
    pub const fn coefficients(&self) -> Coefficients {
        Coefficients {
            lambda: self.imbalance,
            alpha: self.naming,
            beta: self.path,
            mu: self.anchor,
            gamma: self.capacity,
            dependency_only: self.dependency_only,
            companion_separation: self.companion_separation,
        }
    }

    /// Compatibility alias for callers that previously selected coefficients by mode.
    #[must_use]
    pub const fn anchored(&self) -> Coefficients {
        self.coefficients()
    }

    /// Compatibility alias that now honors explicit greenfield path and anchor values.
    #[must_use]
    pub const fn greenfield(&self) -> Coefficients {
        self.coefficients()
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
    /// Multiplier for pass-start same-file edges between runtime symbols.
    #[serde(rename = "same-file-symbol")]
    pub same_file_symbol: f64,
    /// Multiplier for pass-start same-file edges touching a type.
    #[serde(rename = "same-file-type")]
    pub same_file_type: f64,
}

impl Default for WeightsConfig {
    fn default() -> Self {
        Self {
            value_import: 1.0,
            inheritance: 1.5,
            call: 1.0,
            type_reference: 0.3,
            re_export: 0.0,
            same_file_symbol: 1.0,
            same_file_type: 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QualificationWeightsConfig {
    #[serde(rename = "unique-owner")]
    pub unique_owner: f64,
    #[serde(rename = "role-affinity")]
    pub role_affinity: f64,
    #[serde(rename = "source-cohesion")]
    pub source_cohesion: f64,
    #[serde(rename = "destination-cohesion")]
    pub destination_cohesion: f64,
    #[serde(rename = "producer-evidence")]
    pub producer_evidence: f64,
    #[serde(rename = "architectural-reach")]
    pub architectural_reach: f64,
}

impl Default for QualificationWeightsConfig {
    fn default() -> Self {
        Self {
            unique_owner: 0.10,
            role_affinity: 0.10,
            source_cohesion: 0.25,
            destination_cohesion: 0.25,
            producer_evidence: 0.10,
            architectural_reach: 0.20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QualificationConfig {
    #[serde(rename = "minimum-evidence")]
    pub minimum_evidence: f64,
    #[serde(rename = "minimum-structural")]
    pub minimum_structural: f64,
    #[serde(rename = "minimum-ambiguity-margin")]
    pub minimum_ambiguity_margin: f64,
    pub weights: QualificationWeightsConfig,
}

impl Default for QualificationConfig {
    fn default() -> Self {
        Self {
            minimum_evidence: 0.60,
            minimum_structural: 0.50,
            minimum_ambiguity_margin: 0.15,
            weights: QualificationWeightsConfig::default(),
        }
    }
}

/// One complete, independently configurable analysis parameter profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileConfig {
    /// Number of diverse candidates to return (`k`).
    pub candidates: u32,
    /// Deterministic base seed.
    pub seed: u64,
    /// Per-level capacity caps.
    pub capacity: CapacityConfig,
    /// Objective coefficients.
    pub objective: ObjectiveConfig,
    /// Dependency and same-file affinity weights.
    pub weights: WeightsConfig,
    /// MFAS solver budget.
    pub solver: SolverConfig,
    /// Diversification parameters.
    pub diversity: DiversityConfig,
    /// Test-file detection and capping policy.
    pub tests: TestsConfig,
    /// Rules controlling which graph participants may relocate and how tests follow sources.
    pub relocation: RelocationConfig,
    /// Evidence required before safe relocation advice is recommended.
    pub qualification: QualificationConfig,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            candidates: 3,
            seed: 42,
            capacity: CapacityConfig::default(),
            objective: ObjectiveConfig::default(),
            weights: WeightsConfig::default(),
            solver: SolverConfig::default(),
            diversity: DiversityConfig::default(),
            tests: TestsConfig::default(),
            relocation: RelocationConfig::default(),
            qualification: QualificationConfig::default(),
        }
    }
}

/// Per-profile relocation admission and test-mirroring policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RelocationConfig {
    /// Prevent detected test files from relocating independently.
    #[serde(rename = "pin-detected-test-files")]
    pub pin_detected_test_files: bool,
    /// Prevent declarations in detected tests from relocating independently.
    #[serde(rename = "pin-detected-test-symbols")]
    pub pin_detected_test_symbols: bool,
    /// Repo-relative file globs whose matching files cannot relocate independently.
    #[serde(rename = "forbid-file-moves")]
    pub forbid_file_moves: Vec<String>,
    /// Repo-relative file globs that block declarations leaving or entering a match.
    #[serde(rename = "forbid-symbol-moves")]
    pub forbid_symbol_moves: Vec<String>,
    /// Exact immutable source-to-test mirroring policy.
    #[serde(rename = "test-mirroring")]
    pub test_mirroring: TestMirroringConfig,
}

impl Default for RelocationConfig {
    fn default() -> Self {
        Self {
            pin_detected_test_files: true,
            pin_detected_test_symbols: true,
            forbid_file_moves: Vec::new(),
            forbid_symbol_moves: Vec::new(),
            test_mirroring: TestMirroringConfig::default(),
        }
    }
}

/// Exact source-to-test mirror inference settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TestMirroringConfig {
    /// Whether accepted source moves attempt immutable mirror followers.
    pub enabled: bool,
    /// Whether built-in language conventions supplement custom rules.
    pub builtins: bool,
    /// Additional exact source-to-test template rules.
    pub rules: Vec<TestMirrorRule>,
}

impl Default for TestMirroringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtins: true,
            rules: Vec::new(),
        }
    }
}

/// One exact source template and its possible mirrored test paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestMirrorRule {
    /// Source path template containing `{dir}` and `{stem}` captures.
    pub source: String,
    /// Test path templates derived from the source captures.
    pub tests: Vec<String>,
}

pub(crate) fn builtin_test_mirror_rules() -> Vec<TestMirrorRule> {
    let mut rules = Vec::new();
    for source_root in ["src", "source"] {
        for extension in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
            rules.push(TestMirrorRule {
                source: format!("{source_root}/{{dir}}/{{stem}}.{extension}"),
                tests: ["spec", "test", "tests"]
                    .into_iter()
                    .flat_map(|test_root| {
                        ["spec", "test"].into_iter().map(move |marker| {
                            format!("{test_root}/{{dir}}/{{stem}}.{marker}.{extension}")
                        })
                    })
                    .chain(["spec", "test"].into_iter().map(|marker| {
                        format!("{source_root}/{{dir}}/{{stem}}.{marker}.{extension}")
                    }))
                    .collect(),
            });
        }
        rules.push(TestMirrorRule {
            source: format!("{source_root}/{{dir}}/{{stem}}.py"),
            tests: ["test", "tests"]
                .into_iter()
                .flat_map(|test_root| {
                    [
                        format!("{test_root}/{{dir}}/test_{{stem}}.py"),
                        format!("{test_root}/{{dir}}/{{stem}}_test.py"),
                    ]
                })
                .chain([
                    format!("{source_root}/{{dir}}/test_{{stem}}.py"),
                    format!("{source_root}/{{dir}}/{{stem}}_test.py"),
                ])
                .collect(),
        });
    }
    rules
}

impl ProfileConfig {
    /// Returns the greenfield defaults without overriding explicit values later.
    #[must_use]
    pub fn greenfield() -> Self {
        Self {
            objective: ObjectiveConfig {
                path: 0.0,
                anchor: 0.0,
                ..ObjectiveConfig::default()
            },
            ..Self::default()
        }
    }
}

/// The complete built-in parameter-profile catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfilesConfig {
    /// Parameters used by the anchored profile.
    pub anchored: ProfileConfig,
    /// Parameters used by the greenfield profile.
    pub greenfield: ProfileConfig,
}

impl Default for ProfilesConfig {
    fn default() -> Self {
        Self {
            anchored: ProfileConfig::default(),
            greenfield: ProfileConfig::greenfield(),
        }
    }
}

impl<'de> Deserialize<'de> for ProfilesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProfileOverrides {
            anchored: Option<serde_json::Value>,
            greenfield: Option<serde_json::Value>,
        }

        let overrides = ProfileOverrides::deserialize(deserializer)?;
        Ok(Self {
            anchored: merge_profile(ProfileConfig::default(), overrides.anchored)
                .map_err(serde::de::Error::custom)?,
            greenfield: merge_profile(ProfileConfig::greenfield(), overrides.greenfield)
                .map_err(serde::de::Error::custom)?,
        })
    }
}

fn merge_profile(
    defaults: ProfileConfig,
    overrides: Option<serde_json::Value>,
) -> Result<ProfileConfig, serde_json::Error> {
    let mut merged = serde_json::to_value(defaults)?;
    if let Some(overrides) = overrides {
        merge_value(&mut merged, overrides);
    }
    serde_json::from_value(merged)
}

fn merge_value(target: &mut serde_json::Value, overrides: serde_json::Value) {
    match overrides {
        serde_json::Value::Object(overrides) if target.is_object() => {
            if let Some(target) = target.as_object_mut() {
                for (key, value) in overrides {
                    match target.get_mut(&key) {
                        Some(target_value) => merge_value(target_value, value),
                        None => {
                            target.insert(key, value);
                        }
                    }
                }
            }
        }
        value => *target = value,
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

    /// Returns the configuration for a named parameter profile.
    #[must_use]
    pub const fn profile(&self, name: ProfileName) -> &ProfileConfig {
        match name {
            ProfileName::Anchored => &self.profiles.anchored,
            ProfileName::Greenfield => &self.profiles.greenfield,
        }
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

    fn validate_qualification(
        prefix: &str,
        qualification: &QualificationConfig,
    ) -> Result<(), StrataError> {
        let key = |suffix: &str| format!("{prefix}.{suffix}");
        for (suffix, value) in [
            (
                "qualification.minimum-evidence",
                qualification.minimum_evidence,
            ),
            (
                "qualification.minimum-structural",
                qualification.minimum_structural,
            ),
            (
                "qualification.minimum-ambiguity-margin",
                qualification.minimum_ambiguity_margin,
            ),
        ] {
            unit_interval(&key(suffix), value)?;
        }
        let qualification_weights = qualification.weights;
        for (suffix, value) in [
            ("unique-owner", qualification_weights.unique_owner),
            ("role-affinity", qualification_weights.role_affinity),
            ("source-cohesion", qualification_weights.source_cohesion),
            (
                "destination-cohesion",
                qualification_weights.destination_cohesion,
            ),
            ("producer-evidence", qualification_weights.producer_evidence),
            (
                "architectural-reach",
                qualification_weights.architectural_reach,
            ),
        ] {
            non_negative_finite(&key(&format!("qualification.weights.{suffix}")), value)?;
        }
        let qualification_total = qualification_weights.unique_owner
            + qualification_weights.role_affinity
            + qualification_weights.source_cohesion
            + qualification_weights.destination_cohesion
            + qualification_weights.producer_evidence
            + qualification_weights.architectural_reach;
        non_negative_finite(&key("qualification.weights"), qualification_total)?;
        if qualification_total <= 0.0 {
            return Err(StrataError::ConfigInvalid {
                key: Some(key("qualification.weights")),
                reason: "at least one qualification weight must be positive".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_relocation(prefix: &str, relocation: &RelocationConfig) -> Result<(), StrataError> {
        for (field, patterns) in [
            ("forbid-file-moves", &relocation.forbid_file_moves),
            ("forbid-symbol-moves", &relocation.forbid_symbol_moves),
        ] {
            for (index, pattern) in patterns.iter().enumerate() {
                let key = format!("{prefix}.relocation.{field}[{index}]");
                if pattern.is_empty() {
                    return Err(StrataError::ConfigInvalid {
                        key: Some(key),
                        reason: "a pattern must not be empty".to_owned(),
                    });
                }
                glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                    key: Some(key),
                    reason: error.to_string(),
                })?;
            }
        }

        let mirror_prefix = format!("{prefix}.relocation.test-mirroring.rules");
        let builtins = relocation
            .test_mirroring
            .builtins
            .then(builtin_test_mirror_rules)
            .unwrap_or_default();
        let mut parsed_rules: Vec<(MirrorTemplate, Vec<MirrorTemplate>)> = builtins
            .iter()
            .map(|rule| {
                let source = MirrorTemplate::parse(&rule.source).ok_or_else(|| {
                    StrataError::ConfigInvalid {
                        key: Some(format!("{prefix}.relocation.test-mirroring.builtins")),
                        reason: "a built-in source mirror template is invalid".to_owned(),
                    }
                })?;
                let tests = rule
                    .tests
                    .iter()
                    .map(|template| {
                        MirrorTemplate::parse(template).ok_or_else(|| StrataError::ConfigInvalid {
                            key: Some(format!("{prefix}.relocation.test-mirroring.builtins")),
                            reason: "a built-in test mirror template is invalid".to_owned(),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((source, tests))
            })
            .collect::<Result<Vec<_>, StrataError>>()?;
        for (rule_index, rule) in relocation.test_mirroring.rules.iter().enumerate() {
            let source_key = format!("{mirror_prefix}[{rule_index}].source");
            validate_mirror_template(&rule.source, &source_key)?;
            let source =
                MirrorTemplate::parse(&rule.source).ok_or_else(|| StrataError::ConfigInvalid {
                    key: Some(source_key.clone()),
                    reason: "mirror template is invalid".to_owned(),
                })?;
            if parsed_rules
                .iter()
                .any(|(existing, _)| mirror_templates_overlap(existing, &source))
            {
                return Err(StrataError::ConfigInvalid {
                    key: Some(source_key),
                    reason: "overlapping source template is ambiguous".to_owned(),
                });
            }
            let mut parsed_tests = Vec::new();
            for (test_index, test) in rule.tests.iter().enumerate() {
                let test_key = format!("{mirror_prefix}[{rule_index}].tests[{test_index}]");
                validate_mirror_template(test, &test_key)?;
                let template =
                    MirrorTemplate::parse(test).ok_or_else(|| StrataError::ConfigInvalid {
                        key: Some(test_key.clone()),
                        reason: "mirror template is invalid".to_owned(),
                    })?;
                let overlaps_same_rule = parsed_tests
                    .iter()
                    .any(|existing| mirror_templates_overlap(existing, &template));
                let overlaps_source_and_test =
                    parsed_rules.iter().any(|(existing_source, tests)| {
                        mirror_templates_overlap(existing_source, &source)
                            && tests
                                .iter()
                                .any(|existing| mirror_templates_overlap(existing, &template))
                    });
                if overlaps_same_rule || overlaps_source_and_test {
                    return Err(StrataError::ConfigInvalid {
                        key: Some(test_key),
                        reason: "overlapping test template is ambiguous".to_owned(),
                    });
                }
                parsed_tests.push(template);
            }
            parsed_rules.push((source, parsed_tests));
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
    let document: toml::Value =
        toml::from_str(&text).map_err(|error| StrataError::ConfigInvalid {
            key: None,
            reason: error.message().to_owned(),
        })?;
    reject_legacy_keys(&document)?;
    let config: AnalyzeConfig =
        toml::from_str(&text).map_err(|error| StrataError::ConfigInvalid {
            key: None,
            reason: error.message().to_owned(),
        })?;
    config.validate()?;
    Ok(config)
}

/// Rejects removed configuration locations before typed deserialization so the
/// diagnostic retains the complete offending TOML key.
fn reject_legacy_keys(document: &toml::Value) -> Result<(), StrataError> {
    const ANALYSIS_KEYS: [&str; 3] = ["mode", "candidates", "seed"];
    const TOP_LEVEL_KEYS: [&str; 6] = [
        "capacity",
        "objective",
        "weights",
        "solver",
        "diversity",
        "tests",
    ];

    if let Some(analysis) = document.get("analysis").and_then(toml::Value::as_table) {
        for field in ANALYSIS_KEYS {
            if analysis.contains_key(field) {
                return Err(unknown_legacy_key(format!("analysis.{field}"), field));
            }
        }
    }
    for field in TOP_LEVEL_KEYS {
        if document.get(field).is_some() {
            return Err(unknown_legacy_key(field.to_owned(), field));
        }
    }
    Ok(())
}

fn unknown_legacy_key(key: String, field: &str) -> StrataError {
    StrataError::ConfigInvalid {
        key: Some(key),
        reason: format!("unknown field `{field}`"),
    }
}

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
const JOBS_CEILING: u32 = 1_024;

/// The inclusive ceiling for `analysis.candidates` (`k`).
const CANDIDATES_CEILING: u32 = 64;

/// The inclusive ceiling for `diversity.seeds-per-candidate`.
const SEEDS_PER_CANDIDATE_CEILING: u32 = 64;

/// The inclusive ceiling on the whole restart pool, `candidates *
/// seeds-per-candidate`.
///
/// Each pooled seed runs the full assemble, score, and narrate pipeline with no
/// fallback path, so the product — not either factor alone — is what bounds the
/// work. Two individually legal values can still multiply into an effectively
/// unbounded search, which is why this is checked separately.
const RESTART_POOL_CEILING: u32 = 512;

/// The inclusive ceiling for `solver.ilp-threshold`.
///
/// The threshold sizes the exact MFAS solver's branch-and-bound cutover, whose
/// worst case is exponential in the size of the cyclic component handed to it.
const ILP_THRESHOLD_CEILING: u32 = 4_096;

/// Returns `Ok` when `value` is at most `ceiling`, else a `ConfigInvalid` naming
/// `key`, the ceiling, and the offending value.
fn at_most(key: &str, value: u32, ceiling: u32) -> Result<(), StrataError> {
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
fn within_ceiling(key: &str, value: u32) -> Result<(), StrataError> {
    at_most(key, value, CAP_CEILING)
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

fn unit_interval(key: &str, value: f64) -> Result<(), StrataError> {
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
fn at_least_one_finite(key: &str, value: f64) -> Result<(), StrataError> {
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
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn should_default_to_the_config_less_baseline() {
        let config = AnalyzeConfig::default();

        assert_eq!(
            config.analysis.profiles,
            vec![ProfileName::Anchored, ProfileName::Greenfield]
        );
        assert_eq!(config.profiles.anchored.candidates, 3);
        assert_eq!(config.profiles.anchored.seed, 42);
        assert_eq!(config.profiles.anchored.capacity.file, 250);
        assert_eq!(config.profiles.anchored.capacity.folder, 20);
        assert_eq!(config.profiles.anchored.capacity.domain, 16);
        assert_eq!(config.profiles.anchored.capacity.package, 15);
        assert_eq!(config.profiles.anchored.solver.ilp_threshold, 300);
        assert_eq!(config.profiles.anchored.diversity.seeds_per_candidate, 10);
        assert!(config.profiles.greenfield.objective.path.abs() < f64::EPSILON);
        assert!(config.profiles.greenfield.objective.anchor.abs() < f64::EPSILON);
        assert!((config.profiles.anchored.objective.dependency_only - 0.05).abs() < f64::EPSILON);
        assert!((config.profiles.greenfield.objective.dependency_only - 0.05).abs() < f64::EPSILON);
        assert!(
            (config.profiles.anchored.objective.companion_separation - 0.05).abs() < f64::EPSILON
        );
        assert!(
            (config.profiles.greenfield.objective.companion_separation - 0.05).abs() < f64::EPSILON
        );
    }

    #[test]
    fn should_parse_an_explicit_zero_companion_separation_objective() {
        let parsed = toml::from_str::<AnalyzeConfig>(
            "[profiles.greenfield.objective]\ncompanion-separation = 0.0\n",
        );
        assert!(
            parsed.is_ok(),
            "zero disables companion separation for one profile"
        );
        let config = parsed.unwrap_or_default();

        assert!(
            config
                .profiles
                .greenfield
                .objective
                .companion_separation
                .abs()
                < f64::EPSILON
        );
        assert!(
            (config.profiles.anchored.objective.companion_separation - 0.05).abs() < f64::EPSILON
        );
    }

    #[test]
    fn should_reject_invalid_companion_separation_objectives_with_their_key_path() {
        for companion_separation in [-0.01, f64::INFINITY, f64::NAN] {
            let result = AnalyzeConfig {
                profiles: ProfilesConfig {
                    anchored: ProfileConfig {
                        objective: ObjectiveConfig {
                            companion_separation,
                            ..ObjectiveConfig::default()
                        },
                        ..ProfileConfig::default()
                    },
                    ..ProfilesConfig::default()
                },
                ..AnalyzeConfig::default()
            }
            .validate();

            assert!(
                matches!(
                    result,
                    Err(StrataError::ConfigInvalid { key: Some(key), .. })
                        if key == "profiles.anchored.objective.companion-separation"
                ),
                "negative and non-finite companion pricing is invalid"
            );
        }
    }

    #[test]
    fn should_parse_an_explicit_zero_dependency_only_objective() {
        let config = toml::from_str::<AnalyzeConfig>(
            "[profiles.anchored.objective]\ndependency-only = 0.0\n",
        )
        .unwrap_or_default();

        assert!(config.profiles.anchored.objective.dependency_only.abs() < f64::EPSILON);
    }

    #[test]
    fn should_keep_the_dependency_only_default_when_the_objective_is_partial() {
        let parsed =
            toml::from_str::<AnalyzeConfig>("[profiles.greenfield.objective]\nnaming = 0.8\n");
        assert!(
            parsed.is_ok(),
            "partial profile objective should parse: {:?}",
            parsed.as_ref().err()
        );
        let config = parsed.unwrap_or_default();

        assert!((config.profiles.greenfield.objective.naming - 0.8).abs() < f64::EPSILON);
        assert!((config.profiles.greenfield.objective.dependency_only - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn should_reject_invalid_dependency_only_objectives_with_their_key_path() {
        for dependency_only in [-0.01, f64::INFINITY, f64::NAN] {
            let config = AnalyzeConfig {
                profiles: ProfilesConfig {
                    anchored: ProfileConfig {
                        objective: ObjectiveConfig {
                            dependency_only,
                            ..ObjectiveConfig::default()
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
                    if key == "profiles.anchored.objective.dependency-only"
            ));
        }
    }

    #[test]
    fn should_parse_a_partial_toml_filling_omitted_keys_with_defaults() {
        let toml = "[analysis]\nprofiles = [\"anchored\"]\n[profiles.anchored]\ncandidates = 5\n";

        let config: AnalyzeConfig =
            toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

        assert_eq!(config.analysis.profiles, vec![ProfileName::Anchored]);
        assert_eq!(config.profiles.anchored.candidates, 5);
        // an omitted key keeps its default.
        assert_eq!(config.profiles.anchored.capacity.file, 250);
    }

    #[test]
    fn should_round_trip_renamed_kebab_keys() {
        let toml = "[profiles.anchored.capacity]\npackage-group = 7\n[profiles.anchored.weights]\nvalue-import = 2.0\n";

        let config: AnalyzeConfig =
            toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

        assert_eq!(config.profiles.anchored.capacity.package_group, 7);
        assert!((config.profiles.anchored.weights.value_import - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn should_accept_source_roots_in_kebab_and_legacy_spelling() {
        let kebab = "[adapters]\nsource-roots = [\"app\"]\n";
        let legacy = "[adapters]\nsource_roots = [\"app\"]\n";

        let kebab: Result<AnalyzeConfig, _> = toml::from_str(kebab);
        let legacy: Result<AnalyzeConfig, _> = toml::from_str(legacy);

        assert_eq!(
            kebab.map(|config| config.adapters.source_roots).ok(),
            Some(vec!["app".to_owned()]),
            "the kebab key matches every other config key"
        );
        assert_eq!(
            legacy.map(|config| config.adapters.source_roots).ok(),
            Some(vec!["app".to_owned()]),
            "the snake_case spelling remains accepted"
        );
    }

    #[test]
    fn should_reject_an_unknown_key() {
        let toml = "[analysis]\nnonsense = true\n";

        let result: Result<AnalyzeConfig, _> = toml::from_str(toml);

        assert!(result.is_err());
    }

    /// A complete neutral profile document used by migration contract tests.
    fn complete_profiles_toml() -> &'static str {
        r#"
[analysis]
profiles = ["anchored", "greenfield"]
jobs = 0

[profiles.anchored]
candidates = 3
seed = 42
[profiles.anchored.capacity]
file = 250
folder = 20
domain = 16
package = 15
package-group = 12
[profiles.anchored.objective]
imbalance = 0.1
naming = 0.3
path = 0.2
anchor = 1.0
capacity = 4.0
[profiles.anchored.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15
[profiles.anchored.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20
[profiles.anchored.weights]
value-import = 1.0
inheritance = 1.5
call = 1.0
type-reference = 0.3
re-export = 0.0
same-file-symbol = 1.0
same-file-type = 3.0
[profiles.anchored.solver]
ilp-threshold = 300
timeout-seconds = 60
[profiles.anchored.diversity]
seeds-per-candidate = 10
score-tolerance = 0.05
min-distance = 0.05
[profiles.anchored.tests]
helper-cap = 250
patterns = []
builtins = true

[profiles.greenfield]
candidates = 5
seed = 84
[profiles.greenfield.capacity]
file = 240
folder = 18
domain = 14
package = 13
package-group = 11
[profiles.greenfield.objective]
imbalance = 0.2
naming = 0.4
path = 0.7
anchor = 0.8
capacity = 5.0
[profiles.greenfield.qualification]
minimum-evidence = 0.70
minimum-structural = 0.55
minimum-ambiguity-margin = 0.20
[profiles.greenfield.qualification.weights]
unique-owner = 0.05
role-affinity = 0.15
source-cohesion = 0.20
destination-cohesion = 0.30
producer-evidence = 0.10
architectural-reach = 0.20
[profiles.greenfield.weights]
value-import = 1.1
inheritance = 1.6
call = 1.2
type-reference = 0.4
re-export = 0.1
same-file-symbol = 1.0
same-file-type = 1.0
[profiles.greenfield.solver]
ilp-threshold = 301
timeout-seconds = 61
[profiles.greenfield.diversity]
seeds-per-candidate = 11
score-tolerance = 0.06
min-distance = 0.06
[profiles.greenfield.tests]
helper-cap = 240
patterns = ["checks/**"]
builtins = false
"#
    }

    #[test]
    fn should_parse_complete_independent_parameter_profiles() {
        let parsed: Result<AnalyzeConfig, _> = toml::from_str(complete_profiles_toml());

        assert!(
            parsed.is_ok(),
            "complete profile documents must parse: {parsed:?}"
        );
    }

    #[test]
    fn should_honor_explicit_nonzero_greenfield_path_and_anchor_values() {
        let serialized = toml::from_str::<AnalyzeConfig>(complete_profiles_toml())
            .ok()
            .and_then(|config| serde_json::to_value(config).ok());

        assert_eq!(
            serialized
                .as_ref()
                .and_then(|value| value.pointer("/profiles/greenfield/objective/path"))
                .and_then(serde_json::Value::as_f64),
            Some(0.7)
        );
        assert_eq!(
            serialized
                .as_ref()
                .and_then(|value| value.pointer("/profiles/greenfield/objective/anchor"))
                .and_then(serde_json::Value::as_f64),
            Some(0.8)
        );
    }

    #[test]
    fn should_keep_greenfield_objective_defaults_when_its_profile_is_partial() {
        let config = toml::from_str::<AnalyzeConfig>(
            "[profiles.greenfield]\ncandidates = 5\n[profiles.greenfield.objective]\nnaming = 0.8\n",
        )
        .unwrap_or_default();

        assert_eq!(config.profiles.greenfield.candidates, 5);
        assert!((config.profiles.greenfield.objective.naming - 0.8).abs() < f64::EPSILON);
        assert!(config.profiles.greenfield.objective.path.abs() < f64::EPSILON);
        assert!(config.profiles.greenfield.objective.anchor.abs() < f64::EPSILON);
    }

    #[test]
    fn should_parse_complete_independent_qualification_policies() {
        let document = r"
[profiles.anchored.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15
[profiles.anchored.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20

[profiles.greenfield.qualification]
minimum-evidence = 0.70
minimum-structural = 0.55
minimum-ambiguity-margin = 0.20
[profiles.greenfield.qualification.weights]
unique-owner = 0.05
role-affinity = 0.15
source-cohesion = 0.20
destination-cohesion = 0.30
producer-evidence = 0.10
architectural-reach = 0.20
";

        let parsed = toml::from_str::<AnalyzeConfig>(document);
        assert!(
            parsed.is_ok(),
            "qualification is independently configurable per profile: {parsed:?}"
        );
        let config = parsed.unwrap_or_default();

        assert!(
            (config.profiles.anchored.qualification.minimum_evidence - 0.60).abs() < f64::EPSILON
        );
        assert!(
            (config.profiles.greenfield.qualification.minimum_evidence - 0.70).abs() < f64::EPSILON
        );
        assert!(
            (config
                .profiles
                .greenfield
                .qualification
                .weights
                .destination_cohesion
                - 0.30)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn should_default_every_profile_to_the_conservative_qualification_policy() {
        let config = AnalyzeConfig::default();

        for (profile_name, profile) in [
            ("anchored", config.profiles.anchored),
            ("greenfield", config.profiles.greenfield),
        ] {
            let qualification = profile.qualification;
            let expected_values = [
                ("minimum-evidence", qualification.minimum_evidence, 0.60),
                ("minimum-structural", qualification.minimum_structural, 0.50),
                (
                    "minimum-ambiguity-margin",
                    qualification.minimum_ambiguity_margin,
                    0.15,
                ),
                (
                    "weights.unique-owner",
                    qualification.weights.unique_owner,
                    0.10,
                ),
                (
                    "weights.role-affinity",
                    qualification.weights.role_affinity,
                    0.10,
                ),
                (
                    "weights.source-cohesion",
                    qualification.weights.source_cohesion,
                    0.25,
                ),
                (
                    "weights.destination-cohesion",
                    qualification.weights.destination_cohesion,
                    0.25,
                ),
                (
                    "weights.producer-evidence",
                    qualification.weights.producer_evidence,
                    0.10,
                ),
                (
                    "weights.architectural-reach",
                    qualification.weights.architectural_reach,
                    0.20,
                ),
            ];
            for (key, actual, expected) in expected_values {
                assert!(
                    (actual - expected).abs() < f64::EPSILON,
                    "{profile_name} default at {key}"
                );
            }
        }
    }

    #[test]
    fn should_reject_out_of_range_qualification_values_with_precise_paths() {
        let cases = [("minimum-evidence", -0.01), ("minimum-structural", 1.01)];

        for (key, value) in cases {
            let document = if let Some(weight) = key.strip_prefix("weights.") {
                format!("[profiles.anchored.qualification.weights]\n{weight} = {value}\n")
            } else {
                format!("[profiles.anchored.qualification]\n{key} = {value}\n")
            };
            let result = toml::from_str::<AnalyzeConfig>(&document)
                .map_err(|parse| parse.to_string())
                .and_then(|config| config.validate().map_err(|error| error.to_string()));
            let Err(error) = result else {
                assert!(false, "invalid qualification values must be rejected");
                continue;
            };
            assert!(
                error.contains(&format!("profiles.anchored.qualification.{key}")),
                "diagnostic should name the invalid key: {error}"
            );
        }

        for (key, value) in [
            ("minimum-ambiguity-margin", f64::INFINITY),
            ("weights.unique-owner", f64::NAN),
        ] {
            let mut config = AnalyzeConfig::default();
            if key == "minimum-ambiguity-margin" {
                config
                    .profiles
                    .anchored
                    .qualification
                    .minimum_ambiguity_margin = value;
            } else {
                config.profiles.anchored.qualification.weights.unique_owner = value;
            }
            let Err(error) = config.validate() else {
                assert!(false, "non-finite qualification values must be rejected");
                continue;
            };
            let error = error.to_string();
            assert!(
                error.contains(&format!("profiles.anchored.qualification.{key}")),
                "diagnostic should name the invalid key: {error}"
            );
        }
    }

    fn overflowing_qualification_weights(profile: &str) -> Result<(), String> {
        let document = format!(
            "[profiles.{profile}.qualification.weights]\nunique-owner = 1e308\nrole-affinity = 1e308\n"
        );
        let config: AnalyzeConfig = toml::from_str(&document).map_err(|error| error.to_string())?;
        let error = config
            .validate()
            .err()
            .ok_or("finite individual weights with an overflowing total must fail")?
            .to_string();
        assert!(
            error.contains(&format!("profiles.{profile}.qualification.weights")),
            "{error}"
        );
        assert!(error.contains("finite"), "{error}");
        Ok(())
    }

    #[test]
    fn should_reject_anchored_qualification_weight_overflow() -> Result<(), String> {
        overflowing_qualification_weights("anchored")
    }

    #[test]
    fn should_reject_greenfield_qualification_weight_overflow() -> Result<(), String> {
        overflowing_qualification_weights("greenfield")
    }

    #[test]
    fn should_accept_large_finite_qualification_weight_totals() -> Result<(), String> {
        for profile in ["anchored", "greenfield"] {
            let document = format!(
                "[profiles.{profile}.qualification.weights]\nunique-owner = 5e307\nrole-affinity = 5e307\n"
            );
            let config: AnalyzeConfig =
                toml::from_str(&document).map_err(|error| error.to_string())?;
            config.validate().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    #[test]
    fn should_name_every_removed_legacy_key_in_its_diagnostic() {
        let legacy_documents = [
            ("[analysis]\nmode = \"both\"\n", "analysis.mode", "mode"),
            (
                "[analysis]\ncandidates = 3\n",
                "analysis.candidates",
                "candidates",
            ),
            ("[analysis]\nseed = 42\n", "analysis.seed", "seed"),
            ("[capacity]\nfile = 250\n", "capacity", "capacity"),
            ("[objective]\npath = 0.2\n", "objective", "objective"),
            ("[weights]\ncall = 1.0\n", "weights", "weights"),
            ("[solver]\nilp-threshold = 300\n", "solver", "solver"),
            (
                "[diversity]\nseeds-per-candidate = 10\n",
                "diversity",
                "diversity",
            ),
            ("[tests]\nhelper-cap = 250\n", "tests", "tests"),
        ];
        let path = std::env::temp_dir().join(format!(
            "strata-legacy-profile-config-{}.toml",
            std::process::id()
        ));

        for (document, expected_key, legacy_field) in legacy_documents {
            assert!(
                std::fs::write(&path, document).is_ok(),
                "write isolated legacy config fixture"
            );
            let diagnostic = load_config(&path);
            assert!(
                matches!(
                    diagnostic,
                    Err(StrataError::ConfigInvalid { key: Some(ref key), ref reason })
                        if key == expected_key
                            && reason == &format!("unknown field `{legacy_field}`")
                ),
                "expected exact diagnostic key={expected_key:?}, reason={:?}; got {diagnostic:?}",
                format!("unknown field `{legacy_field}`")
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn should_reject_a_zero_cap_with_its_key_path() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    capacity: CapacityConfig {
                        file: 0,
                        ..CapacityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.capacity.file"
        ));
    }

    #[test]
    fn should_parse_tests_patterns_and_builtins_from_toml() {
        let toml = "[profiles.anchored.tests]\npatterns = [\"*.spec.*\", \"apps/web/__tests__/**\"]\nbuiltins = false\n";

        let config: AnalyzeConfig =
            toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

        assert_eq!(
            config.profiles.anchored.tests.patterns,
            vec!["*.spec.*", "apps/web/__tests__/**"]
        );
        assert!(!config.profiles.anchored.tests.builtins);
        // an omitted key keeps its default.
        assert_eq!(config.profiles.anchored.tests.helper_cap, 250);
    }

    #[test]
    fn should_reject_an_empty_tests_pattern_with_its_index() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    tests: TestsConfig {
                        patterns: vec!["*.spec.*".to_owned(), String::new()],
                        ..TestsConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.tests.patterns[1]"
        ));
    }

    #[test]
    fn should_reject_an_invalid_tests_glob_with_its_index() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    tests: TestsConfig {
                        patterns: vec!["[".to_owned()],
                        ..TestsConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.tests.patterns[0]"
        ));
    }

    #[test]
    fn should_reject_a_folder_cap_above_the_ceiling() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    capacity: CapacityConfig {
                        folder: 257,
                        ..CapacityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "profiles.anchored.capacity.folder" && reason.contains("256") && reason.contains("257")
        ));
    }

    #[test]
    fn should_reject_a_domain_cap_above_the_ceiling() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    capacity: CapacityConfig {
                        domain: 300,
                        ..CapacityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "profiles.anchored.capacity.domain" && reason.contains("256") && reason.contains("300")
        ));
    }

    #[test]
    fn should_reject_a_package_cap_above_the_ceiling() {
        // u32::MAX is the classic hostile value: the error must format it, not wrap.
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    capacity: CapacityConfig {
                        package: u32::MAX,
                        ..CapacityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), reason })
                if key == "profiles.anchored.capacity.package" && reason.contains("256") && reason.contains("4294967295")
        ));
    }

    #[test]
    fn should_accept_caps_at_the_ceiling() {
        // 256 is inclusive: the bound rejects only what lies beyond it.
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    capacity: CapacityConfig {
                        folder: 256,
                        domain: 256,
                        package: 256,
                        ..CapacityConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn should_reject_a_negative_coefficient() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    objective: ObjectiveConfig {
                        naming: -1.0,
                        ..ObjectiveConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.objective.naming"
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
            capacity: 4.0,
            dependency_only: 0.8,
            companion_separation: 0.9,
        };

        let coefficients = objective.anchored();

        assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
        assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
        assert!((coefficients.beta - 0.6).abs() < f64::EPSILON);
        assert!((coefficients.mu - 0.7).abs() < f64::EPSILON);
        assert!((coefficients.dependency_only - 0.8).abs() < f64::EPSILON);
        assert!((coefficients.companion_separation - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn should_honor_explicit_path_and_anchor_in_greenfield_coefficients() {
        let objective = ObjectiveConfig {
            imbalance: 0.4,
            naming: 0.5,
            path: 0.6,
            anchor: 0.7,
            capacity: 4.0,
            dependency_only: 0.8,
            companion_separation: 0.9,
        };

        let coefficients = objective.greenfield();

        assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
        assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
        assert!((coefficients.beta - 0.6).abs() < f64::EPSILON);
        assert!((coefficients.mu - 0.7).abs() < f64::EPSILON);
        assert!((coefficients.dependency_only - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn should_map_weights_config_onto_kind_weights() {
        let weights = WeightsConfig {
            value_import: 2.0,
            inheritance: 3.0,
            call: 4.0,
            type_reference: 5.0,
            re_export: 6.0,
            same_file_symbol: 1.0,
            same_file_type: 3.0,
        };

        let table = weights.kind_weights();

        assert!((table.value_import - 2.0).abs() < f64::EPSILON);
        assert!((table.inheritance - 3.0).abs() < f64::EPSILON);
        assert!((table.call - 4.0).abs() < f64::EPSILON);
        assert!((table.type_reference - 5.0).abs() < f64::EPSILON);
        assert!((table.re_export - 6.0).abs() < f64::EPSILON);
    }

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
    fn should_accept_zero_jobs_as_the_use_every_core_sentinel() {
        let config = AnalyzeConfig {
            analysis: AnalysisConfig {
                jobs: 0,
                ..AnalysisConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(config.validate().is_ok());
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
    fn should_reject_a_zero_candidate_count() {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    candidates: 0,
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
    fn should_reject_a_restart_pool_whose_factors_are_each_legal() {
        // 32 and 32 both pass their own ceilings; their product does not.
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    candidates: 32,
                    diversity: DiversityConfig {
                        seeds_per_candidate: 32,
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
                if key == "profiles.anchored.candidates * profiles.anchored.diversity.seeds-per-candidate"
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

    #[test]
    fn should_apply_generic_overrides_to_every_selected_profile() {
        let mut config = AnalyzeConfig::default();

        config.override_candidates(7);
        config.override_seed(99);

        assert_eq!(config.profiles.anchored.candidates, 7);
        assert_eq!(config.profiles.greenfield.candidates, 7);
        assert_eq!(config.profiles.anchored.seed, 99);
        assert_eq!(config.profiles.greenfield.seed, 99);
    }

    #[test]
    fn should_leave_unselected_profiles_unchanged_during_generic_overrides() {
        let mut config = AnalyzeConfig::default();
        config.select_mode(Mode::Greenfield);

        config.override_candidates(7);
        config.override_seed(99);

        assert_eq!(config.profiles.anchored.candidates, 3);
        assert_eq!(config.profiles.anchored.seed, 42);
        assert_eq!(config.profiles.greenfield.candidates, 7);
        assert_eq!(config.profiles.greenfield.seed, 99);
    }

    #[test]
    fn should_reject_same_file_weights_below_one() {
        let mut config = AnalyzeConfig::default();
        config.profiles.greenfield.weights.same_file_type = 0.9;

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. })
                if key == "profiles.greenfield.weights.same-file-type"
        ));
    }

    #[test]
    fn should_accept_the_shipped_defaults_within_every_search_ceiling() {
        assert!(AnalyzeConfig::default().validate().is_ok());
    }
}
