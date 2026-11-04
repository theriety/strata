//! The one engine-level answer to "is this file test code?".
//!
//! Adapters state facts about their own language: a node declared as test code
//! ([`Node::test_source`]), the polarity they computed, a test pair
//! ([`FileFact::test_of`]), a test-support crate. [`TestPolicy`] is the only
//! place those facts meet the `[tests]` settings, and every consumer reads its
//! verdicts: the test-only finding, the solver's test zones and tie-cut, the
//! symbol pass's pinning, polarity findings and scoring (through
//! [`TestPolicy::relabel`]).
//!
//! The order of precedence is fixed. A `!` pattern wins over everything. A
//! positive pattern marks a file whatever its language says. With
//! `tests.builtins` the adapters' own facts count too, and without it they count
//! for nothing.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use unlace_ir::{FileFact, IntermediateRepresentation, Node, Polarity, ScopeLevel};

use crate::analyze::relocation::FileInfo;
use crate::config::TestsConfig;
use crate::error::UnlaceError;
use crate::snapshot::Language;

/// The compiled `[tests]` policy.
///
/// Patterns are globs against a file's project-relative path (its container
/// chain joined with `/`, ending in the file name), so `*.spec.*` applies
/// repo-wide while `spec/mocks/**` stays scoped.
#[derive(Debug, Clone)]
pub(crate) struct TestPolicy {
    /// Whether the adapters' built-in detection participates.
    builtins: bool,
    /// Compiled patterns containing `/`: matched against the full path.
    path_patterns: Vec<glob::Pattern>,
    /// Compiled bare patterns: matched against the file name alone.
    base_patterns: Vec<glob::Pattern>,
    /// Compiled `!` patterns containing `/`: files they match are never tests.
    path_negations: Vec<glob::Pattern>,
    /// Compiled bare `!` patterns: matched against the file name alone.
    base_negations: Vec<glob::Pattern>,
}

impl TestPolicy {
    /// Compiles the configured policy, attributing a failed pattern at its
    /// `tests.patterns[i]` key.
    ///
    /// [`AnalyzeConfig::validate`](crate::config::AnalyzeConfig::validate)
    /// compiles every pattern once during loading; this second compilation
    /// covers embedders who build an
    /// [`AnalyzeConfig`](crate::config::AnalyzeConfig) directly and never
    /// validate it.
    pub(crate) fn new(config: &TestsConfig) -> Result<Self, UnlaceError> {
        let mut policy = Self {
            builtins: config.builtins,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
            path_negations: Vec::new(),
            base_negations: Vec::new(),
        };
        for (index, pattern) in config.patterns.iter().enumerate() {
            let (negated, glob) = match pattern.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, pattern.as_str()),
            };
            let compiled =
                glob::Pattern::new(glob).map_err(|error| UnlaceError::ConfigInvalid {
                    key: Some(format!("tests.patterns[{index}]")),
                    reason: error.to_string(),
                })?;
            let target = match (negated, glob.contains('/')) {
                (false, true) => &mut policy.path_patterns,
                (false, false) => &mut policy.base_patterns,
                (true, true) => &mut policy.path_negations,
                (true, false) => &mut policy.base_negations,
            };
            target.push(compiled);
        }
        Ok(policy)
    }

    /// The shipped default: built-in detection on, no extra globs.
    #[cfg(test)]
    pub(crate) fn defaults() -> Self {
        Self::new(&TestsConfig::default()).unwrap_or_else(|_| Self::disabled())
    }

    /// The inert policy: configuration alone marks nothing as a test.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            builtins: false,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
            path_negations: Vec::new(),
            base_negations: Vec::new(),
        }
    }

    /// Whether `path` matches any positive pattern; bare patterns face the
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

    /// Whether a `!` pattern excludes `path` from test code. Negation wins over
    /// the adapters' facts and over positive patterns alike.
    pub(in crate::analyze) fn negates(&self, path: &str) -> bool {
        let basename = path.rsplit('/').next().unwrap_or(path);
        self.base_negations
            .iter()
            .any(|pattern| pattern.matches(basename))
            || self
                .path_negations
                .iter()
                .any(|pattern| pattern.matches(path))
    }

    /// Whether `node`, declared in the file at `path` (when it has one), is
    /// declared test code: a positive pattern marks the whole file, and with
    /// builtins the adapter's own source fact counts; a `!` pattern overrides
    /// both.
    pub(in crate::analyze) fn node_is_test(&self, path: Option<&str>, node: &Node) -> bool {
        if path.is_some_and(|path| self.negates(path)) {
            return false;
        }
        path.is_some_and(|path| self.matches(path)) || (self.builtins && node.test_source)
    }

    /// Whether an advisory use whose user sits in the file at `path` comes from
    /// test code. A `!` pattern makes it a production use, a positive pattern a
    /// test use, and otherwise the adapter's `from_test` counts with builtins.
    pub(in crate::analyze) fn use_is_from_test(&self, path: Option<&str>, from_test: bool) -> bool {
        match path {
            Some(path) if self.negates(path) => false,
            Some(path) if self.matches(path) => true,
            _ => self.builtins && from_test,
        }
    }

    /// Whether the file at `path` is part of a test-support crate (an adapter
    /// fact, so it needs builtins and gives way to a `!` pattern).
    pub(in crate::analyze) fn is_support_file(
        &self,
        path: &str,
        support_roots: &[SmolStr],
    ) -> bool {
        self.builtins
            && !self.negates(path)
            && support_roots.iter().any(|root| {
                path.strip_prefix(root.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
    }

    /// Re-labels the polarity the adapters assigned so every consumer of the
    /// merged IR reads this policy's verdict: nodes of a `!` file become
    /// production, production nodes of a pattern-marked file become test
    /// support, and without builtins no adapter-stated polarity survives
    /// outside a pattern-marked file. With the default policy (builtins, no
    /// patterns) nothing changes.
    pub(crate) fn relabel(&self, ir: &mut IntermediateRepresentation) {
        let files: BTreeMap<u32, &str> = ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        for node in &mut ir.nodes {
            let Some(path) = files.get(&node.container.0) else {
                continue;
            };
            node.polarity = if self.negates(path) {
                Polarity::Production
            } else if self.matches(path) {
                match node.polarity {
                    Polarity::Production => Polarity::TestSupport,
                    stated => stated,
                }
            } else if self.builtins {
                node.polarity
            } else {
                Polarity::Production
            };
        }
    }

    /// Marks each file-graph vertex whose clustering edges are priced to zero.
    /// A `!` pattern never marks. A positive pattern marks by repo-relative
    /// path regardless of the file's symbols. With builtins the adapters' facts
    /// mark too: a file holding at least one symbol and nothing but
    /// non-production symbols (test cases or test support), a file an adapter
    /// states to be the test of a source, and, for languages whose conventions
    /// the adapter does not state (everything but TypeScript, whose adapter
    /// declares the test polarity itself), an any-depth test root or `.spec.`/
    /// `.test.` name. The returned vector is parallel to `files`, i.e. to the
    /// file graph's vertices.
    pub(in crate::analyze) fn zone_marks(
        &self,
        files: &[FileInfo],
        nodes: &[Node],
        facts: &[FileFact],
    ) -> Vec<bool> {
        let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
        let mut paired: BTreeSet<&str> = BTreeSet::new();
        if self.builtins {
            for node in nodes {
                let entry = case_only.entry(node.container.0).or_insert(true);
                *entry &= node.polarity != Polarity::Production;
            }
            paired.extend(
                facts
                    .iter()
                    .filter(|fact| fact.test_of.is_some())
                    .map(|fact| fact.file.as_str()),
            );
        }
        files
            .iter()
            .map(|file| {
                if self.negates(&file.name) {
                    return false;
                }
                self.matches(&file.name)
                    || (self.builtins
                        && (paired.contains(file.name.as_str())
                            || case_only.get(&file.container).copied().unwrap_or(false)
                            || Self::matches_builtin_path(&file.name)))
            })
            .collect()
    }

    /// The test conventions of languages whose adapter does not state a test
    /// verdict in its polarity. Segments are exact so ordinary names such as
    /// `contest` do not become test roots accidentally. A TypeScript file never
    /// matches: its adapter decides, from the package manifests it can read.
    fn matches_builtin_path(path: &str) -> bool {
        if Language::TypeScript.matches_extension(path) {
            return false;
        }
        let basename = path.rsplit('/').next().unwrap_or(path);
        path.split('/')
            .any(|segment| matches!(segment, "spec" | "test" | "tests"))
            || basename
                .split('.')
                .any(|segment| matches!(segment, "spec" | "test"))
    }
}

#[cfg(test)]
mod tests;
