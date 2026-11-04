//! Minimal `tsconfig.json` reader for `compilerOptions.paths` aliases and the
//! build-output layout.
//!
//! The `baseUrl` and `paths` fields drive module resolution, and `outDir`,
//! `rootDir` and `declarationDir` map a build-output entry back to its source,
//! so only those are read. A missing, unreadable, or alias-free `tsconfig.json`
//! yields an empty alias table — alias resolution is then simply skipped.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use smol_str::SmolStr;

/// The subset of `tsconfig.json` this adapter reads.
#[derive(Debug, Deserialize)]
struct TsConfig {
    /// `compilerOptions`, the only object carrying resolution settings.
    #[serde(rename = "compilerOptions")]
    compiler_options: Option<CompilerOptions>,
}

/// The `compilerOptions` subset that affects module resolution.
#[derive(Debug, Deserialize)]
struct CompilerOptions {
    /// Directory that non-relative specifiers resolve against.
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    /// Alias-pattern -> candidate-target-pattern list.
    paths: Option<BTreeMap<String, Vec<String>>>,
    /// Directory the emitted JavaScript goes to.
    #[serde(rename = "outDir")]
    out_dir: Option<String>,
    /// Directory that mirrors the layout of the emitted output.
    #[serde(rename = "rootDir")]
    root_dir: Option<String>,
    /// Directory the emitted declaration files go to, when apart from `outDir`.
    #[serde(rename = "declarationDir")]
    declaration_dir: Option<String>,
}

/// Where `tsc` writes the build output of a package, as directories relative to
/// the package: an output file `<output>/x.js` comes from `<sources>/x.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutputMapping {
    /// Directory of the emitted JavaScript; empty for the package directory.
    pub(super) output: String,
    /// Directory of the emitted declarations, when set apart from `output`.
    pub(super) declarations: Option<String>,
    /// Directory of the sources the output mirrors; empty for the package directory.
    pub(super) sources: String,
}

/// Reads `dir/tsconfig.json` and returns the mapping from build output back to
/// source.
///
/// A mapping needs both `outDir` and `rootDir`: without `rootDir`, `tsc` derives
/// the source root from the files it compiles, which this reader cannot know. A
/// missing or unparsable `tsconfig.json`, a missing field, or a directory outside
/// the package yields `None`, so entries under the output stay unmapped.
#[must_use]
pub(super) fn load_output_mapping(dir: &Path) -> Option<OutputMapping> {
    let contents = std::fs::read_to_string(dir.join("tsconfig.json")).ok()?;
    let options = serde_json::from_str::<TsConfig>(&contents)
        .ok()?
        .compiler_options?;
    let declarations = match options.declaration_dir.as_deref() {
        Some(directory) => Some(package_relative(directory)?),
        None => None,
    };
    Some(OutputMapping {
        output: package_relative(options.out_dir.as_deref()?)?,
        declarations,
        sources: package_relative(options.root_dir.as_deref()?)?,
    })
}

/// Normalizes a `tsconfig` directory to `/`-separated segments without `.`,
/// `None` when it leaves the package or is absolute.
fn package_relative(directory: &str) -> Option<String> {
    if directory.starts_with('/') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in directory.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            other => segments.push(other),
        }
    }
    Some(segments.join("/"))
}

/// Reads `root/tsconfig.json` and returns alias-prefix -> target-prefix entries.
///
/// Each `"@app/*": ["src/*"]` entry becomes `@app/` -> `<base_url>/src/`, with
/// the trailing `*` stripped. Only the first candidate target is used. Targets
/// are made relative to `root` via `base_url` (default `.`).
#[must_use]
pub(super) fn load_aliases(root: &Path) -> BTreeMap<SmolStr, SmolStr> {
    let path = root.join("tsconfig.json");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(config) = serde_json::from_str::<TsConfig>(&contents) else {
        return BTreeMap::new();
    };
    let Some(options) = config.compiler_options else {
        return BTreeMap::new();
    };
    let Some(paths) = options.paths else {
        return BTreeMap::new();
    };
    let base = options.base_url.unwrap_or_else(|| ".".to_string());

    let mut aliases = BTreeMap::new();
    for (pattern, targets) in paths {
        let Some(target) = targets.into_iter().next() else {
            continue;
        };
        let alias_prefix = pattern.trim_end_matches('*');
        let target_prefix = target.trim_end_matches('*');
        let joined = join_base(&base, target_prefix);
        aliases.insert(SmolStr::new(alias_prefix), SmolStr::new(joined));
    }
    aliases
}

/// Joins a `baseUrl` and a target prefix into a normalized repo-relative path.
fn join_base(base: &str, target: &str) -> String {
    let base = base.trim_start_matches("./").trim_matches('/');
    let target = target.trim_start_matches("./");
    if base.is_empty() || base == "." {
        target.to_string()
    } else {
        format!("{base}/{target}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_return_empty_when_tsconfig_is_absent() {
        let aliases = load_aliases(Path::new("/nonexistent/unlace/root"));

        assert!(aliases.is_empty());
    }

    #[test]
    fn should_join_a_base_url_with_a_target_prefix() {
        assert_eq!(join_base("./src", "lib/"), "src/lib/");
    }

    #[test]
    fn should_drop_a_dot_base_url() {
        assert_eq!(join_base(".", "src/"), "src/");
    }

    /// Writes `json` as the `tsconfig.json` of a throwaway directory and reads
    /// its output mapping.
    fn mapping_of(name: &str, json: &str) -> Result<Option<OutputMapping>, std::io::Error> {
        let dir = std::env::temp_dir().join(format!("unlace-ts-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("tsconfig.json"), json)?;
        let mapping = load_output_mapping(&dir);
        std::fs::remove_dir_all(&dir)?;
        Ok(mapping)
    }

    #[test]
    fn should_read_the_output_directories_relative_to_the_package() -> Result<(), std::io::Error> {
        let mapping = mapping_of(
            "mapping-read",
            r#"{"compilerOptions":{"outDir":"./lib/","rootDir":".","declarationDir":"types"}}"#,
        )?;

        assert_eq!(
            mapping,
            Some(OutputMapping {
                output: "lib".to_owned(),
                declarations: Some("types".to_owned()),
                sources: String::new(),
            })
        );
        Ok(())
    }

    #[test]
    fn should_return_no_mapping_without_a_root_dir() -> Result<(), std::io::Error> {
        let mapping = mapping_of("mapping-no-root", r#"{"compilerOptions":{"outDir":"lib"}}"#)?;

        assert_eq!(mapping, None);
        Ok(())
    }

    #[test]
    fn should_return_no_mapping_for_a_directory_outside_the_package() -> Result<(), std::io::Error>
    {
        let escaping = mapping_of(
            "mapping-escape",
            r#"{"compilerOptions":{"outDir":"../dist","rootDir":"src"}}"#,
        )?;
        let absolute = mapping_of(
            "mapping-absolute",
            r#"{"compilerOptions":{"outDir":"/dist","rootDir":"src"}}"#,
        )?;

        assert_eq!((escaping, absolute), (None, None));
        Ok(())
    }

    #[test]
    fn should_return_no_mapping_for_an_unparsable_tsconfig() -> Result<(), std::io::Error> {
        let mapping = mapping_of("mapping-broken", "{ // comment\n}")?;

        assert_eq!(mapping, None);
        Ok(())
    }
}
