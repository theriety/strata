//! Namespace preservation: recomposes candidate physical parents from their
//! logical destinations (ADR-17, ADR-18).

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::ContainerTree;

use super::paths::physical_directory;
use super::placements::{FilePlacements, index_files};

/// Recomposes each candidate file's physical parent from its logical
/// destination and a namespace that belongs to the destination package
/// (ADR-17, ADR-18).
///
/// A file changes package only by landing in a folder cluster whose members
/// span packages. When every pass-start package of its candidate cluster's
/// members is one package, the file stays in that package whatever the
/// display-only upper levels elected above the cluster, and its scope is
/// re-rooted from the key it was expressed under onto that package. A mixed
/// cluster arises when the package wall is lifted and a file joins another
/// package's folder, or, with the wall up, when a cross-package import cycle
/// condenses into one SCC that stays in its pass-start cluster and admits no
/// newcomer (ADR-17). It takes the candidate's package when that is a real
/// pass-start package, otherwise (a cluster elected under a directory with no
/// manifest) the deepest pass-start package whose key prefixes the logical
/// destination, or the package group itself.
///
/// A namespace is package-relative, so the file keeps its own namespace when
/// the destination package has it, takes the package's only namespace
/// otherwise, and takes none when the package has no single one.
pub(super) fn preserve_pass_start_namespaces(before: &FilePlacements, after: &mut FilePlacements) {
    let mut namespaces_of: BTreeMap<&[String], BTreeSet<&[String]>> = BTreeMap::new();
    for (file, package) in &before.package_of {
        if let Some(namespace) = before.namespace_of.get(file) {
            namespaces_of
                .entry(package.as_slice())
                .or_default()
                .insert(namespace.as_slice());
        }
    }
    let mut packages_of_cluster: BTreeMap<u32, BTreeSet<&[String]>> = BTreeMap::new();
    for (file, cluster) in &after.cluster_of {
        if let Some(package) = before.package_of.get(file) {
            packages_of_cluster
                .entry(*cluster)
                .or_default()
                .insert(package.as_slice());
        }
    }
    for (file, logical) in &after.logical_parent_of {
        let Some(own) = before.namespace_of.get(file) else {
            continue;
        };
        let elected = after.package_of.get(file).map_or(&[][..], Vec::as_slice);
        let unanimous = after
            .cluster_of
            .get(file)
            .and_then(|cluster| packages_of_cluster.get(cluster))
            .filter(|packages| packages.len() == 1)
            .and_then(|packages| packages.first().copied());
        let (package, rerooted) = package_and_scope(&namespaces_of, elected, logical, unanimous);
        let namespace = kept_namespace(&namespaces_of, package, own);
        let mut physical = after.root_of.get(file).cloned().unwrap_or_default();
        physical.extend(physical_directory(package, namespace, &rerooted));
        after.parent_of.insert(file.clone(), physical);
    }
}

/// Chooses the real pass-start package one file is drawn under and its logical
/// scope expressed below that package's key. The candidate's package is kept
/// unless every file of the file's cluster comes from one other package
/// (`unanimous`), in which case the scope is re-rooted below the deepest
/// package key (elected or real) the logical destination is expressed under.
fn package_and_scope<'a>(
    namespaces_of: &BTreeMap<&'a [String], BTreeSet<&'a [String]>>,
    elected: &'a [String],
    logical: &'a [String],
    unanimous: Option<&'a [String]>,
) -> (&'a [String], Vec<String>) {
    let candidate_package = if namespaces_of.contains_key(elected) {
        elected
    } else {
        namespaces_of
            .keys()
            .filter(|package| logical.starts_with(package))
            .max_by_key(|package| package.len())
            .copied()
            .unwrap_or(&[])
    };
    match unanimous {
        Some(own_package) if own_package != candidate_package => {
            let keyed_under = namespaces_of
                .keys()
                .copied()
                .chain(std::iter::once(elected))
                .filter(|package| logical.starts_with(package))
                .max_by_key(|package| package.len())
                .unwrap_or(&[]);
            let mut rerooted = own_package.to_vec();
            rerooted.extend(logical.iter().skip(keyed_under.len()).cloned());
            (own_package, rerooted)
        }
        _ => (candidate_package, logical.to_vec()),
    }
}

/// The namespace a file keeps in `package`: its own when the package has it,
/// the package's only namespace otherwise, and none when it has no single one.
fn kept_namespace<'a>(
    namespaces_of: &BTreeMap<&'a [String], BTreeSet<&'a [String]>>,
    package: &[String],
    own: &'a [String],
) -> &'a [String] {
    let namespaces = namespaces_of.get(package);
    if namespaces.is_some_and(|set| set.contains(own)) {
        own
    } else {
        namespaces
            .filter(|set| set.len() == 1)
            .and_then(|set| set.first().copied())
            .unwrap_or(&[])
    }
}

/// Returns current and proposed physical parents after restoring each file's
/// immutable repository namespace onto the candidate's logical destination.
pub(crate) fn physical_relocation_folders(
    current: &ContainerTree,
    candidate: &ContainerTree,
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, Vec<String>>) {
    let before = index_files(current, false);
    let mut after = index_files(candidate, false);
    preserve_pass_start_namespaces(&before, &mut after);
    (before.parent_of, after.parent_of)
}
