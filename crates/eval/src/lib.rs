//! `strata-eval` — the in-process quality gate for strata's restructuring
//! engine (WS-C, product-perfection).
//!
//! The harness runs [`strata_engine`] over the synthetic corpus in
//! `crates/eval/fixtures` and asserts distance-to-best-state per committed
//! target in `crates/eval/targets` (schema v1, see `targets/CONTRACT.md`). It
//! is dev-only: it ships no library API beyond what its own tests need, and it
//! never asserts a total score magnitude — distance lives in per-assertion
//! verdicts, pair-F1 is report-only, and candidate distinctness is observed,
//! never gated.
//!
//! ```no_run
//! use strata_eval::{eval_fixture_root, eval_target_path, load_target, harness};
//!
//! let spec = load_target("tearing").expect("target parses");
//! let report = harness::run_case(&eval_fixture_root("tearing"), &spec, "tearing")
//!     .expect("case runs");
//! // The gate itself lives in crates/eval/tests/eval.rs.
//! ```

pub mod error;
pub mod harness;
pub mod metrics;
pub mod target;

pub use crate::error::EvalError;
pub use crate::harness::{CaseReport, ModeObservation, Verdict};
pub use crate::metrics::{
    alignment_sharing, co_membership_pairs, is_scoped_container, last_segment, members,
    moved_files, pair_f1, placement_signature, structural_placement, tokenize, walk_containers,
};
pub use crate::target::TargetSpec;

/// The absolute path of one fixture directory under this crate's corpus.
///
/// `CARGO_MANIFEST_DIR` keeps every root absolute so semantic edges resolve
/// identically no matter where `cargo test` runs from.
#[must_use]
pub fn eval_fixture_root(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

/// The absolute path of one committed target file.
#[must_use]
pub fn eval_target_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("targets")
        .join(format!("{name}.toml"))
}

/// The absolute path of one e2e fixture under `crates/cli/tests/e2e/fixtures`.
#[must_use]
pub fn e2e_fixture_root(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("cli")
        .join("tests")
        .join("e2e")
        .join("fixtures")
        .join(name)
}

/// Loads and parses one committed target by name; parse failures are target
/// defects and carry the TOML error verbatim.
///
/// # Errors
///
/// Returns [`EvalError::TargetInvalid] when the file cannot be read or parsed.
pub fn load_target(name: &str) -> Result<TargetSpec, EvalError> {
    let path = eval_target_path(name);
    let raw = std::fs::read_to_string(&path).map_err(|error| EvalError::TargetInvalid {
        target: name.to_owned(),
        message: format!("reading {}: {error}", path.display()),
    })?;
    toml::from_str(&raw).map_err(|error| EvalError::TargetInvalid {
        target: name.to_owned(),
        message: format!("parsing {}: {error}", path.display()),
    })
}
