//! Per-kind assertion rules over one validated `[assert]` block.

use crate::error::EvalError;

use super::validate::{invalid, rationale};
use super::{AdviceTier, AssertBlock, CycleImportCut, FaceMode, RunMode, TargetSpec, Tier};

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
        for pin in &block.cycle_import_cut {
            lift(check_cycle_pin(pin))?;
        }
        for pin in &block.advice_tier {
            lift(check_advice_pin(pin))?;
        }
        lift(check_guard(block))?;
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
/// A cycle pin needs a rationale and two non-empty files; an `expect` needs
/// two different non-empty files and, when given, a 1-based line.
fn check_cycle_pin(pin: &CycleImportCut) -> Result<(), String> {
    check_rationale(&pin.because, "cycle_import_cut")?;
    if pin.files.len() < 2 || pin.files.iter().any(|file| file.trim().is_empty()) {
        return Err("cycle_import_cut needs at least two non-empty file paths".to_owned());
    }
    if let Some(expect) = &pin.expect {
        let blank = expect.importer.trim().is_empty() || expect.imported.trim().is_empty();
        if blank || expect.importer == expect.imported || expect.line == Some(0) {
            return Err(
                "cycle_import_cut.expect needs two different non-empty files and a 1-based line"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

/// An advice pin needs a rationale, a non-empty path (and symbol and
/// destination when given), and exactly one of `max_tier`/`exact_tier`; a
/// `max_tier` of `recommended` caps nothing, so it is rejected as vacuous.
fn check_advice_pin(pin: &AdviceTier) -> Result<(), String> {
    check_rationale(&pin.because, "advice_tier")?;
    let blank = |value: Option<&String>| value.is_some_and(|text| text.trim().is_empty());
    if pin.path.trim().is_empty() || blank(pin.symbol.as_ref()) || blank(pin.to.as_ref()) {
        return Err(
            "advice_tier needs a non-empty path, and a non-empty symbol and to when given"
                .to_owned(),
        );
    }
    match (pin.max_tier, pin.exact_tier) {
        (Some(Tier::Recommended), None) => Err(
            "advice_tier max_tier = \"recommended\" caps nothing; use review or absent".to_owned(),
        ),
        (Some(_), None) | (None, Some(_)) => Ok(()),
        _ => Err("advice_tier needs exactly one of max_tier/exact_tier".to_owned()),
    }
}

/// A block carries at most one `recommended_guard`, each with a rationale,
/// and only beside a mode-free pin it can enforce: the guard skips pins with
/// their own `mode`, so without a mode-free `keep_together`, `preserve_dir` or
/// `preserve_symbol_home` it would pass vacuously.
fn check_guard(block: &AssertBlock) -> Result<(), String> {
    for guard in &block.recommended_guard {
        check_rationale(&guard.because, "recommended_guard")?;
    }
    if block.recommended_guard.len() > 1 {
        return Err(
            "recommended_guard appears more than once; one guard already judges every Recommended move"
                .to_owned(),
        );
    }
    let guardable = block.keep_together.iter().any(|set| set.mode.is_none())
        || block.preserve_dir.iter().any(|dir| dir.mode.is_none())
        || block
            .preserve_symbol_home
            .iter()
            .any(|home| home.mode.is_none());
    if !block.recommended_guard.is_empty() && !guardable {
        return Err(
            "recommended_guard needs a mode-free keep_together, preserve_dir or preserve_symbol_home pin in its block; without one it passes vacuously"
                .to_owned(),
        );
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::{check_advice_pin, check_guard};
    use crate::target::{
        AdviceTier, AssertBlock, FaceMode, PreserveSymbolHome, RecommendedGuard, Tier,
    };

    fn pin(max_tier: Option<Tier>, exact_tier: Option<Tier>) -> AdviceTier {
        AdviceTier {
            path: "src/tree/validation/shape.ts".to_owned(),
            symbol: None,
            to: Some("src/tree".to_owned()),
            max_tier,
            exact_tier,
            because: "F1".to_owned(),
        }
    }

    #[test]
    fn should_accept_one_tier_and_reject_none_both_or_a_vacuous_cap() {
        assert!(check_advice_pin(&pin(Some(Tier::Review), None)).is_ok());
        assert!(check_advice_pin(&pin(None, Some(Tier::Recommended))).is_ok());
        assert!(check_advice_pin(&pin(None, None)).is_err());
        assert!(check_advice_pin(&pin(Some(Tier::Review), Some(Tier::Review))).is_err());
        assert!(
            check_advice_pin(&pin(Some(Tier::Recommended), None))
                .is_err_and(|message| message.contains("caps nothing"))
        );
    }

    #[test]
    fn should_reject_a_blank_destination_or_symbol() {
        let blank_to = AdviceTier {
            to: Some(" ".to_owned()),
            ..pin(Some(Tier::Review), None)
        };
        let blank_symbol = AdviceTier {
            symbol: Some(String::new()),
            ..pin(Some(Tier::Review), None)
        };

        assert!(check_advice_pin(&blank_to).is_err());
        assert!(check_advice_pin(&blank_symbol).is_err());
    }

    fn guard() -> RecommendedGuard {
        RecommendedGuard {
            because: "SC-5".to_owned(),
        }
    }

    fn home(mode: Option<FaceMode>) -> PreserveSymbolHome {
        PreserveSymbolHome {
            symbol: "inferMatches".to_owned(),
            path: "src/differ/inference.ts".to_owned(),
            mode,
            because: "F3".to_owned(),
        }
    }

    fn guarded(guards: Vec<RecommendedGuard>, homes: Vec<PreserveSymbolHome>) -> AssertBlock {
        AssertBlock {
            modes: Vec::new(),
            candidate: 1,
            preserve_dir: Vec::new(),
            keep_together: Vec::new(),
            separate: Vec::new(),
            size_band: Vec::new(),
            move_budget: Vec::new(),
            no_synthetic_bucket: Vec::new(),
            name_alignment: Vec::new(),
            capacity_relief: Vec::new(),
            non_inversion: Vec::new(),
            preserve_symbol_home: homes,
            cycle_import_cut: Vec::new(),
            advice_tier: Vec::new(),
            recommended_guard: guards,
        }
    }

    #[test]
    fn should_accept_one_guard_beside_a_mode_free_pin() {
        assert!(check_guard(&guarded(vec![guard()], vec![home(None)])).is_ok());
        assert!(check_guard(&guarded(Vec::new(), Vec::new())).is_ok());
    }

    #[test]
    fn should_reject_a_guard_with_no_pin_to_enforce() {
        let empty = guarded(vec![guard()], Vec::new());
        let mode_limited = guarded(vec![guard()], vec![home(Some(FaceMode::Anchored))]);

        assert!(check_guard(&empty).is_err_and(|message| message.contains("passes vacuously")));
        assert!(
            check_guard(&mode_limited).is_err_and(|message| message.contains("passes vacuously"))
        );
    }

    #[test]
    fn should_reject_a_duplicate_guard() {
        let duplicate = guarded(vec![guard(), guard()], vec![home(None)]);

        assert!(check_guard(&duplicate).is_err_and(|message| message.contains("more than once")));
    }
}
