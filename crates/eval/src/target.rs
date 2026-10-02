//! The `TargetSpec deserializer for `crates/eval/targets/CONTRACT.md schema v1.
//!
//! Every struct denies unknown fields: a typo in a target file is a harness
//! error at load, never a silently skipped assertion. Structural rules that
//! serde cannot express (exactly-one-of keys, rationale non-emptiness,
//! mode producibility) live in [`TargetSpec::validate`], which every case must
//! pass before the engine runs. Census-dependent checks (referenced paths,
//! `preserve_dir resolution) live in [`crate::harness] because they need the
//! analyzed result.

use serde::Deserialize;

mod assertion;
mod assertion_rules;
mod precondition;
mod validate;

pub use assertion::{
    AssertBlock, BandScope, BucketName, CapacityRelief, MoveBudget, NameAlignment, NonInversion,
    PathSetAssertion, PreserveDir, PreserveSymbolHome, SizeBand,
};
pub use precondition::{Precondition, PreconditionKind, SeverityFilter, ViolationClass};

/// The contract version this crate implements; any other `schema` value is a
/// load-time error.
pub const SCHEMA_VERSION: u8 = 1;

/// One committed constraint target, parsed from
/// `crates/eval/targets/<name>.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    /// The contract version; must equal [`SCHEMA_VERSION`].
    pub schema: u8,
    /// The fixture directory this target constrains.
    pub fixture: String,
    /// What the fixture is about.
    pub meta: TargetMeta,
    /// The exact invocation every assertion was validated against.
    pub run: RunSpec,
    /// Preconditions verified against `current` before scoring; any failure is
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
    /// Production symbols relocated across the source/test boundary into (or out
    /// of) their spec twins (FIX11).
    SourceTestMixing,
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
    /// Returns whether `face` is among the modes this run produces.
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
    /// The fixture's own `strata.toml`.
    Fixture,
}

/// The `[run]` invocation block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// Which modes the invocation produced.
    pub mode: RunMode,
    /// The `-k` candidate count.
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
