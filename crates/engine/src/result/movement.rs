//! Narrated moves: whole-file and symbol relocations with their reasons.

use serde::{Deserialize, Serialize};

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
    /// The follower would leave its pass-start manifest package (ADR-17).
    PackageBoundary,
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
