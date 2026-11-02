//! Per-profile results: the census, current layout, and candidate sets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Candidate, ContainerNode, ScoreBreakdown, Violation};
use crate::config::ProfileConfig;

/// A coarse census of the analyzed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Total symbol and type nodes.
    pub symbols: u32,
    /// Total dependency edges.
    pub edges: u32,
    /// Total source files (file-level containers).
    pub files: u32,
    /// File counts keyed by language tag (`ts`, `rs`, `py`).
    pub files_by_language: BTreeMap<String, u32>,
}

/// The shared current layout and findings identical across executed profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTree {
    /// Today's layout as a nested container tree.
    pub tree: ContainerNode,
    /// Findings whose complete serialized content is identical in both profiles.
    pub shared_findings: Vec<Violation>,
}

/// One profile's scoring and findings for the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCurrent {
    /// The objective `J` of the current tree under this profile.
    pub score: f64,
    /// The per-term decomposition of `score`.
    pub score_breakdown: ScoreBreakdown,
    /// Findings applicable only to this profile after exact sharing.
    pub unique_findings: Vec<Violation>,
    /// Where the current layout stands relative to this profile's candidates.
    pub standing: CurrentStanding,
    /// How many of those violations are capacity findings that hard-breach
    /// their caps (`Severity::Violation` only). Borderline observations stay
    /// listed in `violations` but never count as breaks — the same predicate
    /// that gates `infeasible` standings. This is the authoritative count
    /// behind the "breaks N capacity findings" narration; faces must not
    /// re-tally it.
    pub capacity_breaks: u32,
}

/// Results for selected parameter profiles; an unselected profile is omitted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profiles {
    /// The anchored parameter-profile result, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchored: Option<ProfileResult>,
    /// The greenfield parameter-profile result, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub greenfield: Option<ProfileResult>,
}

/// One parameter profile's effective inputs, current state, and candidate set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileResult {
    /// Complete effective parameters used for this profile.
    pub parameters: ProfileConfig,
    /// This profile's current score, standing, and unique findings.
    pub current: ProfileCurrent,
    /// The returned candidates, best score first.
    pub candidates: Vec<Candidate>,
    /// The variation-of-information matrix over the candidates.
    pub pairwise_distance: Vec<Vec<f64>>,
    /// `true` when fewer than `k` candidates survived diversification.
    pub solution_space_converged: bool,
}

/// Compatibility alias for source consumers migrating from schema version 3.
pub type Modes = Profiles;

/// Compatibility alias for source consumers migrating from schema version 3.
pub type ModeResult = ProfileResult;

/// What a candidate leaves unresolved of the current capacity breaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityRemainder {
    /// Hard capacity findings remaining in the best candidate's tree.
    pub remaining: u32,
    /// How many of `remaining` are file-level breaches, which no move can fix —
    /// only conditional splits can.
    pub file_level: u32,
}

/// Where the current layout stands relative to a parameter profile's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurrentStanding {
    /// No candidate beats the current layout; candidate 1 IS the current tree.
    Optimal,
    /// At least one candidate scores better than the current layout.
    Outscored,
    /// The current layout breaches a capacity cap, so it cannot compete;
    /// candidates fix the breach at whatever cut cost they carry.
    Infeasible,
}
