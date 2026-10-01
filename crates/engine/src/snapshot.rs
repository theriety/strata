//! Source discovery, adapter dispatch, fragment merging, and snapshot assembly.
//!
//! [`snapshot_from_root`] is the engine's only filesystem entry point. It globs
//! sources under a root (honoring the config's include / exclude patterns),
//! groups files by language, dispatches each language's [`Adapter`](strata_ir::Adapter) in parallel
//! (rayon), then merges the resulting [`IrFragment`](strata_ir::IrFragment)s into one namespace:
//! per-adapter node and container ids are re-interned to a single dense range so
//! cross-fragment edges resolve. Re-export edges are flattened to their original
//! definitions under a depth guard, and the merged IR is handed to
//! [`Snapshot::assemble`](strata_ir::Snapshot::assemble) for validation and
//! content hashing.

mod discovery;
mod dispatch;
mod language;
mod merge;
mod reexport;
mod visibility;

#[cfg(test)]
mod tests;

use std::path::Path;

use rayon::prelude::*;
use smol_str::SmolStr;
use strata_ir::Layout;

use self::discovery::discover_sources;
use self::dispatch::{enabled_languages, group_by_language, run_adapter};
use self::merge::merge_fragments;
use self::reexport::flatten_re_exports;
use crate::config::AnalyzeConfig;
use crate::error::StrataError;

pub(crate) use self::language::Language;

/// Discovers sources under `root`, runs the enabled adapters, merges their IR
/// fragments, flattens re-export chains, and assembles a validated, hashed
/// [`Snapshot`].
///
/// File discovery honors `config.adapters.include` and `.exclude` globs relative
/// to `root`, plus any `.gitignore` files under `root`; `.git` and
/// `node_modules` directories are always skipped. Files are grouped by language
/// and the adapters run in parallel. Merging re-interns each fragment's node and
/// container ids into one dense namespace before assembly.
///
/// [`Snapshot`]: strata_ir::Snapshot
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when discovery cannot read the tree,
/// [`StrataError::AdapterParseFailure`] / [`StrataError::AdapterBindFailure`] on
/// adapter failure, [`StrataError::ReExportDepthExceeded`] on a pathological
/// barrel chain, and [`StrataError::SnapshotInvalid`] when assembly rejects the
/// merged IR.
pub fn snapshot_from_root(
    root: impl AsRef<Path>,
    config: &AnalyzeConfig,
) -> Result<strata_ir::Snapshot, StrataError> {
    let root = root.as_ref();
    // rust-analyzer canonicalizes its VFS to an absolute path, so canonicalize the
    // root once here — the engine's only filesystem entry point — and every
    // downstream consumer (discovery, the group-naming path below, and the
    // adapter VFS bind) sees the same canonical path. Fail loud on failure: a root
    // that cannot be canonicalized (missing, unreadable, a broken symlink) is
    // exactly when the VFS would not match, so falling back to the raw path would
    // silently degrade semantic-edge resolution instead of surfacing the bad root.
    let root = std::fs::canonicalize(root).map_err(|error| StrataError::InputUnreadable {
        path: root.to_path_buf(),
        reason: error.to_string(),
    })?;
    let root = root.as_path();
    let languages = enabled_languages(config);
    let discovery = discover_sources(root, config, &languages)?;

    let grouped = group_by_language(&discovery.sources, &languages);
    let fragments = grouped
        .into_par_iter()
        .map(|(language, sources)| run_adapter(language, &sources, root))
        .collect::<Result<Vec<_>, _>>()?;

    // the package group is named after the repository directory; package and
    // source-root boundaries come from the discovered manifests and config.
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let layout = Layout {
        package_roots: discovery.package_roots,
        source_roots: config
            .adapters
            .source_roots
            .iter()
            .map(SmolStr::new)
            .collect(),
    };
    let merged = merge_fragments(fragments, root_name, &layout);
    let flattened = flatten_re_exports(merged)?;

    strata_ir::Snapshot::assemble(flattened).map_err(StrataError::from)
}
