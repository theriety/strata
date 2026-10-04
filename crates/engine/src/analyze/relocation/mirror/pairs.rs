//! Adapter-stated test pairs, turned into the same immutable links user
//! templates produce.
//!
//! The engine knows no language convention: an adapter says which file is the
//! test of which (`FileFact::test_of`), and this module only works out, from the
//! two paths alone, how the test's folder follows its source's folder.

use smol_str::SmolStr;
use strata_ir::IntermediateRepresentation;

use crate::analyze::relocation::mirror::ExactMirrorLink;
use crate::config::{MirrorCaptures, MirrorTemplate, ProfileConfig};

/// The `(test, source)` path pairs adapters state, when the profile uses them
/// (`test-mirroring.builtins`); user rules apply either way.
pub(in crate::analyze) fn adapter_pairs(
    ir: &IntermediateRepresentation,
    profile: &ProfileConfig,
) -> Vec<(SmolStr, SmolStr)> {
    if !profile.relocation.test_mirroring.builtins {
        return Vec::new();
    }
    ir.file_facts
        .iter()
        .filter_map(|fact| Some((fact.file.clone(), fact.test_of.clone()?)))
        .collect()
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').collect()
}

/// Derives the link for a source/test pair from their paths.
///
/// A test below its source's folder (`src/a/x.ts`, `src/a/__tests__/x.test.ts`)
/// follows the source anywhere under the source's top folder. A test in a
/// mirrored tree (`src/a/x.ts`, `test/a/x.spec.ts`) keeps its own root and
/// follows the part of the folder both paths share. Anything else, including
/// a test that shares no folder with a nested source, has no link.
pub(super) fn derive_link(
    source_vertex: u32,
    source_path: &str,
    test_path: &str,
) -> Option<ExactMirrorLink> {
    let (source, test) = (segments(source_path), segments(test_path));
    let (_, source_dir) = source.split_last()?;
    let (leaf, test_dir) = test.split_last()?;
    if source_dir.is_empty()
        || test
            .iter()
            .chain(&source)
            .any(|part| part.is_empty() || part.contains(['{', '}']))
    {
        return None;
    }
    let (source_root, template) = if test_dir.starts_with(source_dir) {
        let root = source_dir.first()?;
        let mut parts = vec![(*root).to_owned(), "{dir}".to_owned()];
        parts.extend(
            test_dir
                .iter()
                .skip(source_dir.len())
                .map(|part| (*part).to_owned()),
        );
        parts.push("{stem}".to_owned());
        ((*root).to_owned(), parts.join("/"))
    } else {
        let shared = source_dir
            .iter()
            .rev()
            .zip(test_dir.iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        // A test that mirrors nothing of a nested source folder (a Rust
        // `#[path = "../x/helpers.rs"]` target, say) would make every source
        // move imply a folder that never existed; it gets no link.
        if shared == 0 && source_dir.len() > 1 {
            return None;
        }
        let mut parts: Vec<String> = test_dir
            .iter()
            .take(test_dir.len() - shared)
            .map(|part| (*part).to_owned())
            .collect();
        parts.push("{dir}".to_owned());
        parts.push("{stem}".to_owned());
        (
            source_dir
                .iter()
                .take(source_dir.len() - shared)
                .copied()
                .collect::<Vec<_>>()
                .join("/"),
            parts.join("/"),
        )
    };
    MirrorTemplate::parse(&template)?;
    Some(ExactMirrorLink {
        source_vertex,
        source_root,
        test_template: template,
        captures: MirrorCaptures {
            dir: String::new(),
            stem: (*leaf).to_owned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(source: &str, test: &str, folder: &[&str]) -> Option<String> {
        let link = derive_link(0, source, test)?;
        let relative: Vec<&str> = folder.to_vec();
        let root: Vec<&str> = link
            .source_root
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let dir = relative.strip_prefix(root.as_slice())?.join("/");
        MirrorTemplate::parse(&link.test_template).map(|t| t.fill(&dir, &link.captures.stem))
    }

    #[test]
    fn should_keep_a_sibling_test_beside_its_source_under_the_source_root() {
        assert_eq!(
            project("src/a/x.ts", "src/a/x.spec.ts", &["src", "b"]).as_deref(),
            Some("src/b/x.spec.ts")
        );
        assert_eq!(
            project("src/a/x.ts", "src/a/x.spec.ts", &["lib", "b"]),
            None
        );
    }

    #[test]
    fn should_follow_a_mirrored_test_tree_and_a_tests_folder() {
        assert_eq!(
            project("src/a/x.ts", "test/a/x.test.ts", &["src", "b", "c"]).as_deref(),
            Some("test/b/c/x.test.ts")
        );
        assert_eq!(
            project("src/a/x.ts", "src/a/__tests__/x.test.ts", &["src", "b"]).as_deref(),
            Some("src/b/__tests__/x.test.ts")
        );
    }

    #[test]
    fn should_derive_no_link_for_an_unrelated_test_folder_of_a_nested_source() {
        assert!(derive_link(0, "src/a/mod.rs", "src/x/helpers.rs").is_none());
        assert!(derive_link(0, "src/x.ts", "test/x.test.ts").is_some());
    }

    #[test]
    fn should_derive_no_link_for_a_rootless_source_or_braced_path() {
        assert!(derive_link(0, "x.ts", "x.spec.ts").is_none());
        assert!(derive_link(0, "src/{a}/x.ts", "src/{a}/x.spec.ts").is_none());
    }
}
