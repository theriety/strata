//! Path-collision folding for file moves (ADR-21).
//!
//! A file move lands the file at `<destination folder>/<its file name>`. When
//! another candidate file already holds that path, printing the move would tell
//! the reader to overwrite it. The move is withdrawn instead, and the file's
//! declarations are offered to the occupant as one symbol-move unit.

use std::collections::BTreeMap;

use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::symbol::SymbolOutcome;
use crate::analyze::scoring::CycleCounts;
use crate::narrate::{basename, physical_relocation_folders};
use crate::snapshot::Language;

#[cfg(test)]
mod tests;
mod withdraw;

/// A withdrawn file move whose declarations may be offered to the file already
/// at its destination path (ADR-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::analyze) struct CollisionFold {
    /// Pass-start container id of the file whose move was withdrawn.
    pub(in crate::analyze) file: u32,
    /// Pass-start container id of the file holding the destination path.
    pub(in crate::analyze) into: u32,
    /// Whether the symbol pass is offered the file's declarations. `false`
    /// when the file is a module root (it is never folded), or when the symbol
    /// pass refused the fold and its withdrawal was undone; the file then only
    /// stays where it is.
    pub(in crate::analyze) offered: bool,
}

/// The partition changes one withdrawal made besides returning the colliding
/// file: each evicted newcomer SCC with the cluster it was evicted from, so a
/// refused fold can be undone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analyze) struct Withdrawals {
    evicted: BTreeMap<u32, Vec<(u32, ClusterId)>>,
}

impl PipelineSolver<'_> {
    /// Settles the restart's path collisions: withdraws every colliding file
    /// move, places the test followers, and repeats both until a round
    /// withdraws nothing, so a follower that changes a display-level election
    /// cannot carry another file onto an occupied path. Returns the evidence of
    /// the last follower pass, carrying every fold so far.
    ///
    /// A faithful layout renders as the current tree, which moves no file and
    /// so prints no collision: nothing is withdrawn while the layout is
    /// faithful, and a layout that settles faithful keeps no fold, because a
    /// fold may only replace a file move the report shows.
    ///
    /// Each round withdraws at least one colliding move, so the file count
    /// bounds the rounds; fixtures and the self-analysis settle in one or two
    /// (ADR-21, Consequences).
    pub(super) fn settle_collisions(
        &self,
        parts: &mut Partition,
        folds: &mut Vec<CollisionFold>,
        withdrawals: &mut Withdrawals,
    ) -> PolishEvidence {
        let mut evidence = PolishEvidence {
            folds: folds.clone(),
            ..PolishEvidence::default()
        };
        for round in 0..=self.files.len() {
            let withdrew = !self.is_faithful(parts)
                && self.fold_path_collisions(parts, &evidence, folds, withdrawals);
            if round > 0 && !withdrew {
                break;
            }
            evidence = self.shadow_tests(parts, folds.clone());
        }
        if self.is_faithful(parts) {
            folds.clear();
            evidence.folds.clear();
            *withdrawals = Withdrawals::default();
        }
        evidence
    }

    /// ADR-21: a fold the symbol pass refused leaves its file where it is and
    /// undoes the evictions its withdrawal made; the layout then settles again,
    /// which can offer new folds. Each round retires at least one offered fold,
    /// so the loop runs until nothing is refused, capped by the file count.
    pub(super) fn settle_refusals(
        &self,
        parts: &mut Partition,
        folds: &mut Vec<CollisionFold>,
        withdrawals: &mut Withdrawals,
        evidence: PolishEvidence,
        symbols: SymbolOutcome,
    ) -> (PolishEvidence, SymbolOutcome) {
        let mut settling = Settling {
            parts,
            folds,
            withdrawals,
        };
        let (latest, symbols) =
            retire_refused_folds(self.files.len(), &mut settling, symbols, |settling| {
                let settled =
                    self.settle_collisions(settling.parts, settling.folds, settling.withdrawals);
                let outcome = self.symbol_polish_with_polish_evidence(settling.parts, &settled);
                (settled, outcome)
            });
        (latest.unwrap_or(evidence), symbols)
    }

    /// Withdraws every file move that lands on a path another candidate file
    /// holds, recording each as a fold; returns whether any was withdrawn.
    ///
    /// Only the colliding file is withdrawn. A mover the polish carried out of
    /// its pass-start folder cluster returns to it. A mover still in that
    /// cluster is being carried by the display levels, which draw the whole
    /// folder in another package; the display levels then keep the folder in
    /// the folded file's pass-start package (`folded_folder_pins`), and when
    /// the fold is offered the cluster's newcomers are evicted toward the
    /// occupant's cluster (see [`Self::evict_newcomers`]). A module root is
    /// never offered: retiring it would break the module structure. Nor is a
    /// file whose fold the symbol pass already refused: its evictions were
    /// undone, and evicting again would leave evictions no refusal undoes.
    /// Either way the partition changes or the pin applies, so the same
    /// collision is never detected twice; the file count bounds the rounds.
    fn fold_path_collisions(
        &self,
        parts: &mut Partition,
        evidence: &PolishEvidence,
        folds: &mut Vec<CollisionFold>,
        withdrawals: &mut Withdrawals,
    ) -> bool {
        let mut withdrew = false;
        for _ in 0..self.files.len() {
            let round = PolishEvidence {
                folds: folds.clone(),
                ..evidence.clone()
            };
            let collisions = self.path_collisions(parts, &round);
            if collisions.is_empty() {
                break;
            }
            withdrew = true;
            let before = parts.clone();
            for &(mover, occupant) in &collisions {
                let (Some(file), Some(into)) = (self.files.get(mover), self.files.get(occupant))
                else {
                    continue;
                };
                let offered = offers_fold(file.container, file.name.as_str(), folds);
                let evicted = self.withdraw(&before, parts, mover, occupant, offered);
                if !evicted.is_empty() {
                    withdrawals
                        .evicted
                        .entry(file.container)
                        .or_default()
                        .extend(evicted);
                }
                folds.push(CollisionFold {
                    file: file.container,
                    into: into.container,
                    offered,
                });
            }
        }
        folds.sort_unstable();
        folds.dedup_by_key(|fold| fold.file);
        withdrew
    }

    /// Evicts the newcomers of the folder cluster `home` (SCCs that joined it
    /// from other pass-start clusters) toward `landing`, the occupant's
    /// cluster, which is the folder they were being drawn into anyway.
    ///
    /// Each eviction is a join the polish never priced, so it must pass what a
    /// polish move passes: the relocation identity guard (package wall,
    /// namespaces, render identities), the physical capacity veto, and the
    /// quotient-cycle veto against the layout before the eviction. A newcomer
    /// refused any of them returns to its own pass-start home instead.
    fn evict_newcomers(
        &self,
        before: &Partition,
        parts: &mut Partition,
        home: ClusterId,
        landing: ClusterId,
    ) -> Vec<(u32, ClusterId)> {
        let newcomers: Vec<u32> = (0..before.assignment().len())
            .filter_map(|member| u32::try_from(member).ok())
            .filter(|&member| {
                before.cluster_of(member) == Some(home)
                    && self.pass_start_partition.cluster_of(member) != Some(home)
            })
            .collect();
        let mut evicted = Vec::with_capacity(newcomers.len());
        for scc in newcomers {
            let cyclic_base = CycleCounts::from_graph(&parts.quotient(&self.condensation.dag));
            let joins = self.relocation_identity.permits_join(parts, scc, landing)
                && self.permits_physical_capacity(parts, scc, landing)
                && parts.move_node(scc, landing);
            if joins
                && CycleCounts::from_graph(&parts.quotient(&self.condensation.dag))
                    .exceeds(cyclic_base)
            {
                let _moved = parts.move_node(scc, home);
            } else if joins {
                evicted.push((scc, home));
                continue;
            }
            if let Some(own_home) = self.pass_start_partition.cluster_of(scc)
                && parts.move_node(scc, own_home)
            {
                evicted.push((scc, home));
            }
        }
        evicted
    }

    /// Pairs each colliding mover (file-graph vertex) with the file that keeps
    /// the path: the file already there when one stays put, otherwise the mover
    /// with the smallest path.
    pub(super) fn path_collisions(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> Vec<(usize, usize)> {
        let assembled = self.assemble_with_polish_evidence(parts, evidence);
        let (current, proposed) =
            physical_relocation_folders(&self.snapshot.ir().containers, &assembled.tree);
        let mut claims: BTreeMap<String, Vec<(bool, &str, usize)>> = BTreeMap::new();
        for (vertex, file) in self.files.iter().enumerate() {
            let path = self.repository_path(file.name.as_str());
            let Some(folder) = proposed.get(&path) else {
                continue;
            };
            let moved = current.get(&path) != Some(folder);
            let mut landing = folder.clone();
            landing.push(basename(&path).to_owned());
            claims
                .entry(landing.join("/"))
                .or_default()
                .push((moved, file.name.as_str(), vertex));
        }
        let mut collisions = Vec::new();
        for mut claimants in claims.into_values().filter(|claimants| claimants.len() > 1) {
            // a file staying put sorts first (`false < true`), then by path.
            claimants.sort_unstable();
            let Some(&(_, _, keeper)) = claimants.first() else {
                continue;
            };
            collisions.extend(
                claimants
                    .iter()
                    .skip(1)
                    .filter(|(moved, _, _)| *moved)
                    .map(|&(_, _, mover)| (mover, keeper)),
            );
        }
        collisions
    }
}

/// Whether a colliding file's withdrawal offers its declarations to the
/// symbol pass. A module root is never offered: retiring it would break the
/// module structure. A file whose fold the symbol pass already refused is not
/// offered again: its evictions were undone, and a fresh withdrawal that
/// evicted again would leave evictions no refusal undoes.
fn offers_fold(file: u32, name: &str, folds: &[CollisionFold]) -> bool {
    let refused = folds.iter().any(|fold| fold.file == file && !fold.offered);
    !refused && !Language::is_module_root(name)
}

/// The layout state a refusal round undoes and settles again.
pub(super) struct Settling<'a> {
    /// The candidate partition.
    pub(super) parts: &'a mut Partition,
    /// Every fold recorded so far.
    pub(super) folds: &'a mut Vec<CollisionFold>,
    /// The evictions each withdrawal made.
    pub(super) withdrawals: &'a mut Withdrawals,
}

/// Runs refusal rounds: undo the refused folds, then `resettle` the layout and
/// rerun the symbol pass, until nothing is refused or `cap` rounds have run.
/// Returns the last round's evidence (`None` when nothing was refused) and the
/// final symbol outcome.
pub(super) fn retire_refused_folds(
    cap: usize,
    settling: &mut Settling<'_>,
    mut symbols: SymbolOutcome,
    mut resettle: impl FnMut(&mut Settling<'_>) -> (PolishEvidence, SymbolOutcome),
) -> (Option<PolishEvidence>, SymbolOutcome) {
    let mut latest = None;
    for _ in 0..=cap {
        if !undo_refused_folds(
            settling.parts,
            settling.folds,
            settling.withdrawals,
            &symbols,
        ) {
            break;
        }
        let (evidence, next) = resettle(settling);
        latest = Some(evidence);
        symbols = next;
    }
    (latest, symbols)
}

/// Undoes the withdrawal of every offered fold the symbol pass refused:
/// the newcomers it evicted return to the cluster the polish gave them, and
/// the fold is no longer offered, so its file only stays where it is.
/// Returns whether anything was undone.
pub(super) fn undo_refused_folds(
    parts: &mut Partition,
    folds: &mut [CollisionFold],
    withdrawals: &mut Withdrawals,
    symbols: &SymbolOutcome,
) -> bool {
    let accepted = symbols.accepted_folds();
    let mut undone = false;
    for fold in folds
        .iter_mut()
        .filter(|fold| fold.offered && !accepted.contains(&fold.file))
    {
        fold.offered = false;
        undone = true;
        for (scc, from) in withdrawals.evicted.remove(&fold.file).unwrap_or_default() {
            let _moved = parts.move_node(scc, from);
        }
    }
    undone
}
