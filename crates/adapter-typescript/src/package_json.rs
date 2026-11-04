//! Minimal `package.json` reader for Node.js subpath imports and package
//! surfaces.
//!
//! The root `imports` field drives module resolution, and each manifest's
//! `exports` field decides what code outside the repository can import, so only
//! those fields (and the package name) are read. A missing, unreadable, or
//! import-free `package.json` yields an empty table — subpath-import resolution
//! is then simply skipped.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use smol_str::SmolStr;

/// The subset of `package.json` this adapter reads.
#[derive(Debug, Deserialize)]
struct PackageJson {
    /// Node.js subpath-import shortcuts (`"#foo/*": "./src/foo/*.ts"`).
    imports: Option<BTreeMap<String, ImportTarget>>,
}

/// A single `imports` value: a target string or a conditions array of them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum ImportTarget {
    /// A direct relative target (`./src/foo.ts`).
    Direct(String),
    /// Runtime-condition alternatives; the first entry wins.
    Conditions(Vec<String>),
}

/// What one `package.json` says about the package's importable surface.
#[derive(Debug, Default, Deserialize)]
pub(super) struct Manifest {
    /// The package name, which lets an import of a package inside the tree be
    /// told apart from an import of an outside dependency.
    pub(super) name: Option<String>,
    /// The `exports` field as written; absent or `null` leaves every file of the
    /// package importable by its path.
    pub(super) exports: Option<serde_json::Value>,
}

/// Reads `dir/package.json`.
///
/// Returns `None` when `dir` holds no manifest. A manifest that cannot be parsed
/// yields an empty [`Manifest`]: nothing is known, so nothing is closed.
#[must_use]
pub(super) fn load_manifest(dir: &Path) -> Option<Manifest> {
    let contents = std::fs::read_to_string(dir.join("package.json")).ok()?;
    Some(serde_json::from_str(&contents).unwrap_or_default())
}

/// Reads `root/package.json` and returns specifier -> target entries.
///
/// `"#agent/*": "./src/agent/*.ts"` keeps its one-star pattern verbatim so the
/// resolver can substitute before joining against `root`; array targets keep
/// only their first string entry, mirroring the first matching runtime
/// condition.
#[must_use]
pub(super) fn load_subpath_imports(root: &Path) -> BTreeMap<SmolStr, SmolStr> {
    let path = root.join("package.json");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(package) = serde_json::from_str::<PackageJson>(&contents) else {
        return BTreeMap::new();
    };
    package
        .imports
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, target)| {
            let target = match target {
                ImportTarget::Direct(value) => value,
                ImportTarget::Conditions(values) => values.into_iter().next()?,
            };
            Some((SmolStr::new(key), SmolStr::new(target)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_return_empty_when_package_json_is_absent() {
        let imports = load_subpath_imports(Path::new("/nonexistent/unlace/root"));

        assert!(imports.is_empty());
    }

    /// Writes `json` as the `package.json` of a throwaway directory and reads it
    /// back through [`load_manifest`].
    fn manifest_of(name: &str, json: &str) -> Result<Option<Manifest>, std::io::Error> {
        let dir = std::env::temp_dir().join(format!("unlace-ts-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("package.json"), json)?;
        let manifest = load_manifest(&dir);
        std::fs::remove_dir_all(&dir)?;
        Ok(manifest)
    }

    #[test]
    fn should_return_no_manifest_for_a_directory_without_package_json() {
        assert!(load_manifest(Path::new("/nonexistent/unlace/root")).is_none());
    }

    #[test]
    fn should_read_the_name_and_exports_of_a_manifest() -> Result<(), std::io::Error> {
        let manifest = manifest_of("manifest-read", r#"{"name":"pkg","exports":"./a.ts"}"#)?;

        assert_eq!(
            manifest.map(|m| (m.name, m.exports)),
            Some((Some("pkg".to_owned()), Some(serde_json::json!("./a.ts")))),
        );
        Ok(())
    }

    #[test]
    fn should_treat_a_null_exports_field_as_absent() -> Result<(), std::io::Error> {
        let manifest = manifest_of("manifest-null", r#"{"exports":null}"#)?;

        assert_eq!(manifest.map(|m| m.exports), Some(None));
        Ok(())
    }

    #[test]
    fn should_read_an_unparsable_manifest_as_an_empty_one() -> Result<(), std::io::Error> {
        let manifest = manifest_of("manifest-broken", "{ not json")?;

        assert_eq!(manifest.map(|m| (m.name, m.exports)), Some((None, None)));
        Ok(())
    }

    /// Parses one fixture; `None` means the parse failed, which the assertions
    /// below report as a missing entry.
    fn parse_imports(json: &str) -> BTreeMap<String, ImportTarget> {
        serde_json::from_str::<PackageJson>(json)
            .ok()
            .and_then(|package| package.imports)
            .unwrap_or_default()
    }

    #[test]
    fn should_keep_a_pattern_target_verbatim() {
        let imports = parse_imports(r##"{"imports":{"#a/*":"./src/a/*.ts"}}"##);

        assert_eq!(
            imports.get("#a/*"),
            Some(&ImportTarget::Direct("./src/a/*.ts".to_owned())),
        );
    }

    #[test]
    fn should_take_the_first_condition_target() {
        let imports = parse_imports(r##"{"imports":{"#b":["./x.ts","./y.ts"]}}"##);
        let target = imports.get("#b");
        let first = match target {
            Some(ImportTarget::Conditions(values)) => values.first().map(String::as_str),
            _ => None,
        };

        assert_eq!(first, Some("./x.ts"));
    }
}
