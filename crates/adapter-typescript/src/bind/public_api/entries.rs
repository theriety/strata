//! The source files a package's `exports` map lists as entries (ADR-29 rules 1
//! and 2).
//!
//! Every string in the map counts, whatever the subpath key or condition it sits
//! under: a consumer somewhere may be served by any of them. A target that is
//! build output is mapped back to source through the package's `tsconfig.json`.

use std::collections::BTreeSet;

use serde_json::Value;
use smol_str::SmolStr;

use crate::tsconfig::OutputMapping;

/// What a package's `exports` map comes to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entries {
    /// Every code target maps to source files in the tree; these are the entries.
    Modules(BTreeSet<SmolStr>),
    /// The map cannot be read, or a target maps to no file in the tree, so the
    /// importable surface cannot be told.
    Unknown,
}

/// How the extension of a target relates to the source files behind it.
struct Extension {
    /// The extension as written in the map, dot included.
    suffix: &'static str,
    /// The source extensions that stand behind it.
    sources: &'static [&'static str],
    /// Whether the target is a source file or build output.
    role: Role,
}

/// Whether a target names a source file or something a build emits.
enum Role {
    /// A file kept in the source tree as written.
    Source,
    /// Emitted output; `declaration` picks the declaration directory over the
    /// JavaScript output directory.
    Output { declaration: bool },
}

/// Longest suffixes first, so `.d.ts` wins over `.ts`.
const EXTENSIONS: [Extension; 11] = [
    Extension {
        suffix: ".d.ts",
        sources: &[".ts", ".tsx", ".d.ts"],
        role: Role::Output { declaration: true },
    },
    Extension {
        suffix: ".d.mts",
        sources: &[".mts", ".d.mts"],
        role: Role::Output { declaration: true },
    },
    Extension {
        suffix: ".d.cts",
        sources: &[".cts", ".d.cts"],
        role: Role::Output { declaration: true },
    },
    Extension {
        suffix: ".tsx",
        sources: &[".tsx"],
        role: Role::Source,
    },
    Extension {
        suffix: ".ts",
        sources: &[".ts"],
        role: Role::Source,
    },
    Extension {
        suffix: ".mts",
        sources: &[".mts"],
        role: Role::Source,
    },
    Extension {
        suffix: ".cts",
        sources: &[".cts"],
        role: Role::Source,
    },
    Extension {
        suffix: ".jsx",
        sources: &[".tsx"],
        role: Role::Output { declaration: false },
    },
    Extension {
        suffix: ".js",
        sources: &[".ts", ".tsx"],
        role: Role::Output { declaration: false },
    },
    Extension {
        suffix: ".mjs",
        sources: &[".mts"],
        role: Role::Output { declaration: false },
    },
    Extension {
        suffix: ".cjs",
        sources: &[".cts"],
        role: Role::Output { declaration: false },
    },
];

/// A target that stops at its `*`, standing for every file below a directory.
const OPEN_PATTERN: Extension = Extension {
    suffix: "",
    sources: &[""],
    role: Role::Output { declaration: false },
};

/// What one target turns out to be.
enum Resolved {
    /// Not code (`./package.json`, `./styles.css`): it exposes no item.
    Asset,
    /// The source files the target stands for.
    Files(Vec<SmolStr>),
    /// The target cannot be tied to a file in the tree.
    Unmapped,
}

/// Maps the `exports` map of the package in `package_dir` to its entry modules.
///
/// `known` is every module path of the tree.
pub(super) fn entry_modules(
    package_dir: &str,
    exports: &Value,
    output: Option<&OutputMapping>,
    known: &BTreeSet<SmolStr>,
) -> Entries {
    let mut targets = Vec::new();
    if !collect_targets(exports, &mut targets) {
        return Entries::Unknown;
    }
    let mut entries = BTreeSet::new();
    for target in targets {
        match resolve_target(target, package_dir, output, known) {
            Resolved::Asset => {}
            Resolved::Files(files) => entries.extend(files),
            Resolved::Unmapped => return Entries::Unknown,
        }
    }
    Entries::Modules(entries)
}

/// Gathers every target string of an `exports` value, `false` when a value has
/// no meaning in the map. A `null` target blocks a path and lists nothing.
fn collect_targets<'map>(value: &'map Value, targets: &mut Vec<&'map str>) -> bool {
    match value {
        Value::String(target) => {
            targets.push(target);
            true
        }
        Value::Null => true,
        Value::Array(items) => items.iter().all(|item| collect_targets(item, targets)),
        Value::Object(map) => map.values().all(|item| collect_targets(item, targets)),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Ties one target to the source files it stands for.
fn resolve_target(
    target: &str,
    package_dir: &str,
    output: Option<&OutputMapping>,
    known: &BTreeSet<SmolStr>,
) -> Resolved {
    let Some(relative) = target.strip_prefix("./").and_then(normalize) else {
        return Resolved::Unmapped;
    };
    if relative.matches('*').count() > 1 {
        return Resolved::Unmapped;
    }
    let extension = if relative.ends_with('*') {
        &OPEN_PATTERN
    } else if let Some(extension) = EXTENSIONS
        .iter()
        .find(|extension| relative.ends_with(extension.suffix))
    {
        extension
    } else if is_asset(&relative) {
        return Resolved::Asset;
    } else {
        return Resolved::Unmapped;
    };

    let candidates = candidate_paths(&relative, extension, package_dir, output);
    let files: Vec<SmolStr> = known
        .iter()
        .filter(|module| {
            candidates
                .iter()
                .any(|candidate| matches_pattern(candidate, module))
        })
        .cloned()
        .collect();
    if files.is_empty() {
        Resolved::Unmapped
    } else {
        Resolved::Files(files)
    }
}

/// Extensions of files that hold data or media and never export a code item.
/// Any other extension may be code the adapter cannot read (`.vue`, `.svelte`),
/// which could re-export items, so it stays unmapped.
const ASSET_EXTENSIONS: [&str; 24] = [
    "json", "css", "scss", "sass", "less", "svg", "png", "jpg", "jpeg", "gif", "webp", "avif",
    "ico", "woff", "woff2", "ttf", "otf", "eot", "wasm", "node", "map", "md", "txt", "html",
];

/// Whether the file a target names is data or media rather than code.
fn is_asset(relative: &str) -> bool {
    relative.rsplit_once('.').is_some_and(|(_, extension)| {
        ASSET_EXTENSIONS
            .iter()
            .any(|asset| extension.eq_ignore_ascii_case(asset))
    })
}

/// Drops `.` segments from a relative path, `None` when it climbs out with `..`.
fn normalize(relative: &str) -> Option<String> {
    let mut segments = Vec::new();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            other => segments.push(other),
        }
    }
    Some(segments.join("/"))
}

/// The repository paths, possibly with a `*`, a target can stand for.
///
/// A source target stands for itself. A build-output target stands for the
/// sources the build emits it from: under the output directory it moves to the
/// root directory, elsewhere it stays in place, and its extension swaps for each
/// source extension that can emit it.
fn candidate_paths(
    relative: &str,
    extension: &Extension,
    package_dir: &str,
    output: Option<&OutputMapping>,
) -> Vec<String> {
    let Role::Output { declaration } = extension.role else {
        return vec![join(package_dir, relative)];
    };
    let base = relative.strip_suffix(extension.suffix).unwrap_or(relative);
    let location = output
        .and_then(|mapping| moved_to_root(base, mapping, declaration))
        .unwrap_or_else(|| base.to_owned());
    extension
        .sources
        .iter()
        .map(|source| join(package_dir, &format!("{location}{source}")))
        .collect()
}

/// Moves `base` from the output directory to the root directory, `None` when it
/// is not below the output directory.
fn moved_to_root(base: &str, mapping: &OutputMapping, declaration: bool) -> Option<String> {
    let output_dir = if declaration {
        mapping.declarations.as_deref().unwrap_or(&mapping.output)
    } else {
        &mapping.output
    };
    let tail = if output_dir.is_empty() {
        base
    } else {
        base.strip_prefix(output_dir)?.strip_prefix('/')?
    };
    Some(join(&mapping.sources, tail))
}

/// Joins a directory and a relative path, either of which may be empty.
fn join(directory: &str, relative: &str) -> String {
    if directory.is_empty() {
        relative.to_owned()
    } else if relative.is_empty() {
        directory.to_owned()
    } else {
        format!("{directory}/{relative}")
    }
}

/// Whether `path` is `pattern`, or fits it when the pattern holds one `*`.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == path,
        Some((prefix, suffix)) => {
            path.len() >= prefix.len() + suffix.len()
                && path.starts_with(prefix)
                && path.ends_with(suffix)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn known(paths: &[&str]) -> BTreeSet<SmolStr> {
        paths.iter().copied().map(SmolStr::new).collect()
    }

    fn mapping(out_dir: &str, root_dir: &str) -> OutputMapping {
        OutputMapping {
            output: out_dir.to_owned(),
            declarations: None,
            sources: root_dir.to_owned(),
        }
    }

    fn modules_of(entries: Entries) -> Vec<String> {
        match entries {
            Entries::Modules(modules) => modules.iter().map(ToString::to_string).collect(),
            Entries::Unknown => vec!["<unknown>".to_owned()],
        }
    }

    #[test]
    fn should_take_every_string_of_a_nested_exports_map_as_a_target() {
        let exports = json!({
            ".": {"import": "./a.ts", "require": ["./b.ts", null]},
            "./c": "./c.ts",
            "./blocked/*": null
        });
        let mut targets = Vec::new();

        assert!(collect_targets(&exports, &mut targets));

        targets.sort_unstable();
        assert_eq!(targets, ["./a.ts", "./b.ts", "./c.ts"]);
    }

    #[test]
    fn should_reject_an_exports_value_that_is_neither_text_nor_a_container() {
        let flag = json!({".": true});
        let number = json!(3);

        assert!(!collect_targets(&flag, &mut Vec::new()));
        assert!(!collect_targets(&number, &mut Vec::new()));
    }

    #[test]
    fn should_use_a_source_target_as_written() {
        let entries = entry_modules(
            "packages/a",
            &json!("./src/index.ts"),
            None,
            &known(&["packages/a/src/index.ts", "packages/a/src/other.ts"]),
        );

        assert_eq!(modules_of(entries), ["packages/a/src/index.ts"]);
    }

    #[test]
    fn should_move_a_built_target_from_the_output_to_the_root_directory() {
        let entries = entry_modules(
            "",
            &json!("./lib/deep/x.js"),
            Some(&mapping("lib", "src")),
            &known(&["src/deep/x.ts", "lib/deep/x.ts"]),
        );

        assert_eq!(modules_of(entries), ["src/deep/x.ts"]);
    }

    #[test]
    fn should_map_an_output_directory_that_is_the_package_directory() {
        let entries = entry_modules(
            "",
            &json!("./x.js"),
            Some(&mapping("", "src")),
            &known(&["src/x.ts"]),
        );

        assert_eq!(modules_of(entries), ["src/x.ts"]);
    }

    #[test]
    fn should_swap_the_extension_of_a_built_target_that_stays_in_place() {
        let entries = entry_modules("", &json!("./src/x.mjs"), None, &known(&["src/x.mts"]));

        assert_eq!(modules_of(entries), ["src/x.mts"]);
    }

    #[test]
    fn should_stop_at_unknown_when_a_target_maps_to_no_file() {
        let entries = entry_modules("", &json!("./dist/x.js"), None, &known(&["src/x.ts"]));

        assert_eq!(entries, Entries::Unknown);
    }

    #[test]
    fn should_stop_at_unknown_for_a_target_that_is_not_relative_or_leaves_the_package() {
        let present = known(&["x.ts"]);

        assert_eq!(
            entry_modules("", &json!("x.ts"), None, &present),
            Entries::Unknown
        );
        assert_eq!(
            entry_modules("", &json!("./../x.ts"), None, &present),
            Entries::Unknown
        );
    }

    #[test]
    fn should_stop_at_unknown_for_a_target_without_an_extension_or_with_two_stars() {
        let present = known(&["bin/cli.ts"]);

        assert_eq!(
            entry_modules("", &json!("./bin/cli"), None, &present),
            Entries::Unknown
        );
        assert_eq!(
            entry_modules("", &json!("./*/*.ts"), None, &present),
            Entries::Unknown
        );
    }

    #[test]
    fn should_skip_a_target_that_is_not_code() {
        let entries = entry_modules(
            "",
            &json!({".": "./index.ts", "./package.json": "./package.json"}),
            None,
            &known(&["index.ts"]),
        );

        assert_eq!(modules_of(entries), ["index.ts"]);
    }

    #[test]
    fn should_stop_at_unknown_for_an_extension_that_may_be_code() {
        let present = known(&["src/Comp.ts"]);

        assert_eq!(
            entry_modules("", &json!("./src/Comp.vue"), None, &present),
            Entries::Unknown
        );
        assert_eq!(
            entry_modules("", &json!("./lib/v1.0/"), None, &present),
            Entries::Unknown
        );
    }

    #[test]
    fn should_skip_a_data_or_media_target_whatever_its_case() {
        let entries = entry_modules(
            "",
            &json!([
                "./index.ts",
                "./logo.PNG",
                "./font.woff2",
                "./types.d.ts.map"
            ]),
            None,
            &known(&["index.ts"]),
        );

        assert_eq!(modules_of(entries), ["index.ts"]);
    }

    #[test]
    fn should_expand_a_pattern_over_nested_files() {
        let entries = entry_modules(
            "",
            &json!("./src/*.ts"),
            None,
            &known(&["src/a.ts", "src/deep/b.ts", "lib/c.ts", "src/d.tsx"]),
        );

        assert_eq!(modules_of(entries), ["src/a.ts", "src/deep/b.ts"]);
    }

    #[test]
    fn should_expand_a_pattern_that_ends_at_its_star_over_every_file_below() {
        let entries = entry_modules(
            "",
            &json!("./src/*"),
            None,
            &known(&["src/a.ts", "src/deep/b.tsx", "lib/c.ts"]),
        );

        assert_eq!(modules_of(entries), ["src/a.ts", "src/deep/b.tsx"]);
    }

    #[test]
    fn should_read_a_declaration_target_through_the_declaration_directory() {
        let declarations = OutputMapping {
            declarations: Some("types".to_owned()),
            ..mapping("dist", "src")
        };
        let entries = entry_modules(
            "",
            &json!("./types/x.d.ts"),
            Some(&declarations),
            &known(&["src/x.ts"]),
        );

        assert_eq!(modules_of(entries), ["src/x.ts"]);
    }

    #[test]
    fn should_match_a_pattern_only_when_both_its_ends_fit() {
        assert!(matches_pattern("a/*.ts", "a/b/c.ts"));
        assert!(matches_pattern("a/b.ts", "a/b.ts"));
        assert!(!matches_pattern("a/*.ts", "a/b.tsx"));
        assert!(!matches_pattern("a/*b.ts", "a/b.ts.ts"));
        assert!(!matches_pattern("a/b*b.ts", "a/b.ts"));
    }
}
