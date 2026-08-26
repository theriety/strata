//! The `TargetSpec deserializer for `crates/eval/targets/CONTRACT.md schema v1.
//!
//! Every struct denies unknown fields: a typo in a target file is a harness
//! error at load, never a silently skipped assertion. Structural rules that
//! serde cannot express (exactly-one-of keys, rationale non-emptiness,
//! mode producibility) live in [`TargetSpec::validate], which every case must
//! pass before the engine runs. Census-dependent checks (referenced paths,
//! `preserve_dir resolution) live in [`crate::harness] because they need the
//! analyzed result.

use serde::Deserialize;

use crate::error::EvalError;

/// The contract version this crate implements; any other `schema value is a
/// load-time error.
pub const SCHEMA_VERSION: u8 = 1;

/// One committed constraint target, parsed from
/// `crates/eval/targets/<name>.toml.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    /// The contract version; must equal [`SCHEMA_VERSION].
    pub schema: u8,
    /// The fixture directory this target constrains.
    pub fixture: String,
    /// What the fixture is about.
    pub meta: TargetMeta,
    /// The exact invocation every assertion was validated against.
    pub run: RunSpec,
    /// Preconditions verified against `current before scoring; any failure is
    /// a harness or fixture error, never distance.
    #[serde(default)]
    pub precondition: Vec<Precondition>,
    /// The assertion block; the corpus carries exactly one per target.
    #[serde(default)]
    pub assert: Vec<AssertBlock>,
    /// The optional reference best state feeding pair-F1 (report only).
    #[serde(default)]
    pub reference: Option<ReferenceSet>,
}

/// Describes the fixture's failure mode and the question it asks.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetMeta {
    /// The canonical failure mode under measure.
    pub failure_mode: FailureMode,
    /// The fixture's language.
    pub language: Language,
    /// The single question this fixture asks.
    pub question: String,
}

/// The canonical failure-mode list from CONTRACT.md; anything else fails to
/// deserialize so a misspelled mode can never silently match nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureMode {
    /// A cohesive real directory torn across containers.
    DirectoryTearing,
    /// Unrelated domains welded together through zero-priced edges.
    CrossDirWelding,
    /// An over-cap folder whose relief never registers.
    OverCapacity,
    /// A synthetic workspace bucket swallowing real directories.
    WorkspaceCollapse,
    /// Anchored proposing more change than greenfield on an optimal layout.
    AnchoredInversion,
    /// Container names misaligned with their members.
    NamingIncoherence,
    /// A genuine import cycle spanning two real directories (FIX07 probe substrate).
    ImportCycle,
    /// Proposal quality at three-figure file counts.
    Scale,
}

/// The language adapters a fixture exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Python sources, no manifest needed.
    Python,
    /// TypeScript sources, optionally package-marked.
    Typescript,
}

/// Which restructuring modes a run produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// Anchored only.
    Anchored,
    /// Greenfield only.
    Greenfield,
    /// Both faces of one run.
    Both,
}

impl RunMode {
    /// Returns whether `face is among the modes this run produces.
    #[must_use]
    pub fn produces(self, face: FaceMode) -> bool {
        matches!(
            (self, face),
            (Self::Both, _)
                | (Self::Anchored, FaceMode::Anchored)
                | (Self::Greenfield, FaceMode::Greenfield)
        )
    }
}

/// Which config source a run reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSource {
    /// Built-in defaults via a missing config path.
    Defaults,
    /// The fixture's own `strata.toml.
    Fixture,
}

/// The `[run] invocation block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// Which modes the invocation produced.
    pub mode: RunMode,
    /// The `-k candidate count.
    pub candidates: u32,
    /// The deterministic seed.
    pub seed: u64,
    /// Where the configuration came from.
    pub config: ConfigSource,
}

/// One face of a run an assertion may target; assertions evaluated on a
/// candidate tree always name one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FaceMode {
    /// The anchored best candidate.
    Anchored,
    /// The greenfield best candidate.
    Greenfield,
}

/// The kind of a precondition check against `result.current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreconditionKind {
    /// `summary.files within [min, max].
    FileCount,
    /// A violation class is present (optionally suffix-scoped).
    ViolationPresent,
    /// A violation class is absent (optionally suffix-scoped).
    ViolationAbsent,
}

/// The violation classes a precondition may reference; mirrors the DTO's
/// `ViolationKind values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViolationClass {
    /// A dependency cycle.
    Cycle,
    /// A polarity-matrix breach.
    Polarity,
    /// A capacity cap exceeded.
    Capacity,
    /// Declared visibility wider than derived scope.
    Visibility,
}

/// The severity a precondition filters on when explicitly opted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityFilter {
    /// Hard violations only.
    Violation,
    /// Borderline observations only.
    Borderline,
}

/// One precondition block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Precondition {
    /// Which check to perform.
    pub kind: PreconditionKind,
    /// Inclusive lower file-count bound (`file_count only).
    #[serde(default)]
    pub min: Option<u32>,
    /// Inclusive upper file-count bound (`file_count only).
    #[serde(default)]
    pub max: Option<u32>,
    /// Which violation class to look for (violation kinds only).
    #[serde(default)]
    pub violation: Option<ViolationClass>,
    /// Dot-segment location filter (violation kinds only).
    #[serde(default)]
    pub location_suffix: Option<String>,
    /// Exact severity filter; absent means findings of any severity qualify.
    #[serde(default)]
    pub severity: Option<SeverityFilter>,
    /// The human-defensible reason the precondition holds.
    pub because: String,
}

/// The `[assert] block: which modes' best candidates must satisfy the
/// constraints below. The corpus carries exactly one block per target.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertBlock {
    /// Modes whose asserted candidate must satisfy every assertion without its
    /// own `mode key.
    #[serde(default)]
    pub modes: Vec<FaceMode>,
    /// 1-based candidate index the assertions read; 1 is best-scoring.
    #[serde(default = "default_candidate")]
    pub candidate: u32,
    /// Real directories that must survive as named containers.
    #[serde(default)]
    pub preserve_dir: Vec<PreserveDir>,
    /// File sets some folder/domain container must co-locate.
    #[serde(default)]
    pub keep_together: Vec<PathSetAssertion>,
    /// File sets no folder/domain container may co-locate.
    #[serde(default)]
    pub separate: Vec<PathSetAssertion>,
    /// Member-count bands over containers.
    #[serde(default)]
    pub size_band: Vec<SizeBand>,
    /// Structural change budgets for one mode.
    #[serde(default)]
    pub move_budget: Vec<MoveBudget>,
    /// Names no non-file node may carry as its last segment.
    #[serde(default)]
    pub no_synthetic_bucket: Vec<BucketName>,
    /// Member-name alignment floors for folder/domain containers.
    #[serde(default)]
    pub name_alignment: Vec<NameAlignment>,
    /// Hard-capacity findings a mode's best candidate must leave behind.
    #[serde(default)]
    pub capacity_relief: Vec<CapacityRelief>,
    /// Anchored-must-not-out-churn-greenfield pins (requires run mode both).
    #[serde(default)]
    pub non_inversion: Vec<NonInversion>,
    /// Symbols that must keep their home file in the best candidate (FIX08:
    /// symbol-grain relocation must not churn homes the best state keeps).
    #[serde(default)]
    pub preserve_symbol_home: Vec<PreserveSymbolHome>,
}

/// Default candidate index: the best-scoring candidate.
fn default_candidate() -> u32 {
    1
}

/// A symbol that must keep its home file in the asserted candidate tree.
///
/// `symbol` names the entity exactly as the engine reports it; `path` is the
/// FULL repo-relative path of the file that houses it in the best state (file
/// paths are never container keys, so no package/source-root resolution
/// applies). The harness verifies against the current layout that the symbol
/// actually lives there today — a typo is a load-time error, never fake
/// distance.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreserveSymbolHome {
    /// The symbol's source-declared name.
    pub symbol: String,
    /// Full repo-relative path of the file the symbol must keep as home.
    pub path: String,
    /// Overrides `[assert].modes` for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why this home holds in the best state.
    pub because: String,
}

/// A directory that must survive as a named container once per package that
/// physically has it; `path is a post-strip package-relative container key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreserveDir {
    /// The container key that must survive.
    pub path: String,
    /// Overrides `[assert].modes for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why the directory survives in the best state.
    pub because: String,
}

/// A set of repo-relative files some folder/domain container must (or must
/// not) co-locate.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathSetAssertion {
    /// The repo-relative file paths.
    pub paths: Vec<String>,
    /// Overrides `[assert].modes for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why these files share — or never share — a container.
    pub because: String,
}

/// The `any_container scope selector for a size band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandScope {
    /// Every folder/domain node; package/packageGroup envelopes are exempt.
    AnyContainer,
}

/// A member-count band over first-level members only: a folder's membership is
/// its direct file children, and nested sub-places contribute nothing, so
/// halves that nest under their base folder each measure on their own.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeBand {
    /// Inclusive maximum member count.
    pub max_files: u32,
    /// Optional inclusive minimum member count.
    #[serde(default)]
    pub min_files: Option<u32>,
    /// Scope selector; exactly one of this and [`SizeBand::container].
    #[serde(default)]
    pub scope: Option<BandScope>,
    /// Named-container selector matching that full-prefix node name at any
    /// non-file level.
    #[serde(default)]
    pub container: Option<String>,
    /// Overrides `[assert].modes for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why the band holds in the best state.
    pub because: String,
}

/// A structural change budget for one mode: moved FILES counted by comparing
/// folder/domain container paths between current and candidate trees.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveBudget {
    /// The mode whose change volume is bounded.
    pub mode: FaceMode,
    /// Inclusive maximum of moved files.
    #[serde(default)]
    pub max_moved_files: Option<u32>,
    /// Inclusive minimum of moved files.
    #[serde(default)]
    pub min_moved_files: Option<u32>,
    /// Why the budget holds in the best state.
    pub because: String,
}

/// A name no non-file node may carry as its last segment.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BucketName {
    /// The forbidden last segment.
    pub name: String,
    /// Overrides `[assert].modes for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why the bucket must not appear.
    pub because: String,
}

/// A member-name alignment floor for folder/domain containers.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameAlignment {
    /// Minimum fraction of members sharing a token with the container name.
    pub min_ratio: f64,
    /// Containers holding fewer files are exempt.
    pub min_members: u32,
    /// Overrides `[assert].modes for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why names align in the best state.
    pub because: String,
}

/// A hard-capacity pin on one mode's asserted candidate.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityRelief {
    /// The mode whose best candidate must leave zero hard findings.
    pub mode: FaceMode,
    /// Why a best state leaves no hard capacity finding.
    pub because: String,
}

/// The anchored-vs-greenfield churn-ordering pin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NonInversion {
    /// Why anchored can never justify more change than greenfield here.
    pub because: String,
}

/// The optional reference best state; pair-F1 reads it, verdicts never do.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSet {
    /// The reference containers; labels only, not naming obligations.
    pub container: Vec<ReferenceContainer>,
}

/// One labeled group of files in the reference best state.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceContainer {
    /// The label; pair-F1 reads the files, never this name.
    pub path: String,
    /// The repo-relative files co-membered under this label.
    pub files: Vec<String>,
}

/// Builds a target-scoped [`EvalError::TargetInvalid] without repeating the
/// case name.
fn invalid(target: &str, message: String) -> EvalError {
    EvalError::TargetInvalid {
        target: target.to_owned(),
        message,
    }
}

impl TargetSpec {
    /// Validates every rule CONTRACT.md states that does not need the analyzed
    /// result. `expected_fixture` is the case name the harness invoked this
    /// target under, so a renamed fixture fails loud instead of measuring
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::TargetInvalid] on the first violated rule.
    pub fn validate(&self, expected_fixture: &str) -> Result<(), EvalError> {
        let block = self.validate_shape(expected_fixture)?;
        for precondition in &self.precondition {
            rationale(precondition.because.as_str()).map_err(|message| {
                invalid(
                    expected_fixture,
                    format!("precondition {:?}: {message}", precondition.kind),
                )
            })?;
            validate_precondition(precondition).map_err(|message| {
                invalid(
                    expected_fixture,
                    format!("precondition {:?}: {message}", precondition.kind),
                )
            })?;
        }
        Self::validate_assertions(block, self.run.mode, expected_fixture)?;
        self.validate_reference(expected_fixture)
    }

    /// Header shape: schema, fixture identity, exactly-one assert block, modes
    /// producible by the run. Returns the validated block.
    fn validate_shape(&self, fixture: &str) -> Result<&AssertBlock, EvalError> {
        if self.schema != SCHEMA_VERSION {
            return Err(invalid(
                fixture,
                format!(
                    "schema {} is not supported (this harness implements schema {SCHEMA_VERSION})",
                    self.schema
                ),
            ));
        }
        if self.fixture != fixture {
            return Err(invalid(
                fixture,
                format!(
                    "`fixture = {:?}` does not match the invoked case name {fixture:?}",
                    self.fixture
                ),
            ));
        }
        if self.assert.len() != 1 {
            return Err(invalid(
                fixture,
                format!(
                    "exactly one [[assert]] block is required, found {}",
                    self.assert.len()
                ),
            ));
        }
        let Some(block) = self.assert.first() else {
            return Err(invalid(
                fixture,
                "exactly one [[assert]] block is required".to_owned(),
            ));
        };
        if block.modes.is_empty() {
            return Err(invalid(
                fixture,
                "[assert].modes is empty and some assertion relies on it".to_owned(),
            ));
        }
        if block.candidate == 0 {
            return Err(invalid(
                fixture,
                "[assert].candidate is 1-based; 0 selects nothing".to_owned(),
            ));
        }
        for mode in &block.modes {
            if !self.run.mode.produces(*mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "[assert].modes lists {mode:?} but [run].mode is {:?}",
                        self.run.mode
                    ),
                ));
            }
        }
        Ok(block)
    }

    /// Per-kind assertion rules over one validated block.
    fn validate_assertions(
        block: &AssertBlock,
        run: RunMode,
        fixture: &str,
    ) -> Result<(), EvalError> {
        let lift =
            |checked: Result<(), String>| checked.map_err(|message| invalid(fixture, message));

        for assertion in &block.preserve_dir {
            lift(check_rationale(&assertion.because, "preserve_dir"))?;
        }
        for assertion in block.keep_together.iter().chain(block.separate.iter()) {
            lift(check_rationale(&assertion.because, "path-set"))?;
            if assertion.paths.len() < 2 {
                return Err(invalid(
                    fixture,
                    "keep_together/separate needs at least two paths to relate".to_owned(),
                ));
            }
            if assertion.paths.iter().any(String::is_empty) {
                return Err(invalid(
                    fixture,
                    "keep_together/separate paths must be non-empty".to_owned(),
                ));
            }
            lift(check_own_mode(run, assertion.mode))?;
        }
        for band in &block.size_band {
            lift(check_rationale(&band.because, "size_band"))?;
            if band.scope.is_some() == band.container.is_some() {
                return Err(invalid(
                    fixture,
                    "size_band needs exactly one of scope/container".to_owned(),
                ));
            }
            lift(check_own_mode(run, band.mode))?;
        }
        for budget in &block.move_budget {
            lift(check_rationale(&budget.because, "move_budget"))?;
            if budget.max_moved_files.is_some() == budget.min_moved_files.is_some() {
                return Err(invalid(
                    fixture,
                    "move_budget needs exactly one of max_moved_files/min_moved_files".to_owned(),
                ));
            }
            if !run.produces(budget.mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "move_budget targets {:?} but [run].mode is {run:?}",
                        budget.mode
                    ),
                ));
            }
        }
        for bucket in &block.no_synthetic_bucket {
            lift(check_rationale(&bucket.because, "no_synthetic_bucket"))?;
            if bucket.name.trim().is_empty() {
                return Err(invalid(
                    fixture,
                    "no_synthetic_bucket name must be non-empty".to_owned(),
                ));
            }
            lift(check_own_mode(run, bucket.mode))?;
        }
        for alignment in &block.name_alignment {
            lift(check_rationale(&alignment.because, "name_alignment"))?;
            if !(0.0..=1.0).contains(&alignment.min_ratio) {
                return Err(invalid(
                    fixture,
                    "name_alignment min_ratio must lie within [0, 1]".to_owned(),
                ));
            }
            lift(check_own_mode(run, alignment.mode))?;
        }
        for relief in &block.capacity_relief {
            lift(check_rationale(&relief.because, "capacity_relief"))?;
            if !run.produces(relief.mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "capacity_relief targets {:?} but [run].mode is {run:?}",
                        relief.mode
                    ),
                ));
            }
        }
        for inversion in &block.non_inversion {
            lift(check_rationale(&inversion.because, "non_inversion"))?;
            if run != RunMode::Both {
                return Err(invalid(
                    fixture,
                    "non_inversion compares both faces and requires [run].mode = both".to_owned(),
                ));
            }
        }
        Self::validate_symbol_homes(block, run, fixture)
    }

    /// Symbol-grain pins need a rationale, a non-empty symbol and path pair,
    /// and a mode the configured run actually produces.
    fn validate_symbol_homes(
        block: &AssertBlock,
        run: RunMode,
        fixture: &str,
    ) -> Result<(), EvalError> {
        let lift =
            |checked: Result<(), String>| checked.map_err(|message| invalid(fixture, message));
        for home in &block.preserve_symbol_home {
            lift(check_rationale(&home.because, "preserve_symbol_home"))?;
            if home.symbol.trim().is_empty() || home.path.trim().is_empty() {
                return Err(invalid(
                    fixture,
                    "preserve_symbol_home needs a non-empty symbol and path".to_owned(),
                ));
            }
            lift(check_own_mode(run, home.mode))?;
        }
        Ok(())
    }

    /// Reference containers must label non-empty file sets.
    fn validate_reference(&self, fixture: &str) -> Result<(), EvalError> {
        let Some(reference) = &self.reference else {
            return Ok(());
        };
        for container in &reference.container {
            if container.files.is_empty() {
                return Err(invalid(
                    fixture,
                    format!("reference container {:?} lists no files", container.path),
                ));
            }
            if container.files.iter().any(String::is_empty) {
                return Err(invalid(
                    fixture,
                    format!(
                        "reference container {:?} carries an empty file path",
                        container.path
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// Rejects empty or whitespace-only rationales.
fn rationale(because: &str) -> Result<(), &'static str> {
    if because.trim().is_empty() {
        Err("every block carries a non-empty `because rationale")
    } else {
        Ok(())
    }
}

/// Applies [`rationale`] with the assertion kind folded into the message.
fn check_rationale(because: &str, kind: &str) -> Result<(), String> {
    rationale(because).map_err(|message| format!("{kind}: {message}"))
}

/// Checks an assertion's optional own-mode override against the run.
fn check_own_mode(run: RunMode, mode: Option<FaceMode>) -> Result<(), String> {
    if let Some(face) = mode
        && !run.produces(face)
    {
        return Err(format!(
            "assertion overrides its mode to {face:?} but [run].mode is {run:?}"
        ));
    }
    Ok(())
}

/// Checks a precondition's per-kind key requirements.
fn validate_precondition(precondition: &Precondition) -> Result<(), String> {
    match precondition.kind {
        PreconditionKind::FileCount => {
            if precondition.min.is_none() || precondition.max.is_none() {
                return Err("file_count requires both min and max".to_owned());
            }
        }
        PreconditionKind::ViolationPresent | PreconditionKind::ViolationAbsent => {
            if precondition.violation.is_none() {
                return Err(format!(
                    "{:?} requires a violation class",
                    precondition.kind
                ));
            }
        }
    }
    Ok(())
}
