//! The single public error type every engine entry point surfaces.
//!
//! All seven failure modes the CLI and library can hit collapse into one typed
//! [`StrataError`] enum (AD-8): config validation, the two adapter phases,
//! candidate lookup, input reads, snapshot validation, and the re-export depth
//! guard. Each variant mirrors the `code`-field convention of the error
//! reference and carries identifying context so a reader never sees a bare
//! "invalid" message.

use std::path::PathBuf;

use strata_ir::{AdapterError, SnapshotError};
use thiserror::Error;

/// Every failure an engine entry point can surface, as one typed enum.
///
/// The variants are exhaustive and stable: the CLI maps every one to exit code
/// `1`, and library consumers match on them directly. Adapter and snapshot
/// failures source-chain their underlying error rather than stringifying it.
#[derive(Debug, Error)]
pub enum StrataError {
    /// A `strata.toml` file or programmatic [`AnalyzeConfig`] failed validation —
    /// an unknown key, a malformed value, or an out-of-range coefficient.
    ///
    /// [`AnalyzeConfig`]: crate::AnalyzeConfig
    #[error("invalid configuration{}: {reason}", key_suffix(.key.as_deref()))]
    ConfigInvalid {
        /// The dotted TOML key path at fault, when one can be attributed.
        key: Option<String>,
        /// Human-readable explanation, including the expected range where known.
        reason: String,
    },

    /// A source file could not be parsed by its language adapter.
    #[error("adapter failed to parse source")]
    AdapterParseFailure {
        /// The underlying adapter parse error.
        #[source]
        source: AdapterError,
    },

    /// Parsing succeeded but reference resolution failed while binding the IR.
    #[error("adapter failed to bind references")]
    AdapterBindFailure {
        /// The underlying adapter bind error.
        #[source]
        source: AdapterError,
    },

    /// A `tree` or `diff` request named a mode or candidate index that the result
    /// does not contain.
    #[error("no candidate at index {index} for mode {mode}")]
    CandidateNotFound {
        /// The requested mode name (`anchored` or `greenfield`).
        mode: String,
        /// The 1-based candidate index that was requested.
        index: usize,
    },

    /// A path under the analysis root could not be read.
    #[error("cannot read {path}: {reason}")]
    InputUnreadable {
        /// The path that could not be read.
        path: PathBuf,
        /// Human-readable read-failure detail.
        reason: String,
    },

    /// A snapshot handed to [`analyze`] failed validation — dangling edges, a
    /// malformed container tree, or a schema mismatch.
    ///
    /// [`analyze`]: fn@crate::analyze
    #[error("snapshot failed validation")]
    SnapshotInvalid {
        /// The underlying snapshot validation error.
        #[source]
        source: SnapshotError,
    },

    /// Re-export flattening hit the chain-depth guard, almost always a barrel
    /// cycle.
    #[error("re-export chain starting at {origin} exceeded the depth guard of {limit}")]
    ReExportDepthExceeded {
        /// The symbol name at the head of the offending chain.
        origin: String,
        /// The chain-depth limit that was exceeded.
        limit: u32,
    },
}

impl StrataError {
    /// Returns the stable `SCREAMING_SNAKE_CASE` code for this error, mirroring
    /// the error-reference `code` field so machine consumers can switch on a
    /// string without depending on the Rust variant layout.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigInvalid { .. } => "CONFIG_INVALID",
            Self::AdapterParseFailure { .. } => "ADAPTER_PARSE_FAILURE",
            Self::AdapterBindFailure { .. } => "ADAPTER_BIND_FAILURE",
            Self::CandidateNotFound { .. } => "CANDIDATE_NOT_FOUND",
            Self::InputUnreadable { .. } => "INPUT_UNREADABLE",
            Self::SnapshotInvalid { .. } => "SNAPSHOT_INVALID",
            Self::ReExportDepthExceeded { .. } => "RE_EXPORT_DEPTH_EXCEEDED",
        }
    }
}

impl From<SnapshotError> for StrataError {
    fn from(source: SnapshotError) -> Self {
        Self::SnapshotInvalid { source }
    }
}

/// Renders the optional config key as a ` at <key>` suffix, or the empty string
/// when no key can be attributed, so the `Display` message reads naturally
/// either way.
fn key_suffix(key: Option<&str>) -> String {
    key.map_or_else(String::new, |key| format!(" at `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_expose_a_stable_code_per_variant() {
        let error = StrataError::ReExportDepthExceeded {
            origin: "barrel".to_owned(),
            limit: 64,
        };

        assert_eq!(error.code(), "RE_EXPORT_DEPTH_EXCEEDED");
    }

    #[test]
    fn should_name_the_offending_config_key_in_its_message() {
        let error = StrataError::ConfigInvalid {
            key: Some("capacity.file".to_owned()),
            reason: "must be positive".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "invalid configuration at `capacity.file`: must be positive"
        );
    }

    #[test]
    fn should_render_a_keyless_config_error_without_a_suffix() {
        let error = StrataError::ConfigInvalid {
            key: None,
            reason: "unparseable".to_owned(),
        };

        assert_eq!(error.to_string(), "invalid configuration: unparseable");
    }

    #[test]
    fn should_wrap_a_snapshot_error_via_from() {
        let source = SnapshotError::SchemaVersion {
            found: 2,
            expected: 1,
        };

        let error = StrataError::from(source);

        assert_eq!(error.code(), "SNAPSHOT_INVALID");
    }
}
