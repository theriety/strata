//! Per-file facts an adapter states about its own language's conventions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// What an adapter knows about one source file beyond its declarations.
///
/// Test pairs, entry files and folder companions are language conventions, so
/// the adapter that owns the language states them and the engine only reads
/// them. A file with no pair, no entry role and no companion folder has no
/// fact. An adapter states a pair only when it
/// can prove it (a module tree, a mirrored path whose source exists); when
/// unsure it records nothing, so a missing fact means "no pair" and "not an
/// entry".
///
/// Paths are repo-relative with `/` separators, the same spelling the file
/// containers use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileFact {
    /// The file the fact describes.
    pub file: SmolStr,
    /// The production source file this test file mirrors, when it is a test
    /// whose source the adapter found. The pair is one-directional: `file` is
    /// the test, `test_of` the source. An inline test module shares its
    /// source's file, so it records nothing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_of: Option<SmolStr>,
    /// Whether the file is a program entry point (a crate root, a binary, a
    /// package entry). An entry file never moves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub entry: bool,
    /// The name of the program's start function declared at this entry file's
    /// root (Rust `fn main` of a binary, example or build script). The symbol
    /// pass never moves it out of the file; other symbols in the file may
    /// still move. Only an adapter that can name a start symbol sets it (the
    /// TypeScript and Python adapters have none), so absent means "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<SmolStr>,
    /// The repo-relative directory `x` this file `x.ts` is the entry of: the
    /// file sits beside `x/` and stands in for `x/index.ts` (decisions D1/E1).
    /// The adapter records nothing, so the file stays an ordinary sibling, when
    /// `x/index.ts` also exists, when both `x.ts` and `x.tsx` sit beside `x/`,
    /// or when `x/` holds no analysed module. A test paired with such a file
    /// gets the mirrored companion of its own (`spec/lexer/block.spec.ts`
    /// gets `spec/lexer/block`), so the pair stays mirrored. Added to schema
    /// 4; absent means "no companion folder".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_companion: Option<SmolStr>,
}

impl FileFact {
    /// A fact stating that `file` is the test of `source`.
    #[must_use]
    pub fn test_pair(file: impl Into<SmolStr>, source: impl Into<SmolStr>) -> Self {
        Self {
            file: file.into(),
            test_of: Some(source.into()),
            entry: false,
            start: None,
            folder_companion: None,
        }
    }

    /// A fact stating that `file` is a program entry point.
    #[must_use]
    pub fn entry_file(file: impl Into<SmolStr>) -> Self {
        Self {
            file: file.into(),
            test_of: None,
            entry: true,
            start: None,
            folder_companion: None,
        }
    }

    /// A fact stating that `file` is a program entry point whose start function
    /// is `start`.
    #[must_use]
    pub fn entry_with_start(file: impl Into<SmolStr>, start: impl Into<SmolStr>) -> Self {
        Self {
            file: file.into(),
            test_of: None,
            entry: true,
            start: Some(start.into()),
            folder_companion: None,
        }
    }

    /// A fact stating that `file` is the entry of the companion directory
    /// `folder` beside it.
    #[must_use]
    pub fn folder_companion(file: impl Into<SmolStr>, folder: impl Into<SmolStr>) -> Self {
        Self {
            file: file.into(),
            test_of: None,
            entry: false,
            start: None,
            folder_companion: Some(folder.into()),
        }
    }
}

/// Pairs each candidate test path that exists with the one source that names it.
///
/// `candidates` lists, per production source, the test paths its language
/// convention would give it. A test path that exists in `present` becomes a
/// [`FileFact::test_pair`] only when exactly one source names it: a test two
/// sources claim is ambiguous, so it records no pair (when unsure: no pair).
/// The result is sorted by test path.
#[must_use]
pub fn unique_test_pairs(
    candidates: impl IntoIterator<Item = (SmolStr, Vec<String>)>,
    present: &BTreeSet<&str>,
) -> Vec<FileFact> {
    let mut claims: BTreeMap<String, Vec<SmolStr>> = BTreeMap::new();
    for (source, tests) in candidates {
        for test in tests {
            if present.contains(test.as_str()) {
                let sources = claims.entry(test).or_default();
                if !sources.contains(&source) {
                    sources.push(source.clone());
                }
            }
        }
    }
    claims
        .into_iter()
        .filter_map(|(test, sources)| match sources.as_slice() {
            [only] => Some(FileFact::test_pair(test, only.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_pair_a_present_test_with_its_only_source() {
        let present: BTreeSet<&str> = ["a.ts", "a.spec.ts"].into_iter().collect();
        let pairs = unique_test_pairs(
            [(
                SmolStr::new("a.ts"),
                vec!["a.spec.ts".to_owned(), "gone.ts".to_owned()],
            )],
            &present,
        );
        assert_eq!(pairs, vec![FileFact::test_pair("a.spec.ts", "a.ts")]);
    }

    #[test]
    fn should_record_no_pair_for_a_test_two_sources_claim() {
        let present: BTreeSet<&str> = ["t.py"].into_iter().collect();
        let pairs = unique_test_pairs(
            [
                (SmolStr::new("src/x.py"), vec!["t.py".to_owned()]),
                (SmolStr::new("source/x.py"), vec!["t.py".to_owned()]),
            ],
            &present,
        );
        assert_eq!(pairs, Vec::<FileFact>::new());
    }
}
