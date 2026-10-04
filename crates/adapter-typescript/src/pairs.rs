//! Test pairs: the spec/test file a TypeScript or JavaScript source mirrors.
//!
//! A source `src/{dir}/{stem}.{ext}` is paired with a test that sits beside it
//! (`src/{dir}/{stem}.spec.{ext}`) or in a mirrored test root
//! (`{spec,test,tests}/{dir}/{stem}.{spec,test}.{ext}`) or in a jest
//! `src/{dir}/__tests__/{stem}.{spec,test}.{ext}` folder, but only when that test
//! file is present in the analysed tree and no other source claims it.
//! `source` stands in for `src` as the source root.
//!
//! Entry files: none are recorded. A package entry is whatever `package.json`
//! `main`/`module`/`exports` name, and those usually point at build output
//! (`dist/`), so mapping them back to a source file would be a guess. When
//! unsure: not an entry.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use strata_ir::{FileFact, unique_test_pairs};

use crate::parse::ParsedModule;

const SOURCE_ROOTS: [&str; 2] = ["src", "source"];
const TEST_ROOTS: [&str; 3] = ["spec", "test", "tests"];
const MARKERS: [&str; 2] = ["spec", "test"];
const EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The test-pair facts for `modules`, sorted by test path.
pub(super) fn file_facts(modules: &[ParsedModule]) -> Vec<FileFact> {
    let present: BTreeSet<&str> = modules.iter().map(|module| module.path.as_str()).collect();
    let candidates = present.iter().filter_map(|path| {
        let (root, dir, stem, extension) = split_source(path)?;
        Some((SmolStr::new(path), test_paths(root, dir, stem, extension)))
    });
    unique_test_pairs(candidates, &present)
}

/// Splits `{root}/{dir}/{stem}.{ext}` into its root, directory, stem and
/// extension; `None` when the path is not a production-looking source there.
fn split_source(path: &str) -> Option<(&str, &str, &str, &str)> {
    let (root, rest) = path.split_once('/')?;
    if !SOURCE_ROOTS.contains(&root) {
        return None;
    }
    let (dir, file) = rest.rsplit_once('/').unwrap_or(("", rest));
    let (stem, extension) = file.rsplit_once('.')?;
    let is_test_name = MARKERS
        .iter()
        .any(|marker| stem.ends_with(&format!(".{marker}")));
    (EXTENSIONS.contains(&extension) && !stem.is_empty() && !is_test_name)
        .then_some((root, dir, stem, extension))
}

/// Every path a test of this source could have, in convention order.
fn test_paths(root: &str, dir: &str, stem: &str, extension: &str) -> Vec<String> {
    let join = |base: &str, marker: &str| {
        let folder = if dir.is_empty() {
            base.to_owned()
        } else {
            format!("{base}/{dir}")
        };
        format!("{folder}/{stem}.{marker}.{extension}")
    };
    let mut paths = Vec::new();
    for test_root in TEST_ROOTS {
        for marker in MARKERS {
            paths.push(join(test_root, marker));
        }
    }
    for marker in MARKERS {
        paths.push(join(root, marker));
    }
    // jest's `__tests__` folder beside the source.
    for marker in MARKERS {
        let beside = join(root, marker);
        let (folder, leaf) = beside.rsplit_once('/').unwrap_or(("", &beside));
        paths.push(format!("{folder}/__tests__/{leaf}"));
    }
    paths
}
