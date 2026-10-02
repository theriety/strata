//! Assertions over which files share, keep, or lose a container or home.

use std::collections::BTreeSet;

use strata_engine::result::ContainerNode;

use crate::harness::Verdict;
use crate::harness::inputs::{EvalInputs, find_package_node, packages_holding};
use crate::metrics;
use crate::target::{FaceMode, PathSetAssertion, PreserveDir, PreserveSymbolHome};

/// `preserve_symbol_home`: the named symbol still resides in the named file of
/// the asserted candidate tree. File identity is the full repo-relative path
/// (stable across trees per the contract), and symbol membership is read off
/// the file's `symbols` list — exactly what FIX08's symbol-grain relocation
/// rewrites, so this is the assertion that catches gratuitous home churn.
pub(super) fn evaluate_preserve_symbol_home(
    assertion: &PreserveSymbolHome,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("preserve_symbol_home({})#{face:?}", assertion.symbol);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut found_home = false;
    let mut now_in: Vec<String> = Vec::new();
    metrics::walk_files(tree, &mut |node| {
        let Some(symbols) = &node.symbols else {
            return;
        };
        let carries = symbols
            .iter()
            .any(|placement| placement.name == assertion.symbol);
        if carries && node.name == assertion.path {
            found_home = true;
        }
        if carries {
            now_in.push(node.name.clone());
        }
    });
    Verdict {
        label,
        passed: found_home,
        detail: if found_home {
            format!("{} still lives in {}", assertion.symbol, assertion.path)
        } else if now_in.is_empty() {
            format!(
                "{} does not appear in any file of the candidate tree",
                assertion.symbol
            )
        } else {
            format!(
                "{} left its pinned home {}: it now appears in {}",
                assertion.symbol,
                assertion.path,
                now_in.join(", ")
            )
        },
    }
}

/// `preserve_dir`: within each package that physically has the directory, some
/// folder/domain node named exactly `path` must still contain all of D(P, path).
pub(super) fn evaluate_preserve_dir(
    assertion: &PreserveDir,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("preserve_dir({})#{face:?}", assertion.path);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let holdings = packages_holding(&inputs.census, &inputs.packages, &assertion.path);
    if holdings.is_empty() {
        return Verdict {
            label,
            passed: false,
            detail: format!(
                "directory {:?} resolves under no analyzed package",
                assertion.path
            ),
        };
    }
    let mut missing_reports = Vec::new();
    for (package, expected) in &holdings {
        let satisfied = find_package_node(tree, &package.name).is_some_and(|package_node| {
            surviving_container(package_node, &assertion.path, expected)
        });
        if !satisfied {
            missing_reports.push(format!(
                "package {}: no folder/domain node named {:?} holds all of D = [{}]",
                package.name,
                assertion.path,
                expected.join(", ")
            ));
        }
    }
    Verdict {
        label,
        passed: missing_reports.is_empty(),
        detail: if missing_reports.is_empty() {
            format!(
                "directory {:?} survives as a named container in every holding package",
                assertion.path
            )
        } else {
            format!("directory tearing: {}", missing_reports.join("; "))
        },
    }
}

/// Whether some folder/domain descendant named exactly `path` contains every
/// file in `expected` anywhere in its subtree.
///
/// Unlike the counting predicates this is deliberately SUBTREE-scoped:
/// `preserve_dir` pins tearing, and tearing means expected files LEAVING the
/// directory's reach — a directory that keeps its files across nested
/// sub-places inside itself has not been torn, whatever the first-level split
/// beneath it looks like.
fn surviving_container(package_node: &ContainerNode, path: &str, expected: &[String]) -> bool {
    let expected_set: BTreeSet<&String> = expected.iter().collect();
    let mut found = false;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if node.name == path && metrics::is_scoped_container(node) {
                let member_list = metrics::members(node);
                let members: BTreeSet<&String> = member_list.iter().collect();
                if expected_set.is_subset(&members) {
                    found = true;
                }
            }
        };
        metrics::walk_containers(package_node, &mut visit);
    }
    found
}

/// `keep_together` / `separate`: co-location required, or co-location banned,
/// over folder/domain containers only. Membership is first-level: a container
/// holds exactly its direct file children, so nesting one place under another
/// never reads as the two places merging.
pub(super) fn evaluate_path_set(
    assertion: &PathSetAssertion,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
    keep: bool,
) -> Verdict {
    let kind = if keep { "keep_together" } else { "separate" };
    let label = format!("{kind}({})#{face:?}", assertion.paths.join("+"));
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let wanted: BTreeSet<&String> = assertion.paths.iter().collect();
    let mut offenders: Vec<String> = Vec::new();
    let mut satisfied = false;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if !metrics::is_scoped_container(node) {
                return;
            }
            let member_list = metrics::direct_members(node);
            let members: BTreeSet<&String> = member_list.iter().collect();
            let held: Vec<String> = wanted
                .intersection(&members)
                .map(|path| (*path).clone())
                .collect();
            if keep {
                if wanted.is_subset(&members) {
                    satisfied = true;
                }
            } else if held.len() >= 2 {
                offenders.push(format!(
                    "{:?} co-locates {}",
                    node.name,
                    held.join(" with ")
                ));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    if keep {
        Verdict {
            label,
            passed: satisfied,
            detail: if satisfied {
                "some folder/domain container co-locates the set".to_owned()
            } else {
                format!(
                    "no folder/domain container holds all of [{}]",
                    assertion.paths.join(", ")
                )
            },
        }
    } else {
        Verdict {
            label,
            passed: offenders.is_empty(),
            detail: if offenders.is_empty() {
                "no folder/domain container co-locates the set".to_owned()
            } else {
                format!("cross-directory welding: {}", offenders.join("; "))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::assertions::fixtures::{inputs_with, laminar_candidate, torn_candidate};
    use crate::harness::inputs::{find_package_node, packages_holding};
    use crate::target::FaceMode;

    use super::surviving_container;

    #[test]
    fn preserve_dir_requires_a_named_container_holding_every_member() {
        // The torn candidate has no `billing` container, so D(P, billing) =
        // [invoice, pricing] cannot be found under any folder/domain node.
        let torn = torn_candidate();
        let torn_inputs = inputs_with(&torn);
        let torn_packages = packages_holding(&torn_inputs.census, &torn_inputs.packages, "billing");
        assert_eq!(
            torn_packages.len(),
            1,
            "exactly the repository-rooted package holds billing/"
        );
        let anchored_tree = torn_inputs
            .faces
            .get(&FaceMode::Anchored)
            .map(|face| face.tree);
        let torn_satisfied = anchored_tree.is_some_and(|tree| {
            torn_packages.first().is_some_and(|(package, expected)| {
                find_package_node(tree, &package.name).is_some_and(|package_node| {
                    surviving_container(package_node, "billing", expected)
                })
            })
        });
        assert!(!torn_satisfied, "torn layout must dissolve billing");

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_packages =
            packages_holding(&laminar_inputs.census, &laminar_inputs.packages, "billing");
        let laminar_tree = laminar_inputs
            .faces
            .get(&FaceMode::Anchored)
            .map(|face| face.tree);
        let laminar_satisfied = laminar_tree.is_some_and(|tree| {
            laminar_packages.first().is_some_and(|(package, expected)| {
                find_package_node(tree, &package.name).is_some_and(|package_node| {
                    surviving_container(package_node, "billing", expected)
                })
            })
        });
        assert!(laminar_satisfied, "laminar layout keeps billing");
    }
}
