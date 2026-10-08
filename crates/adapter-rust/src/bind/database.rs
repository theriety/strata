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
    /// Root source file of every cargo target that is a program entry (library,
    /// binary, example, integration test, bench, build script), or `None` when
    /// the project is not a cargo workspace.
    entry_roots: Option<BTreeSet<PathBuf>>,
    /// Root files of the targets that start a program (every target but a
    /// library); `fn main` at such a root is the start symbol.
    start_roots: Option<BTreeSet<PathBuf>>,
    /// Directories of the workspace crates that only support tests, or `None`
    /// when the project is not a cargo workspace.
    harness_dirs: Option<BTreeSet<PathBuf>>,
}

/// The root source files of the cargo targets, by role.
type TargetRoots = (
    Option<BTreeSet<PathBuf>>,
    Option<BTreeSet<PathBuf>>,
    Option<BTreeSet<PathBuf>>,
    Option<BTreeSet<PathBuf>>,
);

/// Whether the manifest at `path` declares `publish = false` (or an empty
/// registry list) in its `[package]` table. `publish.workspace = true`
/// resolves against `[workspace.package]` in the workspace root manifest.
fn manifest_is_unpublished(path: &Path, workspace_manifest: &Path) -> bool {
    let read = |file: &Path| {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
    };
    let Some(manifest) = read(path) else {
        return false;
    };
    let declared = manifest
        .get("package")
        .and_then(|package| package.get("publish"));
    let inherited = match declared {
        Some(toml::Value::Table(table))
            if table.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
        {
            read(workspace_manifest).and_then(|root| {
                root.get("workspace")?
                    .get("package")?
                    .get("publish")
                    .cloned()
            })
        }
        other => other.cloned(),
    };
    match inherited {
        Some(toml::Value::Boolean(flag)) => !flag,
        Some(toml::Value::Array(registries)) => registries.is_empty(),
        _ => false,
    }
}

/// Directories of the test-support crates of `cargo`: workspace members that
/// are unpublished, have a library and no binary target, and that no other
/// member uses through `[dependencies]` or `[build-dependencies]`. A lone
/// crate has nothing to support, so a workspace of one has none; and when
/// every member would qualify, none is a harness for the others, so the set is
/// empty.
fn harness_crate_dirs(cargo: &ra_ap_project_model::CargoWorkspace) -> BTreeSet<PathBuf> {
    let members: Vec<_> = cargo
        .packages()
        .filter(|package| cargo[*package].is_member)
        .collect();
    if members.len() < 2 {
        return BTreeSet::new();
    }
    let used: BTreeSet<_> = members
        .iter()
        .flat_map(|package| cargo[*package].dependencies.iter())
        // the loader's dependency kind enum is not exported, so it cannot be
        // matched by variant; its `Debug` spelling names the kind.
        .filter(|dependency| format!("{:?}", dependency.kind) != "Dev")
        .map(|dependency| dependency.pkg)
        .collect();
    let workspace_manifest = cargo.workspace_root().join("Cargo.toml");
    let harnesses: Vec<_> = members
        .iter()
        .copied()
        .filter(|package| !used.contains(package))
        .filter(|package| {
            let data = &cargo[*package];
            let has_lib = data
                .targets
                .iter()
                .any(|target| matches!(cargo[*target].kind, TargetKind::Lib { .. }));
            let has_bin = data
                .targets
                .iter()
                .any(|target| matches!(cargo[*target].kind, TargetKind::Bin));
            has_lib
                && !has_bin
                && manifest_is_unpublished(data.manifest.as_ref(), workspace_manifest.as_ref())
        })
        .collect();
    if harnesses.len() == members.len() {
        return BTreeSet::new();
    }
    harnesses
        .into_iter()
        .filter_map(|package| {
            let manifest: &Path = cargo[package].manifest.as_ref();
            manifest.parent().map(canonical)
        })
        .collect()
}

/// Discovers and loads the project at `manifest`, and reads the root file of
/// each of its library targets. The error is the loader's message.
fn load_project(
    manifest: &Path,
    config: &CargoConfig,
) -> Result<(ProjectWorkspace, TargetRoots), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = AbsPathBuf::assert_utf8(current.join(manifest));
    let found = ProjectManifest::discover_single(&root).map_err(|error| error.to_string())?;
    let workspace = ProjectWorkspace::load(found, config, &|_progress| {})
        .map_err(|error| error.to_string())?;
    let targets = match &workspace.kind {
        ProjectWorkspaceKind::Cargo { cargo, .. } => {
            let roots = |keep: fn(&TargetKind) -> bool| -> BTreeSet<PathBuf> {
                cargo
                    .packages()
                    .flat_map(|package| cargo[package].targets.iter().copied())
                    .filter(|target| keep(&cargo[*target].kind))
                    .map(|target| canonical(AsRef::<Path>::as_ref(&cargo[target].root)))
                    .collect()
            };
            (
                Some(roots(|kind| matches!(kind, TargetKind::Lib { .. }))),
                Some(roots(|kind| !matches!(kind, TargetKind::Other))),
                Some(roots(|kind| {
                    !matches!(kind, TargetKind::Lib { .. } | TargetKind::Other)
                })),
                Some(harness_crate_dirs(cargo)),
            )
        }
        _ => (None, None, None, None),
    };
    Ok((workspace, targets))
}

/// The paths in `roots` relative to `root`, `/`-separated.
fn relative_paths(roots: Option<&BTreeSet<PathBuf>>, root: &Path) -> BTreeSet<SmolStr> {
    let base = canonical(root);
    roots
        .into_iter()
        .flatten()
        .filter_map(|target| target.strip_prefix(&base).ok())
        .map(|relative| SmolStr::new(relative.to_string_lossy().replace('\\', "/")))
        .collect()
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
        let (workspace, (lib_roots, entry_roots, start_roots, harness_dirs)) =
            load_project(manifest, &cargo_config)
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
            entry_roots,
            start_roots,
            harness_dirs,
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

    /// Repo-relative paths (under `root`) of the cargo target root files; empty
    /// when the targets are unknown, so nothing is called an entry by guess.
    pub(super) fn entry_paths(&self, root: &Path) -> BTreeSet<SmolStr> {
        relative_paths(self.entry_roots.as_ref(), root)
    }

    /// Repo-relative paths of the root files of program-starting targets
    /// (everything but libraries); empty when the targets are unknown.
    pub(super) fn start_paths(&self, root: &Path) -> BTreeSet<SmolStr> {
        relative_paths(self.start_roots.as_ref(), root)
    }

    /// Repo-relative `/`-separated directories of the test-support crates;
    /// empty when the workspace is unknown or has none.
    pub(super) fn test_support_roots(&self, root: &Path) -> Vec<SmolStr> {
        relative_paths(self.harness_dirs.as_ref(), root)
            .into_iter()
            .filter(|dir| !dir.is_empty())
            .collect()
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
