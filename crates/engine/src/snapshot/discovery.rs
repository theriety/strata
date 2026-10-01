//! Source discovery: walks the root, applies include / exclude globs and
//! `.gitignore` rules, and finds package-root manifests.

use std::path::Path;

use smol_str::SmolStr;
use strata_ir::SourceFile;

use super::Language;
use crate::config::AnalyzeConfig;
use crate::error::StrataError;

/// Reads every file under `root` that matches the include globs, clears the
/// exclude globs, survives the repo's own `.gitignore` rules, and is claimed by
/// an enabled language, returning the source set as repo-relative paths plus
/// contents.
///
/// `.gitignore` files under `root` are honored even outside a git checkout, so
/// build output never pollutes the snapshot; only rules inside `root` apply —
/// no parent, global, or `.git/info/exclude` sources — keeping the same tree
/// deterministic across machines. `.git` and `node_modules` directories are
/// skipped unconditionally, independent of the configurable exclude globs.
///
/// Extension filtering happens here, *before* any file is read, so discovery
/// never touches non-source files (binaries, lockfiles, images, VCS metadata).
/// This keeps a file like `.git/index` — invalid UTF-8 — from aborting the walk:
/// it matches no enabled language and is skipped silently.
pub(super) fn discover_sources(
    root: &Path,
    config: &AnalyzeConfig,
    languages: &[Language],
) -> Result<Discovery, StrataError> {
    let includes = compile_globs(&config.adapters.include)?;
    let excludes = compile_globs(&config.adapters.exclude)?;

    let mut sources = Vec::new();
    let mut package_roots: Vec<SmolStr> = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != "node_modules"
        })
        .build();
    for entry in walker {
        let entry = entry.map_err(|error| StrataError::InputUnreadable {
            path: root.to_path_buf(),
            reason: error.to_string(),
        })?;
        if entry.file_type().is_none_or(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let Some(relative) = relative_path(root, path) else {
            continue;
        };
        // a build manifest marks its directory as a package root, independent of
        // the source include/exclude globs (a manifest is never a source file).
        // lean: package boundaries follow manifest *presence* — the ecosystem
        // norm (Nx/Turbo/Cargo). Upgrade path: parse each root's workspace
        // declaration (package.json `workspaces`, Cargo `[workspace].members`,
        // pnpm-workspace `packages:`) to scope members authoritatively.
        if let Some(package_root) = manifest_dir(&relative) {
            package_roots.push(package_root);
        }
        if !includes.iter().any(|glob| glob.matches(&relative)) {
            continue;
        }
        if excludes.iter().any(|glob| glob.matches(&relative)) {
            continue;
        }
        // skip files no enabled language claims, *before* reading them, so a
        // binary or non-UTF-8 file (e.g. `.git/index`) never aborts the walk.
        if !languages
            .iter()
            .any(|language| language.matches_extension(&relative))
        {
            continue;
        }
        let contents =
            std::fs::read_to_string(path).map_err(|error| StrataError::InputUnreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        sources.push(SourceFile {
            path: SmolStr::new(&relative),
            contents,
        });
    }
    // sort so discovery order — and therefore every downstream id — is stable.
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    package_roots.sort();
    package_roots.dedup();
    Ok(Discovery {
        sources,
        package_roots,
    })
}

/// The output of source discovery: the language sources plus the repo-relative
/// directories that own a build manifest (the package roots).
pub(super) struct Discovery {
    /// Every discovered source file, sorted by repo-relative path.
    pub(super) sources: Vec<SourceFile>,
    /// Directories containing a build manifest, sorted and deduped. Empty (the
    /// repository root) entries denote a single-package repo.
    pub(super) package_roots: Vec<SmolStr>,
}

/// The manifest filenames whose presence marks a directory as a package root.
const PACKAGE_MANIFESTS: [&str; 4] = ["package.json", "Cargo.toml", "pyproject.toml", "setup.py"];

/// Returns the repo-relative directory of `relative` when its filename is a
/// build manifest, or `None` otherwise. A manifest at the repository root yields
/// the empty string.
pub(super) fn manifest_dir(relative: &str) -> Option<SmolStr> {
    let filename = relative.rsplit('/').next().unwrap_or(relative);
    if !PACKAGE_MANIFESTS.contains(&filename) {
        return None;
    }
    let dir = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
    Some(SmolStr::new(dir))
}

/// Compiles each glob pattern, attributing a config error on a bad pattern.
fn compile_globs(patterns: &[String]) -> Result<Vec<glob::Pattern>, StrataError> {
    patterns
        .iter()
        .map(|pattern| {
            glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                key: Some("adapters".to_owned()),
                reason: format!("invalid glob `{pattern}`: {error}"),
            })
        })
        .collect()
}

/// Returns the slash-normalized path of `path` relative to `root`, if `path` is
/// under `root`.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(
        relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}
