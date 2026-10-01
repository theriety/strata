//! Laminar home keys: the folder, domain, and package names a file already resolved to.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_ir::{Container, ScopeLevel};

use crate::narrate::{path_segments, physical_namespace};

/// Renders a file's package-relative namespace (ADR-18) from its real path
/// and laminar home — the see-through source-root segments its folder key
/// omits (`src` for `crates/core/src/x.rs`).
pub(in crate::analyze) fn render_namespace(path: &str, home: &LaminarHome) -> SmolStr {
    let directory = path_segments(path.rsplit_once('/').map_or("", |(directory, _)| directory));
    let package = path_segments(&home.package);
    let folder = if home.synthetic {
        package.clone()
    } else {
        path_segments(&home.folder)
    };
    SmolStr::new(physical_namespace(&directory, &package, &folder).join("/"))
}

/// The laminar container tree's already-resolved folder, domain, and package
/// name keys for one file — full-prefix keys (`ai/adapters`, `ai`) with any
/// transparent source-root segment stripped and the package resolved to its
/// nearest manifest root.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::analyze) struct LaminarHome {
    /// The file's folder-level container name key.
    pub(in crate::analyze) folder: SmolStr,
    /// The file's domain-level container name key.
    pub(in crate::analyze) domain: SmolStr,
    /// The file's package-level container name key.
    pub(in crate::analyze) package: SmolStr,
    /// True when the folder-level container is the synthetic `workspace` bucket
    /// (a root-level file with no real directory of its own). Functionally
    /// determined by `folder`, so it never splits two otherwise-equal homes into
    /// distinct clusters; it carries the current tree's collapse marker across to
    /// the candidate folder so a greenfield render drops the bucket too.
    pub(in crate::analyze) synthetic: bool,
}

/// Reads the laminar folder/domain/package name keys for the file container
/// `file_container` by walking its ancestor chain in `by_id`. The laminar tree
/// (`build_laminar_tree`) already stripped a transparent leading source root and
/// resolved the nearest package root, so reusing its names keeps candidate
/// naming consistent with the current tree instead of re-deriving from raw paths
/// (which would surface `src` as a package/folder). Folders and package roots
/// nest, so the NEAREST ancestor at each level wins — deeper keys are the real
/// place. A level missing from the chain inherits the nearest broader key (a
/// file directly in its domain directory has that directory as its real
/// folder), and only a chain with no package at all falls back to the laminar
/// synthetic `workspace` bucket.
pub(in crate::analyze) fn laminar_home(
    by_id: &BTreeMap<u32, &Container>,
    file_container: u32,
) -> LaminarHome {
    let (mut folder, mut domain, mut package) = (None, None, None);
    let mut synthetic = false;
    let mut current = by_id.get(&file_container).copied();
    while let Some(container) = current {
        match container.level {
            ScopeLevel::Folder if folder.is_none() => {
                folder = Some(container.name.clone());
                synthetic = container.synthetic;
            }
            ScopeLevel::Domain if domain.is_none() => domain = Some(container.name.clone()),
            ScopeLevel::Package if package.is_none() => package = Some(container.name.clone()),
            _ => {}
        }
        current = container
            .parent
            .and_then(|parent| by_id.get(&parent.0).copied());
    }
    let package = package.unwrap_or_else(|| SmolStr::new("workspace"));
    let domain = domain.unwrap_or_else(|| package.clone());
    let folder = folder.unwrap_or_else(|| domain.clone());
    LaminarHome {
        folder,
        domain,
        package,
        synthetic,
    }
}

/// Names each distinct real location by its folder key, qualifying key ties by
/// real location so distinct places never share a name: a folder key unique
/// among the distinct locations stays bare; a key shared across packages
/// qualifies as `{folder} ({package})`; a key shared within one package
/// qualifies as `{folder} ({package} {domain})`. Qualifiers dot their path
/// separators so display folding never splits a qualifier into path segments.
/// Distinct locations always differ in some coordinate, so the tiered names
/// are injective for every laminar-derived snapshot; only a hand-built folder
/// name that textually embeds another location's qualifier can still collide,
/// which the arena's numeric backstop absorbs.
pub(in crate::analyze) fn qualify_folder_names(distinct: &BTreeSet<LaminarHome>) -> Vec<SmolStr> {
    let mut folder_count: BTreeMap<&SmolStr, u32> = BTreeMap::new();
    let mut pair_count: BTreeMap<(&SmolStr, &SmolStr), u32> = BTreeMap::new();
    for home in distinct {
        *folder_count.entry(&home.folder).or_default() += 1;
        *pair_count.entry((&home.folder, &home.package)).or_default() += 1;
    }
    let dotted = |key: &SmolStr| key.replace('/', ".");
    distinct
        .iter()
        .map(|home| {
            let folder_ties = folder_count.get(&home.folder).copied().unwrap_or(0);
            let pair_ties = pair_count
                .get(&(&home.folder, &home.package))
                .copied()
                .unwrap_or(0);
            if folder_ties == 1 {
                home.folder.clone()
            } else if pair_ties == 1 {
                SmolStr::new(format!("{} ({})", home.folder, dotted(&home.package)))
            } else {
                SmolStr::new(format!(
                    "{} ({} {})",
                    home.folder,
                    dotted(&home.package),
                    dotted(&home.domain)
                ))
            }
        })
        .collect()
}
