//! Test pairs read off the module tree.
//!
//! A file reached through `#[cfg(test)] mod x;` (or below such a file) is the
//! test tree of the production file whose module chain declares it. The pair
//! comes from the declarations, never from file names, so `#[path]` test
//! modules pair too. A same-file `#[cfg(test)] mod tests { .. }` is its own
//! pair and records nothing: the source and its tests are one file.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;

use super::Child;

/// Maps each test-module file to the one production file that owns it.
///
/// A file also declared by a production (non-test) module, or claimed by two
/// production owners, is ambiguous and gets no pair.
pub(super) fn test_sources(children: &HashMap<SmolStr, Vec<Child>>) -> HashMap<SmolStr, SmolStr> {
    let mut test_files: HashSet<&SmolStr> = HashSet::new();
    let mut pending: Vec<&SmolStr> = children
        .values()
        .flatten()
        .filter(|child| child.cfg_test)
        .map(|child| &child.path)
        .collect();
    while let Some(path) = pending.pop() {
        if test_files.insert(path) {
            pending.extend(children.get(path).into_iter().flatten().map(|c| &c.path));
        }
    }
    let production_children: HashSet<&SmolStr> = children
        .iter()
        .filter(|(parent, _)| !test_files.contains(parent))
        .flat_map(|(_, kids)| kids)
        .filter(|child| !child.cfg_test)
        .map(|child| &child.path)
        .collect();
    let mut claims: HashMap<SmolStr, HashSet<SmolStr>> = HashMap::new();
    let mut order: Vec<&SmolStr> = children
        .keys()
        .filter(|path| !test_files.contains(path))
        .collect();
    order.sort();
    for owner in order {
        let mut walk: Vec<(&SmolStr, bool)> = vec![(owner, false)];
        let mut seen: HashSet<&SmolStr> = HashSet::new();
        while let Some((path, in_test)) = walk.pop() {
            for child in children.get(path).into_iter().flatten() {
                let test = in_test || child.cfg_test;
                if !test || !seen.insert(&child.path) {
                    continue;
                }
                claims
                    .entry(child.path.clone())
                    .or_default()
                    .insert(owner.clone());
                walk.push((&child.path, true));
            }
        }
    }
    claims
        .into_iter()
        .filter(|(path, claimed)| claimed.len() == 1 && !production_children.contains(path))
        .filter_map(|(path, claimed)| Some((path, claimed.into_iter().next()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(path: &str, cfg_test: bool) -> Child {
        Child {
            path: SmolStr::new(path),
            module: Vec::new(),
            cfg_test,
            may_drop: false,
            via_path: false,
        }
    }

    fn tree(entries: &[(&str, Vec<Child>)]) -> HashMap<SmolStr, Vec<Child>> {
        entries
            .iter()
            .map(|(parent, kids)| {
                let kids = kids
                    .iter()
                    .map(|kid| child(kid.path.as_str(), kid.cfg_test))
                    .collect();
                (SmolStr::new(parent), kids)
            })
            .collect()
    }

    fn pair(sources: &HashMap<SmolStr, SmolStr>, test: &str) -> Option<String> {
        sources.get(test).map(ToString::to_string)
    }

    #[test]
    fn should_pair_a_cfg_test_child_of_a_mod_rs_parent_with_that_mod_rs() {
        let sources = test_sources(&tree(&[(
            "src/a/mod.rs",
            vec![child("src/a/tests.rs", true)],
        )]));
        assert_eq!(
            pair(&sources, "src/a/tests.rs").as_deref(),
            Some("src/a/mod.rs")
        );
    }

    #[test]
    fn should_pair_a_file_declared_inside_a_test_file_with_the_production_owner() {
        // tests.rs declares `mod helpers;` without a cfg gate of its own, but it is
        // only reachable through the test module, so it is test code of widget.rs.
        let sources = test_sources(&tree(&[
            ("src/widget.rs", vec![child("src/widget/tests.rs", true)]),
            (
                "src/widget/tests.rs",
                vec![child("src/widget/tests/helpers.rs", false)],
            ),
        ]));
        assert_eq!(
            pair(&sources, "src/widget/tests/helpers.rs").as_deref(),
            Some("src/widget.rs")
        );
        assert_eq!(
            pair(&sources, "src/widget/tests.rs").as_deref(),
            Some("src/widget.rs")
        );
    }

    #[test]
    fn should_not_let_a_helper_inside_a_test_file_become_a_second_owner() {
        // helpers.rs is test code; its own cfg(test) child still belongs to widget.rs.
        let sources = test_sources(&tree(&[
            ("src/widget.rs", vec![child("src/widget/tests.rs", true)]),
            (
                "src/widget/tests.rs",
                vec![child("src/widget/tests/helpers.rs", false)],
            ),
            (
                "src/widget/tests/helpers.rs",
                vec![child("src/widget/tests/inner.rs", true)],
            ),
        ]));
        assert_eq!(
            pair(&sources, "src/widget/tests/inner.rs").as_deref(),
            Some("src/widget.rs")
        );
    }

    #[test]
    fn should_record_no_pair_for_a_test_file_two_owners_claim() {
        let sources = test_sources(&tree(&[
            ("src/a.rs", vec![child("src/shared/tests.rs", true)]),
            ("src/b.rs", vec![child("src/shared/tests.rs", true)]),
        ]));
        assert_eq!(pair(&sources, "src/shared/tests.rs"), None);
    }

    #[test]
    fn should_record_no_pair_for_a_test_file_a_production_module_also_declares() {
        let sources = test_sources(&tree(&[
            ("src/a.rs", vec![child("src/shared.rs", true)]),
            ("src/b.rs", vec![child("src/shared.rs", false)]),
        ]));
        assert_eq!(pair(&sources, "src/shared.rs"), None);
    }
}
