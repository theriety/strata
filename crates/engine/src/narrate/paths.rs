//! Physical path algebra: package-relative namespaces and package-group
//! root projection (ADR-18).

/// Splits a file's real directory into its package-relative namespace
/// (ADR-18): the see-through source-root segments that the laminar folder
/// key omits.
///
/// `directory` is the file's real directory, `package` its manifest package
/// key and `folder` its laminar folder key, all relative to the package group.
/// The namespace is the directory below the package minus the longest common
/// trailing run it shares with the folder's scope below the package. For a
/// file under a see-through root that is the root itself (`src`); for a file
/// directly in its package it is empty. Relocation admission (each file's
/// pass-start namespace), capacity composition and narration all share this
/// one derivation, so the paths they reason about are the same real paths.
pub(crate) fn physical_namespace(
    directory: &[String],
    package: &[String],
    folder: &[String],
) -> Vec<String> {
    let relative = directory.strip_prefix(package).unwrap_or(directory);
    let scope = folder.strip_prefix(package).unwrap_or(folder);
    let common = relative
        .iter()
        .rev()
        .zip(scope.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    relative
        .get(..relative.len().saturating_sub(common))
        .unwrap_or_default()
        .to_vec()
}

/// Composes the real directory a file occupies in `folder`, restoring its
/// package-relative `namespace` between the package and the folder's scope.
///
/// The inverse of [`physical_namespace`]: `package ++ namespace ++ scope`. A
/// folder outside `package` keeps the namespace as a plain prefix.
pub(crate) fn physical_directory(
    package: &[String],
    namespace: &[String],
    folder: &[String],
) -> Vec<String> {
    let (prefix, scope) = folder
        .strip_prefix(package)
        .map_or((&[][..], folder), |scope| (package, scope));
    let mut directory = prefix.to_vec();
    directory.extend(namespace.iter().cloned());
    directory.extend(scope.iter().cloned());
    directory
}

/// Splits `/`-separated text into its non-empty segments.
pub(crate) fn path_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Drops a leading package-group (`dataset`) prefix from a laminar key.
pub(crate) fn drain_dataset(mut segments: Vec<String>, dataset: &[String]) -> Vec<String> {
    if segments.starts_with(dataset) {
        segments.drain(..dataset.len());
    }
    segments
}

/// Projects a repository-relative path beneath its package-group root.
///
/// `relative` is explicitly unrooted. Repeating the root name as its first
/// real directory is therefore significant and must be retained.
pub(crate) fn project_physical_path(root: &str, relative: &[String]) -> Vec<String> {
    let mut projected: Vec<String> = root
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    projected.extend(relative.iter().cloned());
    projected
}

/// Normalizes a path which may already carry the complete package-group root.
pub(crate) fn normalize_physical_path(root: &str, path: &[String]) -> Vec<String> {
    let root_segments: Vec<String> = root
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    if path.starts_with(&root_segments) {
        path.to_vec()
    } else {
        project_physical_path(root, path)
    }
}

pub(crate) fn project_package_rooted_path(root: &str, path: &[String]) -> Vec<String> {
    let root: Vec<String> = root
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    let overlap = (0..=root.len().min(path.len()))
        .rev()
        .find(|&length| root.get(root.len().saturating_sub(length)..) == path.get(..length))
        .unwrap_or(0);
    let mut projected = root;
    projected.extend(path.iter().skip(overlap).cloned());
    projected
}
