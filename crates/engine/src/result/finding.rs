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
    /// Symbol edge-break suggestions. Cycle findings carry their file-import
    /// cut in [`Violation::import_cycle`] instead (ADR-35), so no finding the
    /// engine builds today sets this; it stays readable in saved results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_suggestions: Option<Vec<EdgeBreak>>,
    /// The file-import cycle a cycle finding reports: the imports whose
    /// removal breaks it, or why they cannot be named; present for cycle
    /// violations (ADR-35).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_cycle: Option<ImportCycle>,
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
    /// Direct sub-folders of a domain that hold files.
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
            Self::Folders => format!("{count} sub-folders directly inside"),
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

/// The file-import cycle behind a cycle finding (ADR-35).
///
/// A cycle finding reports a strongly connected component of the file-import
/// graph the adapters state. Either the cut is named (`status` is `cut`) or a
/// file on the cycle states no imports and the cut cannot be named
/// (`unavailable`); an unavailable cycle is never reported as empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCycle {
    /// Whether the imports to remove are named.
    pub status: ImportCycleStatus,
    /// The imports whose removal breaks the cycle, in path order; empty when
    /// unavailable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<ImportBreak>,
    /// `true` when the cut is proven minimum, `false` for a heuristic cut.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,
    /// The cycle's files whose adapter states no imports; set only when
    /// unavailable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_without_imports: Vec<String>,
}

/// Whether a cycle finding names its cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportCycleStatus {
    /// Every file on the cycle states its imports; the cut is named.
    Cut,
    /// A file on the cycle states no imports, so no cut can be named.
    Unavailable,
}

/// One import a cycle cut removes: every statement from `importer` that loads
/// `imported`, and the symbol references those statements carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBreak {
    /// The file holding the import statements.
    pub importer: String,
    /// The file the statements load.
    pub imported: String,
    /// The statements to remove, by line.
    pub statements: Vec<ImportStatement>,
    /// The hard symbol edges from `importer` into `imported` that must be
    /// relocated or inverted before the import can go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<SymbolReference>,
    /// The cut price: the number of references, and at least 1 (deleting
    /// the statement).
    pub weight: u32,
}

/// One import statement as the source writes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStatement {
    /// The 1-based line of the statement.
    pub line: u32,
    /// The module specifier as written.
    pub specifier: String,
}

/// One hard symbol edge an import carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolReference {
    /// The referencing symbol in the importer.
    pub source: String,
    /// The referenced symbol in the imported file.
    pub target: String,
}

/// How many references [`ImportBreak::describe`] lists before `+N more`.
const LISTED_REFERENCES: usize = 3;

impl ImportBreak {
    /// Words the break, e.g. "remove `./build` (line 7) from
    /// `src/parser/adapters.ts`, which carries 1 symbol reference
    /// (applyAdapters -> buildAst)". `quote` spells each path, specifier and
    /// symbol.
    #[must_use]
    pub fn describe(&self, quote: impl Fn(&str) -> String) -> String {
        let statements = self
            .statements
            .iter()
            .map(|statement| format!("{} (line {})", quote(&statement.specifier), statement.line))
            .collect::<Vec<_>>()
            .join(" and ");
        let carried = match self.references.len() {
            0 => "no symbol reference".to_owned(),
            1 => "1 symbol reference".to_owned(),
            count => format!("{count} symbol references"),
        };
        let mut text = format!(
            "remove {statements} from {}, which carries {carried}",
            quote(&self.importer)
        );
        if !self.references.is_empty() {
            let mut listed: Vec<String> = self
                .references
                .iter()
                .take(LISTED_REFERENCES)
                .map(|reference| {
                    format!(
                        "{} -> {}",
                        quote(&reference.source),
                        quote(&reference.target)
                    )
                })
                .collect();
            if self.references.len() > LISTED_REFERENCES {
                listed.push(format!(
                    "+{} more",
                    self.references.len() - LISTED_REFERENCES
                ));
            }
            text.push_str(" (");
            text.push_str(&listed.join(", "));
            text.push(')');
        }
        text
    }
}

impl ImportCycle {
    /// Words the cycle over `files`: the first import to remove and how many
    /// more follow, or which files leave the cut unavailable.
    #[must_use]
    pub fn describe(&self, files: &[String], quote: impl Fn(&str) -> String) -> String {
        let size = files.len();
        match self.status {
            ImportCycleStatus::Unavailable => {
                let missing = self
                    .files_without_imports
                    .iter()
                    .map(|file| quote(file))
                    .collect::<Vec<_>>()
                    .join(", ");
                // a narrowed finding lists only the files no cut covers, so
                // the count says what the cycle runs through, not its size.
                let noun = if size == 1 { "file" } else { "files" };
                format!(
                    "dependency cycle through {size} {noun}; import facts are unavailable for \
                     {missing}, so the imports to remove cannot be named"
                )
            }
            ImportCycleStatus::Cut => {
                let method = if self.exact { "exact" } else { "heuristic" };
                let Some(first) = self.cut.first() else {
                    return format!("{size}-file import cycle");
                };
                let more = match self.cut.len() {
                    1 => String::new(),
                    2 => ", +1 more import".to_owned(),
                    count => format!(", +{} more imports", count - 1),
                };
                format!(
                    "{size}-file import cycle; to break it, {} ({method}){more}",
                    first.describe(&quote)
                )
            }
        }
    }
}

/// How much of the file-import graph the cycle findings could see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleCoverage {
    /// Whether every, some, or no analysed file states its imports.
    pub status: CycleAnalysis,
    /// How many analysed files state no imports.
    #[serde(default)]
    pub files_without_imports: u32,
}

impl Default for CycleCoverage {
    fn default() -> Self {
        Self {
            status: CycleAnalysis::Complete,
            files_without_imports: 0,
        }
    }
}

/// Whether cycle findings cover the whole run (ADR-35).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CycleAnalysis {
    /// Every analysed file states its imports: no cycle finding means no
    /// import cycle.
    Complete,
    /// Some files state no imports: a cycle through them is reported as
    /// unavailable when the symbol graph shows one, and may otherwise go
    /// unseen.
    Partial,
    /// No file states its imports (an IR before schema 4, or no adapter could
    /// resolve its imports): zero cycle findings does not mean zero cycles.
    Unavailable,
}
