//! Name arithmetic of the rendered tree: increments over a parent's cumulative
//! name, folder nesting, and the merge of duplicate sibling folders.

use crate::result::{ContainerNode, Level};

/// Returns `name`'s increment over its parent's cumulative name: the suffix it
/// adds when it extends the parent, its last segment when it repeats the parent
/// outright, and the whole name when the two are unrelated (or at the root).
pub(super) fn increment_name(name: &str, parent_name: &str) -> String {
    if parent_name.is_empty() {
        return name.to_owned();
    }
    if name == parent_name {
        return name.rsplit('/').next().unwrap_or(name).to_owned();
    }
    name.strip_prefix(parent_name)
        .and_then(|rest| rest.strip_prefix('/'))
        .map_or_else(|| name.to_owned(), str::to_owned)
}

/// Returns a folder's display increment: its increment over the parent domain's
/// key when the two relate, else its increment over the enclosing package's key.
///
/// A folder key that path-extends neither ancestor renders whole — the honest
/// display of a directory foreign to the whole package. The domain strip stays
/// first so the clean case (a folder under a same-key domain) is untouched; the
/// package fallback only replaces the old whole-key fallback, which re-embedded
/// the package segment as a fabricated directory chain (`atlas/agent` under the
/// domain keyed `atlas/core` rendered `atlas` → `agent`, but no `atlas`
/// subdirectory exists under any real `core`). Stripping the package key
/// displays the folder relative to its own package: `agent`.
pub(super) fn folder_increment(name: &str, parent_key: &str, package_key: &str) -> String {
    let against_parent = increment_name(name, parent_key);
    if against_parent != name {
        return against_parent;
    }
    increment_name(name, package_key)
}

/// Expands a folder's parent-relative directory path into a nested chain of
/// folder nodes, one per path segment, with `children` under the deepest.
///
/// Folders are reality: a multi-segment folder increment such as `a/b/c`
/// denotes real nested directories, so the DTO renders the chain
/// `a` → `b` → `c` rather than one slash-named node. Only the rendered
/// boundary nests folders under folders — the internal tree keeps one
/// slash-keyed folder per real directory as its stable identity.
pub(super) fn nest_folder_segments(increment: &str, children: Vec<ContainerNode>) -> ContainerNode {
    let mut segments = increment
        .split('/')
        .filter(|segment| !segment.is_empty())
        .rev();
    let mut node = ContainerNode {
        name: segments.next().unwrap_or(increment).to_owned(),
        level: Level::Folder,
        children: Some(children),
        symbols: None,
        production_sloc: None,
    };
    for segment in segments {
        node = ContainerNode {
            name: segment.to_owned(),
            level: Level::Folder,
            children: Some(vec![node]),
            symbols: None,
            production_sloc: None,
        };
    }
    node
}

/// Merges sibling folder nodes sharing a name into one directory trie.
///
/// Trie expansion can surface the same real parent directory from several
/// internal folder keys (`deep/x` and `deep/y` both render a `deep` node);
/// duplicate siblings would misstate the directory tree, so equal-named folder
/// siblings merge recursively, keeping first-seen order. Non-folder siblings
/// pass through untouched.
pub(super) fn merge_sibling_folders(children: Vec<ContainerNode>) -> Vec<ContainerNode> {
    let mut merged: Vec<ContainerNode> = Vec::new();
    for child in children {
        if child.level == Level::Folder
            && let Some(existing) = merged
                .iter_mut()
                .find(|node| node.level == Level::Folder && node.name == child.name)
        {
            let mut combined: Vec<ContainerNode> =
                existing.children.take().into_iter().flatten().collect();
            combined.extend(child.children.into_iter().flatten());
            existing.children = Some(merge_sibling_folders(combined));
            continue;
        }
        merged.push(child);
    }
    merged
}
