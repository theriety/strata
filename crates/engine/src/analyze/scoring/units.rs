//! Companion units and the moves that cross their boundary (ADR-33).
//!
//! A unit is a companion folder `x/` together with its entry file `x.ts`
//! (ADR-31): only a folder with a folder-companion fact forms one. Two counts
//! read it, at the grain `companion_separations` uses, declarations moved by
//! the symbol pass plus files moved by the file pass:
//!
//! - **entry inflow** counts declarations moved from a file inside `x/` into
//!   the entry `x.ts`;
//! - **unit outflow** counts files and declarations whose pass-start effective
//!   directory lies inside the unit and whose candidate location lies outside
//!   it. A file that leaves counts once, whatever it declares.
//!
//! A declaration's location is its pass-start file's effective directory
//! (`unlace_ir::laminar::effective_directory`); a file's candidate location is
//! the folder key it sits under in the candidate tree, compared against the
//! folder key the unit's entry sits under at pass start.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use unlace_ir::laminar::{Companions, effective_directory};
use unlace_ir::{ContainerId, ContainerTree, NodeKind, Polarity, ScopeLevel, Snapshot};

use super::distance::folder_key_of_files;

/// Where one pass-start file sits relative to the units.
#[derive(Clone, Default)]
struct FileUnits {
    /// The unit this file is the entry of.
    entry_of: Option<usize>,
    /// The units whose folder holds this file's effective directory, ascending.
    within: Vec<usize>,
}

/// The companion units of a snapshot and where each pass-start file sits in
/// them; nothing here depends on a candidate tree or placement.
pub(in crate::analyze) struct UnitFacts {
    /// Folder key each unit's entry sits under at pass start, by unit index;
    /// `None` when the entry has no folder in the pass-start tree.
    unit_keys: Vec<Option<SmolStr>>,
    /// Unit membership by pass-start file container id.
    files: Vec<FileUnits>,
    /// `(node id, pass-start file)` of every production declaration whose
    /// file lies inside a unit, in node order.
    nodes: Vec<(u32, ContainerId)>,
}

impl UnitFacts {
    /// Resolves the units of `snapshot` from its folder-companion facts.
    pub(in crate::analyze) fn new(snapshot: &Snapshot) -> Self {
        let ir = snapshot.ir();
        let companions = Companions::from_facts(&ir.file_facts);
        let containers = ir.containers.containers();
        let file_paths: Vec<(ContainerId, &str)> = containers
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id, container.name.as_str()))
            .collect();

        let mut unit_of_directory: BTreeMap<&str, usize> = BTreeMap::new();
        for &(_, path) in &file_paths {
            if let Some(directory) = companions.folder_of(path) {
                let next = unit_of_directory.len();
                unit_of_directory.entry(directory).or_insert(next);
            }
        }
        if unit_of_directory.is_empty() {
            return Self {
                unit_keys: Vec::new(),
                files: Vec::new(),
                nodes: Vec::new(),
            };
        }

        let folder_key_of = folder_key_of_files(&ir.containers);
        let mut unit_keys = vec![None; unit_of_directory.len()];
        let slots = containers
            .iter()
            .map(|container| container.id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let mut files = vec![FileUnits::default(); slots];
        for &(id, path) in &file_paths {
            let entry_of = companions
                .folder_of(path)
                .and_then(|directory| unit_of_directory.get(directory).copied());
            if let Some(unit) = entry_of
                && let Some(cell) = unit_keys.get_mut(unit)
                && cell.is_none()
            {
                *cell = folder_key_of.get(path).map(|key| SmolStr::new(*key));
            }
            let directory = effective_directory(path, &companions);
            let within = unit_of_directory
                .iter()
                .filter(|&(unit_directory, _)| is_inside(directory, unit_directory))
                .map(|(_, unit)| *unit)
                .collect::<Vec<_>>();
            if let Some(cell) = files.get_mut(id.0 as usize) {
                *cell = FileUnits { entry_of, within };
            }
        }
        for cell in &mut files {
            cell.within.sort_unstable();
        }

        let nodes = ir
            .nodes
            .iter()
            .filter(|node| {
                node.polarity == Polarity::Production
                    && matches!(node.kind, NodeKind::Symbol | NodeKind::Type)
            })
            .filter(|node| {
                files
                    .get(node.container.0 as usize)
                    .is_some_and(|file| !file.within.is_empty())
            })
            .map(|node| (node.id.0, node.container))
            .collect();
        Self {
            unit_keys,
            files,
            nodes,
        }
    }

    /// Counts the declarations that flow into an entry and out of a unit,
    /// `(entry inflows, unit outflows)`, given the pass-start file each
    /// declaration's candidate file stands for.
    pub(in crate::analyze) fn declaration_flows(
        &self,
        destination_of: &dyn Fn(u32) -> Option<ContainerId>,
    ) -> (u32, u32) {
        let (mut inflows, mut outflows) = (0_u32, 0_u32);
        for &(node, source) in &self.nodes {
            let Some(destination) = destination_of(node) else {
                continue;
            };
            if destination == source {
                continue;
            }
            if self.moves_into_entry(source, destination) {
                inflows = inflows.saturating_add(1);
            }
            if self.leaves_a_unit(source, destination) {
                outflows = outflows.saturating_add(1);
            }
        }
        (inflows, outflows)
    }

    /// Counts the files of `tree` whose pass-start file lies inside a unit and
    /// whose candidate folder key lies outside that unit's key. Each file
    /// counts once.
    pub(in crate::analyze) fn file_outflows(
        &self,
        tree: &ContainerTree,
        pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
    ) -> u32 {
        if self.unit_keys.is_empty() {
            return 0;
        }
        let folder_key_of = folder_key_of_files(tree);
        tree.containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .filter(|container| {
                let Some(start) = pass_start_file_by_candidate.get(&container.id) else {
                    return false;
                };
                let Some(file) = self.files.get(start.0 as usize) else {
                    return false;
                };
                let placed = folder_key_of.get(container.name.as_str()).copied();
                file.within.iter().any(|&unit| {
                    self.unit_keys
                        .get(unit)
                        .and_then(Option::as_deref)
                        .is_some_and(|key| placed.is_none_or(|placed| !is_inside(placed, key)))
                })
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Whether a declaration moving from `source` into `destination` enters
    /// the entry of a unit it started inside, from a file other than an entry
    /// of that unit.
    fn moves_into_entry(&self, source: ContainerId, destination: ContainerId) -> bool {
        let (Some(source), Some(destination)) = (
            self.files.get(source.0 as usize),
            self.files.get(destination.0 as usize),
        ) else {
            return false;
        };
        destination.entry_of.is_some_and(|unit| {
            source.entry_of != Some(unit) && source.within.binary_search(&unit).is_ok()
        })
    }

    /// Whether a declaration moving from `source` into `destination` leaves a
    /// unit `source` lies inside.
    fn leaves_a_unit(&self, source: ContainerId, destination: ContainerId) -> bool {
        let Some(source) = self.files.get(source.0 as usize) else {
            return false;
        };
        let destination = self.files.get(destination.0 as usize);
        source.within.iter().any(|unit| {
            destination.is_none_or(|destination| destination.within.binary_search(unit).is_err())
        })
    }
}

/// Whether the slash-separated `path` equals `folder` or lies beneath it.
fn is_inside(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests;
