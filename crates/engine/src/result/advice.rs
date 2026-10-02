//! Relocation advice: evidence-qualified consensus across parameter profiles.

use serde::{Deserialize, Serialize};

use super::{Move, SymbolMove};
use crate::config::ProfileName;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    pub recommended: Vec<RelocationAdvice>,
    pub review_candidates: Vec<RelocationAdvice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationAdvice {
    pub proposal: RelocationProposal,
    pub destination: String,
    pub supporting_profiles: Vec<ProfileName>,
    pub qualified_profiles: Vec<ProfileName>,
    pub absent_profiles: Vec<ProfileName>,
    pub conflicting_destinations: Vec<ProfileConflict>,
    pub assessments: Vec<ProfileAssessment>,
    pub review_reasons: Vec<ReviewReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RelocationProposal {
    File { relocation: Move },
    Symbol { relocation: SymbolMove },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConflict {
    pub profile: ProfileName,
    pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileAssessment {
    pub profile: ProfileName,
    pub destination: String,
    pub evidence: EvidenceSignals,
    pub weighted_score: f64,
    pub structural_score: f64,
    pub ambiguity_margin: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_alternative: Option<String>,
    pub thresholds: QualificationThresholds,
    pub qualified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSignals {
    pub unique_owner: f64,
    pub role_affinity: f64,
    pub source_cohesion: f64,
    pub destination_cohesion: f64,
    pub producer_evidence: f64,
    pub architectural_reach: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationThresholds {
    pub minimum_evidence: f64,
    pub minimum_structural: f64,
    pub minimum_ambiguity_margin: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewReason {
    PartialProfileSupport,
    ConflictingDestinations,
    WeakEvidence,
    WeakStructuralEvidence,
    WeakAmbiguityMargin,
    NoMajoritySupport,
}
