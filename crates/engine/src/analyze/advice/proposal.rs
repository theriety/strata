//! The atomic relocation proposals advice reasons over, and their stable keys.

use crate::result::{Candidate, RelocationAdvice, RelocationProposal, SymbolKind};

#[derive(Clone)]
pub(super) struct AdviceProposal {
    pub(super) proposal: RelocationProposal,
    pub(super) subject: String,
    pub(super) destination: String,
}

impl AdviceProposal {
    pub(super) fn key(&self) -> String {
        format!("{}\0{}", self.subject, self.destination)
    }
}

pub(super) fn advice_key(advice: &RelocationAdvice) -> String {
    match &advice.proposal {
        RelocationProposal::File { relocation } => format!(
            "file:{}:{}",
            relocation
                .files
                .first()
                .map_or("", |file| file.path.as_str()),
            advice.destination
        ),
        RelocationProposal::Symbol { relocation } => format!(
            "symbol:{}:{}:{}:{}",
            symbol_kind_key(relocation.kind),
            relocation.from_path,
            relocation.symbol,
            advice.destination
        ),
    }
}

pub(super) fn atomize_candidate(candidate: &Candidate) -> Vec<AdviceProposal> {
    let mut proposals = Vec::new();
    for relocation in &candidate.delta_narration {
        for file in &relocation.files {
            let mut atom = relocation.clone();
            atom.files = vec![file.clone()];
            atom.mirrors
                .retain(|mirror| same_report_identity(&mirror.source_path, &file.path));
            atom.blocked_mirrors
                .retain(|mirror| same_report_identity(&mirror.source_path, &file.path));
            proposals.push(AdviceProposal {
                proposal: RelocationProposal::File { relocation: atom },
                subject: format!("file:{}", file.path),
                destination: relocation.to.clone(),
            });
        }
    }
    for relocation in &candidate.symbol_moves {
        proposals.push(AdviceProposal {
            proposal: RelocationProposal::Symbol {
                relocation: relocation.clone(),
            },
            subject: format!(
                "symbol:{}:{}:{}",
                symbol_kind_key(relocation.kind),
                relocation.from_path,
                relocation.symbol
            ),
            destination: relocation.to_path.clone(),
        });
    }
    proposals.sort_by_key(AdviceProposal::key);
    proposals
}

fn symbol_kind_key(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Symbol => "symbol",
        SymbolKind::Type => "type",
    }
}

fn same_report_identity(left: &str, right: &str) -> bool {
    left == right
}
