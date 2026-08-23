//! `strata analyze`: the full decomposition pipeline.
//!
//! The command loads the effective config, discovers and snapshots the sources
//! under the root, runs the pure [`analyze`](fn@analyze) pass, and renders the result. It is
//! the canonical producer of the `AnalyzeResult` JSON the other commands consume.

use std::io::Write;
use std::path::{Path, PathBuf};

use strata_engine::{StrataError, analyze, snapshot_from_root};

use crate::commands::{ConfigOverrides, resolve_config};
use crate::render::{Format, render};

/// The parsed inputs of an `analyze` run.
///
/// `format` is the already-resolved effective format (TTY resolution happens in
/// the caller, which knows whether the sink is a terminal); `overrides` carries
/// the CLI flags that layer over the config file.
#[derive(Debug)]
pub struct AnalyzeArgs {
    /// The repository root to analyze.
    pub root: PathBuf,
    /// The config file path (defaults apply when it is absent).
    pub config: PathBuf,
    /// The CLI-flag config overrides.
    pub overrides: ConfigOverrides,
    /// The resolved output format.
    pub format: Format,
}

/// Runs `analyze`, writing the rendered or serialized result to `out`.
///
/// The printed-report face names its project after the analyzed root's final
/// path component; an unresolvable root keeps the given form verbatim.
///
/// # Errors
///
/// Returns any [`StrataError`] from config loading, snapshotting, or analysis;
/// an I/O failure writing the result surfaces as [`StrataError::InputUnreadable`]
/// keyed at the root.
pub fn run(args: &AnalyzeArgs, out: &mut impl Write) -> Result<(), StrataError> {
    let config = resolve_config(&args.config, args.overrides)?;
    let snapshot = snapshot_from_root(&args.root, &config)?;
    let result = analyze(&snapshot, &config)?;
    render(&result, args.format, &project_name(&args.root), out).map_err(|error| {
        StrataError::InputUnreadable {
            path: args.root.clone(),
            reason: error.to_string(),
        }
    })
}

/// Derives the report banner's project name from the analyzed root: the final
/// path component of the canonicalized root, falling back to the literal input.
fn project_name(root: &Path) -> String {
    let resolved = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    resolved.file_name().map_or_else(
        || "project".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Builds a temp dir holding `files` (path, contents) pairs and returns it.
    fn fixture(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("strata-analyze-{}", unique()));
        for (path, contents) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(full, contents);
        }
        dir
    }

    /// Returns a process-unique suffix for temp paths.
    fn unique() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    }

    #[test]
    fn should_analyze_a_root_and_emit_json() {
        let root = fixture(&[("a.py", "def alpha():\n    return 1\n")]);
        let args = AnalyzeArgs {
            root: root.clone(),
            config: root.join("strata.toml"),
            overrides: ConfigOverrides::default(),
            format: Format::Json,
        };
        let mut buffer = Vec::new();

        let outcome = run(&args, &mut buffer);

        let _ = fs::remove_dir_all(&root);
        assert!(outcome.is_ok());
        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("snapshotHash"));
    }

    #[test]
    fn should_derive_the_project_name_from_the_root_basename() {
        let root = fixture(&[("a.py", "def alpha():\n    return 1\n")]);

        let name = project_name(&root);

        let _ = fs::remove_dir_all(&root);
        assert!(!name.is_empty());
        assert!(!name.contains('/'), "the name is a bare basename: {name}");
    }

    #[test]
    fn should_keep_the_literal_form_when_the_root_is_unresolvable() {
        assert_eq!(project_name(Path::new("/nonexistent/repo/acme")), "acme");
        assert_eq!(project_name(Path::new("/")), "project");
    }
}
