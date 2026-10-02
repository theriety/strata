//! The replica's file rows: one per snapshot file, plus path-identity checks.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::ir::{Polarity, ScopeLevel, Snapshot};
use strata_engine::result::{ContainerNode, Level};

/// One file of the analyzed snapshot, indexed in the engine's dense file order.
pub(super) struct FileRow {
    /// Owning file container id (the engine sorts files by this).
    pub(super) container: u32,
    /// Full rendered path (non-synthetic ancestor names plus the file name).
    pub(super) path: String,
    /// Real directory key: the non-synthetic `Folder`-level ancestors, `/`-joined.
    pub(super) dir: String,
    /// Source-declared file name; the dominant-file tie-break key.
    pub(super) name: String,
    /// Summed production SLOC of the production symbols the file owns.
    pub(super) production_sloc: u64,
}

/// Collects the snapshot's files in engine order (ascending container id).
///
/// The snapshot names every file container by its repo-relative path, which is
/// also the identity narration and both rendered trees use; the real directory
/// key is that path minus its last segment.
pub(super) fn collect_files(snapshot: &Snapshot) -> Vec<FileRow> {
    let ir = snapshot.ir();
    let mut sloc_of: BTreeMap<u32, u64> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        *sloc_of.entry(node.container.0).or_insert(0) += u64::from(node.effective_size);
    }
    let mut rows: Vec<FileRow> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|file| {
            let path = file.name.to_string();
            let dir = match path.rsplit_once('/') {
                Some((prefix, _)) => prefix.to_owned(),
                None => String::from("."),
            };
            FileRow {
                container: file.id.0,
                dir,
                name: file.name.to_string(),
                production_sloc: sloc_of.get(&file.id.0).copied().unwrap_or(0),
                path,
            }
        })
        .collect();
    rows.sort_by_key(|row| row.container);
    rows
}

/// Walks the rendered current tree and warns when the IR-derived file paths do
/// not reproduce the DTO's file identity exactly.
pub(super) fn validate_paths(name: &str, rows: &[FileRow], tree: &ContainerNode) {
    let mut rendered: BTreeSet<String> = BTreeSet::new();
    gather_files(tree, &mut rendered);
    let derived: BTreeSet<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    let ir_only: Vec<&str> = derived
        .iter()
        .filter(|path| !rendered.contains(**path))
        .copied()
        .collect();
    let dto_only: Vec<&str> = rendered
        .iter()
        .filter(|path| !derived.contains(path.as_str()))
        .map(String::as_str)
        .collect();
    if ir_only.is_empty() && dto_only.is_empty() {
        println!("== {name}: path identity verified ({} files)", rows.len());
        return;
    }
    println!(
        "== {name}: PATH MISMATCH ir_only={:?} dto_only={:?}",
        ir_only.first(),
        dto_only.first()
    );
}

/// Depth-first collection of every file path in a rendered tree; a file's own
/// container name is its repo-relative path, matching the snapshot's naming.
fn gather_files(node: &ContainerNode, out: &mut BTreeSet<String>) {
    if node.level == Level::File {
        out.insert(node.name.clone());
        return;
    }
    for child in node.children.iter().flatten() {
        gather_files(child, out);
    }
}
