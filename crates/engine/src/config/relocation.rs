//! Relocation admission rules and the exact source-to-test mirroring policy.

use serde::{Deserialize, Serialize};

/// Per-profile relocation admission and test-mirroring policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RelocationConfig {
    /// Prevent detected test files from relocating independently.
    #[serde(rename = "pin-detected-test-files")]
    pub pin_detected_test_files: bool,
    /// Prevent declarations in detected tests, and test-polarity declarations in
    /// any file, from relocating independently.
    #[serde(rename = "pin-detected-test-symbols")]
    pub pin_detected_test_symbols: bool,
    /// Lift the package wall so relocations may cross manifest packages
    /// (ADR-17); off by default. Every other admission rule still applies.
    #[serde(rename = "allow-cross-package-moves")]
    pub allow_cross_package_moves: bool,
    /// Repo-relative file globs whose matching files cannot relocate independently.
    #[serde(rename = "forbid-file-moves")]
    pub forbid_file_moves: Vec<String>,
    /// Repo-relative file globs that block declarations leaving or entering a match.
    #[serde(rename = "forbid-symbol-moves")]
    pub forbid_symbol_moves: Vec<String>,
    /// Exact immutable source-to-test mirroring policy.
    #[serde(rename = "test-mirroring")]
    pub test_mirroring: TestMirroringConfig,
}

impl Default for RelocationConfig {
    fn default() -> Self {
        Self {
            pin_detected_test_files: true,
            pin_detected_test_symbols: true,
            allow_cross_package_moves: false,
            forbid_file_moves: Vec::new(),
            forbid_symbol_moves: Vec::new(),
            test_mirroring: TestMirroringConfig::default(),
        }
    }
}

/// Exact source-to-test mirror inference settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TestMirroringConfig {
    /// Whether accepted source moves attempt immutable mirror followers.
    pub enabled: bool,
    /// Whether the test pairs language adapters state (`file_facts`) are used;
    /// the engine carries no language conventions of its own. Custom `rules`
    /// apply either way, after the adapter pairs.
    pub builtins: bool,
    /// Additional exact source-to-test template rules.
    pub rules: Vec<TestMirrorRule>,
}

impl Default for TestMirroringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtins: true,
            rules: Vec::new(),
        }
    }
}

/// One exact source template and its possible mirrored test paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestMirrorRule {
    /// Source path template containing `{dir}` and `{stem}` captures.
    pub source: String,
    /// Test path templates derived from the source captures.
    pub tests: Vec<String>,
}
