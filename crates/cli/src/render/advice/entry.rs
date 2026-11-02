//! One displayed advice entry: a single item, or symbol items merged because
//! everything they print except the symbol name is identical.

use unlace_engine::{
    ProfileName, RelocationAdvice, RelocationProposal, ReviewReason, SymbolKind, SymbolMove,
};

use super::super::changes::Changes;
use super::super::{RenderOptions, nq, wrap};

/// The symbol-specific part of an entry's subject.
struct Named {
    label: String,
    is_type: bool,
}

/// What an entry prints besides its subject; the merge key for symbols.
#[derive(PartialEq)]
struct Body {
    from: String,
    tail: String,
    detail: Vec<String>,
}

/// One displayed advice entry and the items it stands for.
pub(super) struct Entry<'a> {
    members: Vec<&'a RelocationAdvice>,
    /// The subject exactly as a lone item prints it.
    subject: String,
    /// Symbol names in first-appearance order; empty for a file entry.
    names: Vec<Named>,
    /// `None` for a file entry, which never merges.
    body: Option<Body>,
    tail: String,
    detail: Vec<String>,
}

impl<'a> Entry<'a> {
    /// The advice items this entry displays.
    pub(super) fn members(&self) -> &[&'a RelocationAdvice] {
        &self.members
    }

    /// Renders the entry; a one-item entry prints exactly as a lone item does.
    pub(super) fn lines(&self) -> Vec<String> {
        let subject = if self.members.len() == 1 {
            self.subject.clone()
        } else {
            self.group_subject()
        };
        let mut lines = wrap(&format!("- {subject} {}", self.tail), 3, 3);
        lines.extend(self.detail.iter().cloned());
        lines
    }

    fn group_subject(&self) -> String {
        let kind = if self.names.iter().all(|name| name.is_type) {
            "types"
        } else {
            "symbols"
        };
        let names = self
            .names
            .iter()
            .map(|name| nq(&name.label))
            .collect::<Vec<_>>()
            .join(", ");
        let from = self.body.as_ref().map_or("", |body| body.from.as_str());
        format!("{} {kind} {names} from `{from}`", self.names.len())
    }
}

/// Groups `items` into entries, keeping first-appearance order.
pub(super) fn group<'a>(
    items: &'a [RelocationAdvice],
    paths: &Changes,
    options: RenderOptions,
) -> Vec<Entry<'a>> {
    let mut entries: Vec<Entry<'a>> = Vec::new();
    for item in items {
        let tail = tail_text(item, paths);
        let detail = detail_lines(item, paths, options);
        let (subject, named, from) = match &item.proposal {
            RelocationProposal::File { relocation } => (
                relocation.files.first().map_or_else(
                    || "file `(unknown)`".to_owned(),
                    |file| format!("file `{}`", paths.path(&file.path)),
                ),
                None,
                None,
            ),
            RelocationProposal::Symbol { relocation } => (
                symbol_subject(relocation),
                Some(Named {
                    label: relocation.label().to_owned(),
                    is_type: relocation.kind == SymbolKind::Type,
                }),
                Some(relocation.from_path.clone()),
            ),
        };
        let body = from.map(|from| Body {
            from,
            tail: tail.clone(),
            detail: detail.clone(),
        });
        let target = body.as_ref().and_then(|body| {
            entries
                .iter_mut()
                .find(|entry| entry.body.as_ref() == Some(body))
        });
        match (target, named) {
            (Some(entry), Some(named)) => {
                entry.members.push(item);
                entry.names.push(named);
            }
            (_, named) => entries.push(Entry {
                members: vec![item],
                subject,
                names: named.into_iter().collect(),
                body,
                tail,
                detail,
            }),
        }
    }
    entries
}

fn symbol_subject(relocation: &SymbolMove) -> String {
    let prefix = if relocation.kind == SymbolKind::Type {
        "type "
    } else {
        "symbol "
    };
    format!(
        "{prefix}`{}` from `{}`",
        relocation.label(),
        relocation.from_path
    )
}

/// The destination and profile summary that follows the subject.
fn tail_text(item: &RelocationAdvice, paths: &Changes) -> String {
    format!(
        "→ `{}` · supporting [{}] · qualified [{}] · absent [{}] · conflicts [{}]",
        advice_path(&item.proposal, &item.destination, paths),
        profile_names(&item.supporting_profiles),
        profile_names(&item.qualified_profiles),
        profile_names(&item.absent_profiles),
        item.conflicting_destinations
            .iter()
            .map(|conflict| format!(
                "{}→{}",
                profile_name(conflict.profile),
                nq(&advice_path(&item.proposal, &conflict.destination, paths))
            ))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The verbose evidence lines and review reasons that follow the entry head.
fn detail_lines(item: &RelocationAdvice, paths: &Changes, options: RenderOptions) -> Vec<String> {
    let mut lines = Vec::new();
    for assessment in item.assessments.iter().filter(|_| options.verbose) {
        let evidence = assessment.evidence;
        let best_alternative = assessment.best_alternative.as_deref().map_or_else(
            || "none".to_owned(),
            |path| nq(&advice_path(&item.proposal, path, paths)),
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

fn profile_name(profile: ProfileName) -> &'static str {
    match profile {
        ProfileName::Anchored => "anchored",
        ProfileName::Greenfield => "greenfield",
    }
}

fn advice_path(proposal: &RelocationProposal, path: &str, paths: &Changes) -> String {
    match proposal {
        RelocationProposal::File { .. } => paths.path(path),
        RelocationProposal::Symbol { .. } => path.to_owned(),
    }
}

fn profile_names(profiles: &[ProfileName]) -> String {
    profiles
        .iter()
        .map(|profile| profile_name(*profile))
        .collect::<Vec<_>>()
        .join(", ")
}
