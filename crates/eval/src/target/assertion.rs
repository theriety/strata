//! The `[assert]` block and the per-kind assertions it carries.

use serde::Deserialize;

use super::FaceMode;

/// The `[assert]` block: which modes' best candidates must satisfy the
/// constraints below. The corpus carries exactly one block per target.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertBlock {
    /// Modes whose asserted candidate must satisfy every assertion without its
    /// own `mode` key.
    #[serde(default)]
    pub modes: Vec<FaceMode>,
    /// 1-based state index the assertions read; 1 is the best returned
    /// alternative, or the current tree when no improving alternative exists.
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
/// physically has it; `path` is a post-strip package-relative container key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreserveDir {
    /// The container key that must survive.
    pub path: String,
    /// Overrides `[assert].modes` for this assertion alone.
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
    /// Overrides `[assert].modes` for this assertion alone.
    #[serde(default)]
    pub mode: Option<FaceMode>,
    /// Why these files share — or never share — a container.
    pub because: String,
}

/// The `any_container` scope selector for a size band.
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
    /// Scope selector; exactly one of this and [`SizeBand::container`].
    #[serde(default)]
    pub scope: Option<BandScope>,
    /// Named-container selector matching that full-prefix node name at any
    /// non-file level.
    #[serde(default)]
    pub container: Option<String>,
    /// Overrides `[assert].modes` for this assertion alone.
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
    /// Overrides `[assert].modes` for this assertion alone.
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
    /// Overrides `[assert].modes` for this assertion alone.
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
