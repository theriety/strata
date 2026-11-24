//! Test pairs: the spec/test file a TypeScript or JavaScript source mirrors.
//!
//! A source `src/{dir}/{stem}.{ext}` is paired with a test that sits beside it
//! (`src/{dir}/{stem}.spec.{ext}`) or in a mirrored test root
//! (`{spec,test,tests}/{dir}/{stem}.{spec,test}.{ext}`) or in a jest
//! `src/{dir}/__tests__/{stem}.{spec,test}.{ext}` folder, but only when that test
//! file is present in the analysed tree and no other source claims it.
//! `source` stands in for `src` as the source root.
//!
//! Folder companions: a module `{dir}/{x}.{ext}` beside a folder `{dir}/{x}/`
//! that holds an analysed module is that folder's entry, as if it were
//! `{dir}/{x}/index.{ext}`. The fact is stated only when the match is
//! unambiguous: the folder has no `index` module of its own and no second module
//! shares the stem. A paired test carries the mirrored companion folder (the
//! folder named after the source's stem, in the test's own directory) when that
//! folder holds analysed modules.
//!
//! Entry files: none are recorded. A package entry is whatever `package.json`
//! `main`/`module`/`exports` name, and those usually point at build output
//! (`dist/`), so mapping them back to a source file would be a guess. When
//! unsure: not an entry.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use unlace_ir::{FileFact, unique_test_pairs};

use crate::parse::ParsedModule;
use crate::test_paths::{EXTENSIONS, TEST_ROOTS};

const SOURCE_ROOTS: [&str; 2] = ["src", "source"];
const MARKERS: [&str; 2] = ["spec", "test"];

/// The test-pair and folder-companion facts for `modules`, one fact per file,
/// sorted by file path.
pub(super) fn file_facts(modules: &[ParsedModule]) -> Vec<FileFact> {
    let present: BTreeSet<&str> = modules.iter().map(|module| module.path.as_str()).collect();
    let folders = module_folders(&present);
    let companions = folder_companions(&present, &folders);
    let mut facts: BTreeMap<SmolStr, FileFact> = test_pairs(&present)
        .into_iter()
        .map(|fact| (fact.file.clone(), fact))
        .collect();
    for fact in facts.values_mut() {
        fact.folder_companion = mirrored_companion(fact, &companions, &folders);
    }
    for (file, folder) in companions {
        match facts.get_mut(file) {
            Some(fact) => fact.folder_companion = Some(SmolStr::new(folder)),
            None => {
                facts.insert(SmolStr::new(file), FileFact::folder_companion(file, folder));
            }
        }
    }
    facts.into_values().collect()
}

/// The test-pair facts for the tests among `present`, sorted by test path.
fn test_pairs(present: &BTreeSet<&str>) -> Vec<FileFact> {
    let candidates = present.iter().filter_map(|path| {
        let (root, dir, stem, extension) = split_source(path)?;
        Some((SmolStr::new(path), test_paths(root, dir, stem, extension)))
    });
    unique_test_pairs(candidates, present)
}

/// Every directory that holds a TypeScript-family module, at any depth.
fn module_folders<'a>(present: &BTreeSet<&'a str>) -> BTreeSet<&'a str> {
    present
        .iter()
        .filter(|path| module_stem(path).is_some())
        .flat_map(|path| path.match_indices('/').map(|(at, _)| path.split_at(at).0))
        .collect()
}

/// Each module that is the companion of the folder named after its stem,
/// mapped to that folder. A stem shared by several modules, a missing or
/// module-less folder, or a folder with its own `index` module states nothing.
fn folder_companions<'a>(
    present: &BTreeSet<&'a str>,
    folders: &BTreeSet<&str>,
) -> BTreeMap<&'a str, &'a str> {
    let mut by_stem: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for &path in present {
        if let Some(stem) = module_stem(path) {
            by_stem.entry(stem).or_default().push(path);
        }
    }
    by_stem
        .into_iter()
        .filter_map(|(stem, files)| match files.as_slice() {
            [file] if folders.contains(stem) && !has_index(present, stem) => Some((*file, stem)),
            _ => None,
        })
        .collect()
}

/// A module's path without its extension; `None` for any other file.
fn module_stem(path: &str) -> Option<&str> {
    let (stem, extension) = path.rsplit_once('.')?;
    (EXTENSIONS.contains(&extension) && !stem.is_empty() && !stem.ends_with('/')).then_some(stem)
}

/// Whether `folder` already holds its own `index` module.
fn has_index(present: &BTreeSet<&str>, folder: &str) -> bool {
    EXTENSIONS
        .iter()
        .any(|extension| present.contains(format!("{folder}/index.{extension}").as_str()))
}

/// The companion folder a paired test mirrors from its source, when that
/// mirror holds analysed modules. A test sits beside its source, in a mirrored
/// root or in `__tests__`, so its mirror is the folder named after the
/// source's stem, in the test's own directory.
fn mirrored_companion(
    fact: &FileFact,
    companions: &BTreeMap<&str, &str>,
    folders: &BTreeSet<&str>,
) -> Option<SmolStr> {
    let folder = companions.get(fact.test_of.as_deref()?)?;
    let name = folder.rsplit_once('/').map_or(*folder, |(_, name)| name);
    let mirror = match fact.file.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{name}"),
        None => name.to_owned(),
    };
    folders
        .contains(mirror.as_str())
        .then(|| SmolStr::new(mirror))
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
