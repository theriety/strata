//! Per-kind assertion rules over one validated `[assert]` block.

use crate::error::EvalError;

use super::validate::{invalid, rationale};
use super::{AssertBlock, FaceMode, RunMode, TargetSpec};

impl TargetSpec {
    /// Per-kind assertion rules over one validated block.
    pub(super) fn validate_assertions(
        block: &AssertBlock,
        run: RunMode,
        fixture: &str,
    ) -> Result<(), EvalError> {
        let lift =
            |checked: Result<(), String>| checked.map_err(|message| invalid(fixture, message));

        for assertion in &block.preserve_dir {
            lift(check_rationale(&assertion.because, "preserve_dir"))?;
        }
        for assertion in block.keep_together.iter().chain(block.separate.iter()) {
            lift(check_rationale(&assertion.because, "path-set"))?;
            if assertion.paths.len() < 2 {
                return Err(invalid(
                    fixture,
                    "keep_together/separate needs at least two paths to relate".to_owned(),
                ));
            }
            if assertion.paths.iter().any(String::is_empty) {
                return Err(invalid(
                    fixture,
                    "keep_together/separate paths must be non-empty".to_owned(),
                ));
            }
            lift(check_own_mode(run, assertion.mode))?;
        }
        for band in &block.size_band {
            lift(check_rationale(&band.because, "size_band"))?;
            if band.scope.is_some() == band.container.is_some() {
                return Err(invalid(
                    fixture,
                    "size_band needs exactly one of scope/container".to_owned(),
                ));
            }
            lift(check_own_mode(run, band.mode))?;
        }
        for budget in &block.move_budget {
            lift(check_rationale(&budget.because, "move_budget"))?;
            if budget.max_moved_files.is_some() == budget.min_moved_files.is_some() {
                return Err(invalid(
                    fixture,
                    "move_budget needs exactly one of max_moved_files/min_moved_files".to_owned(),
                ));
            }
            if !run.produces(budget.mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "move_budget targets {:?} but [run].mode is {run:?}",
                        budget.mode
                    ),
                ));
            }
        }
        for bucket in &block.no_synthetic_bucket {
            lift(check_rationale(&bucket.because, "no_synthetic_bucket"))?;
            if bucket.name.trim().is_empty() {
                return Err(invalid(
                    fixture,
                    "no_synthetic_bucket name must be non-empty".to_owned(),
                ));
            }
            lift(check_own_mode(run, bucket.mode))?;
        }
        for alignment in &block.name_alignment {
            lift(check_rationale(&alignment.because, "name_alignment"))?;
            if !(0.0..=1.0).contains(&alignment.min_ratio) {
                return Err(invalid(
                    fixture,
                    "name_alignment min_ratio must lie within [0, 1]".to_owned(),
                ));
            }
            lift(check_own_mode(run, alignment.mode))?;
        }
        for relief in &block.capacity_relief {
            lift(check_rationale(&relief.because, "capacity_relief"))?;
            if !run.produces(relief.mode) {
                return Err(invalid(
                    fixture,
                    format!(
                        "capacity_relief targets {:?} but [run].mode is {run:?}",
                        relief.mode
                    ),
                ));
            }
        }
        for inversion in &block.non_inversion {
            lift(check_rationale(&inversion.because, "non_inversion"))?;
            if run != RunMode::Both {
                return Err(invalid(
                    fixture,
                    "non_inversion compares both faces and requires [run].mode = both".to_owned(),
                ));
            }
        }
        Self::validate_symbol_homes(block, run, fixture)
    }

    /// Symbol-grain pins need a rationale, a non-empty symbol and path pair,
    /// and a mode the configured run actually produces.
    fn validate_symbol_homes(
        block: &AssertBlock,
        run: RunMode,
        fixture: &str,
    ) -> Result<(), EvalError> {
        let lift =
            |checked: Result<(), String>| checked.map_err(|message| invalid(fixture, message));
        for home in &block.preserve_symbol_home {
            lift(check_rationale(&home.because, "preserve_symbol_home"))?;
            if home.symbol.trim().is_empty() || home.path.trim().is_empty() {
                return Err(invalid(
                    fixture,
                    "preserve_symbol_home needs a non-empty symbol and path".to_owned(),
                ));
            }
            lift(check_own_mode(run, home.mode))?;
        }
        Ok(())
    }
}

/// Applies [`rationale`] with the assertion kind folded into the message.
fn check_rationale(because: &str, kind: &str) -> Result<(), String> {
    rationale(because).map_err(|message| format!("{kind}: {message}"))
}

/// Checks an assertion's optional own-mode override against the run.
fn check_own_mode(run: RunMode, mode: Option<FaceMode>) -> Result<(), String> {
    if let Some(face) = mode
        && !run.produces(face)
    {
        return Err(format!(
            "assertion overrides its mode to {face:?} but [run].mode is {run:?}"
        ));
    }
    Ok(())
}
