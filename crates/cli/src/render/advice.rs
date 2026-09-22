//! Advice, evidence, and move-reason presentation.

use strata_engine::{AnalyzeResult, MoveReason, RelocationProposal, ReviewReason, SymbolKind};

use super::changes::Changes;
use super::{RenderOptions, nq, weight, wrap};

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
        for item in items {
            lines.extend(wrap(
                &format!(
                    "- {} → `{}` · supporting [{}] · qualified [{}] · absent [{}] · conflicts [{}]",
                    advice_subject(&item.proposal, &paths),
                    advice_path(&item.proposal, &item.destination, &paths),
                    profile_names(&item.supporting_profiles),
                    profile_names(&item.qualified_profiles),
                    profile_names(&item.absent_profiles),
                    item.conflicting_destinations
                        .iter()
                        .map(|conflict| format!(
                            "{}→{}",
                            profile_name(conflict.profile),
                            nq(&advice_path(&item.proposal, &conflict.destination, &paths))
                        ))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                3,
                3,
            ));
            for assessment in item.assessments.iter().filter(|_| options.verbose) {
                let evidence = assessment.evidence;
                let best_alternative = assessment.best_alternative.as_deref().map_or_else(
                    || "none".to_owned(),
                    |path| nq(&advice_path(&item.proposal, path, &paths)),
                );
                lines.extend(wrap(&format!(
                    "{}: owner {:.2} · role {:.2} · source {:.2} · destination {:.2} · producer {:.2} · reach {:.2} · margin {:.2}; weighted {:.2}/{:.2} · structural {:.2}/{:.2} · margin threshold {:.2} · qualified {} · best alternative {}",
                    profile_name(assessment.profile), evidence.unique_owner, evidence.role_affinity,
                    evidence.source_cohesion, evidence.destination_cohesion, evidence.producer_evidence,
                    evidence.architectural_reach, assessment.ambiguity_margin, assessment.weighted_score,
                    assessment.thresholds.minimum_evidence, assessment.structural_score,
                    assessment.thresholds.minimum_structural, assessment.thresholds.minimum_ambiguity_margin,
                    assessment.qualified, best_alternative
                ), 5, 5));
            }
            if !item.review_reasons.is_empty() {
                lines.extend(wrap(
                    &format!(
                        "review reasons: {}",
                        item.review_reasons
                            .iter()
                            .map(|reason| review_reason_text(*reason))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    5,
                    5,
                ));
            }
        }
    }
    lines.push(String::new());
    lines
}

fn review_reason_text(reason: ReviewReason) -> &'static str {
    match reason {
        ReviewReason::PartialProfileSupport => "selected by only some executed profiles",
        ReviewReason::ConflictingDestinations => "profiles selected different destinations",
        ReviewReason::WeakEvidence => "destination evidence is below the configured minimum",
        ReviewReason::WeakStructuralEvidence => "structural evidence is insufficient",
        ReviewReason::WeakAmbiguityMargin => {
            "the destination is not sufficiently stronger than the best alternative"
        }
        ReviewReason::NoMajoritySupport => {
            "no strict majority of executed profiles provides qualifying support for this destination"
        }
    }
}

fn profile_name(profile: strata_engine::ProfileName) -> &'static str {
    match profile {
        strata_engine::ProfileName::Anchored => "anchored",
        strata_engine::ProfileName::Greenfield => "greenfield",
    }
}

fn advice_path(proposal: &RelocationProposal, path: &str, paths: &Changes) -> String {
    match proposal {
        RelocationProposal::File { .. } => paths.path(path),
        RelocationProposal::Symbol { .. } => path.to_owned(),
    }
}

fn advice_subject(proposal: &RelocationProposal, paths: &Changes) -> String {
    match proposal {
        RelocationProposal::File { relocation } => relocation.files.first().map_or_else(
            || "file `(unknown)`".to_owned(),
            |file| format!("file `{}`", paths.path(&file.path)),
        ),
        RelocationProposal::Symbol { relocation } => {
            let prefix = if relocation.kind == SymbolKind::Type {
                "type "
            } else {
                "symbol "
            };
            format!(
                "{prefix}`{}` from `{}`",
                relocation.symbol, relocation.from_path
            )
        }
    }
}

fn profile_names(profiles: &[strata_engine::ProfileName]) -> String {
    profiles
        .iter()
        .map(|profile| profile_name(*profile))
        .collect::<Vec<_>>()
        .join(", ")
}
