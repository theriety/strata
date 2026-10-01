//! Hand-built result-tree nodes for tests: files, folders, and interior containers.

use crate::result::{ContainerNode, Level};

/// Builds a file node with `sloc` production SLOC.
pub(in crate::analyze) fn file(name: &str, sloc: u32) -> ContainerNode {
    ContainerNode {
        name: name.to_owned(),
        level: Level::File,
        children: None,
        symbols: Some(Vec::new()),
        production_sloc: Some(sloc),
    }
}

/// Builds a folder node holding `children`.
pub(in crate::analyze) fn folder(name: &str, children: Vec<ContainerNode>) -> ContainerNode {
    interior(name, Level::Folder, children)
}

/// Builds an interior node at `level` holding `children`.
pub(in crate::analyze) fn interior(
    name: &str,
    level: Level,
    children: Vec<ContainerNode>,
) -> ContainerNode {
    ContainerNode {
        name: name.to_owned(),
        level,
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

/// Returns the node's only child, or `None` when it has zero or several.
pub(in crate::analyze) fn only_child(node: ContainerNode) -> Option<ContainerNode> {
    node.children.and_then(|children| {
        if children.len() == 1 {
            children.into_iter().next()
        } else {
            None
        }
    })
}
