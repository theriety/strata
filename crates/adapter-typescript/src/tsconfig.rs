//! Minimal `tsconfig.json` reader for `compilerOptions.paths` aliases.
//!
//! Only the `baseUrl` and `paths` fields drive module resolution, so only those
//! are read. A missing, unreadable, or alias-free `tsconfig.json` yields an
//! empty alias table — alias resolution is then simply skipped.

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
}

/// Reads `root/tsconfig.json` and returns alias-prefix -> target-prefix entries.
///
/// Each `"@app/*": ["src/*"]` entry becomes `@app/` -> `<base_url>/src/`, with
/// the trailing `*` stripped. Only the first candidate target is used. Targets
/// are made relative to `root` via `base_url` (default `.`).
#[must_use]
pub fn load_aliases(root: &Path) -> BTreeMap<SmolStr, SmolStr> {
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
        let aliases = load_aliases(Path::new("/nonexistent/strata/root"));

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
}
