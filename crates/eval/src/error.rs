//! The single eval-harness error type.
//!
//! Every failure the harness can raise is a harness or corpus defect, never
//! distance: distance lives in [`crate::harness::Verdict`]s. A precondition
//! failure, a misspelled target path, or an engine error all surface here so
//! tests can distinguish "the measurement broke" from "strata measured badly".

/// Everything that can abort a case before its verdicts are meaningful.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EvalError {
    /// The target TOML violates `crates/eval/targets/CONTRACT.md`: unknown keys,
    /// wrong schema, missing rationale, structural contradictions, or a path
    /// that does not resolve against the fixture census.
    #[error("target `{target}` is invalid: {message}")]
    TargetInvalid {
        /// Which target file failed.
        target: String,
        /// The specific contract violation.
        message: String,
    },

    /// The fixture could not be analyzed (unreadable root, discovery failure).
    #[error("fixture `{fixture}` could not be analyzed: {message}")]
    EngineRun {
        /// Which fixture was being analyzed.
        fixture: String,
        /// The engine's own failure description.
        message: String,
    },

    /// Writing a `STRATA_BLESS_EVAL` diagnostic dump failed.
    #[error("diagnostics dump for `{fixture}` failed: {message}")]
    DumpFailed {
        /// Which fixture's dump failed.
        fixture: String,
        /// The I/O failure description.
        message: String,
    },
}
