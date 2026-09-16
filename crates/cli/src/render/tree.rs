//! Container-tree rendering.

use std::io::{self, Write};

use strata_engine::{ContainerNode, Level};

/// Renders one container `node` as an indented tree to `out`.
///
/// `symbols` lists each file's symbols with their derived visibility; `depth`
/// truncates the tree below the given container depth (the root is depth zero),
/// and `None` renders the full tree.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing fails.
pub fn render_tree(
    node: &ContainerNode,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    write_tree_node(node, 0, symbols, depth, out)
}

/// Writes one tree node at `indent` and recurses into its children.
fn write_tree_node(
    node: &ContainerNode,
    indent: u32,
    symbols: bool,
    depth: Option<u32>,
    out: &mut impl Write,
) -> io::Result<()> {
    let pad = "  ".repeat(indent as usize);
    match node.production_sloc {
        Some(sloc) => writeln!(
            out,
            "{pad}{} [{}] {sloc} sloc",
            node.name,
            level_tag(node.level)
        )?,
        None => writeln!(out, "{pad}{} [{}]", node.name, level_tag(node.level))?,
    }

    if let Some(placements) = node.symbols.as_ref().filter(|_| symbols) {
        for placement in placements {
            writeln!(
                out,
                "{pad}  - {} ({})",
                placement.name,
                level_tag(placement.visibility)
            )?;
        }
    }

    if depth.is_some_and(|limit| indent + 1 > limit) {
        return Ok(());
    }
    if let Some(children) = &node.children {
        for child in children {
            write_tree_node(child, indent + 1, symbols, depth, out)?;
        }
    }
    Ok(())
}

/// Returns the lowercase tag of a scope level.
fn level_tag(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "packageGroup",
    }
}
