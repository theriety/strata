//! Advice-tier assertions over the result's cross-profile advice (SC-5).
//!
//! The advice section aggregates every executed profile into one verdict per
//! relocation, so these assertions read `result.advice`, never a candidate
//! tree, and take no mode. An `advice_tier` pin selects moves by the file they
//! leave (and the symbol, for symbol advice), optionally narrowed to one
//! destination, and compares the strongest tier any selected entry holds
//! against its cap or exact tier; a move the result does not advise is
//! `absent`, which satisfies every cap. A `recommended_guard` fails when the
//! `Recommended` advice breaks one of the block's own mode-free
//! `keep_together`, `preserve_dir`, or `preserve_symbol_home` pins, so a
//! falsified move cannot reach the top tier even when no pin names it.
//!
//! The engine atomizes advice: every whole-file entry relocates exactly one
//! file (plus that file's mirrored tests). Whole-file pins are therefore
//! judged over the whole `Recommended` set applied at once, never entry by
//! entry: three single-file entries carrying a `keep_together` trio into one
//! folder keep it together.

use std::collections::{BTreeMap, BTreeSet};

use unlace_engine::result::{Advice, Move, RelocationAdvice, RelocationProposal, SymbolMove};

use crate::harness::Verdict;
use crate::harness::inputs::{EvalInputs, directory_root, packages_holding};
use crate::target::{AdviceTier, AssertBlock, PathSetAssertion, RecommendedGuard, Tier};

/// The folders each file relocated by a `Recommended` whole-file move lands
/// in: more than one when `Recommended` entries disagree on its destination.
type Landings<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;

/// One verdict per `advice_tier` pin, then one per `recommended_guard`.
pub(in crate::harness) fn evaluate_advice(
    block: &AssertBlock,
    inputs: &EvalInputs<'_>,
    advice: &Advice,
) -> Vec<Verdict> {
    let pins = block
        .advice_tier
        .iter()
        .map(|pin| evaluate_advice_tier(pin, advice));
    let guards = block
        .recommended_guard
        .iter()
        .map(|guard| evaluate_recommended_guard(guard, block, inputs, advice));
    pins.chain(guards).collect()
}

/// Every advice path that names neither a census file nor a folder at or
/// under a census folder, each with the field it came from.
///
/// Advice pins select by repo-relative path, so a change in the result's path
/// format (a group prefix, a basename, an absolute path) would make every pin
/// select nothing and pass as `absent`. The caller fails the case on any
/// drifted path instead.
pub(in crate::harness) fn drifted_advice_paths(
    advice: &Advice,
    census: &BTreeSet<String>,
) -> Vec<String> {
    let folders: BTreeSet<&str> = census.iter().flat_map(|file| ancestors(file)).collect();
    let is_file = |path: &str| census.contains(path);
    // A destination may be a folder (or a file) the move creates, so it only
    // has to sit at or under a folder that exists today.
    let is_placed = |path: &str| {
        is_file(path)
            || (path.is_empty() && folders.contains(""))
            || std::iter::once(path)
                .chain(ancestors(path))
                .any(|folder| !folder.is_empty() && folders.contains(folder))
    };
    let mut drifted = Vec::new();
    let mut check = |field: &str, path: &str, known: bool| {
        if !known {
            drifted.push(format!("{field} {path:?}"));
        }
    };
    for entry in advice.recommended.iter().chain(&advice.review_candidates) {
        check(
            "destination",
            &entry.destination,
            is_placed(&entry.destination),
        );
        match &entry.proposal {
            RelocationProposal::File { relocation } => {
                check("to", &relocation.to, is_placed(&relocation.to));
                for file in &relocation.files {
                    check("file", &file.path, is_file(&file.path));
                    check("from", &file.from, is_placed(&file.from));
                }
                for mirror in &relocation.mirrors {
                    check("mirror", &mirror.path, is_file(&mirror.path));
                    check(
                        "mirror source",
                        &mirror.source_path,
                        is_file(&mirror.source_path),
                    );
                    check("mirror to", &mirror.to, is_placed(&mirror.to));
                }
            }
            RelocationProposal::Symbol { relocation } => {
                check(
                    "fromPath",
                    &relocation.from_path,
                    is_file(&relocation.from_path),
                );
                check(
                    "toPath",
                    &relocation.to_path,
                    is_placed(&relocation.to_path),
                );
            }
        }
    }
    drifted
}

/// The proper ancestor folders of `path`, nearest first; a top-level path's
/// only ancestor is the repository root `""`.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |current| {
        (!current.is_empty()).then(|| parent(current))
    })
    .skip(1)
}

/// The direct parent folder of a repo-relative file path.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// Compares the strongest tier the pin's selected moves hold against its cap
/// or exact tier.
fn evaluate_advice_tier(pin: &AdviceTier, advice: &Advice) -> Verdict {
    let recommended = advice
        .recommended
        .iter()
        .filter(|entry| selects(pin, entry))
        .count();
    let review = advice
        .review_candidates
        .iter()
        .filter(|entry| selects(pin, entry))
        .count();
    let observed = if recommended > 0 {
        Tier::Recommended
    } else if review > 0 {
        Tier::Review
    } else {
        Tier::Absent
    };
    let (passed, expected) = match (pin.max_tier, pin.exact_tier) {
        (_, Some(exact)) => (observed == exact, format!("exactly {}", tier_name(exact))),
        (Some(max), None) => (observed <= max, format!("at most {}", tier_name(max))),
        (None, None) => (false, "a tier (none configured)".to_owned()),
    };
    Verdict {
        label: label(pin),
        passed,
        detail: format!(
            "observed {} ({recommended} recommended, {review} review match(es)), expected {expected}; because {}",
            tier_name(observed),
            pin.because
        ),
    }
}

/// Whether `entry` is a move the pin selects. A mirrored test lands in its
/// own `to`, so a pin on the test compares its destination against that.
fn selects(pin: &AdviceTier, entry: &RelocationAdvice) -> bool {
    let landing = match (&entry.proposal, &pin.symbol) {
        (RelocationProposal::File { relocation }, None) => {
            landing_of(relocation, &entry.destination, &pin.path)
        }
        (RelocationProposal::Symbol { relocation }, Some(symbol)) => (relocation.symbol == *symbol
            && relocation.from_path == pin.path)
            .then_some(entry.destination.as_str()),
        _ => None,
    };
    landing.is_some_and(|landing| pin.to.as_ref().is_none_or(|to| to == landing))
}

/// The folder a whole-file move puts `path` in: the entry's destination for
/// a primary, the mirror's own `to` for a mirrored test, `None` when the move
/// does not relocate `path`.
fn landing_of<'a>(relocation: &'a Move, destination: &'a str, path: &str) -> Option<&'a str> {
    if relocation.files.iter().any(|file| file.path == path) {
        return Some(destination);
    }
    relocation
        .mirrors
        .iter()
        .find(|mirror| mirror.path == path)
        .map(|mirror| mirror.to.as_str())
}

/// Every file the `Recommended` whole-file moves relocate, with the folders
/// it lands in.
fn recommended_landings(advice: &Advice) -> Landings<'_> {
    let mut landings: Landings<'_> = BTreeMap::new();
    for entry in &advice.recommended {
        let RelocationProposal::File { relocation } = &entry.proposal else {
            continue;
        };
        for file in &relocation.files {
            landings
                .entry(file.path.as_str())
                .or_default()
                .insert(entry.destination.as_str());
        }
        for mirror in &relocation.mirrors {
            landings
                .entry(mirror.path.as_str())
                .or_default()
                .insert(mirror.to.as_str());
        }
    }
    landings
}

/// The folders `path` may sit in once every `Recommended` move applies: its
/// landings when one relocates it, its current parent otherwise.
fn folders_after<'a>(path: &'a str, landings: &Landings<'a>) -> BTreeSet<&'a str> {
    landings
        .get(path)
        .cloned()
        .unwrap_or_else(|| BTreeSet::from([parent(path)]))
}

/// Fails when the `Recommended` advice, applied as a whole, breaks one of the
/// block's mode-free pins. A pin with its own `mode` judges one profile's
/// candidate, while advice folds every profile together, so the guard skips
/// it.
fn evaluate_recommended_guard(
    guard: &RecommendedGuard,
    block: &AssertBlock,
    inputs: &EvalInputs<'_>,
    advice: &Advice,
) -> Verdict {
    let landings = recommended_landings(advice);
    let mut breaches: Vec<String> = block
        .keep_together
        .iter()
        .filter(|set| set.mode.is_none())
        .filter_map(|set| keep_together_breach(set, &landings))
        .collect();
    breaches.extend(preserve_dir_breaches(block, inputs, &landings));
    breaches.extend(
        advice
            .recommended
            .iter()
            .flat_map(|entry| match &entry.proposal {
                RelocationProposal::Symbol { relocation } => symbol_breaches(relocation, block),
                RelocationProposal::File { .. } => Vec::new(),
            }),
    );
    let skipped = mode_limited_pins(block);
    let detail = if breaches.is_empty() {
        format!(
            "{} recommended move(s), none breaks a pin ({skipped} mode-limited pin(s) skipped)",
            advice.recommended.len()
        )
    } else {
        breaches.join("; ")
    };
    Verdict {
        label: "recommended_guard".to_owned(),
        passed: breaches.is_empty(),
        detail: format!("{detail}; because {}", guard.because),
    }
}

/// How many of the block's guardable pins carry their own `mode`.
fn mode_limited_pins(block: &AssertBlock) -> usize {
    let keep = block.keep_together.iter().filter(|set| set.mode.is_some());
    let dirs = block.preserve_dir.iter().filter(|dir| dir.mode.is_some());
    let homes = block
        .preserve_symbol_home
        .iter()
        .filter(|home| home.mode.is_some());
    keep.count() + dirs.count() + homes.count()
}

/// A `keep_together` set breaks when its members share one direct parent
/// folder today and no longer do once every `Recommended` move applies. A
/// set already scattered has no co-location to break, and a move gathering
/// it is no breach.
fn keep_together_breach(set: &PathSetAssertion, landings: &Landings<'_>) -> Option<String> {
    let mut before = set.paths.iter().map(|path| parent(path));
    let shared = before.next()?;
    if before.any(|folder| folder != shared) {
        return None;
    }
    let after: Vec<(&str, BTreeSet<&str>)> = set
        .paths
        .iter()
        .map(|path| (path.as_str(), folders_after(path, landings)))
        .collect();
    let folders: BTreeSet<&str> = after
        .iter()
        .flat_map(|(_, landed)| landed.clone())
        .collect();
    if folders.len() <= 1 {
        return None;
    }
    let placements: Vec<String> = after
        .iter()
        .map(|(path, landed)| {
            let joined: Vec<&str> = landed.iter().copied().collect();
            format!("{path} -> {}", joined.join(" | "))
        })
        .collect();
    Some(format!(
        "recommended moves split keep_together members sharing {shared}: {}",
        placements.join(", ")
    ))
}

/// The mode-free `preserve_dir` pins the `Recommended` moves break: a member
/// of the directory lands outside it.
fn preserve_dir_breaches(
    block: &AssertBlock,
    inputs: &EvalInputs<'_>,
    landings: &Landings<'_>,
) -> Vec<String> {
    let mut breaches = Vec::new();
    for preserve in block.preserve_dir.iter().filter(|dir| dir.mode.is_none()) {
        for (package, members) in packages_holding(&inputs.census, &inputs.packages, &preserve.path)
        {
            for file in &members {
                let (Some(root), Some(folders)) = (
                    directory_root(package.prefix.as_deref(), file, &preserve.path),
                    landings.get(file.as_str()),
                ) else {
                    continue;
                };
                for folder in folders {
                    let inside = *folder == root
                        || folder
                            .strip_prefix(root)
                            .is_some_and(|tail| tail.starts_with('/'));
                    if !inside {
                        breaches.push(format!(
                            "recommended move of {file} to {folder} pulls it out of preserve_dir {root}"
                        ));
                    }
                }
            }
        }
    }
    breaches
}

/// The mode-free `preserve_symbol_home` pins a recommended symbol move breaks.
fn symbol_breaches(relocation: &SymbolMove, block: &AssertBlock) -> Vec<String> {
    block
        .preserve_symbol_home
        .iter()
        .filter(|home| {
            home.mode.is_none()
                && home.symbol == relocation.symbol
                && home.path == relocation.from_path
        })
        .map(|home| {
            format!(
                "recommended move of {} from {} to {} breaks its preserve_symbol_home",
                home.symbol, home.path, relocation.to_path
            )
        })
        .collect()
}

/// The TOML spelling of a tier.
fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Absent => "absent",
        Tier::Review => "review",
        Tier::Recommended => "recommended",
    }
}

/// A stable label like `advice_tier(shape.ts->src/tree)` or
/// `advice_tier(inferMatches@inference.ts)`.
fn label(pin: &AdviceTier) -> String {
    let file = pin.path.rsplit('/').next().unwrap_or(&pin.path);
    let origin = pin
        .symbol
        .as_ref()
        .map_or_else(|| file.to_owned(), |symbol| format!("{symbol}@{file}"));
    let destination = pin
        .to
        .as_ref()
        .map(|to| format!("->{to}"))
        .unwrap_or_default();
    format!("advice_tier({origin}{destination})")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use unlace_engine::config::ProfileName;
    use unlace_engine::result::{
        Advice, FileMove, MirrorMove, Move, MoveKind, MoveReason, RelocationAdvice,
        RelocationProposal, ReviewReason, SymbolKind, SymbolMove,
    };

    use super::{drifted_advice_paths, evaluate_advice};
    use crate::harness::inputs::{EvalInputs, PackageScope};
    use crate::target::{
        AdviceTier, AssertBlock, FaceMode, PathSetAssertion, PreserveDir, PreserveSymbolHome,
        RecommendedGuard, Tier,
    };

    const VALIDATION: &str = "src/tree/validation";
    const SHAPE: &str = "src/tree/validation/shape.ts";
    const RECORD: &str = "src/tree/validation/record.ts";
    const CONTENT: &str = "src/tree/validation/content.ts";
    const SHAPE_TEST: &str = "src/tree/validation/shape.test.ts";
    const OWNER: &str = "src/tree/validation.ts";
    const INFERENCE: &str = "src/differ/inference.ts";
    const MATCHER: &str = "src/differ/matcher.ts";

    /// One atomized whole-file entry, as `atomize_candidate` emits it: a
    /// single primary file moving from its parent folder to `to`.
    fn file_move(path: &str, to: &str) -> RelocationAdvice {
        let from = path.rsplit_once('/').map_or("", |(folder, _)| folder);
        entry(
            RelocationProposal::File {
                relocation: Move {
                    kind: MoveKind::Move,
                    files: vec![FileMove {
                        path: path.to_owned(),
                        from: from.to_owned(),
                    }],
                    to: to.to_owned(),
                    reason: MoveReason::Clustering,
                    mirrors: Vec::new(),
                },
            },
            to,
        )
    }

    /// The mdc F1 move as 1.0.0 reported it: `shape.ts` hoisted into `src/tree`.
    fn hoist_shape() -> RelocationAdvice {
        file_move(SHAPE, "src/tree")
    }

    /// The mdc F3 move: `inferMatches` leaving `inference.ts` for `matcher.ts`.
    fn move_infer_matches() -> RelocationAdvice {
        entry(
            RelocationProposal::Symbol {
                relocation: SymbolMove {
                    symbol: "inferMatches".to_owned(),
                    kind: SymbolKind::Symbol,
                    from_path: INFERENCE.to_owned(),
                    to_path: MATCHER.to_owned(),
                    delta: -1.0,
                    broken_imports: 0,
                    terms: None,
                    fold: None,
                    display: None,
                },
            },
            MATCHER,
        )
    }

    /// A `Recommended`-shaped entry: both profiles support and qualify it and
    /// it carries no review reason.
    fn entry(proposal: RelocationProposal, destination: &str) -> RelocationAdvice {
        RelocationAdvice {
            proposal,
            destination: destination.to_owned(),
            supporting_profiles: vec![ProfileName::Anchored, ProfileName::Greenfield],
            qualified_profiles: vec![ProfileName::Anchored, ProfileName::Greenfield],
            absent_profiles: Vec::new(),
            conflicting_destinations: Vec::new(),
            assessments: Vec::new(),
            review_reasons: Vec::new(),
        }
    }

    fn recommending(entries: Vec<RelocationAdvice>) -> Advice {
        Advice {
            recommended: entries,
            review_candidates: Vec::new(),
        }
    }

    /// A `Review candidate`: no profile qualifies it, for weak evidence.
    fn reviewing(entry: RelocationAdvice) -> Advice {
        Advice {
            recommended: Vec::new(),
            review_candidates: vec![RelocationAdvice {
                qualified_profiles: Vec::new(),
                review_reasons: vec![ReviewReason::WeakEvidence],
                ..entry
            }],
        }
    }

    fn pin(symbol: Option<&str>, path: &str, to: Option<&str>) -> AdviceTier {
        AdviceTier {
            path: path.to_owned(),
            symbol: symbol.map(str::to_owned),
            to: to.map(str::to_owned),
            max_tier: Some(Tier::Review),
            exact_tier: None,
            because: "F1".to_owned(),
        }
    }

    fn block() -> AssertBlock {
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
            preserve_symbol_home: Vec::new(),
            cycle_import_cut: Vec::new(),
            advice_tier: Vec::new(),
            recommended_guard: Vec::new(),
        }
    }

    fn census() -> BTreeSet<String> {
        BTreeSet::from([
            SHAPE, RECORD, CONTENT, SHAPE_TEST, OWNER, INFERENCE, MATCHER,
        ])
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    /// A single repository-rooted package holding the companions.
    fn inputs() -> EvalInputs<'static> {
        EvalInputs {
            current_placement: BTreeMap::new(),
            packages: vec![PackageScope {
                name: "companion-folders".to_owned(),
                prefix: None,
            }],
            census: census(),
            faces: BTreeMap::new(),
        }
    }

    fn with_pins(pins: Vec<AdviceTier>) -> AssertBlock {
        AssertBlock {
            advice_tier: pins,
            ..block()
        }
    }

    fn passes(block: &AssertBlock, advice: &Advice) -> Vec<bool> {
        evaluate_advice(block, &inputs(), advice)
            .iter()
            .map(|verdict| verdict.passed)
            .collect()
    }

    fn guard_detail(block: &AssertBlock, advice: &Advice) -> Option<(bool, String)> {
        evaluate_advice(block, &inputs(), advice)
            .into_iter()
            .find(|verdict| verdict.label == "recommended_guard")
            .map(|verdict| (verdict.passed, verdict.detail))
    }

    #[test]
    fn should_fail_a_review_cap_when_the_f1_move_is_recommended() {
        let block = with_pins(vec![pin(None, SHAPE, Some("src/tree"))]);

        let verdicts = evaluate_advice(&block, &inputs(), &recommending(vec![hoist_shape()]));

        assert_eq!(verdicts.len(), 1);
        let verdict = verdicts.first();
        assert!(verdict.is_some_and(|verdict| {
            !verdict.passed
                && verdict.label == "advice_tier(shape.ts->src/tree)"
                && verdict.detail.starts_with(
                    "observed recommended (1 recommended, 0 review match(es)), expected at most review",
                )
        }));
    }

    #[test]
    fn should_pass_a_review_cap_when_the_f1_move_is_a_review_candidate_or_absent() {
        let block = with_pins(vec![pin(None, SHAPE, None)]);

        assert_eq!(passes(&block, &reviewing(hoist_shape())), vec![true]);
        assert_eq!(passes(&block, &Advice::default()), vec![true]);
    }

    #[test]
    fn should_select_by_destination_and_by_grain() {
        let elsewhere = with_pins(vec![pin(None, SHAPE, Some("src"))]);
        let as_symbol = with_pins(vec![pin(Some("shape"), SHAPE, None)]);
        let any_destination = with_pins(vec![pin(None, SHAPE, None)]);

        let advice = recommending(vec![hoist_shape()]);

        assert_eq!(passes(&elsewhere, &advice), vec![true]);
        assert_eq!(passes(&as_symbol, &advice), vec![true]);
        assert_eq!(passes(&any_destination, &advice), vec![false]);
    }

    #[test]
    fn should_compare_a_mirrored_test_pin_against_the_test_own_destination() {
        let mut hoist = hoist_shape();
        if let RelocationProposal::File { relocation } = &mut hoist.proposal {
            relocation.mirrors = vec![MirrorMove {
                source_path: SHAPE.to_owned(),
                path: SHAPE_TEST.to_owned(),
                from: VALIDATION.to_owned(),
                to: "src/tree/__tests__".to_owned(),
            }];
        }
        let advice = recommending(vec![hoist]);
        let at_test_destination =
            with_pins(vec![pin(None, SHAPE_TEST, Some("src/tree/__tests__"))]);
        let at_primary_destination = with_pins(vec![pin(None, SHAPE_TEST, Some("src/tree"))]);

        assert_eq!(passes(&at_test_destination, &advice), vec![false]);
        assert_eq!(passes(&at_primary_destination, &advice), vec![true]);
    }

    #[test]
    fn should_cap_a_symbol_move_and_honor_absent_and_exact_tiers() {
        let review_cap = pin(Some("inferMatches"), INFERENCE, None);
        let absent_cap = AdviceTier {
            max_tier: Some(Tier::Absent),
            ..pin(Some("inferMatches"), INFERENCE, None)
        };
        let exact_review = AdviceTier {
            max_tier: None,
            exact_tier: Some(Tier::Review),
            ..pin(Some("inferMatches"), INFERENCE, None)
        };
        let block = with_pins(vec![review_cap, absent_cap, exact_review]);

        assert_eq!(
            passes(&block, &recommending(vec![move_infer_matches()])),
            vec![false, false, false]
        );
        assert_eq!(
            passes(&block, &reviewing(move_infer_matches())),
            vec![true, false, true]
        );
        assert_eq!(passes(&block, &Advice::default()), vec![true, true, false]);
    }

    fn trio() -> PathSetAssertion {
        PathSetAssertion {
            paths: vec![SHAPE.to_owned(), RECORD.to_owned(), CONTENT.to_owned()],
            mode: None,
            because: "F1".to_owned(),
        }
    }

    fn guarded() -> AssertBlock {
        AssertBlock {
            keep_together: vec![trio()],
            preserve_dir: vec![PreserveDir {
                path: "tree/validation".to_owned(),
                mode: None,
                because: "F1".to_owned(),
            }],
            preserve_symbol_home: vec![PreserveSymbolHome {
                symbol: "inferMatches".to_owned(),
                path: INFERENCE.to_owned(),
                mode: None,
                because: "F3".to_owned(),
            }],
            recommended_guard: vec![RecommendedGuard {
                because: "SC-5".to_owned(),
            }],
            ..block()
        }
    }

    #[test]
    fn should_fail_the_guard_on_a_recommended_move_breaking_any_pin() {
        let file = guard_detail(&guarded(), &recommending(vec![hoist_shape()]));
        let symbol = guard_detail(&guarded(), &recommending(vec![move_infer_matches()]));

        assert!(file.is_some_and(|(passed, detail)| {
            !passed
                && detail.contains("split keep_together members sharing src/tree/validation")
                && detail.contains("src/tree/validation/shape.ts -> src/tree,")
                && detail.contains("src/tree/validation/record.ts -> src/tree/validation")
                && detail.contains("pulls it out of preserve_dir src/tree/validation")
        }));
        assert!(symbol.is_some_and(|(passed, detail)| {
            !passed && detail.contains("inferMatches from src/differ/inference.ts")
        }));
    }

    #[test]
    fn should_pass_the_guard_when_the_breaking_move_is_only_a_review_candidate() {
        assert_eq!(passes(&guarded(), &reviewing(hoist_shape())), vec![true]);
        assert_eq!(
            passes(&guarded(), &reviewing(move_infer_matches())),
            vec![true]
        );
    }

    #[test]
    fn should_pass_the_guard_when_atomized_entries_carry_a_whole_set_together() {
        let rules = "src/tree/validation/rules";
        let advice = recommending(
            [SHAPE, RECORD, CONTENT]
                .map(|path| file_move(path, rules))
                .to_vec(),
        );

        assert_eq!(passes(&guarded(), &advice), vec![true]);
    }

    #[test]
    fn should_fail_the_guard_when_atomized_entries_carry_only_part_of_a_set() {
        let rules = "src/tree/validation/rules";
        let advice = recommending([SHAPE, RECORD].map(|path| file_move(path, rules)).to_vec());

        let verdict = guard_detail(&guarded(), &advice);

        assert!(verdict.is_some_and(|(passed, detail)| {
            !passed
                && detail.contains("src/tree/validation/content.ts -> src/tree/validation")
                && detail.contains("src/tree/validation/shape.ts -> src/tree/validation/rules")
                && !detail.contains("preserve_dir")
        }));
    }

    #[test]
    fn should_pass_the_guard_when_a_move_gathers_a_scattered_set() {
        let scattered = AssertBlock {
            keep_together: vec![PathSetAssertion {
                paths: vec![SHAPE.to_owned(), OWNER.to_owned()],
                mode: None,
                because: "gathered".to_owned(),
            }],
            recommended_guard: vec![RecommendedGuard {
                because: "SC-5".to_owned(),
            }],
            ..block()
        };

        let gather = recommending(vec![file_move(OWNER, VALIDATION)]);

        assert_eq!(passes(&scattered, &gather), vec![true]);
    }

    #[test]
    fn should_fail_the_guard_when_recommended_entries_disagree_on_a_member() {
        let advice = recommending(vec![
            file_move(SHAPE, VALIDATION),
            file_move(SHAPE, "src/tree/validation/rules"),
            file_move(RECORD, VALIDATION),
        ]);

        let verdict = guard_detail(&guarded(), &advice);

        assert!(verdict.is_some_and(|(passed, detail)| {
            !passed
                && detail.contains("shape.ts -> src/tree/validation | src/tree/validation/rules")
        }));
    }

    #[test]
    fn should_judge_a_mirrored_test_by_its_own_destination() {
        let guarded_test = AssertBlock {
            keep_together: vec![PathSetAssertion {
                paths: vec![SHAPE.to_owned(), SHAPE_TEST.to_owned()],
                mode: None,
                because: "a test stays beside its subject".to_owned(),
            }],
            ..guarded()
        };
        let mirrored = |test_to: &str| {
            let mut hoist = file_move(SHAPE, "src/tree/validation/rules");
            if let RelocationProposal::File { relocation } = &mut hoist.proposal {
                relocation.mirrors = vec![MirrorMove {
                    source_path: SHAPE.to_owned(),
                    path: SHAPE_TEST.to_owned(),
                    from: VALIDATION.to_owned(),
                    to: test_to.to_owned(),
                }];
            }
            recommending(
                [hoist]
                    .into_iter()
                    .chain(
                        [RECORD, CONTENT].map(|path| file_move(path, "src/tree/validation/rules")),
                    )
                    .collect(),
            )
        };

        assert_eq!(
            passes(&guarded_test, &mirrored("src/tree/validation/rules")),
            vec![true]
        );
        assert_eq!(
            passes(&guarded_test, &mirrored("src/tree/validation/__tests__")),
            vec![false]
        );
    }

    #[test]
    fn should_skip_mode_limited_pins_in_the_guard() {
        let limited = AssertBlock {
            keep_together: vec![PathSetAssertion {
                mode: Some(FaceMode::Anchored),
                ..trio()
            }],
            preserve_dir: Vec::new(),
            preserve_symbol_home: vec![PreserveSymbolHome {
                symbol: "inferMatches".to_owned(),
                path: INFERENCE.to_owned(),
                mode: Some(FaceMode::Greenfield),
                because: "F3".to_owned(),
            }],
            ..guarded()
        };

        let verdict = guard_detail(
            &limited,
            &recommending(vec![hoist_shape(), move_infer_matches()]),
        );

        assert!(verdict.is_some_and(|(passed, detail)| {
            passed && detail.contains("(2 mode-limited pin(s) skipped)")
        }));
    }

    #[test]
    fn should_accept_census_paths_and_new_folders_under_the_census() {
        let mut advice = recommending(vec![
            file_move(SHAPE, "src/tree/validation/rules"),
            move_infer_matches(),
        ]);
        advice.review_candidates.push(file_move(RECORD, "src/tree"));

        assert!(drifted_advice_paths(&advice, &census()).is_empty());
    }

    #[test]
    fn should_report_every_advice_path_outside_the_census() {
        let prefixed = file_move(
            "companion-folders/src/tree/validation/shape.ts",
            "companion-folders/src/tree",
        );
        let mut basename = move_infer_matches();
        if let RelocationProposal::Symbol { relocation } = &mut basename.proposal {
            relocation.from_path = "inference.ts".to_owned();
        }

        let drifted = drifted_advice_paths(&recommending(vec![prefixed, basename]), &census());

        assert_eq!(
            drifted,
            vec![
                "destination \"companion-folders/src/tree\"".to_owned(),
                "to \"companion-folders/src/tree\"".to_owned(),
                "file \"companion-folders/src/tree/validation/shape.ts\"".to_owned(),
                "from \"companion-folders/src/tree/validation\"".to_owned(),
                "fromPath \"inference.ts\"".to_owned(),
            ]
        );
    }
}
