//! The objective coefficients and per-kind edge weights, with their scorer conversions.

use serde::{Deserialize, Serialize};
use strata_core::score::{Coefficients, KindWeights};

/// The objective coefficients (the `J(T)` weights).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObjectiveConfig {
    /// `lambda`: the sibling size-imbalance penalty weight.
    pub imbalance: f64,
    /// `alpha`: the naming-token cohesion bonus weight.
    pub naming: f64,
    /// `beta`: the current-path cohesion bonus weight (anchored only).
    pub path: f64,
    /// `mu`: the move-distance anchoring penalty weight (anchored only).
    pub anchor: f64,
    /// `gamma`: the scoped over-capacity binding penalty weight (FIX03). Both
    /// modes price it: a layout binding more files than the folder budget at
    /// any folder or domain pays `gamma` per over-cap share, so relief can win
    /// on J instead of relying on selection alone.
    pub capacity: f64,
    /// Fixed charge for relocating a declaration into a file that contains one
    /// of its dependencies but none of its consumers at pass start.
    #[serde(rename = "dependency-only")]
    pub dependency_only: f64,
    /// Fixed charge while a companion type remains outside its owner file.
    #[serde(rename = "companion-separation")]
    pub companion_separation: f64,
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        Self {
            imbalance: 0.1,
            naming: 0.3,
            path: 0.2,
            anchor: 1.0,
            capacity: 4.0,
            dependency_only: 0.05,
            companion_separation: 0.05,
        }
    }
}

impl ObjectiveConfig {
    /// Converts this profile's objective values into scorer coefficients.
    #[must_use]
    pub const fn coefficients(&self) -> Coefficients {
        Coefficients {
            lambda: self.imbalance,
            alpha: self.naming,
            beta: self.path,
            mu: self.anchor,
            gamma: self.capacity,
            dependency_only: self.dependency_only,
            companion_separation: self.companion_separation,
        }
    }

    /// Compatibility alias for callers that previously selected coefficients by mode.
    #[must_use]
    pub const fn anchored(&self) -> Coefficients {
        self.coefficients()
    }

    /// Compatibility alias that now honors explicit greenfield path and anchor values.
    #[must_use]
    pub const fn greenfield(&self) -> Coefficients {
        self.coefficients()
    }
}

/// The per-kind edge weights used when cutting dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeightsConfig {
    /// Weight of a runtime value import.
    #[serde(rename = "value-import")]
    pub value_import: f64,
    /// Weight of a subtype / implements relationship.
    pub inheritance: f64,
    /// Weight of a direct call.
    pub call: f64,
    /// Weight of a type reference.
    #[serde(rename = "type-reference")]
    pub type_reference: f64,
    /// Weight of a re-export (zero — flattened during normalization).
    #[serde(rename = "re-export")]
    pub re_export: f64,
    /// Multiplier for pass-start same-file edges between runtime symbols.
    #[serde(rename = "same-file-symbol")]
    pub same_file_symbol: f64,
    /// Multiplier for pass-start same-file edges touching a type.
    #[serde(rename = "same-file-type")]
    pub same_file_type: f64,
}

impl Default for WeightsConfig {
    fn default() -> Self {
        Self {
            value_import: 1.0,
            inheritance: 1.5,
            call: 1.0,
            type_reference: 0.3,
            re_export: 0.0,
            same_file_symbol: 1.0,
            same_file_type: 3.0,
        }
    }
}

impl WeightsConfig {
    /// Converts the config into the scorer's [`KindWeights`] table.
    #[must_use]
    pub const fn kind_weights(&self) -> KindWeights {
        KindWeights {
            value_import: self.value_import,
            inheritance: self.inheritance,
            call: self.call,
            type_reference: self.type_reference,
            re_export: self.re_export,
        }
    }
}
