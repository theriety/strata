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
    /// File sets whose cycle finding must name an import cut (ADR-35: a
    /// cross-file cycle never offers only symbol cuts).
    #[serde(default)]
    pub cycle_import_cut: Vec<CycleImportCut>,
    /// Moves whose cross-profile advice tier is capped or pinned (SC-5: a
    /// move the target falsifies never reaches `Recommended`).
    #[serde(default)]
    pub advice_tier: Vec<AdviceTier>,
    /// Guards that no `Recommended` move breaks this block's own
    /// `keep_together`, `preserve_dir`, or `preserve_symbol_home` pins.
    #[serde(default)]
    pub recommended_guard: Vec<RecommendedGuard>,
}

/// An advice tier, ordered weakest first: no advice, `Review candidate`,
/// `Recommended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// The result carries no advice entry for the move.
    Absent,
    /// The move appears only among `reviewCandidates`.
    Review,
    /// The move appears among `recommended`.
    Recommended,
}

/// A move whose advice tier is capped (`max_tier`) or pinned (`exact_tier`).
///
/// Read from the result's cross-profile `advice`, never a candidate tree, so
/// it takes no mode. `path` is the full repo-relative file the move leaves:
/// without `symbol` it selects whole-file advice relocating that file
/// (primary or mirrored test); with `symbol` it selects symbol advice moving
/// that symbol out of `path`. `to`, when given, narrows the selection to
/// advice whose destination (a folder for a file move, a file for a symbol
/// move) equals it. The observed tier is the strongest tier any selected
/// entry holds, `absent` when none matches.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdviceTier {
    /// Full repo-relative path of the file the move leaves.
    pub path: String,
    /// The relocated symbol's source-declared name, for symbol advice.
    #[serde(default)]
    pub symbol: Option<String>,
    /// The destination the selection is narrowed to, when given.
    #[serde(default)]
    pub to: Option<String>,
    /// The strongest tier the move may reach; exactly one of this and
    /// [`AdviceTier::exact_tier`].
    #[serde(default)]
    pub max_tier: Option<Tier>,
    /// The tier the move must hold exactly.
    #[serde(default)]
    pub exact_tier: Option<Tier>,
    /// Why the move may not rank higher (or must rank exactly so).
    pub because: String,
}

/// The SC-5 guard: every `Recommended` advice entry must leave each of the
/// block's `keep_together`, `preserve_dir`, and `preserve_symbol_home` pins
/// intact. Read from the cross-profile advice, so it takes no mode.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendedGuard {
    /// Why no recommended move may contradict the pinned best state.
    pub because: String,
}

/// Default candidate index: the best-scoring candidate.
fn default_candidate() -> u32 {
    1
}

/// A cross-file cycle whose finding must name the imports to remove.
///
/// Read from the current layout's findings, never a candidate: some cycle
/// finding must span every listed file, and each such finding must name at
/// least one import between two of its files, with the statement located.
/// A finding that offers only symbol cuts, or reports its cut unavailable,
/// fails. With `expect`, each such finding's cut must also name that import.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleImportCut {
    /// Full repo-relative paths of files on the cycle.
    pub files: Vec<String>,
    /// The import the cut must name, when pinned.
    #[serde(default)]
    pub expect: Option<ExpectedImport>,
    /// Why the cycle exists and must be cut at an import.
    pub because: String,
}

/// One import a `cycle_import_cut` pin expects in the cut.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedImport {
    /// Full repo-relative path of the file holding the import.
    pub importer: String,
    /// Full repo-relative path of the file it loads.
    pub imported: String,
    /// The 1-based line one of its statements must start on, when pinned.
    #[serde(default)]
    pub line: Option<u32>,
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
