//! Advice, evidence, and move-reason presentation.

use std::cmp::Reverse;

use strata_engine::{AnalyzeResult, MoveReason};

use self::entry::group;
use super::changes::Changes;
use super::limit::{more_line, take_worst};
use super::{RenderOptions, nq, weight, wrap};

mod entry;

/// Builds a suggestion caption from the move's dominant reason.
///
/// Every caption opens with the `Why:` anchor and quotes the name tokens it
/// cites; the clustering fallback speaks of import-heavy regrouping, and the
/// naming-cohesion reason (which the approved artifact never had occasion to
/// print) borrows the term-gloss language for names fitting contents.
pub(super) fn caption_for(reason: &MoveReason) -> String {
    match reason {
        MoveReason::PulledBy {
            partner,
            weight: pull,
        } => {
            format!(
                "Why: pulled toward {} — weight {}.",
                nq(partner),
                weight(*pull)
            )
        }
        MoveReason::RelievesOverCap {
            container,
            count,
            cap,
        } => format!(
            "Why: relieves {}, which holds {count} against a cap of {cap}.",
            nq(container)
        ),
        MoveReason::Follows { subject } => {
            format!(
                "Why: follows {}, which this plan places nearby.",
                nq(subject)
            )
        }
        MoveReason::NamingCohesion { .. } => {
            "Why: clusters files whose names fit their destination.".to_owned()
        }
        MoveReason::Clustering => {
            "Why: clusters files that already import each other heavily.".to_owned()
        }
    }
}

pub(super) fn advice_with_options(result: &AnalyzeResult, options: RenderOptions) -> Vec<String> {
    let paths = Changes::paths(result);
    let mut lines = Vec::new();
    let has_assessments = result
        .advice
        .recommended
        .iter()
        .chain(&result.advice.review_candidates)
        .any(|item| !item.assessments.is_empty());
    if has_assessments {
        lines.extend(wrap(
            "Profiles share analysis-start evidence but apply their own weights and thresholds.",
            1,
            1,
        ));
    }
    if options.verbose && has_assessments {
        lines.extend(wrap(
            "Evidence: owner means unique ownership; role means role affinity; source and destination mean cohesion at each side; producer means producer evidence; reach means architectural reach.",
            1,
            1,
        ));
        lines.extend(wrap(
            "Weighted is the normalized score across all six signals; structural excludes role affinity; margin is the selected destination's lead over the best alternative.",
            1,
            1,
        ));
        lines.extend(wrap(
            "The values after weighted and structural, and the margin threshold, are configured minimums.",
            1,
            1,
        ));
    }
    for (heading, items) in [
        ("Recommended", result.advice.recommended.as_slice()),
        (
            "Review candidate",
            result.advice.review_candidates.as_slice(),
        ),
    ] {
        lines.push(format!(" {heading} ({}):", items.len()));
        let entries = group(items, &paths, options);
        let (shown, _) = take_worst(&entries, options, |entry| {
            entry
                .members()
                .iter()
                .map(|item| {
                    (
                        Reverse(item.supporting_profiles.len()),
                        Reverse(item.qualified_profiles.len()),
                        item.conflicting_destinations.len(),
                    )
                })
                .min()
        });
        let shown_items: usize = shown.iter().map(|entry| entry.members().len()).sum();
        for entry in shown {
            lines.extend(entry.lines());
        }
        lines.extend(more_line(items.len() - shown_items, 3));
    }
    lines.push(String::new());
    lines
}
