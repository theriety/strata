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

use std::collections::BTreeSet;
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::FileFact;

use super::database::Database;
use crate::parse::ParsedFile;

/// The facts for `files`, sorted by file.
pub(super) fn file_facts(files: &[ParsedFile], database: &Database, root: &Path) -> Vec<FileFact> {
    let present: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let entries = database.entry_paths(root);
    let mut facts: Vec<FileFact> = files
        .iter()
        .filter_map(|file| {
            let source = file.test_of.as_ref()?;
            present
                .contains(source.as_str())
                .then(|| FileFact::test_pair(file.path.clone(), source.clone()))
        })
        .collect();
    facts.extend(
        present
            .iter()
            .filter(|path| entries.contains(&SmolStr::new(path)))
            .map(|path| FileFact::entry_file(*path)),
    );
    facts.sort();
    facts
}
