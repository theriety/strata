//! The validated, content-addressed [`Snapshot`] handed to the solver.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Affinity;
use crate::container::{ContainerTree, TreeError};
use crate::edge::{Edge, EdgeKind};
use crate::node::{Node, NodeId, ScopeLevel};

/// Current IR schema version, bumped on any contract change.
///
/// Version 3 added [`Node::re_export`] (ADR-20). Version 4 added
/// [`FileFact::folder_companion`](crate::FileFact::folder_companion), the
/// file-import facts and their coverage, because an IR without them must read
/// as "imports not stated" rather than "no imports". Additive default-empty
/// sidecar fields such as `scope_ladders` do not bump it (ADR-24, ADR-25).
pub const SCHEMA_VERSION: u32 = 4;

/// Oldest IR schema version this build still reads.
///
/// [`Snapshot::assemble`] upgrades an older IR: a version-2 IR carries no
/// [`Node::re_export`] flag, so the flag is derived from the re-export edges
/// that version 2 used to mark a re-export; a version-2 or version-3 IR states
/// no imports, so every file's imports read as unavailable.
pub const MIN_SCHEMA_VERSION: u32 = 2;

/// The raw, unvalidated typed graph plus its laminar container tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntermediateRepresentation {
    /// Bumped on any contract change; checked, and upgraded when older, by
    /// [`Snapshot::assemble`].
    pub schema_version: u32,
    /// All nodes in the graph.
    pub nodes: Vec<Node>,
    /// All directed edges in the graph.
    pub edges: Vec<Edge>,
    /// Non-dependency semantic relationships between declarations.
    pub affinities: Vec<Affinity>,
    /// The laminar container tree.
    pub containers: ContainerTree,
    /// Per-node ladder verdicts for declarations whose language states a scope
    /// ladder; empty for every other node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_ladders: Vec<ScopeLadder>,
    /// Name-matched uses of code the loader cannot see, merged from every
    /// fragment. Advice reads them as production users; no graph edge,
    /// score or move does (ADR-25). Added to schema 3; empty when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisory_uses: Vec<crate::AdvisoryUse>,
    /// Adapter-stated test pairs, entry files and folder companions, merged
    /// from every fragment. Pairs are language facts the engine reads before
    /// any user rule; an entry file never moves. Added to schema 3 (folder
    /// companions in schema 4); empty when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_facts: Vec<crate::FileFact>,
    /// Repo-relative directories of test-support crates (harnesses no other
    /// crate ships), merged from every fragment. The testOnly finding skips
    /// items under them. Added to schema 3; empty when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_support_roots: Vec<smol_str::SmolStr>,
    /// Adapter-stated file imports, merged from every fragment whose adapter
    /// states its imports (the merge drops the rest), so every importer is in
    /// `import_coverage`. Added to schema 4; empty when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<crate::FileImport>,
    /// Repo-relative files whose adapter stated their imports, so that a file
    /// listed here with no entry in `imports` imports nothing, while a file
    /// missing from it has unknown imports. Empty after upgrading a schema 2
    /// or 3 IR. Added to schema 4; empty when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_coverage: Vec<smol_str::SmolStr>,
}

/// What the language's scope ladder says about one declaration (ADR-25).
///
/// A declaration appears here whenever its adapter stated a scope for it, so the
/// visibility finding never falls back to level labels for it. `needed` is set
/// only when a strictly narrower rung than the declared one holds the
/// declaration and every user, signature exposure included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeLadder {
    /// The declaration the ladder belongs to.
    pub node: NodeId,
    /// Level of the rung the declaration states today; `None` when unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<ScopeLevel>,
    /// Level of the narrowest rung that still holds every user, set only when
    /// that rung is strictly narrower than the declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed: Option<ScopeLevel>,
    /// The language's spelling of the needed rung, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelling: Option<smol_str::SmolStr>,
}

impl IntermediateRepresentation {
    /// Builds an IR at the current [`SCHEMA_VERSION`] from its parts.
    #[must_use]
    pub fn new(nodes: Vec<Node>, edges: Vec<Edge>, containers: ContainerTree) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            nodes,
            edges,
            affinities: Vec::new(),
            containers,
            scope_ladders: Vec::new(),
            advisory_uses: Vec::new(),
            file_facts: Vec::new(),
            test_support_roots: Vec::new(),
            imports: Vec::new(),
            import_coverage: Vec::new(),
        }
    }
}

/// A validated, content-addressed input to the solver — immutable after assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    ir: IntermediateRepresentation,
    hash: blake3::Hash,
}

/// Reasons [`Snapshot::assemble`] rejects an [`IntermediateRepresentation`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotError {
    /// The IR was produced under an incompatible schema version.
    #[error(
        "unsupported schema version {found}; this build reads {oldest} through {expected}",
        oldest = MIN_SCHEMA_VERSION
    )]
    SchemaVersion {
        /// The version stamped on the IR.
        found: u32,
        /// The version this build understands.
        expected: u32,
    },

    /// An edge endpoint references a node id that has no node.
    #[error("edge from {source_node} to {target_node} references missing node {missing}")]
    DanglingEdge {
        /// The edge's source node id.
        source_node: u32,
        /// The edge's target node id.
        target_node: u32,
        /// The endpoint id (source or target) that has no node.
        missing: u32,
    },

    /// The container tree failed well-formedness validation.
    #[error("invalid container tree: {0}")]
    Tree(#[from] TreeError),

    /// The IR could not be serialized for canonical hashing.
    #[error("failed to serialize snapshot for hashing: {reason}")]
    Serialization {
        /// Human-readable serialization failure detail.
        reason: String,
    },
}

/// Upgrades an older, accepted IR to [`SCHEMA_VERSION`] in place.
///
/// Version 2 had no [`Node::re_export`] flag and marked a re-export only by the
/// [`EdgeKind::ReExport`] edge it sourced, so the flag is derived from those
/// edges. A v2 IR has no edge for an alias of a non-node target (`pub use crate
/// as alias`), so such an alias cannot be flagged here. Versions 2 and 3 state
/// no imports, so any import data they carry is dropped and every file's
/// imports read as unavailable. A current IR is left untouched.
fn upgrade(ir: &mut IntermediateRepresentation) {
    if ir.schema_version >= SCHEMA_VERSION {
        return;
    }
    if ir.schema_version < 3 {
        let re_exports: HashSet<NodeId> = ir
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ReExport)
            .map(|edge| edge.source)
            .collect();
        for node in &mut ir.nodes {
            node.re_export |= re_exports.contains(&node.id);
        }
    }
    ir.imports.clear();
    ir.import_coverage.clear();
    ir.schema_version = SCHEMA_VERSION;
}

impl Snapshot {
    /// Assembles a validated, hashed snapshot from an [`IntermediateRepresentation`].
    ///
    /// Assembly:
    /// 1. the schema version must lie in [`MIN_SCHEMA_VERSION`]..=[`SCHEMA_VERSION`];
    ///    an older IR is upgraded to [`SCHEMA_VERSION`] first,
    /// 2. every edge endpoint must reference an existing node,
    /// 3. the container tree must validate,
    /// 4. nodes, edges, and containers are canonicalized (sorted by id),
    /// 5. the hash is `blake3` over the canonical JSON serialization.
    ///
    /// Canonical sorting makes the hash independent of input order: identical
    /// inputs hash identically regardless of how entities were ordered.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] if the schema version is unsupported, an edge
    /// dangles, the tree is malformed, or serialization fails.
    pub fn assemble(mut ir: IntermediateRepresentation) -> Result<Self, SnapshotError> {
        if !(MIN_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&ir.schema_version) {
            return Err(SnapshotError::SchemaVersion {
                found: ir.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        upgrade(&mut ir);

        let node_ids: HashSet<NodeId> = ir.nodes.iter().map(|node| node.id).collect();
        for edge in &ir.edges {
            if !node_ids.contains(&edge.source) {
                return Err(SnapshotError::DanglingEdge {
                    source_node: edge.source.0,
                    target_node: edge.target.0,
                    missing: edge.source.0,
                });
            }
            if !node_ids.contains(&edge.target) {
                return Err(SnapshotError::DanglingEdge {
                    source_node: edge.source.0,
                    target_node: edge.target.0,
                    missing: edge.target.0,
                });
            }
        }
        for affinity in &ir.affinities {
            for endpoint in [affinity.owner, affinity.companion] {
                if !node_ids.contains(&endpoint) {
                    return Err(SnapshotError::DanglingEdge {
                        source_node: affinity.owner.0,
                        target_node: affinity.companion.0,
                        missing: endpoint.0,
                    });
                }
            }
        }

        ir.containers.validate()?;

        // Canonicalize so the hash is independent of input order. The edge key is
        // total: parallel edges sharing a (source, target, kind) triple are further
        // ordered by hardness and the raw bit pattern of confidence, so no input
        // ordering of duplicate edges can leak into the hash.
        ir.nodes.sort_by_key(|node| node.id.0);
        ir.edges.sort_by_key(|edge| {
            (
                edge.source.0,
                edge.target.0,
                edge.kind,
                edge.hardness,
                edge.confidence.to_bits(),
            )
        });
        ir.affinities.sort();
        ir.affinities.dedup();
        ir.file_facts.sort();
        ir.file_facts.dedup();
        ir.test_support_roots.sort();
        ir.test_support_roots.dedup();
        ir.imports.sort();
        ir.imports.dedup();
        ir.import_coverage.sort();
        ir.import_coverage.dedup();
        ir.containers.sort_by_id();

        let bytes = serde_json::to_vec(&ir).map_err(|error| SnapshotError::Serialization {
            reason: error.to_string(),
        })?;
        let hash = blake3::hash(&bytes);

        Ok(Self { ir, hash })
    }

    /// Returns the content hash of this snapshot.
    #[must_use]
    pub fn hash(&self) -> &blake3::Hash {
        &self.hash
    }

    /// Returns the validated, canonicalized intermediate representation.
    #[must_use]
    pub fn ir(&self) -> &IntermediateRepresentation {
        &self.ir
    }

    /// Whether the adapter of `file` stated its imports, so its entries in
    /// [`IntermediateRepresentation::imports`] are complete and an absent
    /// entry means "imports nothing". When `false`, a cycle finding through
    /// `file` is unavailable rather than empty: the IR predates schema 4, or
    /// the file's adapter cannot resolve imports.
    #[must_use]
    pub fn imports_available_for(&self, file: &str) -> bool {
        self.ir
            .import_coverage
            .binary_search_by(|covered| covered.as_str().cmp(file))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::container::{Container, ContainerId};
    use crate::edge::{EdgeKind, Hardness};
    use crate::node::{NodeKind, Polarity, ScopeLevel};
    use crate::{Affinity, AffinityKind, FileFact, FileImport, ImportKind};

    fn root_container() -> Container {
        Container {
            id: ContainerId(0),
            name: SmolStr::new("root"),
            level: ScopeLevel::File,
            parent: None,
            synthetic: false,
            label_keyed: false,
        }
    }

    fn node(id: u32, name: &str) -> Node {
        Node {
            public_api: false,
            advice_exempt: false,
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
        }
    }

    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    /// Builds a `Call` edge over the same node pair with a chosen hardness and
    /// confidence, used to exercise parallel-edge canonicalization.
    fn edge_with(source: u32, target: u32, hardness: Hardness, confidence: f64) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness,
            confidence,
        }
    }

    fn ir_with(nodes: Vec<Node>, edges: Vec<Edge>) -> IntermediateRepresentation {
        IntermediateRepresentation::new(nodes, edges, ContainerTree::new(vec![root_container()]))
    }

    #[test]
    fn should_assemble_a_valid_ir() {
        let ir = ir_with(vec![node(0, "a"), node(1, "b")], vec![edge(0, 1)]);

        let snapshot = Snapshot::assemble(ir).map(|snapshot| snapshot.ir().nodes.len());

        assert_eq!(snapshot, Ok(2));
    }

    #[test]
    fn should_reject_an_unsupported_schema_version() {
        let mut ir = ir_with(vec![node(0, "a")], vec![]);
        ir.schema_version = SCHEMA_VERSION + 1;

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::SchemaVersion {
                found: SCHEMA_VERSION + 1,
                expected: SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn should_reject_a_schema_version_older_than_the_oldest_readable() {
        let mut ir = ir_with(vec![node(0, "a")], vec![]);
        ir.schema_version = MIN_SCHEMA_VERSION - 1;

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::SchemaVersion {
                found: MIN_SCHEMA_VERSION - 1,
                expected: SCHEMA_VERSION,
            })
        );
    }

    /// A schema-2 IR as serialized before [`Node::re_export`] existed: node 1
    /// re-exports node 0 and is marked only by its re-export edge.
    const SCHEMA_2_IR: &str = r#"{
        "schema_version": 2,
        "nodes": [
            {"id": 0, "name": "original", "kind": "Symbol", "polarity": "Production",
             "container": 0, "visibility": "Package", "effective_size": 3},
            {"id": 1, "name": "original", "kind": "Symbol", "polarity": "Production",
             "container": 0, "visibility": "Package", "effective_size": 0}
        ],
        "edges": [
            {"source": 1, "target": 0, "kind": "ReExport", "hardness": "Soft", "confidence": 1.0}
        ],
        "affinities": [],
        "containers": {"containers": [
            {"id": 0, "name": "root", "level": "File", "parent": null}
        ]}
    }"#;

    #[test]
    fn should_load_a_schema_2_snapshot_and_upgrade_it() -> Result<(), String> {
        let ir: IntermediateRepresentation =
            serde_json::from_str(SCHEMA_2_IR).map_err(|error| error.to_string())?;

        let snapshot = Snapshot::assemble(ir).map_err(|error| error.to_string())?;

        let flags: Vec<bool> = snapshot
            .ir()
            .nodes
            .iter()
            .map(|node| node.re_export)
            .collect();
        assert_eq!(snapshot.ir().schema_version, SCHEMA_VERSION);
        assert_eq!(flags, vec![false, true]);
        Ok(())
    }

    /// A schema-3 IR: node 0 is flagged as a re-export and node 1 sources a
    /// re-export edge without the flag, which version 3 states explicitly.
    /// It carries import data a version-3 adapter could not have stated.
    const SCHEMA_3_IR: &str = r#"{
        "schema_version": 3,
        "nodes": [
            {"id": 0, "name": "alias", "kind": "Symbol", "polarity": "Production",
             "container": 0, "visibility": "Package", "effective_size": 0,
             "re_export": true},
            {"id": 1, "name": "original", "kind": "Symbol", "polarity": "Production",
             "container": 0, "visibility": "Package", "effective_size": 3}
        ],
        "edges": [
            {"source": 1, "target": 0, "kind": "ReExport", "hardness": "Soft", "confidence": 1.0}
        ],
        "affinities": [],
        "containers": {"containers": [
            {"id": 0, "name": "root", "level": "File", "parent": null}
        ]},
        "file_facts": [{"file": "src/a.ts", "entry": true}],
        "imports": [{"importer": "src/a.ts", "imported": "src/b.ts", "kind": "value",
                     "specifier": "./b", "line": 1}],
        "import_coverage": ["src/a.ts"]
    }"#;

    #[test]
    fn should_report_imports_unavailable_after_upgrading_a_schema_2_snapshot() -> Result<(), String>
    {
        let ir: IntermediateRepresentation =
            serde_json::from_str(SCHEMA_2_IR).map_err(|error| error.to_string())?;

        let snapshot = Snapshot::assemble(ir).map_err(|error| error.to_string())?;

        assert!(!snapshot.imports_available_for("root"));
        assert!(snapshot.ir().imports.is_empty());
        Ok(())
    }

    #[test]
    fn should_load_a_schema_3_snapshot_with_imports_unavailable() -> Result<(), String> {
        let ir: IntermediateRepresentation =
            serde_json::from_str(SCHEMA_3_IR).map_err(|error| error.to_string())?;

        let snapshot = Snapshot::assemble(ir).map_err(|error| error.to_string())?;

        let flags: Vec<bool> = snapshot
            .ir()
            .nodes
            .iter()
            .map(|node| node.re_export)
            .collect();
        assert_eq!(snapshot.ir().schema_version, SCHEMA_VERSION);
        assert_eq!(
            flags,
            vec![true, false],
            "v3 flags are kept, not re-derived"
        );
        assert_eq!(
            snapshot.ir().file_facts,
            vec![FileFact::entry_file("src/a.ts")]
        );
        assert!(
            snapshot.ir().imports.is_empty(),
            "stray v3 imports are dropped"
        );
        assert!(snapshot.ir().import_coverage.is_empty());
        assert!(!snapshot.imports_available_for("src/a.ts"));
        Ok(())
    }

    /// A schema-4 IR with a folder companion, two imports and a coverage list
    /// that also names a file importing nothing.
    fn schema_4_ir() -> IntermediateRepresentation {
        let mut ir = ir_with(vec![node(0, "a")], vec![]);
        ir.file_facts = vec![
            FileFact::folder_companion("src/lexer/block.ts", "src/lexer/block"),
            FileFact::entry_file("src/index.ts"),
        ];
        ir.imports = vec![
            FileImport {
                importer: SmolStr::new("src/lexer/block.ts"),
                imported: SmolStr::new("src/lexer/block/fence.ts"),
                kind: ImportKind::Value,
                specifier: SmolStr::new("./block/fence"),
                line: 2,
            },
            FileImport {
                importer: SmolStr::new("src/index.ts"),
                imported: SmolStr::new("src/lexer/block.ts"),
                kind: ImportKind::SideEffect,
                specifier: SmolStr::new("./lexer/block"),
                line: 1,
            },
        ];
        ir.import_coverage = vec![
            SmolStr::new("src/lexer/block/fence.ts"),
            SmolStr::new("src/lexer/block.ts"),
            SmolStr::new("src/index.ts"),
        ];
        ir
    }

    #[test]
    fn should_round_trip_a_schema_4_snapshot_with_a_stable_hash() -> Result<(), String> {
        let snapshot = Snapshot::assemble(schema_4_ir()).map_err(|error| error.to_string())?;

        let json = serde_json::to_string(snapshot.ir()).map_err(|error| error.to_string())?;
        let restored: IntermediateRepresentation =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        let reassembled = Snapshot::assemble(restored).map_err(|error| error.to_string())?;
        let mut reordered = schema_4_ir();
        reordered.imports.reverse();
        reordered.import_coverage.reverse();
        let shuffled = Snapshot::assemble(reordered).map_err(|error| error.to_string())?;

        assert_eq!(reassembled, snapshot);
        assert_eq!(shuffled.hash(), snapshot.hash());
        Ok(())
    }

    #[test]
    fn should_tell_a_file_importing_nothing_from_one_with_unknown_imports() -> Result<(), String> {
        let snapshot = Snapshot::assemble(schema_4_ir()).map_err(|error| error.to_string())?;

        assert!(snapshot.imports_available_for("src/lexer/block/fence.ts"));
        assert!(snapshot.imports_available_for("src/index.ts"));
        assert!(!snapshot.imports_available_for("src/main.rs"));
        Ok(())
    }

    #[test]
    fn should_omit_an_absent_folder_companion_and_round_trip_a_set_one() -> Result<(), String> {
        let plain = FileFact::entry_file("src/index.ts");
        let companion = FileFact::folder_companion("src/lexer/block.ts", "src/lexer/block");

        let plain_json = serde_json::to_string(&plain).map_err(|error| error.to_string())?;
        let companion_json =
            serde_json::to_string(&companion).map_err(|error| error.to_string())?;
        let restored: FileFact =
            serde_json::from_str(&companion_json).map_err(|error| error.to_string())?;

        assert!(!plain_json.contains("folder_companion"), "{plain_json}");
        assert_eq!(restored, companion);
        Ok(())
    }

    #[test]
    fn should_omit_unstated_imports_from_a_schema_4_snapshot() -> Result<(), String> {
        let snapshot = Snapshot::assemble(ir_with(vec![node(0, "a")], vec![]))
            .map_err(|error| error.to_string())?;

        let json = serde_json::to_string(snapshot.ir()).map_err(|error| error.to_string())?;

        assert!(!json.contains("imports"), "{json}");
        assert!(!json.contains("import_coverage"), "{json}");
        Ok(())
    }

    #[test]
    fn should_omit_an_unset_re_export_flag_and_round_trip_a_set_one() -> Result<(), String> {
        let original = node(0, "original");
        let re_export = Node {
            re_export: true,
            display_name: None,
            test_source: false,
            owner: None,
            ..node(1, "alias")
        };

        let plain = serde_json::to_string(&original).map_err(|error| error.to_string())?;
        let marked = serde_json::to_string(&re_export).map_err(|error| error.to_string())?;
        let restored: Node = serde_json::from_str(&marked).map_err(|error| error.to_string())?;

        assert!(!plain.contains("re_export"), "{plain}");
        assert_eq!(restored, re_export);
        Ok(())
    }

    #[test]
    fn should_reject_an_edge_with_a_missing_source() {
        let ir = ir_with(vec![node(1, "b")], vec![edge(0, 1)]);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::DanglingEdge {
                source_node: 0,
                target_node: 1,
                missing: 0,
            })
        );
    }

    #[test]
    fn should_reject_an_edge_with_a_missing_target() {
        let ir = ir_with(vec![node(0, "a")], vec![edge(0, 1)]);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::DanglingEdge {
                source_node: 0,
                target_node: 1,
                missing: 1,
            })
        );
    }

    #[test]
    fn should_preserve_companion_affinity_without_creating_a_dependency() {
        let nodes = vec![
            node(0, "assemble_artifact"),
            node(1, "AssembleArtifactParams"),
        ];
        let affinity = Affinity {
            owner: NodeId(0),
            companion: NodeId(1),
            kind: AffinityKind::CompanionOwner,
        };
        let mut single = ir_with(nodes.clone(), vec![]);
        single.affinities.push(affinity);
        let mut duplicated = ir_with(nodes, vec![]);
        duplicated.affinities.extend([affinity, affinity]);

        let assembled_single = Snapshot::assemble(single);
        let assembled_duplicated = Snapshot::assemble(duplicated);
        let evidence = assembled_single.and_then(|single_snapshot| {
            assembled_duplicated.map(|duplicated_snapshot| {
                (
                    duplicated_snapshot.ir().affinities.len(),
                    duplicated_snapshot.ir().edges.is_empty(),
                    duplicated_snapshot.hash() == single_snapshot.hash(),
                )
            })
        });

        assert_eq!(
            evidence,
            Ok((1, true, true)),
            "duplicate semantic affinities canonicalize and hash like one metadata relation"
        );
    }

    #[test]
    fn should_canonicalize_affinities_independently_of_input_order() {
        let mut forward = ir_with(
            vec![
                node(0, "assemble_artifact"),
                node(1, "AssembleArtifactParams"),
                node(2, "inspect_artifact"),
                node(3, "InspectArtifactParams"),
            ],
            vec![],
        );
        forward.affinities = vec![
            Affinity {
                owner: NodeId(0),
                companion: NodeId(1),
                kind: AffinityKind::CompanionOwner,
            },
            Affinity {
                owner: NodeId(2),
                companion: NodeId(3),
                kind: AffinityKind::CompanionOwner,
            },
        ];
        let mut reverse = forward.clone();
        reverse.affinities.reverse();

        assert_eq!(
            Snapshot::assemble(forward).map(|snapshot| *snapshot.hash().as_bytes()),
            Snapshot::assemble(reverse).map(|snapshot| *snapshot.hash().as_bytes())
        );
    }

    #[test]
    fn should_propagate_a_container_tree_error() {
        let bad_tree = ContainerTree::new(vec![Container {
            id: ContainerId(0),
            name: SmolStr::new("orphan"),
            level: ScopeLevel::File,
            parent: Some(ContainerId(9)),
            synthetic: false,
            label_keyed: false,
        }]);
        let ir = IntermediateRepresentation::new(vec![node(0, "a")], vec![], bad_tree);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::Tree(TreeError::MissingParent {
                child: 0,
                parent: 9,
            }))
        );
    }

    /// Assembles `ir` and returns its hash as bytes, or `None` on failure.
    fn hash_of(ir: IntermediateRepresentation) -> Option<[u8; 32]> {
        Snapshot::assemble(ir)
            .ok()
            .map(|snapshot| *snapshot.hash().as_bytes())
    }

    #[test]
    fn should_hash_identically_regardless_of_input_order() {
        let ordered = ir_with(vec![node(0, "a"), node(1, "b")], vec![edge(0, 1)]);
        let shuffled = ir_with(vec![node(1, "b"), node(0, "a")], vec![edge(0, 1)]);

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_identically_for_reordered_parallel_edges_differing_in_confidence() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let ordered = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 0.5),
                edge_with(0, 1, Hardness::Hard, 1.0),
            ],
        );
        let shuffled = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 1.0),
                edge_with(0, 1, Hardness::Hard, 0.5),
            ],
        );

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_identically_for_reordered_parallel_edges_differing_in_hardness() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let ordered = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 1.0),
                edge_with(0, 1, Hardness::Soft, 1.0),
            ],
        );
        let shuffled = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Soft, 1.0),
                edge_with(0, 1, Hardness::Hard, 1.0),
            ],
        );

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_differently_for_parallel_edges_differing_in_confidence() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let lower = ir_with(nodes(), vec![edge_with(0, 1, Hardness::Hard, 0.5)]);
        let higher = ir_with(nodes(), vec![edge_with(0, 1, Hardness::Hard, 1.0)]);

        assert_ne!(hash_of(lower), hash_of(higher));
    }

    #[test]
    fn should_hash_differently_for_different_content() {
        let one = ir_with(vec![node(0, "a")], vec![]);
        let two = ir_with(vec![node(0, "different")], vec![]);

        let one_hash = hash_of(one);
        let two_hash = hash_of(two);

        assert!(one_hash.is_some());
        assert_ne!(one_hash, two_hash);
    }
}
