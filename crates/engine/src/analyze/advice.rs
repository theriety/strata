use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{
    AffinityKind, Container, ContainerId, EdgeKind, IntermediateRepresentation, Node, NodeId,
    NodeKind, Polarity, ScopeLevel, Snapshot,
};

use crate::config::{AnalyzeConfig, ProfileConfig, ProfileName};
use crate::narrate::tokenize;
use crate::result::{
    Advice, Candidate, EvidenceSignals, ProfileAssessment, ProfileConflict, Profiles,
    QualificationThresholds, RelocationAdvice, RelocationProposal, ReviewReason, SymbolKind,
};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(in crate::analyze) struct AdviceProposal {
    proposal: RelocationProposal,
    subject: String,
    destination: String,
}

impl AdviceProposal {
    fn key(&self) -> String {
        format!("{}\0{}", self.subject, self.destination)
    }
}

#[allow(clippy::too_many_lines)]
pub(in crate::analyze) fn build_advice(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    profiles: &Profiles,
) -> Advice {
    let index = EvidenceIndex::new(snapshot);
    let executed = [
        (ProfileName::Anchored, profiles.anchored.as_ref()),
        (ProfileName::Greenfield, profiles.greenfield.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, result)| result.map(|result| (name, result)))
    .collect::<Vec<_>>();
    let mut selections = BTreeMap::<ProfileName, Vec<AdviceProposal>>::new();
    let mut union = BTreeMap::<String, AdviceProposal>::new();
    for (name, result) in &executed {
        let proposals = result
            .candidates
            .first()
            .map_or_else(Vec::new, atomize_candidate);
        for proposal in &proposals {
            union
                .entry(proposal.key())
                .or_insert_with(|| proposal.clone());
        }
        selections.insert(*name, proposals);
    }
    let mut advice = Advice::default();
    for proposal in union.into_values() {
        let mut supporting_profiles = Vec::new();
        let mut qualified_profiles = Vec::new();
        let mut absent_profiles = Vec::new();
        let mut conflicting_destinations = Vec::new();
        let mut assessments = Vec::new();
        for (profile_name, _) in &executed {
            let selected = selections
                .get(profile_name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if let Some(same) = selected.iter().find(|item| item.key() == proposal.key()) {
                supporting_profiles.push(*profile_name);
                let assessment = index.assess(same, *profile_name, config.profile(*profile_name));
                if assessment.qualified {
                    qualified_profiles.push(*profile_name);
                }
                assessments.push(assessment);
            } else if let Some(conflict) = selected
                .iter()
                .find(|item| item.subject == proposal.subject)
            {
                conflicting_destinations.push(ProfileConflict {
                    profile: *profile_name,
                    destination: conflict.destination.clone(),
                });
            } else {
                absent_profiles.push(*profile_name);
            }
        }
        let majority = qualified_profiles.len() * 2 > executed.len();
        let mut review_reasons = BTreeSet::new();
        if supporting_profiles.len() != executed.len() {
            review_reasons.insert(ReviewReason::PartialProfileSupport);
        }
        if !conflicting_destinations.is_empty() {
            review_reasons.insert(ReviewReason::ConflictingDestinations);
        }
        for assessment in &assessments {
            if assessment.weighted_score < assessment.thresholds.minimum_evidence {
                review_reasons.insert(ReviewReason::WeakEvidence);
            }
            if assessment.structural_score <= 0.0
                || assessment.structural_score < assessment.thresholds.minimum_structural
            {
                review_reasons.insert(ReviewReason::WeakStructuralEvidence);
            }
            if assessment.ambiguity_margin < assessment.thresholds.minimum_ambiguity_margin {
                review_reasons.insert(ReviewReason::WeakAmbiguityMargin);
            }
        }
        if !majority {
            review_reasons.insert(ReviewReason::NoMajoritySupport);
        }
        let item = RelocationAdvice {
            proposal: proposal.proposal,
            destination: proposal.destination,
            supporting_profiles,
            qualified_profiles,
            absent_profiles,
            conflicting_destinations,
            assessments,
            review_reasons: review_reasons.into_iter().collect(),
        };
        if majority {
            advice.recommended.push(item);
        } else {
            advice.review_candidates.push(item);
        }
    }
    advice.recommended.sort_by_key(advice_key);
    advice.review_candidates.sort_by_key(advice_key);
    advice
}

pub(in crate::analyze) fn advice_key(advice: &RelocationAdvice) -> String {
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

pub(in crate::analyze) fn atomize_candidate(candidate: &Candidate) -> Vec<AdviceProposal> {
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

pub(in crate::analyze) fn symbol_kind_key(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Symbol => "symbol",
        SymbolKind::Type => "type",
    }
}

pub(in crate::analyze) fn same_report_identity(left: &str, right: &str) -> bool {
    left == right
}

pub(in crate::analyze) struct EvidenceIndex<'a> {
    ir: &'a IntermediateRepresentation,
    containers: BTreeMap<ContainerId, &'a Container>,
    nodes: BTreeMap<NodeId, &'a Node>,
    dataset_root: Option<String>,
    files_by_path: BTreeMap<String, Option<ContainerId>>,
    folders_by_path: BTreeMap<String, Option<ContainerId>>,
    file_paths_by_id: BTreeMap<ContainerId, Option<String>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::analyze) enum EvidencePlace {
    Container(ContainerId),
    PhysicalFolder {
        container: ContainerId,
        path: String,
    },
    ConceptualFolder(String),
}

impl<'a> EvidenceIndex<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let ir = snapshot.ir();
        let dataset_roots = ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::PackageGroup)
            .map(|container| container.name.to_string())
            .collect::<BTreeSet<_>>();
        let dataset_root = (dataset_roots.len() == 1)
            .then(|| dataset_roots.into_iter().next())
            .flatten();
        let mut files_by_path = BTreeMap::new();
        let mut folders_by_path = BTreeMap::new();
        let mut file_paths_by_id = BTreeMap::new();
        for container in ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
        {
            files_by_path
                .entry(container.name.to_string())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(container.id));
            insert_unique_path(&mut file_paths_by_id, container.id, &container.name);
            if let Some(parent) = container.parent {
                let folder = container
                    .name
                    .rsplit_once('/')
                    .map_or("", |(folder, _)| folder);
                // Physical folders are derived from immutable file facts, not semantic scope
                // levels: a physical directory may be represented by a Domain or Package node.
                folders_by_path
                    .entry(folder.to_owned())
                    .and_modify(|entry| {
                        if *entry != Some(parent) {
                            *entry = None;
                        }
                    })
                    .or_insert(Some(parent));
            }
        }
        Self {
            ir,
            containers: ir
                .containers
                .containers()
                .iter()
                .map(|c| (c.id, c))
                .collect(),
            nodes: ir.nodes.iter().map(|n| (n.id, n)).collect(),
            dataset_root,
            files_by_path,
            folders_by_path,
            file_paths_by_id,
        }
    }

    fn assess(
        &self,
        proposal: &AdviceProposal,
        profile: ProfileName,
        config: &ProfileConfig,
    ) -> ProfileAssessment {
        let subjects = self.subject_nodes(proposal);
        let Some(destination) = self.resolve_destination(proposal) else {
            return Self::empty_assessment(profile, &proposal.destination, config);
        };
        let evidence = self.signals(&subjects, &destination, proposal, config);
        let weighted_score = weighted_evidence(evidence, config);
        let structural_score = structural_evidence(evidence, config);
        // Qualification deliberately uses a conservative, immutable alternative pool rather
        // than replaying the search pass's mutable admission logic.  An extra alternative can
        // only demote advice to review; it can never authorize or suppress a searched move.
        let mut alternatives = self.alternatives(&subjects, &destination, proposal, config);
        alternatives.sort_by_key(|place| self.place_name(place, proposal));
        let best = alternatives
            .into_iter()
            .map(|place| {
                let signals = self.signals(&subjects, &place, proposal, config);
                let score = weighted_evidence(signals, config);
                (place, score)
            })
            .max_by(|(left_place, left_score), (right_place, right_score)| {
                left_score.total_cmp(right_score).then_with(|| {
                    self.place_name(right_place, proposal)
                        .cmp(&self.place_name(left_place, proposal))
                })
            });
        let (best_alternative, best_score) = best.map_or((None, 0.0), |(place, score)| {
            (Some(self.place_name(&place, proposal)), score)
        });
        let ambiguity_margin = (weighted_score - best_score).max(0.0);
        let q = config.qualification;
        let qualified = weighted_score >= q.minimum_evidence
            && structural_score > 0.0
            && structural_score >= q.minimum_structural
            && ambiguity_margin >= q.minimum_ambiguity_margin
            && weighted_score - best_score > 1e-12;
        ProfileAssessment {
            profile,
            destination: proposal.destination.clone(),
            evidence,
            weighted_score,
            structural_score,
            ambiguity_margin,
            best_alternative,
            thresholds: QualificationThresholds {
                minimum_evidence: q.minimum_evidence,
                minimum_structural: q.minimum_structural,
                minimum_ambiguity_margin: q.minimum_ambiguity_margin,
            },
            qualified,
        }
    }

    fn signals(
        &self,
        subjects: &BTreeSet<NodeId>,
        destination: &EvidencePlace,
        proposal: &AdviceProposal,
        config: &ProfileConfig,
    ) -> EvidenceSignals {
        let owners = self
            .ir
            .affinities
            .iter()
            .filter(|a| a.kind == AffinityKind::CompanionOwner && subjects.contains(&a.companion))
            .filter_map(|a| self.nodes.get(&a.owner).copied())
            .collect::<Vec<_>>();
        let owners_in_destination = owners
            .iter()
            .filter(|node| self.node_at_evidence_place(node, destination))
            .count();
        let subject_tokens = subjects
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .flat_map(|node| tokenize(&node.name))
            .collect::<BTreeSet<_>>();
        let destination_tokens = self
            .destination_nodes_at(destination)
            .into_iter()
            .flat_map(|node| tokenize(&node.name))
            .chain(
                Some(self.place_name(destination, proposal))
                    .into_iter()
                    .flat_map(|name| tokenize(&name)),
            )
            .collect::<BTreeSet<_>>();
        let source = self.resolve_source(proposal);
        let mut wd = 0.0;
        let mut ws = 0.0;
        let mut wall = 0.0;
        let mut neighbors = BTreeSet::new();
        let mut consumers = BTreeSet::new();
        for edge in &self.ir.edges {
            let (neighbor, incoming) =
                if subjects.contains(&edge.source) && !subjects.contains(&edge.target) {
                    (edge.target, false)
                } else if subjects.contains(&edge.target) && !subjects.contains(&edge.source) {
                    (edge.source, true)
                } else {
                    continue;
                };
            let Some(node) = self
                .nodes
                .get(&neighbor)
                .copied()
                .filter(|node| node.polarity == Polarity::Production)
            else {
                continue;
            };
            let weight = edge.confidence * dependency_weight(edge.kind, config);
            if weight <= 0.0 {
                continue;
            }
            wall += weight;
            if self.node_at_evidence_place(node, destination) {
                wd += weight;
                neighbors.insert(node.id);
            }
            if source
                .as_ref()
                .is_some_and(|place| self.node_at_evidence_place(node, place))
            {
                ws += weight;
            }
            if incoming {
                consumers.insert(node.container);
            }
        }
        let source_cohesion = ((wd - ws) / wd.max(ws).max(f64::EPSILON)).clamp(0.0, 1.0);
        let destination_cohesion = if wall > 0.0 {
            let breadth = match neighbors.len() {
                0 => 0.0,
                1 => 0.5,
                _ => 1.0,
            };
            (wd / wall) * breadth
        } else {
            0.0
        };
        EvidenceSignals {
            unique_owner: f64::from(owners.len() == 1 && owners_in_destination == 1),
            role_affinity: jaccard_tokens(&subject_tokens, &destination_tokens),
            source_cohesion,
            destination_cohesion,
            producer_evidence: if owners.is_empty() {
                0.0
            } else {
                usize_ratio(owners_in_destination, owners.len())
            },
            architectural_reach: self.architectural_reach(destination, &consumers),
        }
    }

    fn alternatives(
        &self,
        subjects: &BTreeSet<NodeId>,
        destination: &EvidencePlace,
        proposal: &AdviceProposal,
        config: &ProfileConfig,
    ) -> Vec<EvidencePlace> {
        let wants_file = matches!(proposal.proposal, RelocationProposal::Symbol { .. });
        let mut places = BTreeSet::new();
        let mut consumer_folders = BTreeSet::new();
        if let Some(source) = self.resolve_source(proposal) {
            places.insert(source);
        }
        for edge in &self.ir.edges {
            if dependency_weight(edge.kind, config) * edge.confidence <= 0.0 {
                continue;
            }
            let other = if subjects.contains(&edge.source) && !subjects.contains(&edge.target) {
                Some(edge.target)
            } else if subjects.contains(&edge.target) && !subjects.contains(&edge.source) {
                Some(edge.source)
            } else {
                None
            };
            if let Some(node) = other
                .and_then(|id| self.nodes.get(&id))
                .filter(|node| node.polarity == Polarity::Production)
            {
                places.insert(if wants_file {
                    EvidencePlace::Container(node.container)
                } else {
                    self.physical_folder_for_file(node.container)
                        .unwrap_or(EvidencePlace::Container(node.container))
                });
            }
            if subjects.contains(&edge.target)
                && !subjects.contains(&edge.source)
                && let Some(folder) = self
                    .nodes
                    .get(&edge.source)
                    .filter(|node| node.polarity == Polarity::Production)
                    .and_then(|node| self.physical_folder_for_file(node.container))
                    .and_then(|place| match place {
                        EvidencePlace::PhysicalFolder { path, .. } => Some(path),
                        _ => None,
                    })
            {
                consumer_folders.insert(folder);
            }
        }
        // The consumer LCA is a conceptual alternative for either relocation grain.  In
        // particular, a symbol proposed into one sibling branch must clearly beat the shared
        // folder even though symbol search itself ultimately nominates concrete files.
        if consumer_folders.len() >= 2 {
            let names = consumer_folders
                .iter()
                .map(|name| name.split('/').collect::<Vec<_>>())
                .collect::<Vec<_>>();
            if let Some(first) = names.first() {
                let common_len = (0..first.len())
                    .take_while(|i| names.iter().all(|parts| parts.get(*i) == first.get(*i)))
                    .count();
                let common = first
                    .iter()
                    .take(common_len)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("/");
                if !common.is_empty() {
                    let place = self
                        .resolve_folder_place(&common)
                        .unwrap_or(EvidencePlace::ConceptualFolder(common));
                    places.insert(place);
                }
            }
        }
        for affinity in &self.ir.affinities {
            if subjects.contains(&affinity.companion)
                && let Some(owner) = self.nodes.get(&affinity.owner)
            {
                places.insert(if wants_file {
                    EvidencePlace::Container(owner.container)
                } else {
                    self.physical_folder_for_file(owner.container)
                        .unwrap_or(EvidencePlace::Container(owner.container))
                });
            }
        }
        places.remove(destination);
        places.into_iter().collect()
    }

    fn empty_assessment(
        profile: ProfileName,
        destination: &str,
        config: &ProfileConfig,
    ) -> ProfileAssessment {
        let q = config.qualification;
        ProfileAssessment {
            profile,
            destination: destination.to_owned(),
            evidence: EvidenceSignals {
                unique_owner: 0.0,
                role_affinity: 0.0,
                source_cohesion: 0.0,
                destination_cohesion: 0.0,
                producer_evidence: 0.0,
                architectural_reach: 0.0,
            },
            weighted_score: 0.0,
            structural_score: 0.0,
            ambiguity_margin: 0.0,
            best_alternative: None,
            thresholds: QualificationThresholds {
                minimum_evidence: q.minimum_evidence,
                minimum_structural: q.minimum_structural,
                minimum_ambiguity_margin: q.minimum_ambiguity_margin,
            },
            qualified: false,
        }
    }

    fn subject_nodes(&self, proposal: &AdviceProposal) -> BTreeSet<NodeId> {
        match &proposal.proposal {
            RelocationProposal::Symbol { relocation } => {
                let source = self.resolve_file_path(&relocation.from_path);
                let expected_kind = match relocation.kind {
                    SymbolKind::Symbol => NodeKind::Symbol,
                    SymbolKind::Type => NodeKind::Type,
                };
                let matches = self
                    .ir
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.name == relocation.symbol
                            && node.kind == expected_kind
                            && source == Some(node.container)
                    })
                    .map(|node| node.id)
                    .collect::<Vec<_>>();
                // A report atom must resolve to exactly one pass-start declaration.  Ambiguous
                // identity carries no qualifying evidence and therefore remains review-only.
                if matches.len() == 1 {
                    matches.into_iter().collect()
                } else {
                    BTreeSet::new()
                }
            }
            RelocationProposal::File { relocation } => {
                relocation.files.first().map_or_else(BTreeSet::new, |file| {
                    self.ir
                        .nodes
                        .iter()
                        .filter(|node| self.resolve_file_path(&file.path) == Some(node.container))
                        .map(|node| node.id)
                        .collect()
                })
            }
        }
    }
    fn repository_relative<'b>(&self, path: &'b str) -> &'b str {
        let Some(root) = self.dataset_root.as_deref() else {
            return path;
        };
        if path == root {
            ""
        } else {
            path.strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(path)
        }
    }
    fn resolve_file_path(&self, path: &str) -> Option<ContainerId> {
        self.files_by_path
            .get(self.repository_relative(path))
            .copied()
            .flatten()
    }
    fn resolve_folder_path(&self, path: &str) -> Option<ContainerId> {
        self.folders_by_path
            .get(self.repository_relative(path))
            .copied()
            .flatten()
    }
    fn resolve_destination(&self, proposal: &AdviceProposal) -> Option<EvidencePlace> {
        match proposal.proposal {
            RelocationProposal::Symbol { .. } => self
                .resolve_file_path(&proposal.destination)
                .map(EvidencePlace::Container),
            RelocationProposal::File { .. } => self.resolve_folder_place(&proposal.destination),
        }
    }
    fn resolve_source(&self, proposal: &AdviceProposal) -> Option<EvidencePlace> {
        match &proposal.proposal {
            RelocationProposal::Symbol { relocation } => self
                .resolve_file_path(&relocation.from_path)
                .map(EvidencePlace::Container),
            RelocationProposal::File { relocation } => relocation
                .files
                .first()
                .and_then(|f| self.resolve_file_path(&f.path))
                .and_then(|id| self.physical_folder_for_file(id)),
        }
    }
    fn resolve_folder_place(&self, path: &str) -> Option<EvidencePlace> {
        let relative = self.repository_relative(path);
        self.resolve_folder_path(relative)
            .map(|container| EvidencePlace::PhysicalFolder {
                container,
                path: relative.to_owned(),
            })
    }
    fn physical_folder_for_file(&self, file: ContainerId) -> Option<EvidencePlace> {
        let path = self.file_paths_by_id.get(&file)?.as_deref()?;
        let folder_path = path.rsplit_once('/').map_or("", |(folder, _)| folder);
        let container = self.containers.get(&file)?.parent?;
        Some(EvidencePlace::PhysicalFolder {
            container,
            path: folder_path.to_owned(),
        })
    }
    fn is_ancestor(&self, ancestor: ContainerId, mut child: ContainerId) -> bool {
        loop {
            if ancestor == child {
                return true;
            }
            let Some(parent) = self.containers.get(&child).and_then(|c| c.parent) else {
                return false;
            };
            child = parent;
        }
    }
    fn node_at_place(&self, node: &Node, place: ContainerId) -> bool {
        node.container == place || self.is_ancestor(place, node.container)
    }
    fn place_name(&self, place: &EvidencePlace, proposal: &AdviceProposal) -> String {
        let relative = match place {
            EvidencePlace::Container(id) => self
                .file_paths_by_id
                .get(id)
                .and_then(Clone::clone)
                .unwrap_or_default(),
            EvidencePlace::PhysicalFolder { path, .. } | EvidencePlace::ConceptualFolder(path) => {
                path.clone()
            }
        };
        if matches!(proposal.proposal, RelocationProposal::File { .. }) {
            self.dataset_root
                .as_deref()
                .filter(|root| !root.is_empty())
                .map_or(relative.clone(), |root| {
                    if relative.is_empty() {
                        root.to_owned()
                    } else {
                        format!("{root}/{relative}")
                    }
                })
        } else {
            relative
        }
    }
    fn physical_folder_name(&self, place: &EvidencePlace) -> Option<String> {
        match place {
            EvidencePlace::ConceptualFolder(path) | EvidencePlace::PhysicalFolder { path, .. } => {
                Some(path.clone())
            }
            EvidencePlace::Container(id) => self.physical_folder_for_file(*id).and_then(|folder| {
                if let EvidencePlace::PhysicalFolder { path, .. } = folder {
                    Some(path)
                } else {
                    None
                }
            }),
        }
    }
    fn node_at_evidence_place(&self, node: &Node, place: &EvidencePlace) -> bool {
        match place {
            EvidencePlace::Container(id) => self.node_at_place(node, *id),
            EvidencePlace::PhysicalFolder { container, path } => {
                self.node_at_place(node, *container)
                    && self.physical_folder_for_file(node.container).is_some_and(|folder| {
                        matches!(folder, EvidencePlace::PhysicalFolder { path: node_path, .. } if physical_path_contains(path, &node_path))
                    })
            }
            EvidencePlace::ConceptualFolder(_) => false,
        }
    }
    fn destination_nodes_at(&self, place: &EvidencePlace) -> Vec<&Node> {
        self.ir
            .nodes
            .iter()
            .filter(|node| self.node_at_evidence_place(node, place))
            .collect()
    }
    fn architectural_reach(
        &self,
        destination: &EvidencePlace,
        consumer_files: &BTreeSet<ContainerId>,
    ) -> f64 {
        if consumer_files.len() < 2 {
            return 0.0;
        }
        let folders = consumer_files
            .iter()
            .filter_map(|id| self.physical_folder_for_file(*id))
            .filter_map(|place| match place {
                EvidencePlace::PhysicalFolder { path, .. } => Some(path),
                _ => None,
            })
            .map(|name| name.split('/').map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let Some(first) = folders.first() else {
            return 0.0;
        };
        let common_len = (0..first.len())
            .take_while(|index| {
                folders
                    .iter()
                    .all(|parts| parts.get(*index) == first.get(*index))
            })
            .count();
        let Some(destination_name) = self.physical_folder_name(destination) else {
            return 0.0;
        };
        let destination_parts = destination_name
            .split('/')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if destination_parts.len() == common_len
            && first.get(..common_len) == Some(destination_parts.as_slice())
        {
            1.0
        } else if destination_parts.len() < common_len
            && first.get(..destination_parts.len()) == Some(destination_parts.as_slice())
        {
            1.0 / (1.0 + usize_as_f64(common_len - destination_parts.len()))
        } else {
            0.0
        }
    }
}

pub(in crate::analyze) fn insert_unique_path(
    paths: &mut BTreeMap<ContainerId, Option<String>>,
    id: ContainerId,
    path: &str,
) {
    paths
        .entry(id)
        .and_modify(|entry| {
            if entry.as_deref() != Some(path) {
                *entry = None;
            }
        })
        .or_insert_with(|| Some(path.to_owned()));
}

pub(in crate::analyze) fn physical_path_contains(ancestor: &str, candidate: &str) -> bool {
    candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| ancestor.is_empty() || suffix.starts_with('/'))
}

pub(in crate::analyze) fn dependency_weight(kind: EdgeKind, config: &ProfileConfig) -> f64 {
    match kind {
        EdgeKind::ValueImport => config.weights.value_import,
        EdgeKind::TypeReference => config.weights.type_reference,
        EdgeKind::Inheritance => config.weights.inheritance,
        EdgeKind::Call => config.weights.call,
        EdgeKind::ReExport => config.weights.re_export,
    }
}
pub(in crate::analyze) fn jaccard_tokens(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        usize_ratio(left.intersection(right).count(), union)
    }
}

#[allow(clippy::cast_precision_loss)]
pub(in crate::analyze) fn usize_as_f64(value: usize) -> f64 {
    value as f64
}

pub(in crate::analyze) fn usize_ratio(numerator: usize, denominator: usize) -> f64 {
    usize_as_f64(numerator) / usize_as_f64(denominator)
}
pub(in crate::analyze) fn weighted_evidence(e: EvidenceSignals, config: &ProfileConfig) -> f64 {
    let w = config.qualification.weights;
    let total = w.unique_owner
        + w.role_affinity
        + w.source_cohesion
        + w.destination_cohesion
        + w.producer_evidence
        + w.architectural_reach;
    (e.unique_owner * w.unique_owner
        + e.role_affinity * w.role_affinity
        + e.source_cohesion * w.source_cohesion
        + e.destination_cohesion * w.destination_cohesion
        + e.producer_evidence * w.producer_evidence
        + e.architectural_reach * w.architectural_reach)
        / total
}
pub(in crate::analyze) fn structural_evidence(e: EvidenceSignals, config: &ProfileConfig) -> f64 {
    let w = config.qualification.weights;
    let total = w.unique_owner
        + w.source_cohesion
        + w.destination_cohesion
        + w.producer_evidence
        + w.architectural_reach;
    if total == 0.0 {
        0.0
    } else {
        (e.unique_owner * w.unique_owner
            + e.source_cohesion * w.source_cohesion
            + e.destination_cohesion * w.destination_cohesion
            + e.producer_evidence * w.producer_evidence
            + e.architectural_reach * w.architectural_reach)
            / total
    }
}
