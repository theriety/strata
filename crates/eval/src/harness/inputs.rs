//! Construction and validation of the data consumed by harness evaluations.

use std::collections::{BTreeMap, BTreeSet};

use unlace_engine::result::{AnalyzeResult, ContainerNode, Level, ModeResult};

use crate::error::EvalError;
use crate::metrics;
use crate::target::{AssertBlock, FaceMode, TargetSpec};

/// The source-root set mirrored from `AdaptersConfig::default`; all first-batch
/// targets run on built-in defaults, so container keys strip exactly these.
const SOURCE_ROOTS: [&str; 7] = ["src", "spec", "test", "tests", "lib", "dist", "__tests__"];

/// One analyzed package scope: the Package node's name and, when that name is
/// also a real directory prefix of census files, the `<root>/` prefix to strip.
#[derive(Debug, Clone)]
pub(super) struct PackageScope {
    pub(super) name: String,
    pub(super) prefix: Option<String>,
}

/// Per-face evaluation inputs: the asserted candidate tree plus how much hard
/// capacity it leaves behind.
pub(super) struct FaceInputs<'a> {
    pub(super) tree: &'a ContainerNode,
    pub(super) capacity_remaining: Option<u32>,
}

/// Everything the pure evaluator needs; built once per case from the engine
/// result, and synthesizable in unit tests without the engine.
pub(super) struct EvalInputs<'a> {
    pub(super) current_placement: BTreeMap<String, String>,
    pub(super) packages: Vec<PackageScope>,
    pub(super) census: BTreeSet<String>,
    pub(super) faces: BTreeMap<FaceMode, FaceInputs<'a>>,
}

/// CONTRACT.md's D(P, path): fixture files whose repo-relative path lies under
/// `<P.root>/[<source-root>/]<path>/`, computed per package scope over the
/// census.
fn directory_members(census: &BTreeSet<String>, package: &PackageScope, path: &str) -> Vec<String> {
    census
        .iter()
        .filter(|file| directory_contains(package.prefix.as_deref(), file, path))
        .cloned()
        .collect()
}

/// Every package scope that physically holds `path`, with its member list.
pub(super) fn packages_holding<'a>(
    census: &BTreeSet<String>,
    packages: &'a [PackageScope],
    path: &str,
) -> Vec<(&'a PackageScope, Vec<String>)> {
    packages
        .iter()
        .map(|package| (package, directory_members(census, package, path)))
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

/// Whether `file` sits under `[<package-root>/][<source-root>/]<path>/`.
fn directory_contains(package_prefix: Option<&str>, file: &str, path: &str) -> bool {
    directory_root(package_prefix, file, path).is_some()
}

/// The repo-relative directory `[<package-root>/][<source-root>/]<path>` that
/// holds `file`, or `None` when `file` does not sit under it.
pub(super) fn directory_root<'f>(
    package_prefix: Option<&str>,
    file: &'f str,
    path: &str,
) -> Option<&'f str> {
    let mut rest = match package_prefix {
        Some(root) => file
            .strip_prefix(root)
            .and_then(|tail| tail.strip_prefix('/'))?,
        None => file,
    };
    // One leading source-root segment below the package root is transparent.
    if let Some((first, tail)) = rest.split_once('/')
        && SOURCE_ROOTS.contains(&first)
    {
        rest = tail;
    }
    let slash_after_path = || rest.as_bytes().get(path.len()).copied() == Some(b'/');
    let holds =
        rest == path || (rest.starts_with(path) && rest.len() > path.len() && slash_after_path());
    holds.then(|| &file[..file.len() - rest.len() + path.len()])
}

/// Whether `file` lives directly under directory `dir` (`dir/…`).
fn starts_with_segment(file: &str, dir: &str) -> bool {
    file.strip_prefix(dir)
        .is_some_and(|tail| tail.starts_with('/'))
}

/// Derives the package scopes from the current tree's Package nodes: a name
/// that prefixes census files is a real directory root; a lone package whose
/// name prefixes nothing is the repository-rooted single-package case.
pub(super) fn discover_packages(
    current: &ContainerNode,
    census: &BTreeSet<String>,
) -> Vec<PackageScope> {
    let mut names = Vec::new();
    collect_package_names(current, &mut names);
    names
        .into_iter()
        .map(|name| {
            let prefix = census
                .iter()
                .any(|file| starts_with_segment(file, &name))
                .then(|| name.clone());
            PackageScope { name, prefix }
        })
        .collect()
}

/// Collects distinct Package-level node names in tree order.
fn collect_package_names(node: &ContainerNode, names: &mut Vec<String>) {
    if node.level == Level::Package && !names.contains(&node.name) {
        names.push(node.name.clone());
    }
    for child in node.children.iter().flatten() {
        collect_package_names(child, names);
    }
}

/// Finds the Package node with `name`, searching the whole tree.
pub(super) fn find_package_node<'a>(
    node: &'a ContainerNode,
    name: &str,
) -> Option<&'a ContainerNode> {
    if node.level == Level::Package && node.name == name {
        return Some(node);
    }
    node.children
        .iter()
        .flatten()
        .find_map(|child| find_package_node(child, name))
}

/// Validates every census-dependent reference before scoring: referenced file
/// paths must exist, and every `preserve_dir` key must physically resolve under
/// at least one analyzed package. A misspelled path is an error, never fake
/// distance.
pub(super) fn validate_references(
    spec: &TargetSpec,
    block: &AssertBlock,
    census: &BTreeSet<String>,
    packages: &[PackageScope],
    current: &ContainerNode,
    fixture: &str,
) -> Vec<EvalError> {
    let mut errors = Vec::new();
    let mut check_file_set = |kind: &str, paths: &[String]| {
        for path in paths {
            if !census.contains(path) {
                errors.push(EvalError::TargetInvalid {
                    target: fixture.to_owned(),
                    message: format!(
                        "{kind} references {path:?}, which the fixture census does not contain"
                    ),
                });
            }
        }
    };
    for pin in &block.cycle_import_cut {
        check_file_set("cycle_import_cut", &pin.files);
        if let Some(expect) = &pin.expect {
            check_file_set(
                "cycle_import_cut.expect",
                &[expect.importer.clone(), expect.imported.clone()],
            );
        }
    }
    for assertion in block.keep_together.iter().chain(block.separate.iter()) {
        check_file_set("keep_together/separate", &assertion.paths);
    }
    for pin in &block.advice_tier {
        check_file_set("advice_tier", std::slice::from_ref(&pin.path));
        if let (Some(_), Some(to)) = (&pin.symbol, &pin.to) {
            check_file_set("advice_tier.to", std::slice::from_ref(to));
        }
    }
    if let Some(reference) = &spec.reference {
        for container in &reference.container {
            check_file_set("reference.container", &container.files);
        }
    }

    for preserve in &block.preserve_dir {
        if packages_holding(census, packages, &preserve.path).is_empty() {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "preserve_dir {:?} resolves under no analyzed package (checked <root>/[<source-root>/]<path>/ against the census)",
                    preserve.path
                ),
            });
        }
    }

    // FIX08: a symbol home must exist in the CURRENT layout — the assertion
    // pins an existing home, so a misspelled symbol or path is a harness
    // error, never permanent fake distance.
    let homes = current_symbol_homes(current);
    for pin in &block.advice_tier {
        // A file move's destination is a folder: it must hold some census file
        // today, so a typo never selects nothing and passes as `absent`.
        if pin.symbol.is_none()
            && let Some(to) = &pin.to
            && !census.iter().any(|file| starts_with_segment(file, to))
        {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "advice_tier destination {to:?} is no directory of the fixture census"
                ),
            });
        }
        if let Some(symbol) = &pin.symbol
            && !homes
                .get(&pin.path)
                .is_some_and(|symbols| symbols.contains(symbol))
        {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "advice_tier selects {symbol:?} leaving {:?}, which the current layout does not show",
                    pin.path
                ),
            });
        }
    }
    for assertion in &block.preserve_symbol_home {
        if !homes
            .get(&assertion.path)
            .is_some_and(|symbols| symbols.contains(&assertion.symbol))
        {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "preserve_symbol_home pins {:?} in {:?}, which the current layout does not show",
                    assertion.symbol, assertion.path
                ),
            });
        }
    }
    errors
}

/// Maps every file node of `root` to the set of symbol names it carries.
fn current_symbol_homes(root: &ContainerNode) -> BTreeMap<String, BTreeSet<String>> {
    let mut homes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    metrics::walk_files(root, &mut |node| {
        if let Some(symbols) = &node.symbols {
            let entry = homes.entry(node.name.clone()).or_default();
            for placement in symbols {
                entry.insert(placement.name.clone());
            }
        }
    });
    homes
}

/// Builds the evaluator inputs: current-tree placement plus one face entry per
/// mode any assertion needs, each pointing at its asserted candidate.
pub(super) fn build_inputs<'a>(
    result: &'a AnalyzeResult,
    block: &AssertBlock,
    fixture: &str,
) -> Result<EvalInputs<'a>, EvalError> {
    let fail = |message: String| EvalError::TargetInvalid {
        target: fixture.to_owned(),
        message,
    };

    let mut needed: Vec<FaceMode> = block.modes.clone();
    for face in block
        .preserve_dir
        .iter()
        .filter_map(|assertion| assertion.mode)
        .chain(block.keep_together.iter().filter_map(|a| a.mode))
        .chain(block.separate.iter().filter_map(|a| a.mode))
        .chain(block.size_band.iter().filter_map(|a| a.mode))
        .chain(block.move_budget.iter().map(|budget| budget.mode))
        .chain(block.no_synthetic_bucket.iter().filter_map(|a| a.mode))
        .chain(block.name_alignment.iter().filter_map(|a| a.mode))
        .chain(block.capacity_relief.iter().map(|relief| relief.mode))
        .chain(
            block
                .preserve_symbol_home
                .iter()
                .filter_map(|assertion| assertion.mode),
        )
    {
        if !needed.contains(&face) {
            needed.push(face);
        }
    }

    let mut faces = BTreeMap::new();
    for face in needed {
        let mode_result = mode_result_of(result, face).ok_or_else(|| {
            fail(format!(
                "assertions need the {face:?} mode but [run].mode did not produce it"
            ))
        })?;
        let index = usize::try_from(block.candidate - 1);
        let candidate = match index {
            Ok(index) => mode_result.candidates.get(index),
            Err(_) => None,
        };
        let (tree, capacity_remaining) = match candidate {
            Some(candidate) => (
                &candidate.tree,
                candidate
                    .capacity_remainder
                    .map(|remainder| remainder.remaining),
            ),
            None if block.candidate == 1 && mode_result.candidates.is_empty() => (
                &result.current.tree,
                Some(mode_result.current.capacity_breaks),
            ),
            None => {
                return Err(fail(format!(
                    "candidate {} requested but the {face:?} mode returned {} candidates",
                    block.candidate,
                    mode_result.candidates.len()
                )));
            }
        };
        faces.insert(
            face,
            FaceInputs {
                tree,
                capacity_remaining,
            },
        );
    }

    Ok(EvalInputs {
        current_placement: metrics::structural_placement(&result.current.tree),
        packages: discover_packages(&result.current.tree, &census_of(result)),
        census: census_of(result),
        faces,
    })
}

/// The analyzed file census, recomputed from the current tree.
pub(super) fn census_of(result: &AnalyzeResult) -> BTreeSet<String> {
    metrics::members(&result.current.tree).into_iter().collect()
}

/// The mode's [`ModeResult`], or `None` when the run did not produce the face.
pub(super) fn mode_result_of(result: &AnalyzeResult, face: FaceMode) -> Option<&ModeResult> {
    match face {
        FaceMode::Anchored => result.profiles.anchored.as_ref(),
        FaceMode::Greenfield => result.profiles.greenfield.as_ref(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use unlace_engine::result::{ContainerNode, Level, SymbolPlacement};

    use super::validate_references;
    use crate::harness::assertions::fixtures::{file, node};
    use crate::target::TargetSpec;

    const INFERENCE: &str = "src/differ/inference.ts";
    const SHAPE: &str = "src/tree/validation/shape.ts";

    /// The current tree: `inference.ts` exporting `inferMatches`, beside `shape.ts`.
    fn current() -> ContainerNode {
        let mut inference = file(INFERENCE);
        inference.symbols = Some(vec![SymbolPlacement {
            name: "inferMatches".to_owned(),
            display: None,
            visibility: Level::Package,
        }]);
        node(Level::Package, "fixture", vec![inference, file(SHAPE)])
    }

    /// The load-time errors for one block of `advice_tier` pins; a target
    /// that fails to parse reports that instead, so no test passes vacuously.
    fn errors_for(pins: &str) -> Vec<String> {
        let parsed = toml::from_str::<TargetSpec>(&format!(
            r#"
schema = 1
fixture = "fixture"
[meta]
failure_mode = "directory-tearing"
language = "typescript"
question = "q"
[run]
mode = "both"
candidates = 3
seed = 42
config = "defaults"
[[assert]]
modes = ["anchored"]
candidate = 1
{pins}
"#
        ));
        let spec = match parsed {
            Ok(spec) => spec,
            Err(error) => return vec![format!("unparsable test target: {error}")],
        };
        let census: BTreeSet<String> = [INFERENCE, SHAPE].map(str::to_owned).into();
        spec.assert.first().map_or_else(
            || vec!["test target has no assert block".to_owned()],
            |block| {
                validate_references(&spec, block, &census, &[], &current(), "fixture")
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            },
        )
    }

    fn pin(fields: &str) -> String {
        format!("[[assert.advice_tier]]\n{fields}\nmax_tier = \"review\"\nbecause = \"F\"\n")
    }

    #[test]
    fn should_accept_advice_pins_resolving_against_the_census() {
        let pins = [
            pin(&format!("path = \"{SHAPE}\"\nto = \"src/tree\"")),
            pin(&format!(
                "path = \"{INFERENCE}\"\nsymbol = \"inferMatches\"\nto = \"{SHAPE}\""
            )),
        ]
        .concat();

        assert_eq!(errors_for(&pins), Vec::<String>::new());
    }

    #[test]
    fn should_reject_a_misspelled_advice_path() {
        let errors = errors_for(&pin("path = \"src/tree/validation/shap.ts\""));

        assert_eq!(errors.len(), 1);
        assert!(errors.iter().all(|error| {
            error.contains("advice_tier references \"src/tree/validation/shap.ts\"")
        }));
    }

    #[test]
    fn should_reject_an_unknown_symbol_home() {
        let errors = errors_for(&pin(&format!(
            "path = \"{INFERENCE}\"\nsymbol = \"inferMatch\""
        )));

        assert_eq!(errors.len(), 1);
        assert!(errors.iter().all(|error| {
            error.contains("selects \"inferMatch\" leaving \"src/differ/inference.ts\"")
        }));
    }

    #[test]
    fn should_reject_a_destination_outside_the_census() {
        let folder = errors_for(&pin(&format!("path = \"{SHAPE}\"\nto = \"src/nowhere\"")));
        let symbol_file = errors_for(&pin(&format!(
            "path = \"{INFERENCE}\"\nsymbol = \"inferMatches\"\nto = \"src/differ/matcher.ts\""
        )));

        assert_eq!(folder.len(), 1);
        assert!(
            folder
                .iter()
                .all(|error| error.contains("destination \"src/nowhere\" is no directory"))
        );
        assert_eq!(symbol_file.len(), 1);
        assert!(symbol_file.iter().all(|error| {
            error.contains("advice_tier.to references \"src/differ/matcher.ts\"")
        }));
    }
}
