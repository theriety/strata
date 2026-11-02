//! Test-root zones (ADR-26): where a test file may be placed.
//!
//! The engine does not guess which folders are test folders. An adapter marks
//! the test files (polarity, `FileFact::test_of`); a test root is then the
//! topmost folder of a test file that holds test files only. A test file moves
//! only inside its own root, and a production file never enters one.

use std::collections::BTreeSet;

use smol_str::SmolStr;

use crate::analyze::relocation::FileInfo;

/// The zone key of every file vertex, parallel to `files`.
///
/// A production file's key is empty. A test file's key is its package plus the
/// topmost ancestor folder holding only test files; a test file with no such
/// folder (it sits among production files) gets a key of its own, so it can
/// only stay where it is.
pub(in crate::analyze) fn zone_keys(files: &[FileInfo], test_zone: &[bool]) -> Vec<SmolStr> {
    let mut production_folders: BTreeSet<(&str, String)> = BTreeSet::new();
    for (vertex, file) in files.iter().enumerate() {
        if test_zone.get(vertex).copied().unwrap_or(false) {
            continue;
        }
        for folder in ancestor_folders(&file.name) {
            production_folders.insert((file.home.package.as_str(), folder));
        }
    }
    files
        .iter()
        .enumerate()
        .map(|(vertex, file)| {
            if !test_zone.get(vertex).copied().unwrap_or(false) {
                return SmolStr::default();
            }
            let package = file.home.package.as_str();
            ancestor_folders(&file.name)
                .into_iter()
                .find(|folder| !production_folders.contains(&(package, folder.clone())))
                .map_or_else(
                    || SmolStr::new(format!("!{}", file.name)),
                    |folder| SmolStr::new(format!("{package}:{folder}")),
                )
        })
        .collect()
}

/// The folders above `path`, outermost first: `a/b/c.rs` gives `a`, `a/b`.
fn ancestor_folders(path: &str) -> Vec<String> {
    let segments: Vec<&str> = path.split('/').collect();
    (1..segments.len())
        .map(|end| {
            segments
                .iter()
                .take(end)
                .copied()
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::relocation::LaminarHome;

    fn info(path: &str, package: &str) -> FileInfo {
        FileInfo {
            container: 0,
            name: SmolStr::new(path),
            production_sloc: 1,
            namespace: SmolStr::default(),
            home: LaminarHome {
                folder: SmolStr::default(),
                domain: SmolStr::default(),
                package: SmolStr::new(package),
                synthetic: false,
            },
        }
    }

    #[test]
    fn should_list_ancestor_folders_outermost_first() {
        assert_eq!(ancestor_folders("a/b/c.rs"), vec!["a", "a/b"]);
        assert_eq!(ancestor_folders("c.rs"), Vec::<String>::new());
    }

    #[test]
    fn should_key_a_test_by_its_topmost_test_only_folder() {
        let files = vec![
            info("src/a.ts", "p"),
            info("tests/unit/a.spec.ts", "p"),
            info("tests/b.spec.ts", "p"),
        ];
        let keys = zone_keys(&files, &[false, true, true]);
        assert_eq!(keys, vec!["", "p:tests", "p:tests"]);
    }

    #[test]
    fn should_key_a_test_among_production_files_by_itself() {
        let files = vec![info("src/a.ts", "p"), info("src/a.spec.ts", "p")];
        let keys = zone_keys(&files, &[false, true]);
        assert_eq!(keys, vec!["", "!src/a.spec.ts"]);
    }
}
