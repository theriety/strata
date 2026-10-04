//! Test pairs and entry files stated as adapter facts.
//!
//! Pairs come from the module tree: a file reached through `#[cfg(test)] mod x;`
//! is the test of the production file declaring it (`ParsedFile::test_of`). A
//! same-file `#[cfg(test)] mod tests { .. }` records nothing; the source is its
//! own test, and nothing here lets a later pass split it.
//!
//! Entries come from the cargo targets the workspace loader read: every
//! library, binary, example, integration-test, bench and build-script root.
//! A project whose targets are unknown has no entries.
//!
//! The start symbol is a root-level `fn main` of a program-starting target
//! (binary, example, test, bench or build script, never a library); a `main`
//! in a library or inside a nested module is an ordinary function.

use std::collections::BTreeSet;
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::FileFact;

use super::database::Database;
use crate::parse::ParsedFile;

/// The name of a Rust program's start function.
const START_NAME: &str = "main";

/// The facts for `files`, sorted by file.
pub(super) fn file_facts(files: &[ParsedFile], database: &Database, root: &Path) -> Vec<FileFact> {
    let present: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let entries = database.entry_paths(root);
    let starts = database.start_paths(root);
    let mut facts: Vec<FileFact> = files
        .iter()
        .filter_map(|file| {
            let source = file.test_of.as_ref()?;
            present
                .contains(source.as_str())
                .then(|| FileFact::test_pair(file.path.clone(), source.clone()))
        })
        .collect();
    facts.extend(files.iter().filter_map(|file| {
        let path = SmolStr::new(file.path.as_str());
        entries.contains(&path).then(|| {
            if starts.contains(&path) && file.root_main {
                FileFact::entry_with_start(path, START_NAME)
            } else {
                FileFact::entry_file(path)
            }
        })
    }));
    facts.sort();
    facts
}
