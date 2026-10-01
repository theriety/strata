//! The laminar container tree over files, folders, domains, and packages.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use crate::node::ScopeLevel;

/// Stable numeric identifier for a [`Container`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContainerId(pub u32);

/// A single node of the laminar container tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    /// Stable identifier within a snapshot.
    pub id: ContainerId,
    /// Source-declared name of the container.
    pub name: SmolStr,
    /// The scope level this container occupies.
    pub level: ScopeLevel,
    /// Parent container; `None` only for roots (package groups).
    pub parent: Option<ContainerId>,
    /// True for the empty-scope `workspace` domain/folder bucket that hangs a
    /// package's root-level files off an intervening level so the tree stays
    /// strictly ascending. The bucket names no real directory, so the DTO
    /// collapses it at render; this flag is the structural signal for that
    /// collapse. It is a transient render hint, never serialized: the public
    /// result carries the collapsed engine `ContainerNode` DTO, not this tree.
    #[serde(skip)]
    pub synthetic: bool,
}

/// A laminar (forest-shaped) tree over [`Container`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerTree {
    containers: Vec<Container>,
}

/// Reasons a [`ContainerTree`] fails well-formedness validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TreeError {
    /// Container ids are not the dense range `0..len` exactly once each.
    #[error("container ids must be the dense range 0..{expected}, but id {found} breaks density")]
    NonDenseIds {
        /// The number of containers (the expected exclusive upper bound).
        expected: u32,
        /// The offending id.
        found: u32,
    },

    /// The same id appears on more than one container.
    #[error("duplicate container id {id}")]
    DuplicateId {
        /// The repeated id.
        id: u32,
    },

    /// A `parent` link points at an id that has no container.
    #[error("container {child} references missing parent {parent}")]
    MissingParent {
        /// The container holding the dangling link.
        child: u32,
        /// The referenced but absent parent id.
        parent: u32,
    },

    /// A parent's level is not strictly above its child's level.
    #[error(
        "container {child} at level {child_level:?} must sit strictly below its parent {parent} at level {parent_level:?}"
    )]
    LevelNotAscending {
        /// The child container id.
        child: u32,
        /// The child's level.
        child_level: ScopeLevel,
        /// The parent container id.
        parent: u32,
        /// The parent's level.
        parent_level: ScopeLevel,
    },
}

impl ContainerTree {
    /// Builds a tree from a list of containers without validating it.
    #[must_use]
    pub fn new(containers: Vec<Container>) -> Self {
        Self { containers }
    }

    /// Returns the containers in storage order.
    #[must_use]
    pub fn containers(&self) -> &[Container] {
        &self.containers
    }

    /// Sorts the containers by id, producing a canonical ordering.
    pub(super) fn sort_by_id(&mut self) {
        self.containers.sort_by_key(|container| container.id.0);
    }

    /// Validates well-formedness:
    ///
    /// - ids are dense (`0..len`) and unique,
    /// - every `parent` link resolves to an existing container,
    /// - each parent's level is strictly above its child's level,
    /// - parent links form a forest (no cycles).
    ///
    /// # Errors
    ///
    /// Returns the first [`TreeError`] encountered.
    pub fn validate(&self) -> Result<(), TreeError> {
        let len = self.containers.len();
        let Ok(len_u32) = u32::try_from(len) else {
            // A tree with more than u32::MAX containers cannot have dense u32 ids.
            return Err(TreeError::NonDenseIds {
                expected: u32::MAX,
                found: u32::MAX,
            });
        };

        // Index containers by id, rejecting duplicates and out-of-range ids.
        let mut by_id: Vec<Option<&Container>> = vec![None; len];
        for container in &self.containers {
            let id = container.id.0;
            let Some(slot) = by_id.get_mut(id as usize) else {
                return Err(TreeError::NonDenseIds {
                    expected: len_u32,
                    found: id,
                });
            };
            if slot.is_some() {
                return Err(TreeError::DuplicateId { id });
            }
            *slot = Some(container);
        }

        // Each container's immediate parent must resolve and sit strictly above
        // it. Strict level ascent on every parent edge makes the parent links a
        // forest by construction: a finite, strictly increasing chain of levels
        // cannot return to a node it already visited.
        for container in &self.containers {
            let Some(parent_id) = container.parent else {
                continue;
            };
            let Some(Some(parent)) = by_id.get(parent_id.0 as usize) else {
                return Err(TreeError::MissingParent {
                    child: container.id.0,
                    parent: parent_id.0,
                });
            };
            if parent.level <= container.level {
                return Err(TreeError::LevelNotAscending {
                    child: container.id.0,
                    child_level: container.level,
                    parent: parent.id.0,
                    parent_level: parent.level,
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(id: u32, level: ScopeLevel, parent: Option<u32>) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(format!("c{id}")),
            level,
            parent: parent.map(ContainerId),
            synthetic: false,
        }
    }

    #[test]
    fn should_accept_a_well_formed_forest() {
        let tree = ContainerTree::new(vec![
            container(0, ScopeLevel::Package, None),
            container(1, ScopeLevel::Folder, Some(0)),
            container(2, ScopeLevel::File, Some(1)),
        ]);

        assert_eq!(tree.validate(), Ok(()));
    }

    #[test]
    fn should_reject_non_dense_ids() {
        let tree = ContainerTree::new(vec![
            container(0, ScopeLevel::Package, None),
            container(5, ScopeLevel::File, Some(0)),
        ]);

        assert_eq!(
            tree.validate(),
            Err(TreeError::NonDenseIds {
                expected: 2,
                found: 5,
            })
        );
    }

    #[test]
    fn should_reject_duplicate_ids() {
        let tree = ContainerTree::new(vec![
            container(0, ScopeLevel::Package, None),
            container(0, ScopeLevel::File, Some(0)),
        ]);

        assert_eq!(tree.validate(), Err(TreeError::DuplicateId { id: 0 }));
    }

    #[test]
    fn should_reject_a_missing_parent() {
        let tree = ContainerTree::new(vec![container(0, ScopeLevel::File, Some(1))]);

        assert_eq!(
            tree.validate(),
            Err(TreeError::MissingParent {
                child: 0,
                parent: 1,
            })
        );
    }

    #[test]
    fn should_reject_a_parent_not_strictly_above_its_child() {
        let tree = ContainerTree::new(vec![
            container(0, ScopeLevel::Folder, None),
            container(1, ScopeLevel::Folder, Some(0)),
        ]);

        assert_eq!(
            tree.validate(),
            Err(TreeError::LevelNotAscending {
                child: 1,
                child_level: ScopeLevel::Folder,
                parent: 0,
                parent_level: ScopeLevel::Folder,
            })
        );
    }

    #[test]
    fn should_sort_containers_by_id() {
        let mut tree = ContainerTree::new(vec![
            container(2, ScopeLevel::File, Some(1)),
            container(0, ScopeLevel::Package, None),
            container(1, ScopeLevel::Folder, Some(0)),
        ]);

        tree.sort_by_id();

        let ids: Vec<u32> = tree.containers().iter().map(|c| c.id.0).collect();
        assert_eq!(ids, vec![0, 1, 2]);
    }
}
