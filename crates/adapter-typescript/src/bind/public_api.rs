//! Which items code outside the repository can import (ADR-29, TypeScript rules
//! 1 to 4).
//!
//! A package that declares `exports` is importable only through the entries it
//! lists, so only what those entries reach is public API. Every other case keeps
//! the old answer, that an exported item is public: a package without `exports`
//! (deep imports work), an entry that maps to no file in the tree, and any chain
//! of re-exports that cannot be followed to its end.

mod entries;
mod reach;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use smol_str::SmolStr;
use unlace_ir::NodeId;

use super::ExportTable;
use super::resolution::Resolver;
use crate::package_json::{Manifest, load_manifest};
use crate::parse::ParsedModule;
use crate::tsconfig::load_output_mapping;
use entries::{Entries, entry_modules};
use reach::Graph;

/// What the public API pass reads from the bound modules.
pub(super) struct PublicApiInputs<'bound> {
    /// Repository root the module paths are relative to.
    pub(super) root: &'bound Path,
    /// Directories below the root that hold a `package.json`.
    pub(super) package_dirs: &'bound BTreeSet<SmolStr>,
    /// Every parsed module.
    pub(super) modules: &'bound [ParsedModule],
    /// Resolver for module specifiers.
    pub(super) resolver: &'bound Resolver,
    /// Exported nodes of each module.
    pub(super) exports: &'bound ExportTable,
    /// Every node of each module, exported or not.
    pub(super) local: &'bound ExportTable,
}

/// The verdict on which items are public API.
pub(super) enum PublicApi {
    /// No package narrows its surface: every exported item is public.
    Everything,
    /// Some packages list their entries: an item of such a package is public
    /// only when reached; any other item is public when exported.
    Narrowed {
        /// Every item of a package whose `exports` map was followed to its end.
        closed: HashSet<NodeId>,
        /// The items the entries of all packages reach.
        reached: HashSet<NodeId>,
    },
}

impl PublicApi {
    /// Whether the item is public API, given whether its module exports it.
    pub(super) fn admits(&self, node: NodeId, exported: bool) -> bool {
        match self {
            Self::Narrowed { closed, reached } if closed.contains(&node) => reached.contains(&node),
            Self::Everything | Self::Narrowed { .. } => exported,
        }
    }
}

/// Decides which items are public API.
pub(super) fn resolve(inputs: &PublicApiInputs<'_>) -> PublicApi {
    let manifests = load_manifests(inputs.root, inputs.package_dirs);
    if manifests
        .values()
        .all(|manifest| manifest.exports.is_none())
    {
        return PublicApi::Everything;
    }
    let known: BTreeSet<SmolStr> = inputs
        .modules
        .iter()
        .map(|module| module.path.clone())
        .collect();
    let package_names = manifests
        .values()
        .filter_map(|manifest| manifest.name.as_deref().map(SmolStr::new))
        .collect();
    let graph = Graph::new(
        inputs.modules,
        inputs.resolver,
        (inputs.exports, inputs.local),
        package_names,
    );

    let mut closed_dirs: HashSet<&str> = HashSet::new();
    let mut reached = HashSet::new();
    for (dir, manifest) in &manifests {
        let Some(exports) = &manifest.exports else {
            continue;
        };
        let output = load_output_mapping(&inputs.root.join(dir.as_str()));
        let Entries::Modules(entry_files) = entry_modules(dir, exports, output.as_ref(), &known)
        else {
            continue;
        };
        let reach = graph.reach_from(&entry_files, &|_| false);
        reached.extend(reach.nodes);
        if reach.complete {
            closed_dirs.insert(dir.as_str());
        }
    }
    if closed_dirs.is_empty() {
        return PublicApi::Everything;
    }

    // a package left open exposes everything it exports, and what it re-exports
    // from a closed package is importable through it. When that chain cannot be
    // followed, no closed package can be proved unexposed.
    let open_modules = inputs
        .modules
        .iter()
        .map(|module| &module.path)
        .filter(|path| {
            owning_package(path, &manifests).is_none_or(|dir| !closed_dirs.contains(dir))
        });
    // an unresolved relative specifier that leads into an open package, or into
    // none, hides nothing: every module there is a start of this same walk.
    let leads_outside_closed =
        |path: &str| owning_package(path, &manifests).is_none_or(|dir| !closed_dirs.contains(dir));
    let open_reach = graph.reach_from(open_modules, &leads_outside_closed);
    if !open_reach.complete {
        return PublicApi::Everything;
    }
    reached.extend(open_reach.nodes);

    let closed = inputs
        .local
        .values()
        .flat_map(|names| names.values().copied())
        .filter(|&node| {
            graph
                .module_of(node)
                .and_then(|path| owning_package(path, &manifests))
                .is_some_and(|dir| closed_dirs.contains(dir))
        })
        .collect();
    PublicApi::Narrowed { closed, reached }
}

/// The manifests of the packages in the tree, by directory (`""` is the root).
fn load_manifests(root: &Path, package_dirs: &BTreeSet<SmolStr>) -> BTreeMap<SmolStr, Manifest> {
    std::iter::once(&SmolStr::default())
        .chain(package_dirs)
        .filter_map(|dir| {
            load_manifest(&root.join(dir.as_str())).map(|manifest| (dir.clone(), manifest))
        })
        .collect()
}

/// The directory of the nearest package that encloses `path`, if any.
///
/// `path` may name a file or a folder, so a folder that holds a manifest is its
/// own package, with or without a trailing slash.
fn owning_package<'manifests>(
    path: &str,
    manifests: &'manifests BTreeMap<SmolStr, Manifest>,
) -> Option<&'manifests str> {
    let mut directory = Some(path.trim_end_matches('/'));
    while let Some(current) = directory {
        if let Some((dir, _)) = manifests.get_key_value(current) {
            return Some(dir.as_str());
        }
        directory = current.rsplit_once('/').map(|(parent, _)| parent);
    }
    manifests.get_key_value("").map(|(dir, _)| dir.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifests(dirs: &[&str]) -> BTreeMap<SmolStr, Manifest> {
        dirs.iter()
            .map(|dir| (SmolStr::new(dir), Manifest::default()))
            .collect()
    }

    #[test]
    fn should_attribute_a_file_to_its_nearest_enclosing_package() {
        let packages = manifests(&["", "packages/a", "packages/a/nested"]);

        assert_eq!(
            owning_package("packages/a/src/x.ts", &packages),
            Some("packages/a")
        );
        assert_eq!(
            owning_package("packages/a/nested/x.ts", &packages),
            Some("packages/a/nested")
        );
        assert_eq!(owning_package("other/x.ts", &packages), Some(""));
        assert_eq!(owning_package("x.ts", &packages), Some(""));
    }

    #[test]
    fn should_attribute_a_manifest_folder_to_itself() {
        let packages = manifests(&["", "packages/a"]);

        assert_eq!(owning_package("packages/a", &packages), Some("packages/a"));
        assert_eq!(owning_package("packages/a/", &packages), Some("packages/a"));
        assert_eq!(owning_package("packages/b", &packages), Some(""));
        assert_eq!(owning_package("", &packages), Some(""));
    }

    #[test]
    fn should_attribute_a_file_to_no_package_without_a_manifest_above_it() {
        let packages = manifests(&["packages/a"]);

        assert_eq!(owning_package("packages/b/x.ts", &packages), None);
    }

    #[test]
    fn should_admit_an_exported_item_when_no_package_narrows_its_surface() {
        assert!(PublicApi::Everything.admits(NodeId(0), true));
        assert!(!PublicApi::Everything.admits(NodeId(0), false));
    }

    #[test]
    fn should_admit_an_item_of_a_closed_package_only_when_reached() {
        let narrowed = PublicApi::Narrowed {
            closed: HashSet::from([NodeId(0), NodeId(1)]),
            reached: HashSet::from([NodeId(1)]),
        };

        assert!(!narrowed.admits(NodeId(0), true));
        assert!(narrowed.admits(NodeId(1), false));
        assert!(narrowed.admits(NodeId(2), true));
        assert!(!narrowed.admits(NodeId(2), false));
    }
}
