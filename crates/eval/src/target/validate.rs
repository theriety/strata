//! Structural validation of a parsed target; needs no analyzed result.

use crate::error::EvalError;

use super::{AssertBlock, Precondition, PreconditionKind, SCHEMA_VERSION, TargetSpec};

/// Builds a target-scoped [`EvalError::TargetInvalid`] without repeating the
/// case name.
pub(super) fn invalid(target: &str, message: String) -> EvalError {
    EvalError::TargetInvalid {
        target: target.to_owned(),
        message,
    }
}

impl TargetSpec {
    /// Validates every rule CONTRACT.md states that does not need the analyzed
    /// result. `expected_fixture` is the case name the harness invoked this
    /// target under, so a renamed fixture fails loud instead of measuring
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::TargetInvalid`] on the first violated rule.
    pub fn validate(&self, expected_fixture: &str) -> Result<(), EvalError> {
        let block = self.validate_shape(expected_fixture)?;
        for precondition in &self.precondition {
            rationale(precondition.because.as_str()).map_err(|message| {
                invalid(
                    expected_fixture,
                    format!("precondition {:?}: {message}", precondition.kind),
                )
            })?;
            validate_precondition(precondition).map_err(|message| {
                invalid(
                    expected_fixture,
                    format!("precondition {:?}: {message}", precondition.kind),
                )
            })?;
        }
        Self::validate_assertions(block, self.run.mode, expected_fixture)?;
        self.validate_reference(expected_fixture)
    }

    /// Header shape: schema, fixture identity, exactly-one assert block, modes
    /// producible by the run. Returns the validated block.
    fn validate_shape(&self, fixture: &str) -> Result<&AssertBlock, EvalError> {
        if self.schema != SCHEMA_VERSION {
            return Err(invalid(
                fixture,
                format!(
                    "schema {} is not supported (this harness implements schema {SCHEMA_VERSION})",
                    self.schema
                ),
            ));
        }
        if self.fixture != fixture {
            return Err(invalid(
                fixture,
                format!(
                    "`fixture = {:?}` does not match the invoked case name {fixture:?}",
                    self.fixture
                ),
            ));
        }
        if self.assert.len() != 1 {
            return Err(invalid(
                fixture,
                format!(
                    "exactly one [[assert]] block is required, found {}",
                    self.assert.len()
                ),
            ));
        }
        let Some(block) = self.assert.first() else {
            return Err(invalid(
                fixture,
                "exactly one [[assert]] block is required".to_owned(),
            ));
        };
        if block.modes.is_empty() {
            return Err(invalid(
                fixture,
                "[assert].modes is empty and some assertion relies on it".to_owned(),
            ));
        }
        if block.candidate == 0 {
            return Err(invalid(
                fixture,
                "[assert].candidate is 1-based; 0 selects nothing".to_owned(),
            ));
        }
        for mode in &block.modes {
            if !self.run.mode.produces(*mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "[assert].modes lists {mode:?} but [run].mode is {:?}",
                        self.run.mode
                    ),
                ));
            }
        }
        Ok(block)
    }

    /// Reference containers must label non-empty file sets.
    fn validate_reference(&self, fixture: &str) -> Result<(), EvalError> {
        let Some(reference) = &self.reference else {
            return Ok(());
        };
        for container in &reference.container {
            if container.files.is_empty() {
                return Err(invalid(
                    fixture,
                    format!("reference container {:?} lists no files", container.path),
                ));
            }
            if container.files.iter().any(String::is_empty) {
                return Err(invalid(
                    fixture,
                    format!(
                        "reference container {:?} carries an empty file path",
                        container.path
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// Rejects empty or whitespace-only rationales.
pub(super) fn rationale(because: &str) -> Result<(), &'static str> {
    if because.trim().is_empty() {
        Err("every block carries a non-empty `because rationale")
    } else {
        Ok(())
    }
}

/// Checks a precondition's per-kind key requirements.
fn validate_precondition(precondition: &Precondition) -> Result<(), String> {
    match precondition.kind {
        PreconditionKind::FileCount => {
            if precondition.min.is_none() || precondition.max.is_none() {
                return Err("file_count requires both min and max".to_owned());
            }
        }
        PreconditionKind::ViolationPresent | PreconditionKind::ViolationAbsent => {
            if precondition.violation.is_none() {
                return Err(format!(
                    "{:?} requires a violation class",
                    precondition.kind
                ));
            }
        }
    }
    Ok(())
}
