//! The language adapters and source-discovery globs configured for a run.

use serde::{Deserialize, Serialize};

/// The enabled language adapters and the globs that bound source discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdaptersConfig {
    /// Enabled adapters, by language name.
    pub languages: Vec<String>,
    /// Source-inclusion globs, relative to the analysis root.
    pub include: Vec<String>,
    /// Source-exclusion globs, applied after inclusion.
    pub exclude: Vec<String>,
    /// Directory names treated as transparent when deriving container levels, so
    /// a source file and its test share a domain/folder. One leading source-root
    /// segment below each package root is stripped (`src/adapters/x` and
    /// `spec/adapters/x` both resolve to the `adapters` domain). Spelled
    /// `source-roots` like every other key; `source_roots` remains accepted.
    #[serde(rename = "source-roots", alias = "source_roots")]
    pub source_roots: Vec<String>,
}

impl Default for AdaptersConfig {
    fn default() -> Self {
        Self {
            languages: vec![
                "typescript".to_owned(),
                "rust".to_owned(),
                "python".to_owned(),
            ],
            include: vec!["**/*".to_owned()],
            exclude: vec![
                "**/.git/**".to_owned(),
                "**/node_modules/**".to_owned(),
                "**/target/**".to_owned(),
                "**/.venv/**".to_owned(),
            ],
            source_roots: vec![
                "src".to_owned(),
                "spec".to_owned(),
                "test".to_owned(),
                "tests".to_owned(),
                "lib".to_owned(),
                "dist".to_owned(),
                "__tests__".to_owned(),
            ],
        }
    }
}
