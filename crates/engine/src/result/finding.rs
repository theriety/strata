//! Structural findings: violations, capacity breaches, and conditional splits.

use serde::{Deserialize, Serialize};

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
