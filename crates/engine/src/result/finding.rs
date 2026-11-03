//! Structural findings: violations, capacity breaches, and conditional splits.

use serde::{Deserialize, Serialize};

use super::GroupKeyed;

/// A structural violation of the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    /// The violation class.
    pub kind: ViolationKind,
    /// Whether the finding is a hard violation or a borderline observation.
    pub severity: Severity,
    /// The real repo-relative path(s) involved, each whole: a cycle's or
    /// polarity finding's files, a capacity finding's one real path, or a
    /// visibility or test-only finding's file followed by the symbol's name.
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

impl Violation {
    /// Rewrites a group-keyed location to its real repo-relative form.
    ///
    /// Only one finding names its place in the group-keyed form: a capacity
    /// breach with no real path, which falls back to its container's ancestry
    /// as one path (`proj/src/hub`). Every other location is a real path from the
    /// start (a file's own path, plus a symbol name), and a breach with real
    /// paths locates itself by them, so those are left exactly as they are even
    /// when a flat layout makes one begin with the group's own name. The detail
    /// is re-described so it names the same place as the location.
    pub(crate) fn rewrite_group_keyed_location(&mut self, group: &str) {
        let Some(breach) = self
            .capacity
            .as_ref()
            .filter(|breach| breach.real_paths.is_empty())
        else {
            return;
        };
        let mut keyed = self.location.join("/");
        GroupKeyed::new(&mut keyed).into_real(group);
        self.location = vec![keyed];
        self.detail = breach.describe(&self.location);
    }
}

/// What a capacity breach counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapacityUnit {
    /// Production source lines of a file.
    Lines,
    /// Files plus distinct subfolders directly inside a folder.
    Entries,
    /// Folders holding files beneath a domain.
    Folders,
    /// Domains holding files beneath a package.
    Domains,
    /// Packages holding files beneath a package group.
    Packages,
}

/// The structured facts of a capacity violation: what was measured, in which
/// unit, against which cap and config key, and where on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityBreach {
    /// The measured size, in `unit`.
    pub measured: u32,
    /// The configured cap the measure breached.
    pub cap: u32,
    /// What `measured` counts.
    pub unit: CapacityUnit,
    /// The configuration key of the breached cap, e.g. `capacity.domain`.
    pub config_key: String,
    /// The repo-relative real directories the breach covers (the file path for
    /// a file); `.` is the repository root. Empty when no real path is known.
    pub real_paths: Vec<String>,
    /// The full repo-relative path; present for file-level breaches only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl CapacityUnit {
    /// Names the level whose cap counts this unit, e.g. `domain`.
    #[must_use]
    pub const fn level_word(self) -> &'static str {
        match self {
            Self::Lines => "file",
            Self::Entries => "folder",
            Self::Folders => "domain",
            Self::Domains => "package",
            Self::Packages => "package group",
        }
    }

    /// Words `count` of this unit for a sentence like "has 24 entries".
    #[must_use]
    pub fn counted(self, count: u32) -> String {
        match self {
            Self::Lines => format!("{count} production lines"),
            Self::Entries => format!("{count} entries (files and subfolders)"),
            Self::Folders => format!("{count} folders holding files"),
            Self::Domains => format!("{count} domains holding files"),
            Self::Packages => format!("{count} packages holding files"),
        }
    }

    /// Words `count` against its cap, e.g. "24 entries (files and subfolders)
    /// against the folder cap of 20 (`capacity.folder`)": the one phrase every
    /// face uses for a measure that breaches its cap.
    #[must_use]
    pub fn against_cap(self, count: u32, cap: u32, config_key: &str) -> String {
        format!(
            "{} against the {} cap of {cap} (`{config_key}`)",
            self.counted(count),
            self.level_word(),
        )
    }
}

impl CapacityBreach {
    /// The finding's `location`: the one first entry of `realPaths` as a whole
    /// real path (they are sorted, so a domain merging several real folders is
    /// located by the lexicographically first), or `None` when no real path is
    /// known.
    #[must_use]
    pub fn real_location(&self) -> Option<Vec<String>> {
        self.real_paths.first().map(|path| vec![path.clone()])
    }

    /// Describes the breach: where, what was counted, the cap and its key.
    ///
    /// `fallback_location` is one container's path, used when no real path is
    /// known; it reads as that single path. When several real
    /// folders merge into one container, the note says they count together.
    #[must_use]
    pub fn describe(&self, fallback_location: &[String]) -> String {
        let place = if self.real_paths.is_empty() {
            format!("`{}`", fallback_location.join("/"))
        } else {
            self.real_paths
                .iter()
                .map(|path| format!("`{path}`"))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let mut text = format!(
            "in {place}: {}; the {} cap is {} (`{}`).",
            self.unit.counted(self.measured),
            self.unit.level_word(),
            self.cap,
            self.config_key
        );
        if self.real_paths.len() > 1 {
            text.push(' ');
            text.push_str(&merged_note(&self.real_paths));
        }
        text
    }
}

/// Names the differing roots of merged real folders, e.g.
/// `src/ and spec/ count together.`
fn merged_note(paths: &[String]) -> String {
    let reversed: Vec<Vec<&str>> = paths
        .iter()
        .map(|path| path.rsplit('/').collect())
        .collect();
    let Some(first) = reversed.first() else {
        return String::new();
    };
    let shared = (0..first.len())
        .take_while(|&depth| {
            reversed
                .iter()
                .all(|parts| parts.get(depth) == first.get(depth))
        })
        .count();
    let mut roots: Vec<String> = reversed
        .iter()
        .filter_map(|parts| parts.get(shared).map(|root| format!("{root}/")))
        .collect();
    roots.dedup();
    match roots.as_slice() {
        [init @ .., last] if roots.len() > 1 && roots.len() == paths.len() => {
            format!("{} and {last} count together.", init.join(", "))
        }
        _ => "These folders count together.".to_owned(),
    }
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
    /// A production item whose every user is test code.
    TestOnly,
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
    /// `true` when both endpoints live in one file: the cut is needed to
    /// resolve the cycle but does not by itself separate files.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_file: bool,
}
