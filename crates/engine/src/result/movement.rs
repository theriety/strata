//! Narrated moves: whole-file and symbol relocations with their reasons.

use serde::{Deserialize, Serialize};

use super::candidate::ScoreBreakdown;
use super::finding::CapacityUnit;
use super::group_keyed::GroupKeyed;

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
/// objective change the relocation brought when the symbol polish accepted it
/// (negative for an ordinary move; a collision-fold member may be positive,
/// because only the fold's sum must improve — see `fold`), `terms`
/// splits that change across the objective's terms, and `broken_imports`
/// counts the distinct other files whose imports would need re-pointing once
/// the move applies (see the engine's symbol narration).
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
    /// Objective change contributed by this relocation: `ΔJ < 0` for an
    /// ordinary move. A collision-fold member is priced in acceptance order
    /// and only the fold's sum must improve, so its own `ΔJ` may be positive.
    pub delta: f64,
    /// Distinct files other than the origin housing a direct caller or callee
    /// that is NOT a future co-resident — the imports this move severs.
    pub broken_imports: u32,
    /// The objective's per-term breakdown either side of the acceptance
    /// (schema 12). Absent on a result saved at schema 11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<TermShift>,
    /// Repo-relative path of the file whose collision fold (ADR-21) this move
    /// belongs to (schema 12). A fold is accepted as one unit, so a member's
    /// `delta` and `terms` may individually regress. Absent for an ordinary move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold: Option<String>,
    /// Human-facing name when it differs from `symbol` (a method reads
    /// `Type::method`). Display only: matching keys on `symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl SymbolMove {
    /// The name a human reads: `display` when present, else `symbol`.
    #[must_use]
    pub fn label(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.symbol)
    }
}

/// The objective's per-term breakdown either side of one accepted symbol
/// relocation (FIX08 Option A).
///
/// `before` is the symbol pass's running best when the move was priced, so the
/// shift is path-dependent: it reflects the moves accepted earlier, never a
/// leave-one-out counterfactual. Within a collision fold (see
/// [`SymbolMove::fold`]) a member's shift may regress; only the fold's sum
/// must improve. Every term of [`TermShift::deltas`] sums to
/// the move's `delta`, because both breakdowns' terms sum to their `J`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TermShift {
    /// The breakdown the relocation was priced against.
    pub before: ScoreBreakdown,
    /// The breakdown once the relocation was accepted.
    pub after: ScoreBreakdown,
}

impl TermShift {
    /// The per-term change `after − before`.
    #[must_use]
    pub fn deltas(&self) -> ScoreBreakdown {
        let (before, after) = (&self.before, &self.after);
        ScoreBreakdown {
            cut: after.cut - before.cut,
            imbalance: after.imbalance - before.imbalance,
            naming: after.naming - before.naming,
            path: after.path - before.path,
            anchor: after.anchor - before.anchor,
            capacity: after.capacity - before.capacity,
            dependency_only: after.dependency_only - before.dependency_only,
            companion_separation: after.companion_separation - before.companion_separation,
            entry_inflow: after.entry_inflow - before.entry_inflow,
            unit_outflow: after.unit_outflow - before.unit_outflow,
        }
    }
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

/// The strongest tie between the moved files and one file that stays in the
/// source folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftBehind {
    /// The full path of the file left behind.
    pub partner: String,
    /// The summed two-way edge weight between it and the moved files.
    pub weight: f64,
}

/// The dominant reason a narrated change was proposed, first match wins:
/// followed subject > cap relief > dependency pull > naming cohesion >
/// clustering fallback.
///
/// `subject` and `partner` carry full repo-relative file paths; the `Display`
/// impl basenames them, reproducing the prose the CLI faces print.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
        /// What `count` counts.
        unit: CapacityUnit,
        /// The configuration key of the breached cap, e.g. `capacity.folder`.
        config_key: String,
    },
    /// The move is pulled by its strongest dependency partner at the destination.
    PulledBy {
        /// The full path of the pulling resident file.
        partner: String,
        /// The summed two-way edge weight of the pull.
        weight: f64,
        /// The strongest pull the move leaves behind in the source folder,
        /// measured by the same rule; absent when nothing there ties to the
        /// moved files. Results written before it existed still load.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        left_behind: Option<LeftBehind>,
    },
    /// The moved files share naming tokens with the destination's residents.
    NamingCohesion {
        /// The mean pairwise basename-token Jaccard similarity.
        cohesion: f64,
    },
    /// No stronger signal applied; the clustering regrouped the files.
    Clustering,
}

impl FileMove {
    /// The group-keyed paths: the file and the folder it leaves.
    pub(crate) fn group_keyed(&mut self) -> [GroupKeyed<'_>; 2] {
        [
            GroupKeyed::new(&mut self.path),
            GroupKeyed::new(&mut self.from),
        ]
    }
}

impl MirrorMove {
    /// The group-keyed paths: the source, the test, and its two folders.
    pub(crate) fn group_keyed(&mut self) -> [GroupKeyed<'_>; 4] {
        [
            GroupKeyed::new(&mut self.source_path),
            GroupKeyed::new(&mut self.path),
            GroupKeyed::new(&mut self.from),
            GroupKeyed::new(&mut self.to),
        ]
    }
}

impl MoveReason {
    /// The group-keyed path the reason names, when it names one.
    pub(crate) fn group_keyed(&mut self) -> Vec<GroupKeyed<'_>> {
        match self {
            Self::Follows { subject } => vec![GroupKeyed::new(subject)],
            Self::PulledBy {
                partner,
                left_behind,
                ..
            } => {
                let mut keyed = vec![GroupKeyed::new(partner)];
                keyed.extend(
                    left_behind
                        .iter_mut()
                        .map(|l| GroupKeyed::new(&mut l.partner)),
                );
                keyed
            }
            Self::RelievesOverCap { container, .. } => vec![GroupKeyed::new(container)],
            Self::NamingCohesion { .. } | Self::Clustering => Vec::new(),
        }
    }
}

impl Move {
    /// Every group-keyed path of the move: its files, destination, reason and
    /// mirrored tests.
    pub(crate) fn group_keyed(&mut self) -> Vec<GroupKeyed<'_>> {
        let Self {
            files,
            to,
            reason,
            mirrors,
            ..
        } = self;
        let mut keyed: Vec<GroupKeyed<'_>> =
            files.iter_mut().flat_map(FileMove::group_keyed).collect();
        keyed.push(GroupKeyed::new(to));
        keyed.extend(reason.group_keyed());
        keyed.extend(mirrors.iter_mut().flat_map(MirrorMove::group_keyed));
        keyed
    }
}

impl std::fmt::Display for MoveReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Follows { subject } => write!(f, "follows {}", crate::narrate::basename(subject)),
            Self::RelievesOverCap {
                container,
                count,
                cap,
                unit,
                config_key,
            } => write!(
                f,
                "relieves {container}, which has {}",
                unit.against_cap(*count, *cap, config_key),
            ),
            Self::PulledBy {
                partner,
                weight,
                left_behind,
            } => {
                write!(
                    f,
                    "pulled by {} (w {weight:.1}",
                    crate::narrate::basename(partner)
                )?;
                if let Some(left) = left_behind {
                    write!(
                        f,
                        ", vs {:.1} to {} left behind",
                        left.weight,
                        crate::narrate::basename(&left.partner)
                    )?;
                }
                write!(f, ")")
            }
            Self::NamingCohesion { cohesion } => {
                write!(f, "naming cohesion {cohesion:.2} with destination")
            }
            Self::Clustering => write!(f, "regrouped by clustering"),
        }
    }
}
