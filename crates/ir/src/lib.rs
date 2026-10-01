//! Strata's intermediate representation: the typed-graph contract every other
//! crate depends on.
//!
//! This crate defines the nodes, edges, and laminar container tree that model a
//! codebase, the shared [`Adapter`] trait language frontends implement, and the
//! validated, content-addressed [`Snapshot`] handed to the solver.

pub mod adapter;
pub mod affinity;
pub mod container;
pub mod edge;
pub mod laminar;
pub mod node;
pub mod snapshot;

pub use adapter::{
    Adapter, AdapterError, IrFragment, ParseTree, ScopeRung, SourceFile, VisibilityScope,
};
pub use affinity::{Affinity, AffinityKind};
pub use container::{Container, ContainerId, ContainerTree, TreeError};
pub use edge::{Edge, EdgeKind, Hardness};
pub use laminar::{LaminarTree, Layout, build_laminar_tree};
pub use node::{Node, NodeId, NodeKind, Polarity, ScopeLevel};
pub use snapshot::{
    IntermediateRepresentation, MIN_SCHEMA_VERSION, SCHEMA_VERSION, ScopeLadder, Snapshot,
    SnapshotError,
};
