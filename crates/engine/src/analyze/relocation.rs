use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::seed::SeedLevel;
use strata_core::cluster::{ClusterId, LevelCaps, Partition};
use strata_core::condense::Condensation;
use strata_core::diversify::SolvedCandidate;
use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights, score};
use strata_ir::{
    Container, ContainerId, ContainerTree, IntermediateRepresentation, Node, Polarity, ScopeLevel,
    Snapshot,
};

use crate::analyze::layout::{
    ContainerArena, LaminarHome, NameTally, anchor_min, cluster_level, elect, home_affinity,
    laminar_home, plurality, qualify_elected, qualify_folder_names, record_undecorated_key,
    render_namespace, vote,
};
use crate::analyze::rendering::render_tree;
use crate::analyze::scoring::{
    ContainerSpec, move_distance, score_candidate, score_current_with_affinity,
};
use crate::config::{CapacityConfig, TestMirrorRule, TestsConfig};
use crate::error::StrataError;
use crate::narrate::{
    FileFacts, narrate_repository_relative, project_package_rooted_path, project_physical_path,
};
use crate::result::{Candidate, ConditionalSplit, ScoreBreakdown};

#[cfg(test)]
mod tests;

pub(in crate::analyze) mod mirror;
pub(in crate::analyze) mod solver;
pub(in crate::analyze) mod symbol;

pub(in crate::analyze) use mirror::MirrorEvidence;

/// The compiled `[tests]` policy deciding which files count as tests for the
/// clustering tie-cut and the subject-following shadow pass.
///
/// Built-in detection stays polarity-driven — the adapters already mark
/// symbols from `.spec.`/`.test.` paths, `tests/` directories, and language
/// test attributes. Patterns extend that with glob matching against a file's
/// project-relative place (its container chain joined with `/`, ending in the
/// file name), so `*.spec.*` applies repo-wide while `spec/mocks/**` stays
/// scoped.
#[derive(Debug, Clone)]
pub(in crate::analyze) struct TestPolicy {
    /// Whether the built-in per-language detection participates.
    builtins: bool,
    /// Compiled patterns containing `/`: matched against the full path.
    path_patterns: Vec<glob::Pattern>,
    /// Compiled bare patterns: matched against the file name alone.
    base_patterns: Vec<glob::Pattern>,
}

impl TestPolicy {
    /// Compiles the configured policy, attributing a failed pattern at its
    /// `tests.patterns[i]` key.
    ///
    /// [`AnalyzeConfig::validate`] compiles every pattern once during loading;
    /// this second compilation covers embedders who build an
    /// [`AnalyzeConfig`] directly and never validate it.
    pub(in crate::analyze) fn new(config: &TestsConfig) -> Result<Self, StrataError> {
        let mut policy = Self {
            builtins: config.builtins,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        };
        for (index, pattern) in config.patterns.iter().enumerate() {
            let compiled =
                glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                    key: Some(format!("tests.patterns[{index}]")),
                    reason: error.to_string(),
                })?;
            if pattern.contains('/') {
                policy.path_patterns.push(compiled);
            } else {
                policy.base_patterns.push(compiled);
            }
        }
        Ok(policy)
    }

    /// The shipped default: built-in detection on, no extra globs.
    #[cfg(test)]
    pub(in crate::analyze) fn defaults() -> Self {
        Self {
            builtins: true,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        }
    }

    /// The inert policy: configuration alone marks nothing as a test.
    #[cfg(test)]
    pub(in crate::analyze) fn disabled() -> Self {
        Self {
            builtins: false,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        }
    }

    /// Whether `path` matches any configured pattern; bare patterns face the
    /// final segment alone so `*.spec.*` needs no directory knowledge.
    fn matches(&self, path: &str) -> bool {
        let basename = path.rsplit('/').next().unwrap_or(path);
        self.base_patterns
            .iter()
            .any(|pattern| pattern.matches(basename))
            || self
                .path_patterns
                .iter()
                .any(|pattern| pattern.matches(path))
    }

    /// Built-in path conventions also classify support files whose declarations
    /// remain production-polarity. Directory segments are exact so ordinary
    /// names such as `contest` do not become test roots accidentally.
    fn matches_builtin_path(path: &str) -> bool {
        let mut segments = path.split('/');
        let basename = path.rsplit('/').next().unwrap_or(path);
        segments.any(|segment| matches!(segment, "spec" | "test" | "tests"))
            || basename
                .split('.')
                .any(|segment| matches!(segment, "spec" | "test"))
    }
}

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

/// Keeps physical homes separate from the dominant representative used to
/// search a cross-directory SCC as one atomic unit.
fn cycle_homes(files: &[FileInfo], condensation: &Condensation) -> BTreeMap<u32, SmolStr> {
    let homes: BTreeSet<LaminarHome> = files.iter().map(|file| file.home.clone()).collect();
    let names = qualify_folder_names(&homes);
    let name_by_home: BTreeMap<_, _> = homes.into_iter().zip(names).collect();
    let mut retained = BTreeMap::new();
    for members in &condensation.members {
        let member_homes: BTreeSet<_> = members
            .iter()
            .filter_map(|member| files.get(member.0 as usize).map(|file| &file.home))
            .collect();
        if member_homes.len() > 1 {
            for member in members {
                if let Some(file) = files.get(member.0 as usize)
                    && let Some(name) = name_by_home.get(&file.home)
                {
                    retained.insert(member.0, name.clone());
                }
            }
        }
    }
    retained
}

/// Preserves pass-start render namespaces and namespace-scoped leaf identity.
pub(in crate::analyze) struct RelocationIdentityGuard {
    identities_by_scc: Vec<Vec<RenderIdentity>>,
    allowed_namespaces: Vec<BTreeSet<SmolStr>>,
}

pub(in crate::analyze) type RenderIdentity = (SmolStr, SmolStr);
pub(in crate::analyze) type IdentitySubgroup = (Vec<u32>, BTreeSet<RenderIdentity>);

impl RelocationIdentityGuard {
    pub(in crate::analyze) fn new(
        files: &[FileInfo],
        condensation: &Condensation,
        identity: &Partition,
        relieved: &Partition,
        roof_rebuild: Option<&Partition>,
    ) -> Self {
        let identities_by_scc: Vec<Vec<RenderIdentity>> = condensation
            .members
            .iter()
            .map(|members| {
                members
                    .iter()
                    .filter_map(|member| files.get(member.0 as usize))
                    .map(|file| {
                        (
                            file.namespace.clone(),
                            SmolStr::new(
                                file.name.rsplit('/').next().unwrap_or(file.name.as_str()),
                            ),
                        )
                    })
                    .collect()
            })
            .collect();
        let mut allowed_namespaces = Self::cluster_namespaces(identity, &identities_by_scc);
        Self::append_fresh_cluster_namespaces(
            &mut allowed_namespaces,
            relieved,
            &identities_by_scc,
        );
        if let Some(rebuilt) = roof_rebuild {
            Self::append_fresh_cluster_namespaces(
                &mut allowed_namespaces,
                rebuilt,
                &identities_by_scc,
            );
        }
        Self {
            identities_by_scc,
            allowed_namespaces,
        }
    }

    fn permits_join(&self, parts: &Partition, moving: u32, target: ClusterId) -> bool {
        let Some(moving_identities) = self.identities_by_scc.get(moving as usize) else {
            return false;
        };
        let Some(allowed) = self.allowed_namespaces.get(target.0 as usize) else {
            return false;
        };
        if moving_identities
            .iter()
            .any(|(namespace, _)| !allowed.contains(namespace))
        {
            return false;
        }
        let mut occupied = BTreeSet::new();
        for (scc, identities) in self.identities_by_scc.iter().enumerate() {
            let scc = u32::try_from(scc).unwrap_or(u32::MAX);
            if scc == moving || parts.cluster_of(scc) != Some(target) {
                continue;
            }
            occupied.extend(identities.iter().cloned());
        }
        let unique: BTreeSet<&(SmolStr, SmolStr)> = moving_identities.iter().collect();
        unique.len() == moving_identities.len()
            && moving_identities
                .iter()
                .all(|identity| !occupied.contains(identity))
    }

    /// Permits a pinned test follower to join a production-owned logical
    /// cluster while retaining its own render namespace. Exact mirror rules
    /// establish that cross-namespace relationship; leaf collisions remain a
    /// hard veto within the projected namespace.
    fn permits_shadow_join(&self, parts: &Partition, moving: u32, target: ClusterId) -> bool {
        let Some(moving_identities) = self.identities_by_scc.get(moving as usize) else {
            return false;
        };
        let mut occupied = BTreeSet::new();
        for (scc, identities) in self.identities_by_scc.iter().enumerate() {
            let scc = u32::try_from(scc).unwrap_or(u32::MAX);
            if scc == moving || parts.cluster_of(scc) != Some(target) {
                continue;
            }
            occupied.extend(identities.iter().cloned());
        }
        let unique: BTreeSet<&RenderIdentity> = moving_identities.iter().collect();
        unique.len() == moving_identities.len()
            && moving_identities
                .iter()
                .all(|identity| !occupied.contains(identity))
    }

    #[cfg(test)]
    pub(in crate::analyze) fn accepts(&self, parts: &Partition) -> bool {
        self.accepts_with_mirrors(parts, &MirrorEvidence::default())
    }

    pub(in crate::analyze) fn accepts_with_mirrors(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
    ) -> bool {
        let applied = evidence.applied_sccs();
        let mut occupied: BTreeSet<(ClusterId, &SmolStr, &SmolStr)> = BTreeSet::new();
        self.identities_by_scc
            .iter()
            .enumerate()
            .all(|(scc, identities)| {
                let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                    return false;
                };
                let Some(allowed) = self.allowed_namespaces.get(cluster.0 as usize) else {
                    return false;
                };
                identities.iter().all(|(namespace, leaf)| {
                    (allowed.contains(namespace)
                        || applied.contains(&u32::try_from(scc).unwrap_or(u32::MAX)))
                        && occupied.insert((cluster, namespace, leaf))
                })
            })
    }

    pub(in crate::analyze) fn collision_free_subgroups(&self, group: &[u32]) -> Vec<Vec<u32>> {
        let mut subgroups: Vec<IdentitySubgroup> = Vec::new();
        for &scc in group {
            let Some(identities) = self.identities_by_scc.get(scc as usize) else {
                continue;
            };
            let unique: BTreeSet<RenderIdentity> = identities.iter().cloned().collect();
            if unique.len() != identities.len() {
                continue;
            }
            if let Some((members, occupied)) = subgroups
                .iter_mut()
                .find(|(_, occupied)| occupied.is_disjoint(&unique))
            {
                members.push(scc);
                occupied.extend(unique);
            } else {
                subgroups.push((vec![scc], unique));
            }
        }
        subgroups.into_iter().map(|(members, _)| members).collect()
    }

    fn cluster_namespaces(
        parts: &Partition,
        identities_by_scc: &[Vec<RenderIdentity>],
    ) -> Vec<BTreeSet<SmolStr>> {
        let mut namespaces = vec![BTreeSet::new(); parts.cluster_count()];
        Self::merge_cluster_namespaces(&mut namespaces, parts, identities_by_scc);
        namespaces
    }

    fn append_fresh_cluster_namespaces(
        namespaces: &mut Vec<BTreeSet<SmolStr>>,
        parts: &Partition,
        identities_by_scc: &[Vec<RenderIdentity>],
    ) {
        let first_fresh = namespaces.len();
        namespaces.resize_with(parts.cluster_count(), BTreeSet::new);
        for (scc, identities) in identities_by_scc.iter().enumerate() {
            let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                continue;
            };
            if cluster.0 as usize >= first_fresh
                && let Some(allowed) = namespaces.get_mut(cluster.0 as usize)
            {
                allowed.extend(identities.iter().map(|(namespace, _)| namespace.clone()));
            }
        }
    }

    fn merge_cluster_namespaces(
        namespaces: &mut [BTreeSet<SmolStr>],
        parts: &Partition,
        identities_by_scc: &[Vec<RenderIdentity>],
    ) {
        for (scc, identities) in identities_by_scc.iter().enumerate() {
            let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                continue;
            };
            let Some(allowed) = namespaces.get_mut(cluster.0 as usize) else {
                continue;
            };
            allowed.extend(identities.iter().map(|(namespace, _)| namespace.clone()));
        }
    }
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

/// Inventories the current tree's file containers in file-graph vertex order:
/// every file with its laminar home keys, ascending container id, paired with
/// the container-to-vertex index. Production SLOC is folded into each entry so
/// relief and narration can weigh files without re-walking the IR nodes.
pub(in crate::analyze) fn file_inventory(
    ir: &IntermediateRepresentation,
) -> (Vec<FileInfo>, BTreeMap<u32, u32>) {
    // index the laminar containers by id so each file can read its already-
    // resolved folder/domain/package name keys off its ancestor chain.
    let by_id: BTreeMap<u32, &Container> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut files: Vec<FileInfo> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| {
            let home = laminar_home(&by_id, container.id.0);
            FileInfo {
                container: container.id.0,
                name: container.name.clone(),
                production_sloc: 0,
                namespace: render_namespace(&container.name, &home),
                home,
            }
        })
        .collect();
    files.sort_by_key(|file| file.container);
    let index_of: BTreeMap<u32, u32> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.container, u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        let Some(&index) = index_of.get(&node.container.0) else {
            continue;
        };
        if let Some(file) = files.get_mut(index as usize) {
            file.production_sloc = file.production_sloc.saturating_add(node.effective_size);
        }
    }
    (files, index_of)
}

impl PipelineSolver<'_> {
    /// Attaches solver-owned follower outcomes to their primary source moves.
    fn finish(
        &self,
        parts: Partition,
        total: f64,
        evidence: MirrorEvidence,
    ) -> SolvedCandidate<MirrorEvidence> {
        if let Some(identity) = &self.identity
            && identity == &parts
        {
            return self.identity_entry(identity);
        }
        SolvedCandidate {
            partition: parts,
            score: total,
            evidence,
        }
    }

    /// The identity pool entry: the "change nothing" layout scored on the actual
    /// current tree, so the anchored pool always contains the current score and
    /// a suggested candidate can never silently lose to it.
    fn identity_entry(&self, identity: &Partition) -> SolvedCandidate<MirrorEvidence> {
        SolvedCandidate {
            partition: identity.clone(),
            score: score_current_with_affinity(
                self.snapshot,
                &self.coefficients,
                &self.weights,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            )
            .total,
            evidence: MirrorEvidence::default(),
        }
    }

    /// Groups the file-graph vertices by the folder cluster their SCC lands in,
    /// members sorted by file path for deterministic emission.
    fn folder_members(&self, parts: &Partition) -> BTreeMap<u32, Vec<u32>> {
        let mut members_of: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for vertex in 0..self.files.len() {
            let Some(scc) = self.condensation.membership.get(vertex) else {
                continue;
            };
            let Some(cluster) = parts.cluster_of(scc.0) else {
                debug_assert!(false, "the folder partition must cover every file scc");
                continue;
            };
            members_of
                .entry(cluster.0)
                .or_default()
                .push(u32::try_from(vertex).unwrap_or(u32::MAX));
        }
        for members in members_of.values_mut() {
            members.sort_by(|&left, &right| {
                let left_name = self
                    .files
                    .get(left as usize)
                    .map_or("", |file| file.name.as_str());
                let right_name = self
                    .files
                    .get(right as usize)
                    .map_or("", |file| file.name.as_str());
                left_name.cmp(right_name).then(left.cmp(&right))
            });
        }
        members_of
    }

    /// Dominant laminar home key (via `key`, weighted by production SLOC) per
    /// base vertex of an upper clustering level.
    ///
    /// `cluster_of` maps a folder cluster to the base vertex it contributes to at
    /// this level — the identity for the domain level (each folder is a vertex),
    /// the domain partition for the package level (each domain is a vertex). An
    /// absent (empty) vertex falls back to `workspace`. The result seeds and gains
    /// [`cluster_level`] so containers group by home directory.
    fn home_keys(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        vertex_count: usize,
        cluster_of: impl Fn(u32) -> u32,
        key: impl Fn(&FileInfo) -> &SmolStr,
    ) -> Vec<SmolStr> {
        let mut tally: NameTally = BTreeMap::new();
        for (&folder, members) in members_of {
            let cluster = cluster_of(folder);
            for &vertex in members {
                if let Some(file) = self.files.get(vertex as usize) {
                    vote(&mut tally, cluster, key(file).clone(), file.production_sloc);
                }
            }
        }
        (0..vertex_count)
            .map(|vertex| {
                tally
                    .get(&u32::try_from(vertex).unwrap_or(u32::MAX))
                    .map_or_else(|| SmolStr::new("workspace"), plurality)
            })
            .collect()
    }

    /// Assembles the five-level candidate tree a folder partition induces.
    ///
    /// Folders are the partition's non-empty clusters and keep their real
    /// directory names ([`real_dir_partition`]) — folders are reality, so no
    /// election happens at that level. The upper levels come from clustering
    /// each level's weighted quotient in turn (folders → domains → packages →
    /// package groups) and are named from the files they transitively hold —
    /// each file votes its laminar domain and package name keys
    /// (source-root-transparent, package-root-resolved), weighted by production
    /// SLOC then file count, and [`elect`] names the cluster through its
    /// never-mixed, never-numeric ladder — strict-majority home, shared home
    /// prefix, top-two join, dominant token, then an anchored non-numeric last
    /// resort; the group takes the current root's name — and file leaves keep
    /// their full current paths so file identity stays stable across trees.
    fn assemble(&self, parts: &Partition) -> CandidateTree {
        self.assemble_with_mirror_evidence(parts, &MirrorEvidence::default())
    }

    fn assemble_with_mirror_evidence(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
    ) -> CandidateTree {
        if self.files.is_empty() {
            let root = Container {
                id: ContainerId(0),
                name: self.root_name.clone(),
                level: ScopeLevel::PackageGroup,
                parent: None,
                synthetic: false,
            };
            return CandidateTree {
                tree: ContainerTree::new(vec![root]),
                placement: BTreeMap::new(),
                pass_start_file_by_candidate: BTreeMap::new(),
                zone_by_file: BTreeMap::new(),
                namespace_by_file: BTreeMap::new(),
                key_by_id: BTreeMap::new(),
            };
        }

        let members_of = self.folder_members(parts);

        // one clustering pass per upper level, each over the previous level's
        // weighted quotient graph. Each pass carries a home-directory seed
        // affinity keyed by the dominant laminar home of the containers below it,
        // so folders group by home directory into named domains instead of pooling
        // by index order into a cut-minimal grab-bag no single home could honestly
        // name. The package-group level has no home key, so it keeps the neutral
        // descending-layer order.
        let folder_quotient = parts.quotient(&self.condensation.dag);
        let domain_homes = self.home_keys(
            &members_of,
            folder_quotient.vertex_count(),
            |folder| folder,
            |file| &file.home.domain,
        );
        let domain_parts = cluster_level(
            &folder_quotient,
            &self.caps,
            SeedLevel::Domain,
            &home_affinity(&domain_homes),
        );
        let domain_quotient = domain_parts.quotient(&folder_quotient);
        let package_homes = self.home_keys(
            &members_of,
            domain_quotient.vertex_count(),
            |folder| {
                domain_parts
                    .cluster_of(folder)
                    .map_or(0, |cluster| cluster.0)
            },
            |file| &file.home.package,
        );
        let package_parts = cluster_level(
            &domain_quotient,
            &self.caps,
            SeedLevel::Package,
            &home_affinity(&package_homes),
        );
        let package_quotient = package_parts.quotient(&domain_quotient);
        let group_parts =
            cluster_level(&package_quotient, &self.caps, SeedLevel::PackageGroup, &[]);

        // ancestry of every non-empty folder cluster, plus the directory tallies
        // each level's containers are named from.
        let mut chain_of: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
        let mut domain_tally: NameTally = BTreeMap::new();
        let mut package_tally: NameTally = BTreeMap::new();
        for (&folder, members) in &members_of {
            let domain = domain_parts
                .cluster_of(folder)
                .map_or(0, |cluster| cluster.0);
            let package = package_parts
                .cluster_of(domain)
                .map_or(0, |cluster| cluster.0);
            let group = group_parts
                .cluster_of(package)
                .map_or(0, |cluster| cluster.0);
            chain_of.insert(folder, (domain, package, group));
            for &vertex in members {
                let Some(file) = self.files.get(vertex as usize) else {
                    continue;
                };
                let sloc = file.production_sloc;
                // vote with the laminar tree's resolved name keys, not raw path
                // prefixes, so source roots stay transparent and the package
                // resolves to its manifest root (never a bare `src`).
                vote(&mut domain_tally, domain, file.home.domain.clone(), sloc);
                vote(&mut package_tally, package, file.home.package.clone(), sloc);
            }
        }

        self.emit(
            parts,
            &members_of,
            &chain_of,
            &domain_tally,
            &package_tally,
            evidence,
        )
    }

    /// Interns the candidate containers parent-before-child — package groups,
    /// packages, domains, then each folder with its files — and records every
    /// symbol's file placement.
    fn emit(
        &self,
        parts: &Partition,
        members_of: &BTreeMap<u32, Vec<u32>>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
        mirror_evidence: &MirrorEvidence,
    ) -> CandidateTree {
        let mut arena = ContainerArena::default();
        let mut key_by_id: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let domain_ids = self.intern_upper_levels(
            &mut arena,
            &mut key_by_id,
            chain_of,
            domain_tally,
            package_tally,
        );

        let mut file_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        let mut folder_ids = BTreeMap::new();
        let mut restored_ids = BTreeMap::new();
        let mut zone_by_file: BTreeMap<ContainerId, bool> = BTreeMap::new();
        let mut namespace_by_file: BTreeMap<ContainerId, SmolStr> = BTreeMap::new();
        let mut pass_start_file_by_candidate: BTreeMap<ContainerId, ContainerId> = BTreeMap::new();
        let cluster_by_folder: BTreeMap<&SmolStr, u32> = self
            .real_folder_names
            .iter()
            .enumerate()
            .map(|(cluster, name)| (name, u32::try_from(cluster).unwrap_or(u32::MAX)))
            .collect();
        for (&folder, members) in members_of {
            let Some(&(domain, _, _)) = chain_of.get(&folder) else {
                continue;
            };
            // folders are reality: the cluster keeps its full real key — one
            // container per distinct real location with an injective name
            // (`qualify_folder_names`), so sibling folders never collide and no
            // synthetic `-N` twin can arise. A key that doesn't path-extend its
            // elected domain displays relative to its enclosing package at the
            // render boundary (`folder_increment`), the honest display of a
            // foreign real directory folded into the suggested domain.
            let key = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let members_by_folder =
                self.projected_folder_members(parts, members, mirror_evidence, &key);
            for (projected_key, namespace_members) in members_by_folder {
                let projected_cluster = cluster_by_folder.get(&projected_key).copied();
                // A cycle can be the only resident of its nondominant home.
                // Such a home has no solver cluster: restore its original
                // ancestor chain, including transparent/synthetic containers.
                let restored_folder = namespace_members.first().and_then(|&vertex| {
                    (projected_cluster.is_none()
                        && self.retained_cycle_home(parts, vertex, mirror_evidence)
                            == Some(&projected_key))
                    .then(|| self.restore_cycle_folder(vertex, &mut arena, &mut restored_ids))
                    .flatten()
                });
                let projected_domain = projected_cluster
                    .and_then(|cluster| chain_of.get(&cluster))
                    .map_or(domain, |&(projected_domain, _, _)| projected_domain);
                let parent = domain_ids.get(&projected_domain).copied();
                let folder_id = restored_folder.unwrap_or_else(|| {
                    *folder_ids
                        .entry((parent, projected_key.clone()))
                        .or_insert_with(|| {
                            arena.push(ContainerSpec {
                                name: &projected_key,
                                level: ScopeLevel::Folder,
                                parent,
                                synthetic: self
                                    .real_folder_synthetic
                                    .get(projected_cluster.unwrap_or(folder) as usize)
                                    .copied()
                                    .unwrap_or(false),
                            })
                        })
                });
                for vertex in namespace_members {
                    let Some(file) = self.files.get(vertex as usize) else {
                        continue;
                    };
                    let id = arena.push(ContainerSpec {
                        name: &file.name,
                        level: ScopeLevel::File,
                        parent: Some(folder_id),
                        synthetic: false,
                    });
                    file_ids.insert(vertex, id);
                    pass_start_file_by_candidate.insert(id, ContainerId(file.container));
                    namespace_by_file.insert(id, file.namespace.clone());
                    zone_by_file.insert(
                        id,
                        self.test_zone
                            .get(vertex as usize)
                            .copied()
                            .unwrap_or(false),
                    );
                }
            }
        }

        CandidateTree {
            tree: ContainerTree::new(arena.containers),
            placement: self.placements(&file_ids),
            pass_start_file_by_candidate,
            zone_by_file,
            namespace_by_file,
            key_by_id,
        }
    }

    /// Groups physical members independently of their atomic solver representative.
    fn projected_folder_members(
        &self,
        parts: &Partition,
        members: &[u32],
        mirror_evidence: &MirrorEvidence,
        fallback: &SmolStr,
    ) -> BTreeMap<SmolStr, Vec<u32>> {
        let mut groups: BTreeMap<SmolStr, Vec<u32>> = BTreeMap::new();
        for &vertex in members {
            let key = self
                .projected_folder_for_vertex(parts, vertex, mirror_evidence)
                .map_or_else(
                    || fallback.clone(),
                    |projected| projected.repository_relative(self.root_name.as_str()),
                );
            groups.entry(key).or_default().push(vertex);
        }
        groups
    }

    /// Reuses the original ancestor chain for a physical cycle home with no
    /// representative cluster. Exact existing containers are reused so the
    /// restored files share their package and domain with ordinary residents.
    fn restore_cycle_folder(
        &self,
        vertex: u32,
        arena: &mut ContainerArena,
        restored_ids: &mut BTreeMap<ContainerId, ContainerId>,
    ) -> Option<ContainerId> {
        let file = self.files.get(vertex as usize)?;
        let by_id: BTreeMap<_, _> = self
            .snapshot
            .ir()
            .containers
            .containers()
            .iter()
            .map(|container| (container.id, container))
            .collect();
        let mut current = by_id.get(&ContainerId(file.container))?.parent;
        let mut ancestors = Vec::new();
        while let Some(id) = current {
            let container = by_id.get(&id)?;
            ancestors.push(*container);
            current = container.parent;
        }
        let mut parent = None;
        for container in ancestors.into_iter().rev() {
            let id = restored_ids
                .get(&container.id)
                .copied()
                .or_else(|| {
                    arena
                        .containers
                        .iter()
                        .find(|existing| {
                            existing.parent == parent
                                && existing.level == container.level
                                && existing.name == container.name
                                && existing.synthetic == container.synthetic
                        })
                        .map(|existing| existing.id)
                })
                .unwrap_or_else(|| {
                    arena.push(ContainerSpec {
                        name: &container.name,
                        level: container.level,
                        parent,
                        synthetic: container.synthetic,
                    })
                });
            restored_ids.insert(container.id, id);
            parent = Some(id);
        }
        parent
    }

    /// Interns the upper naming ladder — package groups over packages over
    /// domains — into `arena`, parent before child, and returns each domain
    /// cluster's [`ContainerId`] so [`emit`](Self::emit) can hang folders and
    /// files beneath it.
    ///
    /// Every level's sibling names are elected through the never-mixed,
    /// never-numeric [`elect`] ladder and then made injective by
    /// [`qualify_elected`], disambiguating a shared elected name with the
    /// cluster's lexicographically smallest real folder — its *anchor* — the
    /// same real-location qualifier folder twins use, so no reachable elected
    /// path ever falls back to the arena's numeric backstop.
    fn intern_upper_levels(
        &self,
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
    ) -> BTreeMap<u32, ContainerId> {
        // folders are reality: each upper cluster anchors on the smallest real
        // directory name it holds, so two siblings that elect one name split
        // apart by their true locations rather than a synthetic `-N` twin.
        let mut group_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut package_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut domain_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        for (&folder, &(domain, package, group)) in chain_of {
            let name = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            anchor_min(&mut group_anchor, group, &name);
            anchor_min(&mut package_anchor, package, &name);
            anchor_min(&mut domain_anchor, domain, &name);
        }

        let groups: BTreeSet<u32> = chain_of.values().map(|&(_, _, group)| group).collect();
        let group_raw: BTreeMap<u32, (u32, SmolStr)> = groups
            .iter()
            .map(|&group| (group, (0, self.root_name.clone())))
            .collect();
        let group_names = qualify_elected(&group_raw, &group_anchor);
        let mut group_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&group, name) in &group_names {
            let id = arena.push(ContainerSpec {
                name,
                level: ScopeLevel::PackageGroup,
                parent: None,
                synthetic: false,
            });
            if let Some((_, raw)) = group_raw.get(&group) {
                record_undecorated_key(key_by_id, id, raw, name);
            }
            group_ids.insert(group, id);
        }

        let packages: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(_, package, group)| (package, group))
            .collect();
        let package_raw: BTreeMap<u32, (u32, SmolStr)> = packages
            .iter()
            .map(|(&package, &group)| {
                // the fallback is unreachable: `packages` and `package_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = package_anchor
                    .get(&package)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = package_tally
                    .get(&package)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (package, (group, name))
            })
            .collect();
        let package_names = qualify_elected(&package_raw, &package_anchor);
        let mut package_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&package, &group) in &packages {
            let name = package_names
                .get(&package)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Package,
                parent: group_ids.get(&group).copied(),
                synthetic: false,
            });
            if let Some((_, raw)) = package_raw.get(&package) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            package_ids.insert(package, id);
        }

        Self::intern_domains(
            arena,
            key_by_id,
            chain_of,
            domain_tally,
            &domain_anchor,
            &package_ids,
        )
    }

    /// Interns the domain level beneath already-interned packages.
    ///
    /// Each domain elects its name through the [`elect`] ladder, disambiguates
    /// with its cluster anchor via [`qualify_elected`], and hangs off its parent
    /// package. The elected key lands verbatim — a name foreign to its parent
    /// renders whole, the honest display of a suggested grouping that spans real
    /// locations (folders set the precedent). Returns each cluster's domain id.
    fn intern_domains(
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        domain_anchor: &BTreeMap<u32, SmolStr>,
        package_ids: &BTreeMap<u32, ContainerId>,
    ) -> BTreeMap<u32, ContainerId> {
        let domains: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(domain, package, _)| (domain, package))
            .collect();
        let domain_raw: BTreeMap<u32, (u32, SmolStr)> = domains
            .iter()
            .map(|(&domain, &package)| {
                // the fallback is unreachable: `domains` and `domain_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = domain_anchor
                    .get(&domain)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = domain_tally
                    .get(&domain)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (domain, (package, name))
            })
            .collect();
        let domain_names = qualify_elected(&domain_raw, domain_anchor);
        let mut domain_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&domain, &package) in &domains {
            let name = domain_names
                .get(&domain)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Domain,
                parent: package_ids.get(&package).copied(),
                synthetic: false,
            });
            if let Some((_, raw)) = domain_raw.get(&domain) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            domain_ids.insert(domain, id);
        }

        domain_ids
    }

    /// Maps every symbol node to the candidate file container holding it, by the
    /// file vertex the node's current container indexes to.
    fn placements(&self, file_ids: &BTreeMap<u32, ContainerId>) -> BTreeMap<u32, ContainerId> {
        let mut placement = BTreeMap::new();
        for node in &self.snapshot.ir().nodes {
            let Some(&vertex) = self.index_of.get(&node.container.0) else {
                continue;
            };
            if let Some(&file_id) = file_ids.get(&vertex) {
                placement.insert(node.id.0, file_id);
            }
        }
        placement
    }

    /// Builds one DTO [`Candidate`] from a solved partition.
    ///
    /// The identity survivor is emitted as a verbatim clone of the current tree
    /// — zero moves, byte-equal layout — never re-derived through assembly, so
    /// "already optimal" genuinely means nothing changes. Every other partition
    /// is assembled, rescored under this mode's coefficients, and narrated
    /// against the current layout.
    ///
    /// # Errors
    ///
    /// Returns [`StrataError::SnapshotInvalid`] if the tree cannot be rendered.
    pub(in crate::analyze) fn build_candidate(
        &self,
        current_tree: &ContainerTree,
        solved: &SolvedCandidate<MirrorEvidence>,
        index: u32,
        splits: &[ConditionalSplit],
    ) -> Result<Candidate, StrataError> {
        let nodes = &self.snapshot.ir().nodes;
        // FIX05: the faithful exit keys on the layout being reality, not on the
        // analysis mode. Gating it on `identity` alone (anchored-only) meant a
        // greenfield candidate whose partition is byte-for-byte the real
        // directory layout still went through assemble() — which re-elects
        // upper-level containers and nests them differently from the current
        // tree — so "change nothing" rendered as structural moves for every
        // file whose fabricated chain differed. That phantom churn is what
        // inverted anchored against greenfield: the unbiased mode was billed
        // for movement it never proposed. When relief split an over-capacity
        // folder, the search's start is no longer reality, so the assemble
        // path stays (the split is exactly what the proposal must show).
        let faithful = self.identity.as_ref() == Some(&solved.partition)
            || (self.real_is_identity && self.real_partition == solved.partition);
        if faithful {
            let breakdown = score_current_with_affinity(
                self.snapshot,
                &self.coefficients,
                &self.weights,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            );
            let node = render_tree(
                current_tree,
                nodes,
                &|node: &Node| Some(node.container),
                &BTreeMap::new(),
            )?;
            return Ok(Candidate {
                index,
                score: breakdown.total,
                score_breakdown: ScoreBreakdown::from(breakdown),
                improvement: 0.0,
                tree: node,
                conditional_splits: splits.to_vec(),
                delta_narration: Vec::new(),
                symbol_moves: Vec::new(),
                capacity_remainder: None,
            });
        }

        let assembled = self.assemble_with_mirror_evidence(&solved.partition, &solved.evidence);
        // FIX08: re-run the deterministic symbol pass on this exact partition.
        // `solve` already priced its result into the ranking score, so the DTO
        // score here matches what ranked this candidate by construction.
        let symbols = self.symbol_polish_with_mirror_evidence(&solved.partition, &solved.evidence);
        if !symbols.preserves_namespaces(&assembled) {
            return Err(StrataError::SnapshotInvalid {
                source: strata_ir::SnapshotError::Serialization {
                    reason: "symbol relocation crossed a pass-start render namespace".to_owned(),
                },
            });
        }
        let merged = |id: u32| {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| assembled.placement.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &assembled.tree, &merged);
        let breakdown = score(
            &score_candidate(
                self.snapshot,
                &merged,
                &assembled.pass_start_file_by_candidate,
                &assembled.tree,
                &assembled.namespace_by_file,
                distance,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            ),
            &self.coefficients,
            &self.weights,
        );
        let placement_of = |node: &Node| merged(node.id.0);
        let node = render_tree(&assembled.tree, nodes, &placement_of, &assembled.key_by_id)?;
        let mut delta = narrate_repository_relative(current_tree, &assembled.tree, &self.facts);
        Self::attach_test_mirrors(&mut delta, &solved.evidence);
        let symbol_moves = self.symbol_narrate(&assembled, &symbols);

        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: delta,
            symbol_moves,
            capacity_remainder: None,
        })
    }
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
}

/// Builds the weighted file-dependency graph: every symbol edge — at its
/// configured price, not the hard edges alone — is mapped onto its endpoints'
/// owning files, intra-file edges vanish (layout cannot cut them), parallel
/// crossings are summed, and each crossing is priced by the config's kind-weight
/// table — so heavy-edge matching and FM gains see the same prices the objective
/// charges.
///
/// lean: admitting every edge is kept (not gated back to `Hardness::Hard`)
/// because it aligns the search's cut with the cut the score reports (AD-6) and
/// gives soft-only files a non-empty move-set. It does densify the folder
/// quotient, which under the upper levels' current `cut_only` gain nudges toward
/// one low-cut grab-bag domain; the fix is the directory-cohesion term Stage 2
/// adds to those levels (which counterbalances the extra crossings), not a
/// narrower graph here — re-gating would misalign search from score and hide the
/// collapse rather than resolve it.
/// Marks each file-graph vertex whose clustering edges are priced to zero.
/// Built-in detection marks a file that holds at least one symbol and nothing
/// but non-production symbols — test cases or test support — reusing the
/// polarity adapters already compute; configured `[tests]` patterns mark a
/// file by repo-relative path regardless of its symbols. Disabling builtins
/// leaves only the patterns to decide. The returned slice is parallel to
/// `files`, i.e. to the file graph's vertices.
#[cfg(test)]
#[cfg(test)]
pub(in crate::analyze) fn file_facts(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
) -> FileFacts {
    file_facts_with_rootedness(snapshot, weights, folder_cap, files, test_zone, true)
}

/// Builds the per-file facts narration consults from priced dependencies,
/// test-zone membership, and the configured folder capacity.
pub(in crate::analyze) fn file_facts_repository_relative(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
) -> FileFacts {
    file_facts_with_rootedness(snapshot, weights, folder_cap, files, test_zone, false)
}

pub(in crate::analyze) fn file_facts_with_rootedness(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
    package_rooted: bool,
) -> FileFacts {
    let ir = snapshot.ir();
    let by_id: BTreeMap<u32, &Container> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let file_of: BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| {
            let mut ancestor = Some(container);
            let mut dataset = None;
            while let Some(current) = ancestor {
                if current.level == ScopeLevel::PackageGroup {
                    dataset = Some(current.name.as_str());
                }
                ancestor = current
                    .parent
                    .and_then(|parent| by_id.get(&parent.0).copied());
            }
            let relative: Vec<String> = container
                .name
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            let path = dataset.map_or_else(
                || relative.join("/"),
                |dataset| {
                    if package_rooted {
                        project_package_rooted_path(dataset, &relative).join("/")
                    } else {
                        project_physical_path(dataset, &relative).join("/")
                    }
                },
            );
            (container.id.0, path)
        })
        .collect();
    let container_of: BTreeMap<u32, u32> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();

    let mut edge_weights: BTreeMap<(String, String), f64> = BTreeMap::new();
    for edge in &ir.edges {
        let (Some(source), Some(target)) = (
            container_of
                .get(&edge.source.0)
                .and_then(|container| file_of.get(container)),
            container_of
                .get(&edge.target.0)
                .and_then(|container| file_of.get(container)),
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        *edge_weights
            .entry((source.clone(), target.clone()))
            .or_insert(0.0) += weights.edge_weight(edge.kind, edge.confidence);
    }

    // a spec file holds at least one symbol and nothing but test cases.
    let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
    for node in &ir.nodes {
        let entry = case_only.entry(node.container.0).or_insert(true);
        *entry &= node.polarity == Polarity::TestCase;
    }
    let test_case_files = case_only
        .iter()
        .filter(|&(_, &only_cases)| only_cases)
        .filter_map(|(container, _)| file_of.get(container).cloned())
        .collect();

    // the tie-cut zone beyond the case-only set: pattern-marked paths and
    // support-polarity helpers narrate their moves as following a subject
    // exactly like spec files do.
    let mut shadow_test_files = BTreeSet::new();
    for (index, file) in files.iter().enumerate() {
        let zone = test_zone.get(index).copied().unwrap_or(false);
        let only_cases = case_only.get(&file.container).copied().unwrap_or(false);
        if zone
            && !only_cases
            && let Some(path) = file_of.get(&file.container)
        {
            shadow_test_files.insert(path.clone());
        }
    }

    FileFacts {
        edge_weights,
        test_case_files,
        shadow_test_files,
        folder_cap,
    }
}
