//! The `AnalyzeResult` DTO.
//!
//! This is the contract both faces of Strata share: the library returns an owned
//! [`AnalyzeResult`], and the CLI serializes the very same value as JSON for
//! `--format json`. Serialization is camelCase so the JSON matches the reference
//! `AnalyzeResult` interface byte-for-byte; every nested type carries the same
//! field names the reference documents.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strata_ir::ScopeLevel;

use crate::config::ProfileConfig;

/// The schema version stamped into every [`AnalyzeResult`] this crate produces.
///
/// Readers of a saved result reject any other version rather than misreading a
/// future shape.
pub const RESULT_SCHEMA_VERSION: u32 = 7;

/// The top-level analysis result: the snapshot hash, the current tree with its
/// violations, and the per-mode candidate sets.
///
/// `snapshot_hash` keys a deterministic cache — an identical snapshot and config
/// always produce an identical result. `modes` carries up to one [`ModeResult`]
/// per requested mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    /// The result schema version ([`RESULT_SCHEMA_VERSION`]); a saved result
    /// with any other version is rejected on read. Defaults to `0` (unsupported)
    /// when absent, so pre-versioned files fail with a clear message.
    #[serde(default)]
    pub schema_version: u32,
    /// The hex blake3 hash of the analyzed snapshot.
    pub snapshot_hash: String,
    /// A coarse census of the analyzed graph.
    pub summary: Summary,
    /// The shared current layout and findings identical across executed profiles.
    pub current: CurrentTree,
    /// The independently configured parameter-profile results.
    pub profiles: Profiles,
}

/// A coarse census of the analyzed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Total symbol and type nodes.
    pub symbols: u32,
    /// Total dependency edges.
    pub edges: u32,
    /// Total source files (file-level containers).
    pub files: u32,
    /// File counts keyed by language tag (`ts`, `rs`, `py`).
    pub files_by_language: BTreeMap<String, u32>,
}

/// The shared current layout and findings identical across executed profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTree {
    /// Today's layout as a nested container tree.
    pub tree: ContainerNode,
    /// Findings whose complete serialized content is identical in both profiles.
    pub shared_findings: Vec<Violation>,
}

/// One profile's scoring and findings for the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCurrent {
    /// The objective `J` of the current tree under this profile.
    pub score: f64,
    /// The per-term decomposition of `score`.
    pub score_breakdown: ScoreBreakdown,
    /// Findings applicable only to this profile after exact sharing.
    pub unique_findings: Vec<Violation>,
    /// Where the current layout stands relative to this profile's candidates.
    pub standing: CurrentStanding,
    /// How many of those violations are capacity findings that hard-breach
    /// their caps (`Severity::Violation` only). Borderline observations stay
    /// listed in `violations` but never count as breaks — the same predicate
    /// that gates `infeasible` standings. This is the authoritative count
    /// behind the "breaks N capacity findings" narration; faces must not
    /// re-tally it.
    pub capacity_breaks: u32,
}

/// Results for selected parameter profiles; an unselected profile is omitted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profiles {
    /// The anchored parameter-profile result, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchored: Option<ProfileResult>,
    /// The greenfield parameter-profile result, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub greenfield: Option<ProfileResult>,
}

/// One parameter profile's effective inputs, current state, and candidate set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileResult {
    /// Complete effective parameters used for this profile.
    pub parameters: ProfileConfig,
    /// This profile's current score, standing, and unique findings.
    pub current: ProfileCurrent,
    /// The returned candidates, best score first.
    pub candidates: Vec<Candidate>,
    /// The variation-of-information matrix over the candidates.
    pub pairwise_distance: Vec<Vec<f64>>,
    /// `true` when fewer than `k` candidates survived diversification.
    pub solution_space_converged: bool,
}

/// Compatibility alias for source consumers migrating from schema version 3.
pub type Modes = Profiles;

/// Compatibility alias for source consumers migrating from schema version 3.
pub type ModeResult = ProfileResult;

/// What a candidate leaves unresolved of the current capacity breaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityRemainder {
    /// Hard capacity findings remaining in the best candidate's tree.
    pub remaining: u32,
    /// How many of `remaining` are file-level breaches, which no move can fix —
    /// only conditional splits can.
    pub file_level: u32,
}

/// Where the current layout stands relative to a mode's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurrentStanding {
    /// No candidate beats the current layout; candidate 1 IS the current tree.
    Optimal,
    /// At least one candidate scores better than the current layout.
    Outscored,
    /// The current layout breaches a capacity cap, so it cannot compete;
    /// candidates fix the breach at whatever cut cost they carry.
    Infeasible,
}

/// One proposed restructure: its rank, score, tree, conditional splits, and the
/// narrated moves relative to the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The 1-based candidate index within its mode.
    pub index: u32,
    /// The total objective `J`; lower is better.
    pub score: f64,
    /// The per-term decomposition of `score`.
    pub score_breakdown: ScoreBreakdown,
    /// `current_score − score`; positive means the candidate beats current.
    pub improvement: f64,
    /// The proposed laminar structure.
    pub tree: ContainerNode,
    /// Over-cap SCCs with the break preconditions that make a split legal.
    pub conditional_splits: Vec<ConditionalSplit>,
    /// The explained moves versus the current layout.
    pub delta_narration: Vec<Move>,
    /// Scored per-symbol relocations accompanying the whole-file moves (FIX08).
    /// Absent when the candidate proposes none, so file-only candidates keep
    /// their exact prior shape on the wire (the FIX10 additive-field precedent;
    /// no schema bump).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_moves: Vec<SymbolMove>,
    /// The hard capacity findings left in this candidate's tree; present only
    /// when the mode's `current_standing` is `Infeasible`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_remainder: Option<CapacityRemainder>,
}

/// The per-term objective decomposition surfaced in the DTO.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBreakdown {
    /// The height-weighted cut cost.
    pub cut: f64,
    /// The sibling size-imbalance penalty.
    pub imbalance: f64,
    /// The (negated) naming-cohesion bonus.
    pub naming: f64,
    /// The (negated) path-cohesion bonus.
    pub path: f64,
    /// The move-distance anchoring penalty.
    pub anchor: f64,
    /// The scoped over-capacity binding penalty (FIX03).
    pub capacity: f64,
    /// The charge for moves into files containing dependencies but no consumers.
    pub dependency_only: f64,
    /// The charge for companion types separated from their immutable owners.
    pub companion_separation: f64,
}

impl From<strata_core::score::ScoreBreakdown> for ScoreBreakdown {
    fn from(value: strata_core::score::ScoreBreakdown) -> Self {
        Self {
            cut: value.cut,
            imbalance: value.imbalance,
            naming: value.naming,
            path: value.path,
            anchor: value.anchor,
            capacity: value.capacity,
            dependency_only: value.dependency_only,
            companion_separation: value.companion_separation,
        }
    }
}

/// A node of the rendered container tree.
///
/// Files carry their `symbols` and `production_sloc`; interior containers carry
/// their `children`. The two are mutually exclusive — a file has no children and
/// an interior node has no symbols — but both fields are optional so the shape
/// matches the reference `ContainerNode` exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerNode {
    /// The container's source-declared name.
    pub name: String,
    /// The level this container occupies.
    pub level: Level,
    /// Child containers; absent on files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<ContainerNode>>,
    /// The symbols placed in this file; present on files only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols: Option<Vec<SymbolPlacement>>,
    /// The production SLOC of this file; present on files only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_sloc: Option<u32>,
}

/// The camelCase scope level used in the DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    /// A single source file.
    File,
    /// A directory of files.
    Folder,
    /// A cohesive functional domain.
    Domain,
    /// A distributable package.
    Package,
    /// A group of related packages.
    PackageGroup,
}

impl From<ScopeLevel> for Level {
    fn from(value: ScopeLevel) -> Self {
        match value {
            ScopeLevel::File => Self::File,
            ScopeLevel::Folder => Self::Folder,
            ScopeLevel::Domain => Self::Domain,
            ScopeLevel::Package => Self::Package,
            ScopeLevel::PackageGroup => Self::PackageGroup,
        }
    }
}

/// A symbol placed in a file, with its declared visibility level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolPlacement {
    /// The symbol's source-declared name.
    pub name: String,
    /// The declared external visibility of the symbol.
    pub visibility: Level,
}

/// What kind of program entity a [`SymbolMove`] relocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SymbolKind {
    /// A value-level symbol (function, constant, variable).
    Symbol,
    /// A type-level entity (struct, enum, interface, alias).
    Type,
}

/// One symbol-grain relocation proposed alongside whole-file moves (FIX08).
///
/// Whole-file [`Move`]s relocate containers; a `SymbolMove` relocates a single
/// symbol BETWEEN two files that both survive the proposal. `delta` is the
/// objective improvement the relocation earned when the symbol polish accepted
/// it (always negative — acceptance requires strict J improvement), and
/// `broken_imports` counts the distinct other files whose imports would need
/// re-pointing once the move applies (see the engine's symbol narration).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolMove {
    /// The relocated symbol's source-declared name.
    pub symbol: String,
    /// Whether the entity is a value symbol or a type.
    pub kind: SymbolKind,
    /// Full repo-relative path of the file the symbol leaves.
    pub from_path: String,
    /// Full repo-relative path of the file the symbol lands in.
    pub to_path: String,
    /// Objective improvement contributed by this relocation (`ΔJ < 0`).
    pub delta: f64,
    /// Distinct files other than the origin housing a direct caller or callee
    /// that is NOT a future co-resident — the imports this move severs.
    pub broken_imports: u32,
}

/// The kind of a narrated change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveKind {
    /// Symbols relocate from one container to another.
    Move,
    /// One container's symbols divide across several.
    Split,
    /// Several containers' symbols combine into one.
    Merge,
}

/// One file's relocation within a narrated change: its path and the folded
/// source folder it leaves. The shared destination and reason live on the
/// parent [`Move`], so a merge's several sources are recorded per file here
/// rather than collapsed to a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMove {
    /// The moved file's full repo-relative path — its identity across trees.
    pub path: String,
    /// The folded source folder the file leaves.
    pub from: String,
}

/// One narrated change between the current tree and a candidate.
///
/// `files` are the per-file relocations (each carrying its own source folder);
/// `to` is the single destination folder they all land in; `reason` is the
/// dominant driver of the move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    /// Whether this is a move, split, or merge.
    pub kind: MoveKind,
    /// The per-file relocations, ordered by path.
    pub files: Vec<FileMove>,
    /// The destination folder path all the files land in.
    pub to: String,
    /// The dominant reason the change was proposed.
    pub reason: MoveReason,
    /// Test files that followed this primary source relocation.
    #[serde(default)]
    pub mirrors: Vec<MirrorMove>,
    /// Test files that could not follow without violating a hard constraint.
    #[serde(default)]
    pub blocked_mirrors: Vec<BlockedMirror>,
}

/// One test-file follower attached to its primary source move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorMove {
    /// Full pass-start path of the source whose move triggered this follower.
    pub source_path: String,
    /// Full pass-start path of the mirrored test.
    pub path: String,
    /// Folder the mirrored test leaves.
    pub from: String,
    /// Folder the mirrored test joins.
    pub to: String,
}

/// One best-effort test follower rejected by a hard constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedMirror {
    /// Full pass-start path of the source whose move triggered this follower.
    pub source_path: String,
    /// Full pass-start path of the mirrored test.
    pub path: String,
    /// Folder the mirrored test remains in.
    pub from: String,
    /// Folder the mirrored test would have joined.
    pub intended_to: String,
    /// Deterministic first hard constraint that rejected the follower.
    pub reason: BlockedMirrorReason,
}

/// Stable reason ordering for blocked mirror followers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockedMirrorReason {
    /// More than one immutable source/template pairing claimed the test.
    AmbiguousMapping,
    /// The follower would cross its pass-start render namespace.
    NamespaceBoundary,
    /// The destination would exceed its configured physical capacity.
    Capacity,
    /// Another file already occupies the derived destination path.
    PathCollision,
}

/// The dominant reason a narrated change was proposed, first match wins:
/// followed subject > cap relief > dependency pull > naming cohesion >
/// clustering fallback.
///
/// `subject` and `partner` carry full repo-relative file paths; the `Display`
/// impl basenames them, reproducing the prose the CLI faces print.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MoveReason {
    /// A spec file trails its subject under test projection.
    Follows {
        /// The full path of the subject the spec follows.
        subject: String,
    },
    /// The move relieves an over-cap source folder.
    RelievesOverCap {
        /// The folded path of the over-cap folder.
        container: String,
        /// How many files the folder held.
        count: u32,
        /// The folder member cap.
        cap: u32,
    },
    /// The move is pulled by its strongest dependency partner at the destination.
    PulledBy {
        /// The full path of the pulling resident file.
        partner: String,
        /// The summed two-way edge weight of the pull.
        weight: f64,
    },
    /// The moved files share naming tokens with the destination's residents.
    NamingCohesion {
        /// The mean pairwise basename-token Jaccard similarity.
        cohesion: f64,
    },
    /// No stronger signal applied; the clustering regrouped the files.
    Clustering,
}

impl std::fmt::Display for MoveReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Follows { subject } => write!(f, "follows {}", crate::narrate::basename(subject)),
            Self::RelievesOverCap {
                container,
                count,
                cap,
            } => write!(
                f,
                "relieves over-cap folder {container} ({count}/{cap} entries)"
            ),
            Self::PulledBy { partner, weight } => write!(
                f,
                "pulled by {} (w {weight:.1})",
                crate::narrate::basename(partner)
            ),
            Self::NamingCohesion { cohesion } => {
                write!(f, "naming cohesion {cohesion:.2} with destination")
            }
            Self::Clustering => write!(f, "regrouped by clustering"),
        }
    }
}

/// A structural violation of the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    /// The violation class.
    pub kind: ViolationKind,
    /// Whether the finding is a hard violation or a borderline observation.
    pub severity: Severity,
    /// The container path(s) involved.
    pub location: Vec<String>,
    /// A human-readable description of the violation.
    pub detail: String,
    /// Edge-break suggestions; present for cycle violations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_suggestions: Option<Vec<EdgeBreak>>,
    /// The measured size against the breached cap; present for capacity
    /// violations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<CapacityBreach>,
}

/// The structured facts of a capacity violation: what was measured against
/// which cap, and — for file-level breaches — the full path of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityBreach {
    /// The measured size (SLOC for files, member count above).
    pub measured: u32,
    /// The configured cap the measure breached.
    pub cap: u32,
    /// The full repo-relative path; present for file-level breaches only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The class of a structural violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViolationKind {
    /// A dependency cycle.
    Cycle,
    /// A polarity-matrix breach (production depending on test code).
    Polarity,
    /// A capacity cap exceeded.
    Capacity,
    /// A declared visibility wider than the derived scope.
    Visibility,
}

/// The severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// A hard violation.
    Violation,
    /// A borderline observation within the tolerance band; never gates CI.
    Borderline,
}

/// An over-cap SCC and the edge breaks that make a split legal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionalSplit {
    /// The member symbol names of the SCC.
    pub scc: Vec<String>,
    /// The edges to break before the split becomes legal.
    pub preconditions: Vec<EdgeBreak>,
    /// The number of files the split yields once the preconditions are met.
    pub resulting_files: u32,
}

/// One edge a cycle break or split precondition cuts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeBreak {
    /// The source symbol name.
    pub source: String,
    /// The target symbol name.
    pub target: String,
    /// The cut weight of the edge.
    pub weight: f64,
    /// `true` when the break is ILP-proven minimal, `false` for a heuristic.
    pub exact: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_serialize_schema_version_seven_without_modes() {
        let result = AnalyzeResult {
            schema_version: RESULT_SCHEMA_VERSION,
            snapshot_hash: "snapshot".to_owned(),
            summary: Summary {
                symbols: 0,
                edges: 0,
                files: 0,
                files_by_language: BTreeMap::new(),
            },
            current: CurrentTree {
                tree: ContainerNode {
                    name: "workspace".to_owned(),
                    level: Level::PackageGroup,
                    children: Some(Vec::new()),
                    symbols: None,
                    production_sloc: None,
                },
                shared_findings: Vec::new(),
            },
            profiles: Profiles::default(),
        };

        let json = serde_json::to_value(result).unwrap_or_default();

        assert_eq!(
            json.pointer("/schemaVersion")
                .and_then(serde_json::Value::as_u64),
            Some(7)
        );
        assert!(json.get("profiles").is_some());
        assert!(json.get("modes").is_none());
        assert!(json.pointer("/current/sharedFindings").is_some());
    }

    #[test]
    fn should_serialize_a_breakdown_as_camel_case() {
        let breakdown = ScoreBreakdown {
            cut: 1.0,
            imbalance: 0.0,
            naming: -0.5,
            path: 0.0,
            anchor: 0.25,
            capacity: 0.75,
            dependency_only: 0.05,
            companion_separation: 0.05,
        };

        let json = serde_json::to_string(&breakdown).unwrap_or_default();

        assert!(json.contains("\"cut\":1.0"));
        assert!(json.contains("\"anchor\":0.25"));
        assert!(json.contains("\"capacity\":0.75"));
        assert!(json.contains("\"dependencyOnly\":0.05"));
        assert!(json.contains("\"companionSeparation\":0.05"));
    }

    #[test]
    fn should_serialize_package_group_level_as_camel_case() {
        let json = serde_json::to_string(&Level::PackageGroup).unwrap_or_default();

        assert_eq!(json, "\"packageGroup\"");
    }

    #[test]
    fn should_map_every_scope_level_to_its_dto_level() {
        assert_eq!(Level::from(ScopeLevel::File), Level::File);
        assert_eq!(Level::from(ScopeLevel::PackageGroup), Level::PackageGroup);
    }

    #[test]
    fn should_omit_modes_that_were_not_requested() {
        let modes = Modes {
            anchored: None,
            greenfield: None,
        };

        let json = serde_json::to_string(&modes).unwrap_or_default();

        assert_eq!(json, "{}");
    }

    #[test]
    fn should_serialize_current_standing_as_camel_case() {
        let json = serde_json::to_string(&CurrentStanding::Outscored).unwrap_or_default();

        assert_eq!(json, "\"outscored\"");
    }

    #[test]
    fn should_serialize_a_capacity_breach_as_camel_case() {
        let breach = CapacityBreach {
            measured: 412,
            cap: 300,
            path: Some("src/core/huge.ts".to_owned()),
        };

        let json = serde_json::to_string(&breach).unwrap_or_default();

        assert_eq!(
            json,
            "{\"measured\":412,\"cap\":300,\"path\":\"src/core/huge.ts\"}"
        );
        let folder = CapacityBreach {
            measured: 20,
            cap: 15,
            path: None,
        };
        assert_eq!(
            serde_json::to_string(&folder).unwrap_or_default(),
            "{\"measured\":20,\"cap\":15}"
        );
    }

    #[test]
    fn should_serialize_a_move_reason_with_its_kind_tag() {
        let reason = MoveReason::Follows {
            subject: "src/core/app.ts".to_owned(),
        };

        let json = serde_json::to_string(&reason).unwrap_or_default();

        assert_eq!(
            json,
            "{\"kind\":\"follows\",\"subject\":\"src/core/app.ts\"}"
        );
        assert_eq!(
            serde_json::to_string(&MoveReason::Clustering).unwrap_or_default(),
            "{\"kind\":\"clustering\"}"
        );
        assert_eq!(
            serde_json::to_string(&MoveReason::RelievesOverCap {
                container: "app/src/core".to_owned(),
                count: 4,
                cap: 3,
            })
            .unwrap_or_default(),
            "{\"kind\":\"relievesOverCap\",\"container\":\"app/src/core\",\"count\":4,\"cap\":3}"
        );
    }

    #[test]
    fn should_display_every_reason_as_its_prose() {
        let cases = vec![
            (
                MoveReason::Follows {
                    subject: "src/core/app.ts".to_owned(),
                },
                "follows app.ts",
            ),
            (
                MoveReason::RelievesOverCap {
                    container: "app/src/core".to_owned(),
                    count: 4,
                    cap: 3,
                },
                "relieves over-cap folder app/src/core (4/3 entries)",
            ),
            (
                MoveReason::PulledBy {
                    partner: "src/core/engine.ts".to_owned(),
                    weight: 2.5,
                },
                "pulled by engine.ts (w 2.5)",
            ),
            (
                MoveReason::NamingCohesion {
                    cohesion: 2.0 / 3.0,
                },
                "naming cohesion 0.67 with destination",
            ),
            (MoveReason::Clustering, "regrouped by clustering"),
        ];

        for (reason, prose) in cases {
            assert_eq!(reason.to_string(), prose);
        }
    }
}
