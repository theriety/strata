//! Precondition blocks: the check kinds and violation vocabulary a target
//! may assert against the current layout.

use serde::Deserialize;

/// The kind of a precondition check against `result.current`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreconditionKind {
    /// `summary.files` within [min, max].
    FileCount,
    /// A violation class is present (optionally suffix-scoped).
    ViolationPresent,
    /// A violation class is absent (optionally suffix-scoped).
    ViolationAbsent,
}

/// The violation classes a precondition may reference; mirrors the DTO's
/// `ViolationKind` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViolationClass {
    /// A dependency cycle.
    Cycle,
    /// A polarity-matrix breach.
    Polarity,
    /// A capacity cap exceeded.
    Capacity,
    /// Declared visibility wider than derived scope.
    Visibility,
}

/// The severity a precondition filters on when explicitly opted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityFilter {
    /// Hard violations only.
    Violation,
    /// Borderline observations only.
    Borderline,
}

/// One precondition block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Precondition {
    /// Which check to perform.
    pub kind: PreconditionKind,
    /// Inclusive lower file-count bound (`file_count` only).
    #[serde(default)]
    pub min: Option<u32>,
    /// Inclusive upper file-count bound (`file_count` only).
    #[serde(default)]
    pub max: Option<u32>,
    /// Which violation class to look for (violation kinds only).
    #[serde(default)]
    pub violation: Option<ViolationClass>,
    /// Dot-segment location filter (violation kinds only).
    #[serde(default)]
    pub location_suffix: Option<String>,
    /// Exact severity filter; absent means findings of any severity qualify.
    #[serde(default)]
    pub severity: Option<SeverityFilter>,
    /// The human-defensible reason the precondition holds.
    pub because: String,
}
