//! Cycle-cut assertions over the current layout's findings (ADR-35).
//!
//! A `cycle_import_cut` pin holds when some cycle finding spans every listed
//! file, and every such finding names an import cut: at least one import to
//! remove, each between two different files of the cycle and each locating
//! its statement. A finding that offers only symbol edges, same-file or not,
//! fails: removing a call inside one file never removes an import cycle (C1).
//! A pin with `expect` also requires the cut to name that import, and its
//! line when given.

use unlace_engine::result::{ImportCycleStatus, Violation, ViolationKind};

use crate::harness::Verdict;
use crate::target::{CycleImportCut, ExpectedImport};

/// One verdict per pin, read from the deduplicated findings of every profile.
pub(in crate::harness) fn evaluate_cycle_import_cuts(
    pins: &[CycleImportCut],
    findings: &[Violation],
) -> Vec<Verdict> {
    pins.iter()
        .map(|pin| {
            let spanning: Vec<&Violation> = findings
                .iter()
                .filter(|finding| finding.kind == ViolationKind::Cycle)
                .filter(|finding| pin.files.iter().all(|file| finding.location.contains(file)))
                .collect();
            let defects: Vec<String> = spanning
                .iter()
                .filter_map(|finding| {
                    cut_defect(finding).or_else(|| {
                        pin.expect
                            .as_ref()
                            .and_then(|expect| expect_defect(finding, expect))
                    })
                })
                .collect();
            let (passed, observed) = if spanning.is_empty() {
                (false, "no cycle finding spans every listed file".to_owned())
            } else if defects.is_empty() {
                (
                    true,
                    format!("{} cycle finding(s) name an import cut", spanning.len()),
                )
            } else {
                (false, defects.join("; "))
            };
            Verdict {
                label: label(pin),
                passed,
                detail: format!("{observed}; because {}", pin.because),
            }
        })
        .collect()
}

/// Why `finding` does not name an import cut, or `None` when it does.
fn cut_defect(finding: &Violation) -> Option<String> {
    let files = finding.location.join(", ");
    let Some(cycle) = &finding.import_cycle else {
        let symbol_cuts = finding.break_suggestions.as_ref().map_or(0, Vec::len);
        let same_file = finding
            .break_suggestions
            .iter()
            .flatten()
            .filter(|edge| edge.same_file)
            .count();
        return Some(format!(
            "cycle over [{files}] names no import cut ({symbol_cuts} symbol cut(s), {same_file} same-file)"
        ));
    };
    if cycle.status == ImportCycleStatus::Unavailable {
        return Some(format!(
            "cycle over [{files}] reports its cut unavailable (no import facts for {})",
            cycle.files_without_imports.join(", ")
        ));
    }
    if cycle.cut.is_empty() {
        return Some(format!("cycle over [{files}] names an empty import cut"));
    }
    cycle.cut.iter().find_map(|removed| {
        let crosses = removed.importer != removed.imported
            && finding.location.contains(&removed.importer)
            && finding.location.contains(&removed.imported);
        (!crosses || removed.statements.is_empty()).then(|| {
            format!(
                "cycle over [{files}] cuts {} -> {}, which is not a located import between two of its files",
                removed.importer, removed.imported
            )
        })
    })
}

/// Why `finding`'s cut does not name the `expect`ed import, or `None` when
/// it does.
fn expect_defect(finding: &Violation, expect: &ExpectedImport) -> Option<String> {
    let cut = finding.import_cycle.iter().flat_map(|cycle| &cycle.cut);
    let named = cut.clone().any(|removed| {
        removed.importer == expect.importer
            && removed.imported == expect.imported
            && expect.line.is_none_or(|line| {
                removed
                    .statements
                    .iter()
                    .any(|statement| statement.line == line)
            })
    });
    if named {
        return None;
    }
    let line = expect
        .line
        .map(|line| format!(" (line {line})"))
        .unwrap_or_default();
    let actual: Vec<String> = cut
        .map(|removed| {
            let lines: Vec<String> = removed
                .statements
                .iter()
                .map(|statement| statement.line.to_string())
                .collect();
            format!(
                "{} -> {} (line {})",
                removed.importer,
                removed.imported,
                lines.join(", ")
            )
        })
        .collect();
    Some(format!(
        "cycle over [{}] does not cut the expected {} -> {}{line}; it cuts {}",
        finding.location.join(", "),
        expect.importer,
        expect.imported,
        actual.join("; ")
    ))
}

/// A stable label like `cycle_import_cut(adapters.ts+build.ts)`.
fn label(pin: &CycleImportCut) -> String {
    let names: Vec<&str> = pin
        .files
        .iter()
        .map(|file| file.rsplit('/').next().unwrap_or(file))
        .collect();
    format!("cycle_import_cut({})", names.join("+"))
}

#[cfg(test)]
mod tests {
    use unlace_engine::result::{
        EdgeBreak, ImportBreak, ImportCycle, ImportCycleStatus, ImportStatement, Severity,
        Violation, ViolationKind,
    };

    use super::evaluate_cycle_import_cuts;
    use crate::target::{CycleImportCut, ExpectedImport};

    const ADAPTERS: &str = "src/parser/adapters.ts";
    const BUILD: &str = "src/parser/build.ts";

    fn pin() -> CycleImportCut {
        CycleImportCut {
            files: vec![ADAPTERS.to_owned(), BUILD.to_owned()],
            expect: None,
            because: "C1".to_owned(),
        }
    }

    /// A pin expecting `importer -> imported`, optionally at `line`.
    fn expecting(importer: &str, imported: &str, line: Option<u32>) -> CycleImportCut {
        CycleImportCut {
            expect: Some(ExpectedImport {
                importer: importer.to_owned(),
                imported: imported.to_owned(),
                line,
            }),
            ..pin()
        }
    }

    /// The mdc C1 cycle cut at `adapters.ts`'s `./build` import on line 7.
    fn cut_at_build() -> Violation {
        cycle(
            Some(ImportCycle {
                status: ImportCycleStatus::Cut,
                cut: vec![ImportBreak {
                    importer: ADAPTERS.to_owned(),
                    imported: BUILD.to_owned(),
                    statements: vec![ImportStatement {
                        line: 7,
                        specifier: "./build".to_owned(),
                    }],
                    references: Vec::new(),
                    weight: 1,
                }],
                exact: true,
                files_without_imports: Vec::new(),
            }),
            None,
        )
    }

    #[test]
    fn should_pass_an_expect_pin_the_cut_names_with_its_line() {
        let verdicts =
            evaluate_cycle_import_cuts(&[expecting(ADAPTERS, BUILD, Some(7))], &[cut_at_build()]);

        assert!(
            verdicts.iter().all(|verdict| verdict.passed),
            "{verdicts:?}"
        );
    }

    #[test]
    fn should_fail_an_expect_pin_on_the_other_import_or_another_line() {
        let reversed =
            evaluate_cycle_import_cuts(&[expecting(BUILD, ADAPTERS, None)], &[cut_at_build()]);
        let moved =
            evaluate_cycle_import_cuts(&[expecting(ADAPTERS, BUILD, Some(8))], &[cut_at_build()]);

        assert!(reversed.first().is_some_and(|verdict| {
            !verdict.passed
                && verdict.detail.contains(
                    "does not cut the expected src/parser/build.ts -> src/parser/adapters.ts; \
                     it cuts src/parser/adapters.ts -> src/parser/build.ts (line 7)",
                )
        }));
        assert!(
            moved
                .first()
                .is_some_and(|verdict| { !verdict.passed && verdict.detail.contains("(line 8)") })
        );
    }

    fn cycle(import_cycle: Option<ImportCycle>, breaks: Option<Vec<EdgeBreak>>) -> Violation {
        Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec![ADAPTERS.to_owned(), BUILD.to_owned()],
            detail: String::new(),
            break_suggestions: breaks,
            import_cycle,
            capacity: None,
        }
    }

    #[test]
    fn should_fail_a_same_file_only_symbol_cut_as_released_in_1_0_0() {
        let released = cycle(
            None,
            Some(vec![EdgeBreak {
                source: "applyAdapters".to_owned(),
                target: "recognizeAdapter".to_owned(),
                weight: 1.0,
                exact: true,
                same_file: true,
            }]),
        );

        let verdicts = evaluate_cycle_import_cuts(&[pin()], &[released]);

        assert_eq!(verdicts.len(), 1);
        let verdict = verdicts.first();
        assert!(verdict.is_some_and(|verdict| !verdict.passed));
        assert!(
            verdict.is_some_and(|verdict| verdict.detail.contains("1 symbol cut(s), 1 same-file"))
        );
        assert!(
            verdict
                .is_some_and(|verdict| verdict.label == "cycle_import_cut(adapters.ts+build.ts)")
        );
    }

    #[test]
    fn should_pass_a_cycle_that_names_a_located_cross_file_import() {
        let named = cycle(
            Some(ImportCycle {
                status: ImportCycleStatus::Cut,
                cut: vec![ImportBreak {
                    importer: ADAPTERS.to_owned(),
                    imported: BUILD.to_owned(),
                    statements: vec![ImportStatement {
                        line: 7,
                        specifier: "./build".to_owned(),
                    }],
                    references: Vec::new(),
                    weight: 1,
                }],
                exact: true,
                files_without_imports: Vec::new(),
            }),
            None,
        );

        let verdicts = evaluate_cycle_import_cuts(&[pin()], &[named]);

        assert!(
            verdicts.iter().all(|verdict| verdict.passed),
            "{verdicts:?}"
        );
    }

    #[test]
    fn should_fail_when_no_cycle_spans_the_files_or_the_cut_is_unavailable() {
        let unavailable = cycle(
            Some(ImportCycle {
                status: ImportCycleStatus::Unavailable,
                cut: Vec::new(),
                exact: false,
                files_without_imports: vec![BUILD.to_owned()],
            }),
            None,
        );

        let none = evaluate_cycle_import_cuts(&[pin()], &[]);
        let blind = evaluate_cycle_import_cuts(&[pin()], &[unavailable]);

        assert!(none.first().is_some_and(|verdict| {
            !verdict.passed && verdict.detail.starts_with("no cycle finding spans")
        }));
        assert!(
            blind.first().is_some_and(|verdict| {
                !verdict.passed && verdict.detail.contains("unavailable")
            })
        );
    }
}
