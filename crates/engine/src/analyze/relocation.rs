//! Relocation: the restartable candidate pipeline, its file facts, and the
//! pass-start policies that keep every move inside its package and namespace.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::cluster::{LevelCaps, Partition};
use strata_core::condense::Condensation;
use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights};
use strata_ir::{ContainerId, ContainerTree, Snapshot};

use crate::analyze::layout::LaminarHome;
use crate::config::{CapacityConfig, TestMirrorRule};
use crate::narrate::FileFacts;

#[cfg(test)]
mod tests;

mod assemble;
mod candidate;
pub(in crate::analyze) mod collision;
mod emit;
mod facts;
mod folder_projection;
mod identity_guard;
mod inventory;
mod members;
pub(in crate::analyze) mod mirror;
mod partition_keys;
pub(in crate::analyze) mod solver;
pub(in crate::analyze) mod symbol;
mod upper_levels;

#[cfg(test)]
use crate::analyze::rendering::render_tree;
pub(in crate::analyze) use identity_guard::RelocationIdentityGuard;
pub(in crate::analyze) use inventory::file_inventory;
pub(in crate::analyze) use solver::TestPolicy;
#[cfg(test)]
use strata_ir::{Container, ScopeLevel};

#[cfg(test)]
use facts::file_facts;
use facts::file_facts_repository_relative;
use inventory::cycle_homes;

/// Analyzes `snapshot` under `config`, returning the owned [`AnalyzeResult`].
///
/// The pass is pure: it reads only the snapshot and config and returns owned
/// data. It derives the current tree's violations (cycles, polarity breaches,
/// over-exports), scores the current layout, then runs the restructuring pipeline
/// to produce one [`ModeResult`] of up to `k` diverse candidates per requested
/// mode.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the snapshot's container tree
/// cannot be rendered (it is otherwise pre-validated at assembly).
pub(in crate::analyze) struct FileInfo {
    /// The file's container id in the current tree.
    pub(in crate::analyze) container: u32,
    /// The file's full repo-relative path (its container name).
    pub(in crate::analyze) name: SmolStr,
    /// Summed production SLOC of the symbols currently in the file.
    pub(in crate::analyze) production_sloc: u32,
    /// The file's folder/domain/package name keys, read from the laminar
    /// container tree so naming honors the manifest package roots and
    /// transparent source roots (`src`/`spec`) that tree already resolved,
    /// rather than re-electing from raw leading path segments.
    pub(in crate::analyze) home: LaminarHome,
    /// Opaque path prefix removed by the laminar render normalization.
    pub(in crate::analyze) namespace: SmolStr,
}

/// Derives the opaque path prefix removed from a file's rendered laminar home.
pub(in crate::analyze) const POLISH_SWEEPS: usize = 2;

/// Candidate destination folders examined per move unit during polish.
pub(in crate::analyze) const POLISH_TARGETS: usize = 4;

/// Bound on symbol-polish sweeps (FIX08): the same two-pass shape as the file
/// polish — a second pass catches relocations the first pass unlocked — with an
/// early stop once a sweep relocates nothing.
pub(in crate::analyze) const SYMBOL_SWEEPS: usize = 2;

/// Candidate destination FILES examined per symbol during the symbol polish.
pub(in crate::analyze) const SYMBOL_TARGETS: usize = 4;

/// Floor on symbol-polish acceptance (FIX08): an improvement smaller than this
/// is float dust, not signal. Accepting it would fabricate movement — the very
/// thing D-47 forbids — so the pass demands a real margin before relocating a
/// symbol. The file polish does not need this: its moves are whole files, whose
/// deltas dwarf any rounding error.
pub(in crate::analyze) const SYMBOL_MIN_IMPROVEMENT: f64 = 1e-12;

/// Coherence floor under which a folder's residual population is held to be
/// misdescribed by its own roof (FIX09): when fewer than half the files that
/// would remain under a real directory share a basename token with it, the
/// directory is the naming defect itself, and the synthesis dissolves it into
/// evidence-backed places instead of leaving a misleading label over bonded
/// company. Same majority semantics the eval harness's `name_alignment`
/// verdict applies, so synthesis and measurement agree on what "misnamed"
/// means.
pub(in crate::analyze) const ROOF_COHERENCE_FLOOR: f64 = 0.5;

/// The restartable solver that runs the candidate pipeline once per seed.
///
/// All of the seed-independent work — the weighted file-dependency graph, its
/// SCC condensation, and the real-directory folder partition — is computed once
/// at construction; each [`Solver::solve`] call starts from that real partition
/// (folders are reality, not a clustering product), polishes it under the full
/// objective, and assembles the five-level layout, so every seed yields a pure,
/// reproducible candidate. In anchored mode the pool additionally carries the
/// identity layout ("change nothing"), so a suggested restructuring can never
/// silently score worse than the current tree.
pub(in crate::analyze) struct PipelineSolver<'a> {
    /// The analyzed snapshot.
    snapshot: &'a Snapshot,
    /// Whether each file-graph vertex is a test-zone file under the `[tests]`
    /// policy — polarity detection plus configured patterns. Drives the shadow
    /// pass that follows subjects.
    test_zone: Vec<bool>,
    /// Whole file SCCs forbidden from independent movement by immutable policy.
    pinned_scc: Vec<bool>,
    /// Repo-relative file patterns that block symbol departures and arrivals.
    forbidden_symbol_files: Vec<glob::Pattern>,
    /// Immutable exact source/test templates compiled for this profile.
    mirror_rules: Vec<TestMirrorRule>,
    /// Whether accepted source moves attempt their test followers.
    mirror_enabled: bool,
    /// Whether declarations in detected test files are independently pinned.
    pin_test_symbols: bool,
    /// The current tree's file containers, ascending container id; vertex `i` of
    /// the file graph is `files[i]`.
    files: Vec<FileInfo>,
    /// File-container id to file-graph vertex.
    pub(in crate::analyze) index_of: BTreeMap<u32, u32>,
    /// The SCC condensation of the weighted hard-edge file graph.
    pub(in crate::analyze) condensation: Condensation,
    /// Pass-start file identities used to reject unrenderable folder joins.
    pub(in crate::analyze) relocation_identity: RelocationIdentityGuard,
    /// The condensation DAG with every edge reversed, for pull ranking.
    reverse_dag: Csr,
    /// The per-level member caps.
    caps: LevelCaps,
    /// The complete capacity profile used by scoring and symbol admission.
    capacity: CapacityConfig,
    /// The objective coefficients for this mode.
    coefficients: Coefficients,
    /// The configured edge-kind weights pricing the cut term.
    weights: KindWeights,
    /// Pass-start same-file multiplier for runtime-only edges.
    same_file_symbol: f64,
    /// Pass-start same-file multiplier for edges touching a type.
    same_file_type: f64,
    /// The identity partition (anchored mode on a cap-clean tree), else `None`.
    /// Cloned before relief, so it always mirrors the current tree exactly.
    pub(in crate::analyze) identity: Option<Partition>,
    /// Immutable SCC representatives, distinguishing actual moves from unchanged
    /// cycles whose members occupy several physical directories.
    pass_start_partition: Partition,
    /// Original folder keys for members of cross-directory file cycles.
    cycle_home_by_vertex: BTreeMap<u32, SmolStr>,
    /// Whether relief left the search's real-directory partition identical to
    /// the current tree — the mode-independent "nothing changed yet" shape. The
    /// faithful candidate exit (FIX05) keys on this so greenfield reports an
    /// unchanged layout through the same truthful render anchored uses, instead
    /// of re-assembling reality and pricing its own fabrication.
    real_is_identity: bool,
    /// The relieved real-directory folder partition: each file SCC starts in
    /// the cluster of its current parent folder, except that an over-capacity
    /// real folder's SCCs are pre-split along priced connectivity into
    /// evidence-backed child groups ([`relieve_over_capacity`]). Folders stay
    /// reality, so this is still the one folder-grain start every non-identity
    /// seed shares; only the identity entry bypasses it.
    pub(in crate::analyze) real_partition: Partition,
    /// Each relieved folder cluster's directory name, indexed by cluster id;
    /// split halves extend the table with `{folder}/{token}` labels that path-
    /// extend their base folder at render time.
    real_folder_names: Vec<SmolStr>,
    /// Whether each relieved folder cluster is the synthetic `workspace`
    /// bucket, indexed by cluster id in lockstep with `real_folder_names`.
    /// Carries the current tree's collapse marker onto the candidate folder it
    /// induces; split halves are always real places, so they carry `false`.
    real_folder_synthetic: Vec<bool>,
    /// The configured base seed; seed offset 0 selects the identity entry.
    base_seed: u64,
    /// The current root's name, reused for candidate package groups.
    root_name: SmolStr,
    /// The per-file facts narration consults when explaining moves.
    facts: FileFacts,
    /// FIX09 (naming-incoherence): the synthesized roof-rebuild start, present
    /// exactly when some real folder hosts a mixed population — bonded files
    /// plus a token-coherent group of zero-priced strangers — under a roof the
    /// evidence says it does not describe. Seed offset 1 starts from this
    /// partition instead of reality, giving the pool a genuinely different
    /// shape that the ordinary polish/score path then ratifies or rejects; no
    /// other seed or fixture is perturbed.
    roof_rebuild: Option<Partition>,
}

pub(in crate::analyze) struct CandidateTree {
    /// The candidate container tree: package groups over packages, domains, and
    /// folders derived per level, each folder holding whole current files.
    tree: ContainerTree,
    /// The file container each symbol node lands in, keyed by node id.
    placement: BTreeMap<u32, ContainerId>,
    /// Pass-start identity of every freshly interned candidate file. This keeps
    /// a whole file's folder relocation distinct from a declaration changing
    /// files inside that candidate tree.
    pass_start_file_by_candidate: BTreeMap<ContainerId, ContainerId>,
    /// Whether each candidate file sits inside the test zone (FIX11). Candidate
    /// file ids are fresh arena ids, so the file graph's vertex-parallel zone
    /// marks cannot be consulted directly at symbol grain — they ride here,
    /// populated where files are emitted, so every grain shares one boundary.
    zone_by_file: BTreeMap<ContainerId, bool>,
    /// Opaque pass-start render namespace of each emitted candidate file.
    namespace_by_file: BTreeMap<ContainerId, SmolStr>,
    /// Pass-start manifest package key of each emitted candidate file. Render
    /// namespaces are package-relative, so only this key keeps a declaration
    /// from being relocated into another package.
    package_by_file: BTreeMap<ContainerId, SmolStr>,
    /// The undecorated elected key of each upper container whose display name
    /// `qualify_elected` had to disambiguate, keyed by container id — empty when
    /// no sibling name collided. The render boundary strips a folder's increment
    /// against its domain's key from this map, never the decorated display
    /// label, so a disambiguated domain never re-embeds a folder's key as a
    /// fabricated directory chain.
    key_by_id: BTreeMap<u32, SmolStr>,
}

impl CandidateTree {
    fn shares_namespace(&self, first: ContainerId, second: ContainerId) -> bool {
        self.namespace_by_file.get(&first) == self.namespace_by_file.get(&second)
    }

    /// Returns `true` when both candidate files belong to the same manifest
    /// package; an unknown file shares no package.
    fn shares_package(&self, first: ContainerId, second: ContainerId) -> bool {
        self.package_by_file
            .get(&first)
            .is_some_and(|package| self.package_by_file.get(&second) == Some(package))
    }
}
