//! Workspace loading and low-level semantic resolution.
//!
//! Wraps the rust-analyzer database loaded from the cargo workspace and exposes
//! the primitives the other binding phases build on: loading, `goto_definition`
//! resolution, and vfs path mapping.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ra_ap_ide::{
    AnalysisHost, FilePosition, GotoDefinitionConfig, RaFixtureConfig, SymbolKind, TextSize,
};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_project_model::{
    CargoConfig, ProjectManifest, ProjectWorkspace, ProjectWorkspaceKind, RustLibSource, TargetKind,
};
use ra_ap_vfs::{AbsPathBuf, Vfs};
use smol_str::SmolStr;

use super::BindOutcome;

/// Loads the cargo workspace into a rust-analyzer database, wrapping the loader
/// errors in [`BindOutcome`].
pub(super) struct Database {
    /// The analysis host owning the loaded semantic database.
    pub(super) host: AnalysisHost,
    /// The virtual file system mapping file ids to on-disk paths.
    pub(super) vfs: Vfs,
    /// Root source file of every library target of the cargo workspace, or
    /// `None` when the project is not a cargo workspace and they are unknown.
    lib_roots: Option<BTreeSet<PathBuf>>,
}

/// Discovers and loads the project at `manifest`, and reads the root file of
/// each of its library targets. The error is the loader's message.
fn load_project(
    manifest: &Path,
    config: &CargoConfig,
) -> Result<(ProjectWorkspace, Option<BTreeSet<PathBuf>>), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = AbsPathBuf::assert_utf8(current.join(manifest));
    let found = ProjectManifest::discover_single(&root).map_err(|error| error.to_string())?;
    let workspace = ProjectWorkspace::load(found, config, &|_progress| {})
        .map_err(|error| error.to_string())?;
    let lib_roots = match &workspace.kind {
        ProjectWorkspaceKind::Cargo { cargo, .. } => Some(
            cargo
                .packages()
                .flat_map(|package| cargo[package].targets.iter().copied())
                .filter(|target| matches!(cargo[*target].kind, TargetKind::Lib { .. }))
                .map(|target| canonical(AsRef::<Path>::as_ref(&cargo[target].root)))
                .collect(),
        ),
        _ => None,
    };
    Ok((workspace, lib_roots))
}

/// The canonical form of `path`, or `path` itself when it cannot be resolved.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A semantic target, including definitions outside the analyzed workspace.
pub(super) enum ResolvedTarget {
    /// A definition that can be looked up in the snapshot's node assignment.
    /// `module` marks a module definition, whose range (a whole file, for a
    /// file module) says nothing about the declarations it happens to contain.
    Workspace {
        path: SmolStr,
        offset: u32,
        module: bool,
    },
    /// A known definition outside the workspace, never a name-fallback candidate.
    External,
}

impl Database {
    /// Loads the workspace whose manifest is at `manifest`.
    pub(super) fn load(manifest: &Path) -> Result<Self, BindOutcome> {
        let cargo_config = CargoConfig {
            sysroot: Some(RustLibSource::Discover),
            // Workspace crates are analysed with `cfg(test)` on, so test modules
            // resolve semantically and count as users of the items they touch.
            set_test: true,
            ..CargoConfig::default()
        };
        let load_config = LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 0,
        };
        // the same two steps `load_workspace_at` takes, split so the cargo
        // targets are read before the workspace is consumed by the loader.
        let (workspace, lib_roots) = load_project(manifest, &cargo_config)
            .map_err(|reason| BindOutcome::LoadFailed { reason })?;
        let (db, vfs, _proc_macro) =
            load_workspace(workspace, &cargo_config.extra_env, &load_config).map_err(|error| {
                BindOutcome::LoadFailed {
                    reason: error.to_string(),
                }
            })?;
        Ok(Self {
            host: AnalysisHost::with_database(db),
            vfs,
            lib_roots,
        })
    }

    /// Whether `file` is the root source file of a library target. A project
    /// whose targets are unknown (not a cargo workspace) counts every root as a
    /// library, the side that only silences advice.
    pub(super) fn is_lib_root(&self, file: ra_ap_ide::FileId) -> bool {
        let Some(roots) = &self.lib_roots else {
            return true;
        };
        let vfs_path = self.vfs.file_path(file);
        vfs_path.as_path().is_none_or(|on_disk| {
            let on_disk: &Path = on_disk.as_ref();
            // symlinked roots compare equal once canonical; a path that cannot
            // be canonicalised is compared as given.
            roots.contains(on_disk) || roots.contains(&canonical(on_disk))
        })
    }

    /// Resolves the reference at `(relative_path, offset)` to the definition's
    /// workspace location or an external target. Only absent semantic targets
    /// return `None`; a known external definition must not trigger name fallback.
    pub(super) fn resolve(
        &self,
        path: &str,
        offset: u32,
        workspace_root: &Path,
    ) -> Option<ResolvedTarget> {
        let file_id = self.file_id_for(path, workspace_root)?;
        let analysis = self.host.analysis();
        let position = FilePosition {
            file_id,
            offset: TextSize::new(offset),
        };
        let config = GotoDefinitionConfig {
            ra_fixture: RaFixtureConfig {
                disable_ra_fixture: true,
                ..RaFixtureConfig::default()
            },
        };
        let range_info = analysis.goto_definition(position, &config).ok()??;
        let target = range_info.info.into_iter().next()?;
        Some(self.relative_path(target.file_id, workspace_root).map_or(
            ResolvedTarget::External,
            |target_path| ResolvedTarget::Workspace {
                path: target_path,
                offset: u32::from(target.full_range.start()),
                module: target.kind == Some(SymbolKind::Module),
            },
        ))
    }

    /// Maps a repository-relative source path to its vfs file id.
    pub(super) fn file_id_for(
        &self,
        path: &str,
        workspace_root: &Path,
    ) -> Option<ra_ap_ide::FileId> {
        let absolute = workspace_root.join(path);
        self.vfs.iter().find_map(|(file_id, vfs_path)| {
            let on_disk = vfs_path.as_path()?;
            (AsRef::<Path>::as_ref(on_disk) == absolute).then_some(file_id)
        })
    }

    /// Maps a vfs file id back to a repository-relative path within the
    /// workspace, or `None` for sysroot / out-of-tree files.
    pub(super) fn relative_path(
        &self,
        file_id: ra_ap_ide::FileId,
        workspace_root: &Path,
    ) -> Option<SmolStr> {
        let vfs_path = self.vfs.file_path(file_id);
        let on_disk = vfs_path.as_path()?;
        let raw: &Path = on_disk.as_ref();
        let relative = raw.strip_prefix(workspace_root).ok()?;
        Some(SmolStr::new(relative.to_string_lossy().replace('\\', "/")))
    }
}
