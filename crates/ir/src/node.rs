//! Graph nodes: symbols and types extracted by language adapters.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::container::ContainerId;

/// Stable numeric identifier assigned at snapshot assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// What kind of program entity a [`Node`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A value-level symbol (function, constant, variable).
    Symbol,
    /// A type-level entity (struct, enum, interface, alias).
    Type,
}

/// Three-valued test polarity (ad-3): production never depends on the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Polarity {
    /// Production code that ships to consumers.
    Production,
    /// A test case (the entry point of a test).
    TestCase,
    /// Test support code (fixtures, helpers) used only by test cases.
    TestSupport,
}

/// Container levels, strictly ordered: file < folder < domain < package < package_group.
///
/// The derived [`Ord`] follows declaration order, so `File < Folder < Domain <
/// Package < PackageGroup` holds by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ScopeLevel {
    /// A single source file.
    File,
    /// A directory of files.
    Folder,
    /// A cohesive functional domain spanning folders.
    Domain,
    /// A distributable package.
    Package,
    /// A group of related packages (a workspace root).
    PackageGroup,
}

/// A symbol or type extracted by a language adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier within a snapshot.
    pub id: NodeId,
    /// Source-declared name of the entity.
    pub name: SmolStr,
    /// Whether the node is a symbol or a type.
    pub kind: NodeKind,
    /// Production / test-case / test-support classification.
    pub polarity: Polarity,
    /// Owning file in the current (pre-restructure) tree.
    pub container: ContainerId,
    /// Declared export scope as written in source.
    pub visibility: ScopeLevel,
    /// Production sloc attributed to this node (drives the file cap).
    pub effective_size: u32,
}
