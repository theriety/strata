//! Graph nodes: semantic program entities extracted by language adapters.

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
    /// Executable file-scope content that has no independent declaration.
    FileBody,
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

/// A semantic program entity extracted by a language adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier within a snapshot.
    pub id: NodeId,
    /// Source-declared name, or an adapter-assigned name for a synthetic entity.
    pub name: SmolStr,
    /// The program entity represented by this node.
    pub kind: NodeKind,
    /// Production / test-case / test-support classification.
    pub polarity: Polarity,
    /// Owning file in the current (pre-restructure) tree.
    pub container: ContainerId,
    /// Declared export scope as written in source.
    pub visibility: ScopeLevel,
    /// Production sloc attributed to this node (drives the file cap).
    pub effective_size: u32,
    /// Whether this node is a re-export declaration (ADR-20): it declares no
    /// entity of its own but publishes another one under this name (`pub use`,
    /// `export { x } from`, an `__init__.py` import binding).
    ///
    /// Added in schema 3; absent from older snapshots, where it reads `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub re_export: bool,
    /// Whether the declaration is part of a library's public API, so users
    /// outside the analysed tree may depend on it (ADR-25): visibility advice
    /// never narrows it. An adapter sets it only when it is sure, and also
    /// when it is unsure, because the flag only ever silences advice.
    ///
    /// Added to schema 3; absent means `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub public_api: bool,
    /// Whether the declaration may be used from code the adapter could not see
    /// (a dropped `cfg` item, a glob import it could not resolve), so advice
    /// must not narrow it (ADR-25). Unlike `public_api` this says nothing about
    /// outside users. Anything that treats "externally reachable" specially
    /// (such as a test-only check) must look at both flags.
    ///
    /// Added to schema 3; absent means `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub advice_exempt: bool,
    /// Human-facing name when it differs from `name` (a method reads
    /// `Type::method`). Display only: matching and lookup key on `name`.
    ///
    /// Added in schema 3; absent means `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<SmolStr>,
}

/// Serde predicate that omits the default `false` flag from serialized output.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes the field by reference
const fn is_false(value: &bool) -> bool {
    !*value
}
