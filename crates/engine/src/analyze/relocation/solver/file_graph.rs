//! File-grain inputs to the solver: test-zone marks and the priced file graph.

use std::collections::BTreeMap;

use strata_core::graph::csr::Csr;
use strata_core::score::KindWeights;
use strata_ir::{Edge, Node, Polarity};

use crate::analyze::relocation::FileInfo;
use crate::config::TestsConfig;
use crate::error::StrataError;

/// Marks each file-graph vertex whose clustering edges are priced to zero.
/// Built-in detection marks a file that holds at least one symbol and nothing
/// but non-production symbols — test cases or test support — reusing the
/// polarity adapters already compute; configured `[tests]` patterns mark a
/// file by repo-relative path regardless of its symbols. Disabling builtins
/// leaves only the patterns to decide. The returned slice is parallel to
/// `files`, i.e. to the file graph's vertices.
pub(in crate::analyze) fn test_zone_marks(
    tests: &TestPolicy,
    files: &[FileInfo],
    nodes: &[Node],
) -> Vec<bool> {
    let mut marks: Vec<bool> = files
        .iter()
        .map(|file| {
            tests.matches(&file.name)
                || (tests.builtins && TestPolicy::matches_builtin_path(&file.name))
        })
        .collect();
    if tests.builtins {
        let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
        for node in nodes {
            let entry = case_only.entry(node.container.0).or_insert(true);
            *entry &= node.polarity != Polarity::Production;
        }
        for (index, file) in files.iter().enumerate() {
            if !case_only.get(&file.container).copied().unwrap_or(false) {
                continue;
            }
            if let Some(mark) = marks.get_mut(index) {
                *mark = true;
            }
        }
    }
    marks
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
pub(in crate::analyze) fn build_file_graph(
    edges: &[Edge],
    nodes: &[Node],
    index_of: &BTreeMap<u32, u32>,
    file_count: usize,
    weights: &KindWeights,
    test_zone: &[bool],
) -> Csr {
    let container_of: BTreeMap<u32, u32> = nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for edge in edges {
        // admit every edge at its configured price — not Hard edges alone — so
        // the search optimizes the same cut the score reports and soft-only
        // files (e.g. type-reference-only TS) get a non-empty move-set. A zero-
        // priced edge stays in the graph but binds nothing: polish never
        // nominates it as a move target and matching never contracts across it
        // (the FIX04 doctrine).
        let (Some(source), Some(target)) = (
            container_of.get(&edge.source.0),
            container_of.get(&edge.target.0),
        ) else {
            continue;
        };
        let (Some(&from), Some(&to)) = (index_of.get(source), index_of.get(target)) else {
            continue;
        };
        if from == to {
            continue;
        }
        // reason: csr weights are f32 by contract (ad-6); narrowing the f64 price is the one lossy step
        #[allow(clippy::cast_possible_truncation)]
        let weight = weights.edge_weight(edge.kind, edge.confidence) as f32;
        // The test tie-cut prices an edge touching a test-zone file at zero —
        // both directions, test↔test included — so test coupling can neither
        // weld a spec to its subject nor bond test files into a place of their
        // own. This is the single pricing choke point every downstream stage
        // (relief piles, polish moves, heavy-edge matching) reads.
        let weight = if test_zone.get(from as usize).copied().unwrap_or(false)
            || test_zone.get(to as usize).copied().unwrap_or(false)
        {
            0.0
        } else {
            weight
        };
        crossings.push((from, to, weight));
    }
    Csr::from_weighted_edges(file_count, &crossings)
}

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
    pub(in crate::analyze::relocation) builtins: bool,
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
    pub(in crate::analyze::relocation) fn matches(&self, path: &str) -> bool {
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
    pub(in crate::analyze::relocation) fn matches_builtin_path(path: &str) -> bool {
        let mut segments = path.split('/');
        let basename = path.rsplit('/').next().unwrap_or(path);
        segments.any(|segment| matches!(segment, "spec" | "test" | "tests"))
            || basename
                .split('.')
                .any(|segment| matches!(segment, "spec" | "test"))
    }
}
