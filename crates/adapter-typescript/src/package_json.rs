//! Minimal `package.json` reader for Node.js subpath imports.
//!
//! Only the top-level `imports` field drives module resolution here, so only
//! that is read. A missing, unreadable, or import-free `package.json` yields an
//! empty table — subpath-import resolution is then simply skipped.

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

/// Reads `root/package.json` and returns specifier -> target entries.
///
/// `"#agent/*": "./src/agent/*.ts"` keeps its one-star pattern verbatim so the
/// resolver can substitute before joining against `root`; array targets keep
/// only their first string entry, mirroring the first matching runtime
/// condition.
#[must_use]
pub fn load_subpath_imports(root: &Path) -> BTreeMap<SmolStr, SmolStr> {
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
        let imports = load_subpath_imports(Path::new("/nonexistent/strata/root"));

        assert!(imports.is_empty());
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
