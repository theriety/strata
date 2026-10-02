//! The complete analysis parameter profile and the built-in profile catalog.

use serde::{Deserialize, Serialize};

use super::{
    CapacityConfig, DiversityConfig, ObjectiveConfig, QualificationConfig, RelocationConfig,
    SolverConfig, TestsConfig, WeightsConfig,
};

/// One complete, independently configurable analysis parameter profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileConfig {
    /// Number of diverse candidates to return (`k`).
    pub candidates: u32,
    /// Deterministic base seed.
    pub seed: u64,
    /// Per-level capacity caps.
    pub capacity: CapacityConfig,
    /// Objective coefficients.
    pub objective: ObjectiveConfig,
    /// Dependency and same-file affinity weights.
    pub weights: WeightsConfig,
    /// MFAS solver budget.
    pub solver: SolverConfig,
    /// Diversification parameters.
    pub diversity: DiversityConfig,
    /// Test-file detection and capping policy.
    pub tests: TestsConfig,
    /// Rules controlling which graph participants may relocate and how tests follow sources.
    pub relocation: RelocationConfig,
    /// Evidence required before safe relocation advice is recommended.
    pub qualification: QualificationConfig,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            candidates: 3,
            seed: 42,
            capacity: CapacityConfig::default(),
            objective: ObjectiveConfig::default(),
            weights: WeightsConfig::default(),
            solver: SolverConfig::default(),
            diversity: DiversityConfig::default(),
            tests: TestsConfig::default(),
            relocation: RelocationConfig::default(),
            qualification: QualificationConfig::default(),
        }
    }
}

impl ProfileConfig {
    /// Returns the greenfield defaults without overriding explicit values later.
    #[must_use]
    pub fn greenfield() -> Self {
        Self {
            objective: ObjectiveConfig {
                path: 0.0,
                anchor: 0.0,
                ..ObjectiveConfig::default()
            },
            ..Self::default()
        }
    }
}

/// The complete built-in parameter-profile catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfilesConfig {
    /// Parameters used by the anchored profile.
    pub anchored: ProfileConfig,
    /// Parameters used by the greenfield profile.
    pub greenfield: ProfileConfig,
}

impl Default for ProfilesConfig {
    fn default() -> Self {
        Self {
            anchored: ProfileConfig::default(),
            greenfield: ProfileConfig::greenfield(),
        }
    }
}

impl<'de> Deserialize<'de> for ProfilesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProfileOverrides {
            anchored: Option<serde_json::Value>,
            greenfield: Option<serde_json::Value>,
        }

        let overrides = ProfileOverrides::deserialize(deserializer)?;
        Ok(Self {
            anchored: merge_profile(ProfileConfig::default(), overrides.anchored)
                .map_err(serde::de::Error::custom)?,
            greenfield: merge_profile(ProfileConfig::greenfield(), overrides.greenfield)
                .map_err(serde::de::Error::custom)?,
        })
    }
}

fn merge_profile(
    defaults: ProfileConfig,
    overrides: Option<serde_json::Value>,
) -> Result<ProfileConfig, serde_json::Error> {
    let mut merged = serde_json::to_value(defaults)?;
    if let Some(overrides) = overrides {
        merge_value(&mut merged, overrides);
    }
    serde_json::from_value(merged)
}

fn merge_value(target: &mut serde_json::Value, overrides: serde_json::Value) {
    match overrides {
        serde_json::Value::Object(overrides) if target.is_object() => {
            if let Some(target) = target.as_object_mut() {
                for (key, value) in overrides {
                    match target.get_mut(&key) {
                        Some(target_value) => merge_value(target_value, value),
                        None => {
                            target.insert(key, value);
                        }
                    }
                }
            }
        }
        value => *target = value,
    }
}
