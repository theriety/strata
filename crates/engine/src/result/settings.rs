//! The resolved settings an analysis ran under that are not per-profile.

use serde::{Deserialize, Serialize};

use crate::config::TestsConfig;

/// The resolved `[tests]` section: one policy for every profile, recorded once
/// at the top of the result so a report can show what counted as test code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSettings {
    /// File cap (in production SLOC) for test-support files.
    pub helper_cap: u32,
    /// The configured test patterns, `!` negations included, in order.
    pub patterns: Vec<String>,
    /// Whether the adapters' built-in detection took part.
    pub builtins: bool,
}

impl From<&TestsConfig> for TestSettings {
    fn from(config: &TestsConfig) -> Self {
        Self {
            helper_cap: config.helper_cap,
            patterns: config.patterns.clone(),
            builtins: config.builtins,
        }
    }
}

impl Default for TestSettings {
    fn default() -> Self {
        Self::from(&TestsConfig::default())
    }
}
