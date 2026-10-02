//! The evidence thresholds and weights gating safe relocation advice.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QualificationWeightsConfig {
    #[serde(rename = "unique-owner")]
    pub unique_owner: f64,
    #[serde(rename = "role-affinity")]
    pub role_affinity: f64,
    #[serde(rename = "source-cohesion")]
    pub source_cohesion: f64,
    #[serde(rename = "destination-cohesion")]
    pub destination_cohesion: f64,
    #[serde(rename = "producer-evidence")]
    pub producer_evidence: f64,
    #[serde(rename = "architectural-reach")]
    pub architectural_reach: f64,
}

impl Default for QualificationWeightsConfig {
    fn default() -> Self {
        Self {
            unique_owner: 0.10,
            role_affinity: 0.10,
            source_cohesion: 0.25,
            destination_cohesion: 0.25,
            producer_evidence: 0.10,
            architectural_reach: 0.20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QualificationConfig {
    #[serde(rename = "minimum-evidence")]
    pub minimum_evidence: f64,
    #[serde(rename = "minimum-structural")]
    pub minimum_structural: f64,
    #[serde(rename = "minimum-ambiguity-margin")]
    pub minimum_ambiguity_margin: f64,
    pub weights: QualificationWeightsConfig,
}

impl Default for QualificationConfig {
    fn default() -> Self {
        Self {
            minimum_evidence: 0.60,
            minimum_structural: 0.50,
            minimum_ambiguity_margin: 0.15,
            weights: QualificationWeightsConfig::default(),
        }
    }
}
