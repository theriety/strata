//! The TypeScript / TSX language adapter for Strata.
//!
//! It implements the two-phase [`strata_ir::Adapter`] contract: `parse`
//! turns `.ts` / `.tsx` sources into syntax summaries (via swc, in parallel),
//! and `bind` resolves the module graph into a language-agnostic
//! [`strata_ir::IrFragment`] — typed edges, three-valued test polarity, and
//! per-symbol production SLOC.
//!
//! The trait threads the intermediate `parse::ParsedModule` summary through
//! the opaque [`strata_ir::ParseTree`] payload as canonical JSON, so the engine
//! never needs to understand TypeScript to merge fragments.

mod bind;
mod package_json;
mod parse;
mod sloc;
mod tsconfig;

use std::collections::BTreeMap;
use std::path::PathBuf;

use smol_str::SmolStr;
use strata_ir::{Adapter, AdapterError, IrFragment, ParseTree, SourceFile};

use crate::parse::ParsedModule;

/// The TypeScript adapter: parses and binds `.ts` / `.tsx` sources.
///
/// The adapter is anchored at a repository `root`; module specifiers resolve
/// relative to it through three config surfaces read once at construction:
/// `tsconfig` `paths` aliases and the Node.js subpath-import map from the root
/// `package.json`.
#[derive(Debug, Clone)]
pub struct TypeScriptAdapter {
    /// Repository root that module paths are relative to.
    root: PathBuf,
    /// `tsconfig` `paths` alias prefix -> repo-relative target prefix.
    aliases: BTreeMap<SmolStr, SmolStr>,
    /// Node.js subpath-import specifier -> target from the root `package.json`.
    subpath_imports: BTreeMap<SmolStr, SmolStr>,
}

impl TypeScriptAdapter {
    /// Creates an adapter anchored at `root`, reading `root/tsconfig.json` for
    /// `paths` aliases and `root/package.json` for `imports` shortcuts when
    /// present.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let aliases = tsconfig::load_aliases(&root);
        let subpath_imports = package_json::load_subpath_imports(&root);
        Self {
            root,
            aliases,
            subpath_imports,
        }
    }
}

impl Default for TypeScriptAdapter {
    fn default() -> Self {
        Self::new(".")
    }
}

impl Adapter for TypeScriptAdapter {
    fn parse(&self, files: &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError> {
        let modules = parse::parse(files)?;
        modules.iter().map(serialize_module).collect()
    }

    fn bind(&self, trees: Vec<ParseTree>) -> Result<IrFragment, AdapterError> {
        let modules = trees
            .iter()
            .map(deserialize_module)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(bind::bind(
            &modules,
            &self.root,
            &self.aliases,
            &self.subpath_imports,
        ))
    }
}

/// Serializes a [`ParsedModule`] into an opaque [`ParseTree`] payload.
fn serialize_module(module: &ParsedModule) -> Result<ParseTree, AdapterError> {
    let payload = serde_json::to_string(module).map_err(|error| AdapterError::Parse {
        path: module.path.clone(),
        reason: format!("failed to serialize parse summary: {error}"),
    })?;
    Ok(ParseTree {
        path: module.path.clone(),
        payload,
    })
}

/// Deserializes a [`ParseTree`] payload back into a [`ParsedModule`].
fn deserialize_module(tree: &ParseTree) -> Result<ParsedModule, AdapterError> {
    serde_json::from_str(&tree.payload).map_err(|error| AdapterError::Bind {
        path: tree.path.clone(),
        reason: format!("failed to read parse summary: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use strata_ir::{AffinityKind, EdgeKind, NodeKind};

    use super::*;

    fn bind_source(contents: &str) -> IrFragment {
        bind_sources(&[("src/fixture.ts", contents)])
    }

    fn bind_sources(sources: &[(&str, &str)]) -> IrFragment {
        let adapter = TypeScriptAdapter::default();
        let files: Vec<SourceFile> = sources
            .iter()
            .map(|(path, contents)| SourceFile {
                path: SmolStr::new(*path),
                contents: (*contents).to_owned(),
            })
            .collect();
        let trees = adapter.parse(&files);
        assert!(trees.is_ok(), "neutral TypeScript fixture must parse");
        let fragment = adapter.bind(trees.unwrap_or_default());
        assert!(fragment.is_ok(), "neutral TypeScript fixture must bind");
        fragment.unwrap_or_default()
    }

    fn companion_pairs(fragment: &IrFragment) -> Vec<(String, String)> {
        fragment
            .affinities
            .iter()
            .filter(|affinity| affinity.kind == AffinityKind::CompanionOwner)
            .filter_map(|affinity| {
                let owner = fragment
                    .nodes
                    .iter()
                    .find(|node| node.id == affinity.owner)?;
                let companion = fragment
                    .nodes
                    .iter()
                    .find(|node| node.id == affinity.companion)?;
                Some((owner.name.to_string(), companion.name.to_string()))
            })
            .collect()
    }

    fn companion_file(fragment: &IrFragment) -> Option<&str> {
        let affinity = fragment
            .affinities
            .iter()
            .find(|affinity| affinity.kind == AffinityKind::CompanionOwner)?;
        let companion = fragment
            .nodes
            .iter()
            .find(|node| node.id == affinity.companion)?;
        fragment
            .containers
            .iter()
            .find(|container| container.id == companion.container)
            .map(|container| container.name.as_str())
    }

    fn bind_with_serialized_re_export_binding(
        binding: &serde_json::Value,
    ) -> Result<IrFragment, AdapterError> {
        let adapter = TypeScriptAdapter::default();
        let files = [
            SourceFile {
                path: SmolStr::new("src/public.ts"),
                contents: "export type { BuildArtifactParams } from './artifact'".to_owned(),
            },
            SourceFile {
                path: SmolStr::new("src/artifact.ts"),
                contents: "export interface BuildArtifactParams { label: string }".to_owned(),
            },
        ];
        let parsed = adapter.parse(&files);
        assert!(parsed.is_ok());
        let mut trees = parsed.unwrap_or_default();
        if let Some(barrel) = trees.iter_mut().find(|tree| tree.path == "src/public.ts") {
            let mut payload =
                serde_json::from_str::<serde_json::Value>(&barrel.payload).unwrap_or_default();
            if let Some(names) = payload.pointer_mut("/re_exports/0/names") {
                *names = serde_json::json!([binding]);
            }
            barrel.payload = serde_json::to_string(&payload).unwrap_or_default();
        }
        adapter.bind(trees)
    }

    #[test]
    fn should_emit_affinity_for_an_exact_function_signature_companion() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(input: BuildArtifactParams): string { return input.label }",
        );

        assert_eq!(
            companion_pairs(&fragment),
            vec![("buildArtifact".to_owned(), "BuildArtifactParams".to_owned())]
        );
        assert!(
            fragment.edges.iter().any(|edge| {
                let owner = fragment.nodes.iter().find(|node| node.id == edge.source);
                let companion = fragment.nodes.iter().find(|node| node.id == edge.target);
                owner.is_some_and(|node| node.name == "buildArtifact")
                    && companion.is_some_and(|node| node.name == "BuildArtifactParams")
            }),
            "signature affinity does not replace the ordinary type-reference edge"
        );
    }

    #[test]
    fn should_emit_affinity_for_a_class_method_signature_companion() {
        let fragment = bind_source(
            "export interface VendorBuildRequestParams { label: string }\n\
             export class VendorAdapter {\n\
               buildRequest(input: VendorBuildRequestParams): string { return input.label }\n\
             }",
        );

        assert_eq!(
            companion_pairs(&fragment),
            vec![(
                "VendorAdapter".to_owned(),
                "VendorBuildRequestParams".to_owned()
            )]
        );
    }

    #[test]
    fn should_normalize_grammatical_and_ing_tokens_for_companion_matching() {
        let fragment = bind_source(
            "export interface StatusErrorMappingParams { status: number }\n\
             export function mapStatusToError(input: StatusErrorMappingParams): Error {\n\
               return new Error(String(input.status))\n\
             }",
        );

        assert_eq!(
            companion_pairs(&fragment),
            vec![(
                "mapStatusToError".to_owned(),
                "StatusErrorMappingParams".to_owned()
            )]
        );
    }

    #[test]
    fn should_not_infer_affinity_from_a_single_mismatched_signature_consumer() {
        let fragment = bind_source(
            "export interface ArchiveArtifactParams { label: string }\n\
             export function buildArtifact(input: ArchiveArtifactParams): string { return input.label }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_not_infer_affinity_for_a_generic_one_token_companion_name() {
        let fragment = bind_source(
            "export interface RequestOptions { retries: number }\n\
             export function request(input: RequestOptions): number { return input.retries }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_not_infer_affinity_from_a_body_only_type_reference() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(): string {\n\
               const input = {} as BuildArtifactParams\n\
               return input.label\n\
             }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_not_infer_affinity_when_multiple_owners_match() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(input: BuildArtifactParams): string { return input.label }\n\
             export function artifactBuild(input: BuildArtifactParams): string { return input.label }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_not_infer_affinity_when_two_methods_of_one_class_match() {
        let fragment = bind_source(
            "export interface VendorBuildRequestParams { label: string }\n\
             export class VendorAdapter {\n\
               buildRequest(input: VendorBuildRequestParams): string { return input.label }\n\
               requestBuild(input: VendorBuildRequestParams): string { return input.label }\n\
             }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_recognize_annotated_destructured_signature_patterns() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export type CollectArtifactInput = [string]\n\
             export function buildArtifact({ label }: BuildArtifactParams): string { return label }\n\
             export function collectArtifact([label]: CollectArtifactInput): string { return label }",
        );

        let mut pairs = companion_pairs(&fragment);
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("buildArtifact".to_owned(), "BuildArtifactParams".to_owned()),
                (
                    "collectArtifact".to_owned(),
                    "CollectArtifactInput".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn should_recognize_an_annotated_rest_parameter_as_signature_evidence() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(...inputs: BuildArtifactParams[]): string {\n\
               return inputs[0]?.label ?? ''\n\
             }",
        );

        assert_eq!(
            companion_pairs(&fragment),
            vec![("buildArtifact".to_owned(), "BuildArtifactParams".to_owned())]
        );
    }

    #[test]
    fn should_recognize_an_annotated_defaulted_parameter_as_signature_evidence() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(\n\
               input: BuildArtifactParams = { label: 'fixture' },\n\
             ): string { return input.label }",
        );

        assert_eq!(
            companion_pairs(&fragment),
            vec![("buildArtifact".to_owned(), "BuildArtifactParams".to_owned())]
        );
    }

    #[test]
    fn should_not_infer_affinity_from_a_type_used_only_in_a_default_expression() {
        let fragment = bind_source(
            "export interface BuildArtifactParams { label: string }\n\
             export function buildArtifact(\n\
               input = {} as BuildArtifactParams,\n\
             ): string { return input.label }",
        );

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_resolve_a_barrel_imported_companion_to_its_originating_declaration() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import type { BuildArtifactParams } from './types'\n\
                 export function buildArtifact(input: BuildArtifactParams): string {\n\
                   return input.label\n\
                 }",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        let affinity = fragment
            .affinities
            .iter()
            .find(|affinity| affinity.kind == AffinityKind::CompanionOwner);
        assert!(affinity.is_some(), "the signature still emits an affinity");
        let companion = affinity.and_then(|affinity| {
            fragment
                .nodes
                .iter()
                .find(|node| node.id == affinity.companion)
        });
        let companion_file = companion.and_then(|node| {
            fragment
                .containers
                .iter()
                .find(|container| container.id == node.container)
        });

        assert_eq!(
            companion_file.map(|container| container.name.as_str()),
            Some("src/types/artifact.ts"),
            "affinity targets the originating type declaration, never its barrel binding"
        );
    }

    #[test]
    fn should_resolve_an_adverse_order_multi_hop_barrel_to_the_originating_declaration() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import type { BuildArtifactParams } from './public'\n\
                 export function buildArtifact(input: BuildArtifactParams): string {\n\
                   return input.label\n\
                 }",
            ),
            (
                "src/public.ts",
                "export type { BuildArtifactParams } from './types'",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        assert_eq!(
            companion_file(&fragment),
            Some("src/types/artifact.ts"),
            "multi-hop affinity resolution reaches the declaration even when outer barrels bind first"
        );
    }

    #[test]
    fn should_resolve_multi_hop_barrel_affinity_independently_of_module_order() {
        let consumer = (
            "src/feature.ts",
            "import type { BuildArtifactParams } from './public'\n\
             export function buildArtifact(input: BuildArtifactParams): string { return input.label }",
        );
        let outer = (
            "src/public.ts",
            "export type { BuildArtifactParams } from './types'",
        );
        let inner = (
            "src/types/index.ts",
            "export type { BuildArtifactParams } from './artifact'",
        );
        let origin = (
            "src/types/artifact.ts",
            "export interface BuildArtifactParams { label: string }",
        );

        let adverse = bind_sources(&[consumer, outer, inner, origin]);
        let favorable = bind_sources(&[origin, inner, outer, consumer]);

        assert_eq!(companion_file(&adverse), companion_file(&favorable));
        assert_eq!(companion_file(&favorable), Some("src/types/artifact.ts"));
    }

    #[test]
    fn should_terminate_and_suppress_affinity_for_a_re_export_loop_without_a_declaration() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import type { BuildArtifactParams } from './first'\n\
                 export function buildArtifact(input: BuildArtifactParams): string { return input.label }",
            ),
            (
                "src/first.ts",
                "export type { BuildArtifactParams } from './second'",
            ),
            (
                "src/second.ts",
                "export type { BuildArtifactParams } from './first'",
            ),
        ]);

        assert!(companion_pairs(&fragment).is_empty());
    }

    #[test]
    fn should_leave_dependency_edges_unchanged_when_resolving_companion_affinity() {
        let matching = bind_sources(&[
            (
                "src/feature.ts",
                "import type { BuildArtifactParams } from './types'\n\
                 export function buildArtifact(input: BuildArtifactParams): string { return input.label }",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);
        let mismatched = bind_sources(&[
            (
                "src/feature.ts",
                "import type { BuildArtifactParams } from './types'\n\
                 export function archiveArtifact(input: BuildArtifactParams): string { return input.label }",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        assert_eq!(matching.edges, mismatched.edges);
        assert_eq!(
            matching
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::ReExport)
                .count(),
            1,
            "affinity resolution preserves the ordinary barrel re-export edge"
        );
        assert!(
            matching
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::TypeReference),
            "affinity resolution preserves the ordinary signature dependency"
        );
    }

    #[test]
    fn should_resolve_a_renamed_barrel_companion_to_its_originating_declaration() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import type { AssembleArtifactParams } from './types'\n\
                 export function assembleArtifact(input: AssembleArtifactParams): string {\n\
                   return input.label\n\
                 }",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams as AssembleArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        assert_eq!(
            companion_file(&fragment),
            Some("src/types/artifact.ts"),
            "the public alias resolves affinity to the original declaration"
        );
        assert!(
            fragment
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::TypeReference),
            "the alias keeps its ordinary signature type-reference edge"
        );
        let re_exports: Vec<_> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ReExport)
            .collect();
        assert_eq!(re_exports.len(), 1);
        let target = re_exports
            .first()
            .and_then(|edge| fragment.nodes.iter().find(|node| node.id == edge.target));
        assert_eq!(
            target.map(|node| node.name.as_str()),
            Some("BuildArtifactParams")
        );
    }

    #[test]
    fn should_resolve_a_multi_hop_renamed_barrel_companion_and_terminate_its_edges() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import type { AssembleArtifactParams } from './public'\n\
                 export function assembleArtifact(input: AssembleArtifactParams): string {\n\
                   return input.label\n\
                 }",
            ),
            (
                "src/public.ts",
                "export type { PrepareArtifactParams as AssembleArtifactParams } from './types'",
            ),
            (
                "src/types/index.ts",
                "export type { BuildArtifactParams as PrepareArtifactParams } from './artifact'",
            ),
            (
                "src/types/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        assert_eq!(companion_file(&fragment), Some("src/types/artifact.ts"));
        assert!(
            fragment
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::TypeReference)
        );
        let re_exports: Vec<_> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ReExport)
            .collect();
        assert_eq!(re_exports.len(), 2, "both alias hops remain graph edges");
        assert!(
            re_exports.iter().any(|edge| {
                fragment
                    .nodes
                    .iter()
                    .find(|node| node.id == edge.target)
                    .is_some_and(|node| node.name == "BuildArtifactParams")
            }),
            "the re-export chain terminates at the original declaration"
        );
    }

    #[test]
    fn should_preserve_a_namespace_re_export_without_manufacturing_companion_affinity() {
        let fragment = bind_sources(&[
            (
                "src/feature.ts",
                "import { toolkit } from './public'\n\
                 import type { BuildArtifactParams } from './artifact'\n\
                 export function assembleArtifact(input: toolkit.BuildArtifactParams): string {\n\
                   return input.label\n\
                 }\n\
                 export function inspectArtifact(input: BuildArtifactParams): string {\n\
                   return input.label\n\
                 }",
            ),
            ("src/public.ts", "export * as toolkit from './artifact'"),
            (
                "src/artifact.ts",
                "export interface BuildArtifactParams { label: string }",
            ),
        ]);

        assert!(
            fragment
                .nodes
                .iter()
                .any(|node| node.name == "toolkit" && node.re_export),
            "the namespace re-export remains a graph-visible binding, flagged as a re-export"
        );
        assert!(
            fragment
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::ReExport),
            "the namespace re-export remains an ordinary graph edge"
        );
        assert!(
            fragment
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::TypeReference),
            "the direct ordinary type import remains a dependency control"
        );
        assert!(
            companion_pairs(&fragment).is_empty(),
            "qualified namespace use alone never manufactures companion ownership"
        );
    }

    #[test]
    fn should_serialize_renamed_re_export_bindings_as_typed_unicode_safe_data() {
        let adapter = TypeScriptAdapter::default();
        let files = [SourceFile {
            path: SmolStr::new("src/public.ts"),
            contents: "export type { Δομή as 組立ArtifactParams } from './artifact'".to_owned(),
        }];
        let parsed = adapter.parse(&files);
        assert!(parsed.is_ok());
        let tree = parsed.unwrap_or_default().into_iter().next();
        let payload = tree
            .as_ref()
            .and_then(|tree| serde_json::from_str::<serde_json::Value>(&tree.payload).ok())
            .unwrap_or_default();

        assert_eq!(
            payload.pointer("/re_exports/0/names/0"),
            Some(&serde_json::json!({
                "original": "Δομή",
                "exported": "組立ArtifactParams"
            })),
            "renamed bindings serialize structurally without packed control strings"
        );
        let round_trip = serde_json::to_string(&payload)
            .ok()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .unwrap_or_default();
        assert_eq!(
            round_trip.pointer("/re_exports/0/names/0"),
            payload.pointer("/re_exports/0/names/0"),
            "Unicode original and exported identifiers survive serialization"
        );
    }

    #[test]
    fn should_bind_a_legacy_plain_name_re_export_payload_equivalently() {
        let adapter = TypeScriptAdapter::default();
        let files = [
            SourceFile {
                path: SmolStr::new("src/public.ts"),
                contents: "export type { BuildArtifactParams } from './artifact'".to_owned(),
            },
            SourceFile {
                path: SmolStr::new("src/artifact.ts"),
                contents: "export interface BuildArtifactParams { label: string }".to_owned(),
            },
        ];
        let parsed = adapter.parse(&files);
        assert!(parsed.is_ok());
        let canonical_trees = parsed.unwrap_or_default();
        let mut legacy_trees = canonical_trees.clone();
        if let Some(barrel) = legacy_trees
            .iter_mut()
            .find(|tree| tree.path == "src/public.ts")
        {
            let mut payload =
                serde_json::from_str::<serde_json::Value>(&barrel.payload).unwrap_or_default();
            if let Some(names) = payload.pointer_mut("/re_exports/0/names") {
                *names = serde_json::json!(["BuildArtifactParams"]);
            }
            barrel.payload = serde_json::to_string(&payload).unwrap_or_default();
        }

        let canonical = adapter.bind(canonical_trees);
        let legacy = adapter.bind(legacy_trees);
        assert!(canonical.is_ok() && legacy.is_ok());
        assert_eq!(canonical.unwrap_or_default(), legacy.unwrap_or_default());
    }

    #[test]
    fn should_never_infer_companion_affinity_from_a_matching_namespace_alias_or_collapse_fanout() {
        let consumer = (
            "src/feature.ts",
            "import { AssembleArtifactParams } from './public'\n\
             export function assembleArtifact(\n\
               input: AssembleArtifactParams.BuildArtifactParams,\n\
             ): string { return input.label }",
        );
        let namespace = (
            "src/public.ts",
            "export * as AssembleArtifactParams from './artifact'",
        );
        let target = (
            "src/artifact.ts",
            "export interface BuildArtifactParams { label: string }\n\
             export interface InspectArtifactOptions { verbose: boolean }",
        );

        for fragment in [
            bind_sources(&[consumer, namespace, target]),
            bind_sources(&[target, namespace, consumer]),
        ] {
            assert!(
                companion_pairs(&fragment).is_empty(),
                "a namespace binding is not a type declaration and cannot own companion affinity"
            );
            assert!(
                fragment
                    .nodes
                    .iter()
                    .any(|node| node.name == "AssembleArtifactParams"),
                "the namespace alias remains graph-visible"
            );
            assert_eq!(
                fragment
                    .edges
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::ReExport)
                    .count(),
                2,
                "namespace fanout retains one re-export edge per exported declaration"
            );
        }
    }

    #[test]
    fn should_never_infer_companion_affinity_from_a_matching_single_target_namespace() {
        let consumer = (
            "src/feature.ts",
            "import { AssembleArtifactParams } from './public'\n\
             export function assembleArtifact(\n\
               input: AssembleArtifactParams.BuildArtifactParams,\n\
             ): string { return input.label }",
        );
        let namespace = (
            "src/public.ts",
            "export * as AssembleArtifactParams from './artifact'",
        );
        let target = (
            "src/artifact.ts",
            "export interface BuildArtifactParams { label: string }",
        );

        for fragment in [
            bind_sources(&[consumer, namespace, target]),
            bind_sources(&[target, namespace, consumer]),
        ] {
            assert!(
                companion_pairs(&fragment).is_empty(),
                "a namespace alias never becomes a companion even with one target"
            );
            assert!(
                fragment
                    .nodes
                    .iter()
                    .any(|node| node.name == "AssembleArtifactParams")
            );
            assert!(
                fragment
                    .edges
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::ReExport)
            );
        }
    }

    #[test]
    fn should_reject_a_malformed_serialized_re_export_binding_instead_of_dropping_it() {
        let adapter = TypeScriptAdapter::default();
        let files = [
            SourceFile {
                path: SmolStr::new("src/public.ts"),
                contents: "export type { BuildArtifactParams } from './artifact'".to_owned(),
            },
            SourceFile {
                path: SmolStr::new("src/artifact.ts"),
                contents: "export interface BuildArtifactParams { label: string }".to_owned(),
            },
        ];
        let parsed = adapter.parse(&files);
        assert!(parsed.is_ok());
        let mut trees = parsed.unwrap_or_default();
        if let Some(barrel) = trees.iter_mut().find(|tree| tree.path == "src/public.ts") {
            let mut payload =
                serde_json::from_str::<serde_json::Value>(&barrel.payload).unwrap_or_default();
            if let Some(names) = payload.pointer_mut("/re_exports/0/names") {
                *names = serde_json::json!([{ "original": "BuildArtifactParams" }]);
            }
            barrel.payload = serde_json::to_string(&payload).unwrap_or_default();
        }

        assert!(
            adapter.bind(trees).is_err(),
            "malformed serialized bindings fail closed instead of disappearing"
        );
    }

    #[test]
    fn should_reject_a_serialized_binding_that_mixes_named_and_namespace_fields() {
        let result = bind_with_serialized_re_export_binding(&serde_json::json!({
            "original": "BuildArtifactParams",
            "exported": "AssembleArtifactParams",
            "namespace": "toolkit"
        }));

        assert!(matches!(
            result,
            Err(AdapterError::Bind { reason, .. })
                if reason.contains("failed to read parse summary")
        ));
    }

    #[test]
    fn should_reject_unknown_fields_on_a_serialized_named_binding() {
        let result = bind_with_serialized_re_export_binding(&serde_json::json!({
            "original": "BuildArtifactParams",
            "exported": "AssembleArtifactParams",
            "unexpected": true
        }));

        assert!(matches!(
            result,
            Err(AdapterError::Bind { reason, .. })
                if reason.contains("failed to read parse summary")
        ));
    }

    #[test]
    fn should_reject_unknown_fields_on_a_serialized_namespace_binding() {
        let result = bind_with_serialized_re_export_binding(&serde_json::json!({
            "namespace": "toolkit",
            "unexpected": true
        }));

        assert!(matches!(
            result,
            Err(AdapterError::Bind { reason, .. })
                if reason.contains("failed to read parse summary")
        ));
    }

    #[test]
    fn should_not_infer_companion_affinity_for_a_runtime_symbol() {
        let fragment = bind_source(
            "export const BuildArtifactParams = { label: 'fixture' }\n\
             export function buildArtifact(input = BuildArtifactParams): string { return input.label }",
        );

        assert!(
            fragment
                .nodes
                .iter()
                .any(|node| node.name == "BuildArtifactParams" && node.kind == NodeKind::Symbol)
        );
        assert!(companion_pairs(&fragment).is_empty());
    }
}
