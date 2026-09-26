//! Sink-independent rendering of an [`AnalyzeResult`] into human-readable text.
//!
//! Rendering is a pure function of the result and an output sink: the same result
//! always renders the same bytes, and `--format json` bypasses this module
//! entirely (the JSON path serializes the library value verbatim, the AD-5
//! parity guarantee). Terminal and Markdown share deterministic report content:
//! project metadata, structural findings, candidate layouts, and qualified advice.
//! Candidates include exact actions and paired affected-branch trees. Human text
//! uses no terminal escapes and wraps losslessly within 100 display columns.

mod advice;
mod changes;
mod diff;
mod findings;
mod report;
mod tree;

use std::io::{self, Write};

use strata_engine::{AnalyzeResult, ModeResult, ProfileConfig, ScoreBreakdown};

pub use diff::render_diff;
pub use findings::write_violation_table;
pub use tree::render_tree;

#[cfg(test)]
use advice::caption_for;
#[cfg(test)]
use diff::symbol_move_line;
#[cfg(test)]
use findings::cycle_finding_text;

/// The output format the `analyze` command renders in.
///
/// `Summary` is the human-readable printed-report face; `Json` is the serialized
/// library result, the AD-5 parity face. The `violations` command carries its
/// own table face so the two surfaces never share a variant they do not both
/// use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The default human-readable printed report.
    Summary,
    /// The serialized `AnalyzeResult` JSON.
    Json,
}

impl Format {
    /// Resolves the effective format from an optional explicit choice.
    ///
    /// An explicit choice wins; otherwise every sink uses `default_human`.
    /// The terminal argument is retained for API compatibility and is ignored.
    #[must_use]
    pub fn resolve(explicit: Option<Format>, _is_terminal: bool, default_human: Format) -> Format {
        explicit.unwrap_or(default_human)
    }
}

/// Renders the analysis `result` in `format` to `out`.
///
/// The JSON face is the parity-pinned serialization of the library result; the
/// summary face presents findings, candidate layouts, and qualified advice.
/// `project` names the saved analyzed repository and banners the report.
pub fn render(
    result: &AnalyzeResult,
    format: Format,
    project: &str,
    out: &mut impl Write,
) -> io::Result<()> {
    render_with_options(result, format, project, RenderOptions::default(), out)
}

/// Human-readable report detail. JSON serialization ignores these options.
#[derive(Debug, Default, Clone, Copy)]
pub struct RenderOptions {
    /// Includes numerical evidence, score components, and effective parameters.
    pub verbose: bool,
}

/// Renders an analysis with explicit human-readable detail options.
///
/// # Errors
/// Returns the output sink's I/O error.
pub fn render_with_options(
    result: &AnalyzeResult,
    format: Format,
    project: &str,
    options: RenderOptions,
    out: &mut impl Write,
) -> io::Result<()> {
    match format {
        Format::Json => write_json(result, out),
        Format::Summary => report::Report::build(result, project, options).write_text(out),
    }
}

/// Renders the same report content as Markdown, using the saved project name.
pub(crate) fn markdown(result: &AnalyzeResult, options: RenderOptions) -> String {
    report::Report::build(result, &result.current.tree.name, options).markdown()
}

/// Serializes `result` as pretty JSON to `out`.
///
/// # Errors
///
/// Returns an [`io::Error`] if serialization or writing fails.
pub fn write_json(result: &AnalyzeResult, out: &mut impl Write) -> io::Result<()> {
    let json = serde_json::to_vec(result).map_err(io::Error::other)?;
    out.write_all(&json)?;
    out.write_all(b"\n")
}

/// Formats a score figure with four decimals.
fn f4(value: f64) -> String {
    format!("{value:.4}")
}

/// Formats a signed figure with four decimals and an explicit sign.
fn sf(value: f64) -> String {
    if value >= 0.0 {
        format!("+{value:.4}")
    } else {
        format!("-{:.4}", value.abs())
    }
}

/// Describes a candidate's score change without calling a regression a gain.
fn improvement_summary(improvement: f64) -> String {
    if improvement > 0.0 {
        format!("gain {}", sf(improvement))
    } else if improvement < 0.0 {
        format!("regression {}", f4(improvement.abs()))
    } else {
        "no improvement".to_owned()
    }
}

/// Formats a pull weight: at least one decimal, trailing zeros trimmed.
fn weight(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

/// Quotes a file, folder, or symbol name in backticks — every name token in
/// prose, titles, captions, and table cells carries them.
fn nq(name: &str) -> String {
    format!("`{name}`")
}

/// Wraps `text` on spaces to a 99-column budget.
///
/// `first_pad` indents the first line, `cont_pad` every continuation; both are
/// counted against the budget, mirroring the approved artifact's geometry.
/// Original separator whitespace is retained at line ends, including whitespace
/// inside source names containing literal backticks.
fn wrap(text: &str, first_pad: usize, cont_pad: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut indent = " ".repeat(first_pad);
    for word in text.split_inclusive(char::is_whitespace) {
        let grown = cur.chars().count() + word.chars().count();
        if !cur.is_empty() && indent.chars().count() + grown > 99 {
            lines.push(format!("{indent}{cur}"));
            word.clone_into(&mut cur);
            indent = " ".repeat(cont_pad);
        } else {
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(format!("{indent}{cur}"));
    }
    lines
}

/// The modes present in `result`, in their fixed report order.
fn present_modes(result: &AnalyzeResult) -> Vec<(&'static str, &ModeResult)> {
    let mut modes = Vec::new();
    if let Some(mode) = &result.profiles.anchored {
        modes.push(("anchored", mode));
    }
    if let Some(mode) = &result.profiles.greenfield {
        modes.push(("greenfield", mode));
    }
    modes
}

/// Renders every effective value in one selected parameter profile.
pub(crate) fn effective_parameter_lines(name: &str, p: &ProfileConfig) -> Vec<String> {
    vec![
        format!(" {name} parameter profile (effective):"),
        format!("   search    candidates {} · seed {}", p.candidates, p.seed),
        format!(
            "   capacity  file {} · folder {} · domain {} · package {} · package-group {}",
            p.capacity.file,
            p.capacity.folder,
            p.capacity.domain,
            p.capacity.package,
            p.capacity.package_group
        ),
        format!(
            "   objective imbalance {} · naming {} · path {} · anchor {} · capacity {}",
            weight(p.objective.imbalance),
            weight(p.objective.naming),
            weight(p.objective.path),
            weight(p.objective.anchor),
            weight(p.objective.capacity)
        ),
        format!(
            "             dependency-only {} · companion-separation {}",
            weight(p.objective.dependency_only),
            weight(p.objective.companion_separation)
        ),
        format!(
            "   weights   value-import {} · inheritance {} · call {} · type-reference {} · re-export {}",
            weight(p.weights.value_import),
            weight(p.weights.inheritance),
            weight(p.weights.call),
            weight(p.weights.type_reference),
            weight(p.weights.re_export)
        ),
        format!(
            "             same-file-symbol {} · same-file-type {}",
            weight(p.weights.same_file_symbol),
            weight(p.weights.same_file_type)
        ),
        format!(
            "   solver    ilp-threshold {} · timeout-seconds {}",
            p.solver.ilp_threshold, p.solver.timeout_seconds
        ),
        format!(
            "   diversity seeds-per-candidate {} · score-tolerance {} · min-distance {}",
            p.diversity.seeds_per_candidate,
            weight(p.diversity.score_tolerance),
            weight(p.diversity.min_distance)
        ),
        format!(
            "   tests     helper-cap {} · patterns {} · builtins {}",
            p.tests.helper_cap,
            p.tests.patterns.len(),
            p.tests.builtins
        ),
        format!(
            "   relocation pin-test-files {} · pin-test-symbols {} · cross-package {} · file patterns {} · symbol patterns {}",
            p.relocation.pin_detected_test_files,
            p.relocation.pin_detected_test_symbols,
            p.relocation.allow_cross_package_moves,
            p.relocation.forbid_file_moves.len(),
            p.relocation.forbid_symbol_moves.len()
        ),
        format!(
            "              mirroring enabled {} · builtins {} · rules {}",
            p.relocation.test_mirroring.enabled,
            p.relocation.test_mirroring.builtins,
            p.relocation.test_mirroring.rules.len()
        ),
        format!(
            "   qualification evidence {} · structural {} · ambiguity-margin {}",
            weight(p.qualification.minimum_evidence),
            weight(p.qualification.minimum_structural),
            weight(p.qualification.minimum_ambiguity_margin)
        ),
        format!(
            "                 owner {} · role {} · source {} · destination {}",
            weight(p.qualification.weights.unique_owner),
            weight(p.qualification.weights.role_affinity),
            weight(p.qualification.weights.source_cohesion),
            weight(p.qualification.weights.destination_cohesion)
        ),
        format!(
            "                 producer {} · architectural-reach {}",
            weight(p.qualification.weights.producer_evidence),
            weight(p.qualification.weights.architectural_reach)
        ),
    ]
}

/// Accessor for one scored term of a breakdown.
type Term = fn(&ScoreBreakdown) -> f64;

/// The scored terms that always print, in their fixed table order.
const TERMS: [(&str, Term); 7] = [
    ("cut", |breakdown| breakdown.cut),
    ("imbalance", |breakdown| breakdown.imbalance),
    ("naming", |breakdown| breakdown.naming),
    ("path", |breakdown| breakdown.path),
    ("anchor", |breakdown| breakdown.anchor),
    ("dependency-only", |breakdown| breakdown.dependency_only),
    ("companion-separation", |breakdown| {
        breakdown.companion_separation
    }),
];

#[cfg(test)]
fn report_lines(result: &AnalyzeResult, project: &str) -> Vec<String> {
    report::Report::build(result, project, RenderOptions::default()).lines()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use strata_engine::{
        Candidate, CapacityRemainder, ContainerNode, CurrentStanding, CurrentTree, EdgeBreak,
        FileMove, Level, Modes, Move, MoveKind, MoveReason, ProfileConfig, ProfileCurrent,
        ScoreBreakdown, Severity, Summary, SymbolKind, SymbolMove, SymbolPlacement, Violation,
        ViolationKind,
    };

    use super::*;

    /// Builds an empty-but-valid result with `violations` on its current tree.
    /// `capacity_breaks` mirrors the engine's own predicate so synthetic results
    /// stay internally consistent; a test that needs the mismatch overrides it.
    fn result_with(violations: Vec<Violation>) -> AnalyzeResult {
        let hard = violations
            .iter()
            .filter(|violation| {
                violation.kind == ViolationKind::Capacity
                    && violation.severity == Severity::Violation
            })
            .count();
        let capacity_breaks = u32::try_from(hard).unwrap_or(u32::MAX);
        AnalyzeResult {
            schema_version: 1,
            snapshot_hash: "deadbeef".to_owned(),
            summary: Summary {
                symbols: 2,
                edges: 1,
                files: 4,
                files_by_language: BTreeMap::from([("ts".to_owned(), 4)]),
            },
            current: CurrentTree {
                tree: file_node("lib", 2),
                shared_findings: Vec::new(),
            },
            advice: strata_engine::Advice::default(),
            profiles: Modes {
                anchored: Some(ModeResult {
                    parameters: ProfileConfig::default(),
                    current: ProfileCurrent {
                        score: 1.5,
                        score_breakdown: zero_breakdown(),
                        unique_findings: violations,
                        standing: CurrentStanding::Outscored,
                        capacity_breaks,
                    },
                    candidates: Vec::new(),
                    pairwise_distance: Vec::new(),
                    solution_space_converged: false,
                }),
                greenfield: None,
            },
        }
    }

    /// Builds a result whose anchored mode carries one candidate whose tree has a
    /// `proposed` container, for exercising the candidate faces.
    fn result_with_candidate() -> AnalyzeResult {
        let mut result = result_with(Vec::new());
        result.profiles = Modes {
            anchored: Some(ModeResult {
                parameters: ProfileConfig::default(),
                current: ProfileCurrent {
                    score: 1.5,
                    score_breakdown: zero_breakdown(),
                    unique_findings: Vec::new(),
                    standing: CurrentStanding::Outscored,
                    capacity_breaks: 0,
                },
                candidates: vec![Candidate {
                    index: 1,
                    score: 0.25,
                    score_breakdown: zero_breakdown(),
                    improvement: 1.25,
                    tree: ContainerNode {
                        name: "proposed".to_owned(),
                        level: Level::Folder,
                        children: Some(vec![file_node("lib", 1)]),
                        symbols: None,
                        production_sloc: None,
                    },
                    conditional_splits: Vec::new(),
                    delta_narration: Vec::new(),
                    symbol_moves: Vec::new(),
                    capacity_remainder: None,
                }],
                pairwise_distance: Vec::new(),
                solution_space_converged: true,
            }),
            greenfield: None,
        };
        result
    }

    #[test]
    fn should_follow_combined_multi_package_moves_in_terminal()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut result = result_with_candidate();
        super::combined_package_moves::configure(&mut result)?;
        let mut buffer = Vec::new();

        render(&result, Format::Summary, "workspace", &mut buffer)?;

        super::combined_package_moves::assert_paths(&String::from_utf8(buffer)?)?;
        Ok(())
    }

    /// Builds a zeroed score breakdown.
    fn zero_breakdown() -> ScoreBreakdown {
        ScoreBreakdown {
            cut: 0.0,
            imbalance: 0.0,
            naming: 0.0,
            path: 0.0,
            anchor: 0.0,
            dependency_only: 0.0,
            companion_separation: 0.0,
            capacity: 0.0,
        }
    }

    /// Builds a file container node with `sloc` symbols.
    fn file_node(name: &str, sloc: u32) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(vec![SymbolPlacement {
                name: "thing".to_owned(),
                visibility: Level::File,
            }]),
            production_sloc: Some(sloc),
        }
    }

    /// Builds a sample cycle violation.
    fn cycle() -> Violation {
        Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["a".to_owned(), "b".to_owned()],
            detail: "dependency cycle".to_owned(),
            break_suggestions: None,
            capacity: None,
        }
    }

    /// Builds a capacity violation for `name`.
    fn capacity_violation(name: &str) -> Violation {
        Violation {
            kind: ViolationKind::Capacity,
            severity: Severity::Violation,
            location: vec![name.to_owned()],
            detail: format!("{name} over cap"),
            break_suggestions: None,
            capacity: None,
        }
    }

    /// Builds a move of one or more files sharing one origin.
    fn move_of(kind: MoveKind, paths: &[&str], from: &str, to: &str, reason: MoveReason) -> Move {
        Move {
            kind,
            files: paths
                .iter()
                .map(|path| FileMove {
                    path: (*path).to_owned(),
                    from: from.to_owned(),
                })
                .collect(),
            to: to.to_owned(),
            reason,
            mirrors: Vec::new(),
            blocked_mirrors: Vec::new(),
        }
    }

    #[test]
    fn should_resolve_to_summary_when_piped_without_an_explicit_format() {
        assert_eq!(
            Format::resolve(None, false, Format::Summary),
            Format::Summary
        );
    }

    #[test]
    fn should_resolve_to_the_human_face_on_a_terminal() {
        assert_eq!(
            Format::resolve(None, true, Format::Summary),
            Format::Summary
        );
    }

    #[test]
    fn should_honour_an_explicit_format_over_the_tty_default() {
        assert_eq!(
            Format::resolve(Some(Format::Json), true, Format::Summary),
            Format::Json
        );
    }

    #[test]
    fn should_render_json_byte_identically_to_the_serialized_result() {
        let result = result_with(Vec::new());
        let mut buffer = Vec::new();

        write_json(&result, &mut buffer).unwrap_or_default();

        let mut expected = serde_json::to_vec(&result).unwrap_or_default();
        expected.push(b'\n');
        assert_eq!(buffer, expected);
    }

    #[test]
    fn should_report_no_violations_when_the_list_is_empty() {
        let mut buffer = Vec::new();

        write_violation_table(&[], &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no violations\n");
    }

    #[test]
    fn should_tabulate_a_violation_with_its_kind_and_severity() {
        let mut buffer = Vec::new();

        write_violation_table(&[cycle()], &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "cycle [violation] a, b :: dependency cycle\n");
    }

    #[test]
    fn should_render_a_tree_with_symbols_and_visibility() {
        let mut buffer = Vec::new();

        render_tree(&file_node("lib", 1), true, None, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "lib [file] 1 sloc\n  - thing (file)\n");
    }

    #[test]
    fn should_truncate_a_tree_below_the_requested_depth() {
        let root = ContainerNode {
            name: "root".to_owned(),
            level: Level::Folder,
            children: Some(vec![file_node("child", 1)]),
            symbols: None,
            production_sloc: None,
        };
        let mut buffer = Vec::new();

        render_tree(&root, false, Some(0), &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(text, "root [folder]\n");
    }

    #[test]
    fn should_quote_every_name_token_in_backticks() {
        assert_eq!(nq("logger.ts"), "`logger.ts`");
        assert_eq!(nq("io/util"), "`io/util`");
    }

    #[test]
    fn should_wrap_prose_within_the_grid_with_continuation_indents() {
        let long_text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi \
                         omicron pi rho sigma tau upsilon phi chi psi omega";
        let lines = wrap(long_text, 3, 7);

        assert!(lines.len() >= 2, "long prose wraps: {lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 99, "wrapped line fits: {line}");
        }
        assert!(
            lines.first().is_some_and(|first| first.starts_with("   ")),
            "first pad applies: {lines:?}"
        );
        assert!(
            lines
                .get(1)
                .is_some_and(|second| second.starts_with("       ")),
            "continuation pad applies"
        );
    }

    #[test]
    fn should_trim_weights_to_one_meaningful_decimal() {
        assert_eq!(weight(1.0), "1.0");
        assert_eq!(weight(7.8), "7.8");
        assert_eq!(weight(0.25), "0.25");
        assert_eq!(weight(6.0), "6.0");
    }

    #[test]
    fn should_sign_deltas_explicitly() {
        assert_eq!(sf(2.4194), "+2.4194");
        assert_eq!(sf(-0.0041), "-0.0041");
        // Halfway values follow binary-float formatting (round-half-to-even on
        // the stored value); pin the unambiguous neighbors instead.
        assert_eq!(f4(10.23876), "10.2388");
        assert_eq!(f4(10.23874), "10.2387");
    }

    #[test]
    fn should_caption_each_reason_in_the_approved_voice() {
        assert_eq!(
            caption_for(&MoveReason::PulledBy {
                partner: "src/core/engine.ts".to_owned(),
                weight: 1.0,
            }),
            "Why: pulled toward `src/core/engine.ts` — weight 1.0."
        );
        assert_eq!(
            caption_for(&MoveReason::RelievesOverCap {
                container: "ai/adapters".to_owned(),
                count: 33,
                cap: 20,
            }),
            "Why: relieves `ai/adapters`, which holds 33 against a cap of 20."
        );
        assert_eq!(
            caption_for(&MoveReason::Follows {
                subject: "spec/log.ts".to_owned(),
            }),
            "Why: follows `spec/log.ts`, which this plan places nearby."
        );
        assert_eq!(
            caption_for(&MoveReason::Clustering),
            "Why: clusters files that already import each other heavily."
        );
    }

    #[test]
    fn should_rebuild_cycle_findings_with_quoted_symbols_and_priced_cuts() {
        let violation = Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["alpha".to_owned(), "beta".to_owned()],
            detail: "2-symbol cycle".to_owned(),
            break_suggestions: Some(vec![
                EdgeBreak {
                    source: "beta".to_owned(),
                    target: "alpha".to_owned(),
                    weight: 1.0,
                    exact: true,
                },
                EdgeBreak {
                    source: "alpha".to_owned(),
                    target: "beta".to_owned(),
                    weight: 2.0,
                    exact: false,
                },
            ]),
            capacity: None,
        };

        assert_eq!(
            cycle_finding_text(&violation),
            "`alpha`/`beta` — 2-symbol cycle; break `beta` -> `alpha` (w=1.0, exact), +1 more"
        );
    }

    #[test]
    fn should_explain_cycle_location_and_placement_consequence() {
        let violation = Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["src/core/left.ts".to_owned(), "src/core/right.ts".to_owned()],
            detail: "`left`/`right` form one placement unit and must remain in one file unless a suggested dependency edge is broken".to_owned(),
            break_suggestions: Some(vec![EdgeBreak {
                source: "right".to_owned(),
                target: "left".to_owned(),
                weight: 1.0,
                exact: true,
            }]),
            capacity: None,
        };

        let rendered = cycle_finding_text(&violation);

        assert!(rendered.contains("src/core/left.ts"));
        assert!(rendered.contains("src/core/right.ts"));
        assert!(rendered.contains("one placement unit"));
        assert!(rendered.contains("must remain in one file"));
        assert!(rendered.contains("break `right` -> `left` (w=1.0, exact)"));
    }

    #[test]
    fn should_render_the_cheapest_cycle_cut_once() {
        let violation = Violation {
            kind: ViolationKind::Cycle,
            severity: Severity::Violation,
            location: vec!["left.ts".to_owned(), "right.ts".to_owned()],
            detail: "2-symbol cycle; symbols must remain in one file; break right -> left (w=1.0, exact)".to_owned(),
            break_suggestions: Some(vec![EdgeBreak {
                source: "right".to_owned(),
                target: "left".to_owned(),
                weight: 1.0,
                exact: true,
            }]),
            capacity: None,
        };

        let rendered = cycle_finding_text(&violation);

        assert_eq!(rendered.matches("break ").count(), 1, "{rendered}");
    }

    #[test]
    fn should_describe_nonzero_greenfield_coefficients_without_fixed_zero_claims() {
        let mut result = result_with_candidate();
        let mut greenfield = result
            .profiles
            .anchored
            .clone()
            .unwrap_or_else(|| unreachable!());
        greenfield.parameters.objective.path = 0.7;
        greenfield.parameters.objective.anchor = 0.8;
        result.profiles.greenfield = Some(greenfield);
        let mut output = Vec::new();

        let rendered_ok = render_with_options(
            &result,
            Format::Summary,
            "fixture",
            RenderOptions { verbose: true },
            &mut output,
        )
        .is_ok();
        let rendered = String::from_utf8_lossy(&output);

        assert!(rendered_ok);
        assert!(rendered.contains("path 0.7 · anchor 0.8"), "{rendered}");
        assert!(!rendered.contains("drops anchor credit"), "{rendered}");
        assert!(
            rendered.contains("anchored — candidate 1")
                && rendered.contains("greenfield — candidate 1"),
            "{rendered}"
        );
    }

    #[test]
    fn should_prefix_type_moves_without_prefixing_runtime_moves() {
        let make = |kind| SymbolMove {
            symbol: "WorkInput".to_owned(),
            kind,
            from_path: "src/task/run.rs".to_owned(),
            to_path: "src/task/types.rs".to_owned(),
            delta: -0.002,
            broken_imports: 4,
        };

        assert!(symbol_move_line(&make(SymbolKind::Type)).starts_with(" - move type `WorkInput`"));
        assert!(symbol_move_line(&make(SymbolKind::Symbol)).starts_with(" - move `WorkInput`"));
    }

    #[test]
    fn should_render_every_effective_parameter_group_for_a_profile() {
        let rendered = effective_parameter_lines("anchored", &ProfileConfig::default()).join("\n");

        for expected in [
            "anchored parameter profile (effective)",
            "search    candidates",
            "capacity  file",
            "objective imbalance",
            "weights   value-import",
            "same-file-symbol",
            "same-file-type",
            "solver    ilp-threshold",
            "diversity seeds-per-candidate",
            "tests     helper-cap",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
        assert_eq!(
            rendered.matches("dependency-only").count(),
            1,
            "the objective exposes dependency-only exactly once: {rendered}"
        );
        assert_eq!(
            rendered.matches("companion-separation").count(),
            1,
            "the objective exposes companion-separation exactly once: {rendered}"
        );
    }

    #[test]
    fn should_print_the_claimed_candidate_count_and_list_every_candidate() {
        let text = report_lines(&result_with_candidate(), "fixture").join("\n");
        assert!(text.contains("anchored — 1 candidate(s)"));
        assert_eq!(text.matches("anchored — candidate 1").count(), 1);
    }

    #[test]
    fn should_show_each_profile_with_its_own_baseline() {
        let mut result = result_with_candidate();
        result.profiles.greenfield = Some(ModeResult {
            parameters: ProfileConfig::greenfield(),
            current: ProfileCurrent {
                score: 1.75,
                score_breakdown: zero_breakdown(),
                unique_findings: Vec::new(),
                standing: CurrentStanding::Outscored,
                capacity_breaks: 0,
            },
            candidates: vec![Candidate {
                index: 1,
                score: 0.5,
                score_breakdown: zero_breakdown(),
                improvement: 1.0,
                tree: ContainerNode {
                    name: "rebuilt".to_owned(),
                    level: Level::Folder,
                    children: Some(vec![file_node("lib", 1)]),
                    symbols: None,
                    production_sloc: None,
                },
                conditional_splits: Vec::new(),
                delta_narration: Vec::new(),
                symbol_moves: Vec::new(),
                capacity_remainder: None,
            }],
            pairwise_distance: Vec::new(),
            solution_space_converged: false,
        });

        let lines = report_lines(&result, "fixture");

        let text = lines.join("\n");
        assert!(text.contains("anchored — candidate 1"));
        assert!(text.contains("greenfield — candidate 1"));
        assert!(text.contains("Score: 1.5000 → 0.2500"));
        assert!(text.contains("Score: 1.7500 → 0.5000"));
    }

    #[test]
    fn should_not_claim_two_gains_when_both_profiles_have_no_candidates() {
        let mut result = result_with_candidate();
        if let Some(anchored) = result.profiles.anchored.as_mut() {
            anchored.candidates.clear();
        }
        result.profiles.greenfield = result.profiles.anchored.clone();

        let text = report_lines(&result, "fixture").join("\n");

        assert!(!text.contains("these two gains"), "{text}");
        assert!(text.contains("No candidates were produced."), "{text}");
    }

    #[test]
    fn should_label_grain_on_every_suggestion_block() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
        }

        let lines = report_lines(&result, "fixture");

        let grains = lines
            .iter()
            .filter(|line| line.contains("- Move file"))
            .count();
        assert_eq!(grains, 1, "each suggestion states its grain: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("- Move file `logger.ts`"))
        );
    }

    #[test]
    fn should_keep_every_report_line_inside_the_hundred_column_grid() {
        let mut result = result_with_candidate();
        result.summary.files = 900;
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Merge,
                &["a-very-long-filename-indeed.spec.integration.ts", "b.ts"],
                "src/deeply/nested/adapters/anthropic/dispatch/openrouter",
                "src/equally/deeply/nested/generators/synthographers/adapters/openrouter",
                MoveReason::PulledBy {
                    partner: "src/deeply/nested/generators/synthographers/helpers.ts".to_owned(),
                    weight: 7.8,
                },
            )];
        }

        let lines = report_lines(&result, "fixture");

        for line in &lines {
            assert!(
                line.chars().count() <= 100,
                "line crosses the grid ({} cols): {line}",
                line.chars().count()
            );
        }
        let joined = lines.join("").replace(' ', "");
        assert!(
            joined.contains(
                "src/equally/deeply/nested/generators/synthographers/adapters/openrouter"
            )
        );
    }

    #[test]
    fn should_state_no_change_when_the_best_candidate_matches_today() {
        let text = report_lines(&result_with_candidate(), "fixture").join("\n");
        assert!(text.contains("No moves versus the current layout."));
        assert!(text.contains("No files enter or leave a folder."));
        assert!(text.contains("No affected branches."));
    }

    #[test]
    fn should_not_state_no_change_when_only_symbols_move() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|profile| profile.candidates.first_mut())
        {
            candidate.symbol_moves = vec![SymbolMove {
                symbol: "helper".to_owned(),
                kind: SymbolKind::Symbol,
                from_path: "src/a.ts".to_owned(),
                to_path: "src/b.ts".to_owned(),
                delta: -0.1,
                broken_imports: 0,
            }];
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            !text.contains("No moves versus the current layout."),
            "{text}"
        );
        assert!(text.contains("helper"), "{text}");
    }

    #[test]
    fn should_show_each_component_delta_without_instructing_adoption() {
        let mut result = result_with_candidate();
        if let Some(mode) = result.profiles.anchored.as_mut() {
            mode.current.score_breakdown.imbalance = 1.0;
            mode.current.score_breakdown.cut = 0.1000;
        }
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
            candidate.score_breakdown.imbalance = 0.5;
            candidate.score_breakdown.cut = 0.0999;
        }
        result
            .profiles
            .greenfield
            .clone_from(&result.profiles.anchored);

        let text = report::Report::build(&result, "fixture", RenderOptions { verbose: true })
            .lines()
            .join("\n");
        assert!(text.contains("-0.5000"));
        assert!(text.contains("-0.0001"));
        assert!(text.contains("Partial plans are not"));
        assert!(!text.contains("apply §2"));
    }

    #[test]
    fn should_show_capacity_breaches_and_candidate_remainder() {
        let mut result = result_with_candidate();
        result.current.shared_findings = vec![
            capacity_violation("big_folder"),
            capacity_violation("huge_file"),
        ];
        // the helper seeded the field before these violations existed
        if let Some(mode) = result.profiles.anchored.as_mut() {
            mode.current.capacity_breaks = 2;
            mode.current.standing = CurrentStanding::Infeasible;
        }
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.capacity_remainder = Some(CapacityRemainder {
                remaining: 1,
                file_level: 0,
            });
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(text.contains("Current layout violates 2 capacity cap(s)."));
        assert!(text.contains("Capacity remaining: 1 (0 at file level)."));
    }

    #[test]
    fn should_not_recommend_or_call_a_negative_improvement_a_gain() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|profile| profile.candidates.first_mut())
        {
            candidate.score = 2.0;
            candidate.improvement = -0.5;
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["worker.ts"],
                "src/feature",
                "src/shared",
                MoveReason::Clustering,
            )];
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(!text.contains(" adopt      candidate"), "{text}");
        assert!(!text.contains("gain -0.5000"), "{text}");
        assert!(text.contains("improvement -0.5000"), "{text}");
        assert!(!text.contains("adopt"), "{text}");
    }

    #[test]
    fn should_keep_shared_findings_once_and_each_profiles_capacity_status() {
        let mut result = result_with_candidate();
        result.profiles.greenfield = result.profiles.anchored.clone();
        result.current.shared_findings = vec![capacity_violation("big_folder")];
        // the helper seeded the field before this violation existed
        if let Some(mode) = result.profiles.anchored.as_mut() {
            mode.current.capacity_breaks = 1;
            mode.current.standing = CurrentStanding::Infeasible;
        }
        if let Some(mode) = result.profiles.greenfield.as_mut() {
            mode.current.capacity_breaks = 1;
            mode.current.standing = CurrentStanding::Infeasible;
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert_eq!(
            text.matches("Current layout violates 1 capacity cap(s).")
                .count(),
            2
        );
        assert_eq!(text.matches("Shared findings (1)").count(), 1);
    }

    #[test]
    fn should_count_only_hard_capacity_findings_as_breaks() {
        let mut result = result_with_candidate();
        // Two hard breaches plus a borderline observation: the note counts the
        // breaks from the engine's `capacity_breaks`, while Structural findings
        // lists all three findings with their severity tags.
        result.current.shared_findings = vec![
            capacity_violation("big_folder"),
            capacity_violation("huge_file"),
            {
                let mut borderline = capacity_violation("warm_folder");
                borderline.severity = Severity::Borderline;
                borderline
            },
        ];
        // what the engine reports for exactly this shape (borderline excluded)
        if let Some(mode) = result.profiles.anchored.as_mut() {
            mode.current.capacity_breaks = 2;
            mode.current.standing = CurrentStanding::Infeasible;
        }

        let text = report_lines(&result, "fixture").join("\n");

        assert!(
            text.contains("Current layout violates 2 capacity cap(s).")
                && text.contains("[borderline]"),
            "borderline observations never count as breaks: {text}"
        );
    }

    #[test]
    fn should_show_score_components_and_definitions_only_with_verbose() {
        let result = result_with_candidate();
        let quiet = report_lines(&result, "fixture").join("\n");
        let verbose = report::Report::build(&result, "fixture", RenderOptions { verbose: true })
            .lines()
            .join("\n");
        assert!(!quiet.contains("Score components"));
        assert!(verbose.contains("Score components"));
        assert!(verbose.contains("capacity penalizes"));
        assert!(
            verbose
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("dependency-only penalizes")
        );
    }

    #[test]
    fn should_preserve_wrapped_tree_geometry_and_literals_in_both_report_formats()
    -> Result<(), Box<dyn std::error::Error>> {
        use unicode_width::UnicodeWidthStr;

        let mut result = result_with_candidate();
        let source_name = format!("a{}  literal*.ts", "界e\u{301} part  ".repeat(28));
        let destination_name = format!("z{}  destination.ts", "wide界  ".repeat(35));
        let source = format!("src/deep/{source_name}");
        let destination = format!("src/deep/{destination_name}");
        let symbol = format!("Long{}Name", "界e\u{301}".repeat(100));
        let candidate = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|profile| profile.candidates.first_mut())
            .ok_or("missing candidate")?;
        candidate.symbol_moves = vec![SymbolMove {
            symbol: symbol.clone(),
            kind: SymbolKind::Type,
            from_path: source.clone(),
            to_path: destination.clone(),
            delta: -0.1,
            broken_imports: 0,
        }];
        let mut terminal = Vec::new();

        render(
            &result,
            Format::Summary,
            &result.current.tree.name,
            &mut terminal,
        )?;
        let terminal = String::from_utf8(terminal)?;
        let markdown = markdown(&result, RenderOptions::default());

        let mut rendered_trees = Vec::new();
        for output in [&terminal, &markdown] {
            assert!(output.lines().all(|line| line.width() <= 100), "{output}");
            let (_, trees) = output.split_once("Before").ok_or("missing Before")?;
            let (before, _) = trees.split_once("After").ok_or("missing After")?;
            assert_wrapped_tree_label(
                before,
                "            ├── ",
                "            │   ",
                &format!("{source_name} *"),
            )?;
            assert_wrapped_tree_label(
                before,
                "            └── ",
                "                ",
                &format!("{destination_name} *"),
            )?;
            assert_wrapped_tree_label(
                before,
                "            │   └── ",
                "            │       ",
                &format!("type `{symbol}` [to {destination}]"),
            )?;
            rendered_trees.push(before.to_owned());
        }
        let [terminal_tree, markdown_tree] = rendered_trees.as_slice() else {
            return Err("missing report format".into());
        };
        assert_eq!(terminal_tree, markdown_tree);
        Ok(())
    }

    fn assert_wrapped_tree_label(
        tree: &str,
        first_prefix: &str,
        continuation: &str,
        expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut lines = tree.lines();
        let first = lines
            .find_map(|line| line.strip_prefix(first_prefix))
            .ok_or_else(|| format!("missing branch {first_prefix:?}: {tree}"))?;
        let mut restored = first.to_owned();
        let mut segments = 1;
        while restored.len() < expected.len() {
            let next = lines.next().ok_or("missing wrapped continuation")?;
            let content = next
                .strip_prefix(continuation)
                .ok_or_else(|| format!("continuation lost geometry {continuation:?}: {next:?}"))?;
            restored.push_str(content);
            segments += 1;
        }
        assert!(
            segments >= 3,
            "fixture must exercise intermediate continuations"
        );
        assert_eq!(
            restored, expected,
            "literal spaces, names, paths, and stars must survive"
        );
        Ok(())
    }

    #[test]
    fn should_keep_deep_tree_paths_inside_the_report_width()
    -> Result<(), Box<dyn std::error::Error>> {
        use unicode_width::UnicodeWidthStr;

        let mut result = result_with_candidate();
        let components = (0..24)
            .map(|index| format!("component{index:02}/"))
            .collect::<Vec<_>>();
        let ancestry = components.concat();
        let destination = format!("{ancestry}界z.ts");
        let candidate = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|profile| profile.candidates.first_mut())
            .ok_or("missing candidate")?;
        candidate.symbol_moves = vec![SymbolMove {
            symbol: "Options".into(),
            kind: SymbolKind::Type,
            from_path: format!("{ancestry}界a.ts"),
            to_path: destination.clone(),
            delta: -0.1,
            broken_imports: 0,
        }];
        let mut terminal = Vec::new();

        render(
            &result,
            Format::Summary,
            &result.current.tree.name,
            &mut terminal,
        )?;
        let terminal = String::from_utf8(terminal)?;
        let markdown = markdown(&result, RenderOptions::default());

        let mut trees = Vec::new();
        for output in [&terminal, &markdown] {
            for line in output.lines() {
                assert!(line.width() <= 100, "{} columns: {line:?}", line.width());
            }
            let (_, tree) = output.split_once("Before").ok_or("missing Before")?;
            let (before, _) = tree.split_once("After").ok_or("missing After")?;
            let mut lines = before
                .lines()
                .skip_while(|line| !line.contains("component00/"));
            for (index, component) in components.iter().enumerate() {
                let indentation = "    ".repeat(index + 1);
                assert_deep_tree_line(
                    &mut lines,
                    &format!("{indentation}└── {component}"),
                    &format!("{indentation}    "),
                )?;
            }
            let indentation = "    ".repeat(25);
            assert_deep_tree_line(
                &mut lines,
                &format!("{indentation}├── 界a.ts *"),
                &format!("{indentation}│   "),
            )?;
            assert_deep_tree_line(
                &mut lines,
                &format!("{indentation}│   └── type `Options` [to {destination}]"),
                &format!("{indentation}│       "),
            )?;
            assert_deep_tree_line(
                &mut lines,
                &format!("{indentation}└── 界z.ts *"),
                &format!("{indentation}    "),
            )?;
            assert!(lines.all(|line| line.is_empty() || line == "  "));
            trees.push(before.to_owned());
        }
        let [terminal_tree, markdown_tree] = trees.as_slice() else {
            return Err("missing report format".into());
        };
        assert_eq!(terminal_tree, markdown_tree);
        Ok(())
    }

    fn assert_deep_tree_line<'text>(
        lines: &mut impl Iterator<Item = &'text str>,
        expected: &str,
        continuation: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut restored = lines.next().ok_or("missing tree line")?.to_owned();
        while restored.len() < expected.len() {
            assert!(
                expected.starts_with(&restored),
                "unexpected tree payload {restored:?}"
            );
            let next = lines.next().ok_or("missing deep continuation")?;
            let prefix = if next.starts_with("… ") {
                let suffix: String = continuation.chars().rev().take(76).collect();
                format!("… {}", suffix.chars().rev().collect::<String>())
            } else {
                continuation.to_owned()
            };
            let payload = next.strip_prefix(&prefix).ok_or("invalid deep geometry")?;
            assert!(!payload.is_empty(), "continuation must make progress");
            restored.push_str(payload);
        }
        assert_eq!(restored, expected, "deep tree payload must survive exactly");
        Ok(())
    }

    #[test]
    fn should_report_the_converged_solution_space_exactly_when_a_mode_converges() {
        let converged = report_lines(&result_with_candidate(), "fixture").join("\n");
        assert!(converged.contains("the solution space converged"));

        let mut diverged = result_with_candidate();
        if let Some(mode) = diverged.profiles.anchored.as_mut() {
            mode.solution_space_converged = false;
        }
        let diverged_text = report_lines(&diverged, "fixture").join("\n");
        assert!(!diverged_text.contains("solution space converged"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn should_present_only_recommended_and_review_candidate_advice_groups() {
        let mut result = result_with_candidate();
        let assessment = strata_engine::ProfileAssessment {
            profile: strata_engine::ProfileName::Anchored,
            destination: "shared/owner.ts".to_owned(),
            evidence: strata_engine::EvidenceSignals {
                unique_owner: 1.0,
                role_affinity: 0.8,
                source_cohesion: 0.7,
                destination_cohesion: 0.6,
                producer_evidence: 0.5,
                architectural_reach: 0.4,
            },
            weighted_score: 0.75,
            structural_score: 0.65,
            ambiguity_margin: 0.2,
            best_alternative: Some("shared/alternative.ts".to_owned()),
            thresholds: strata_engine::QualificationThresholds {
                minimum_evidence: 0.6,
                minimum_structural: 0.5,
                minimum_ambiguity_margin: 0.15,
            },
            qualified: true,
        };
        let relocation = SymbolMove {
            symbol: "RecordOptions".to_owned(),
            kind: SymbolKind::Type,
            from_path: "origin/options.ts".to_owned(),
            to_path: "shared/owner.ts".to_owned(),
            delta: -0.1,
            broken_imports: 1,
        };
        let candidate = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|profile| profile.candidates.first_mut());
        assert!(candidate.is_some(), "fixture has an anchored candidate");
        if let Some(candidate) = candidate {
            candidate.symbol_moves = vec![relocation.clone()];
        }
        let item = strata_engine::RelocationAdvice {
            proposal: strata_engine::RelocationProposal::Symbol { relocation },
            destination: "shared/owner.ts".to_owned(),
            supporting_profiles: vec![strata_engine::ProfileName::Anchored],
            qualified_profiles: vec![strata_engine::ProfileName::Anchored],
            absent_profiles: vec![strata_engine::ProfileName::Greenfield],
            conflicting_destinations: vec![strata_engine::ProfileConflict {
                profile: strata_engine::ProfileName::Greenfield,
                destination: "shared/alternative.ts".to_owned(),
            }],
            assessments: vec![assessment],
            review_reasons: vec![strata_engine::ReviewReason::PartialProfileSupport],
        };
        result.advice.recommended = vec![item.clone()];
        result.advice.review_candidates = vec![item];
        let text = report::Report::build(&result, "fixture", RenderOptions { verbose: true })
            .lines()
            .join("\n");
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(text.contains("Recommended"), "{text}");
        assert!(text.contains("Review candidate"), "{text}");
        for expected in [
            "supporting [anchored]",
            "qualified [anchored]",
            "absent [greenfield]",
            "greenfield→`shared/alternative.ts`",
            "owner 1.00",
            "role 0.80",
            "source 0.70",
            "destination 0.60",
            "producer 0.50",
            "reach 0.40",
            "weighted 0.75/0.60",
            "structural 0.65/0.50",
            "margin threshold 0.15",
            "qualified true",
            "best alternative `shared/alternative.ts`",
            "review reasons: selected by only some executed profiles",
            "RecordOptions",
            "origin/options.ts",
        ] {
            assert!(
                compact.contains(expected),
                "missing `{expected}` from:\n{text}"
            );
        }
        assert!(
            !text.contains("Rejected"),
            "ordinary non-moves stay absent: {text}"
        );
        assert!(
            !compact.contains("adopt candidate"),
            "a surfaced review-only atom makes blanket adoption unsafe: {text}"
        );

        let mut fully_recommended = result;
        fully_recommended.advice.review_candidates.clear();
        let recommended_text = report_lines(&fully_recommended, "fixture")
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            recommended_text.contains("Recommended (1)")
                && recommended_text.contains("Review candidate (0)"),
            "qualified actions remain recommended without endorsing an entire candidate: {recommended_text}"
        );
    }

    #[test]
    fn should_render_file_advice_alternatives_with_their_physical_source_root() {
        let mut result = result_with_candidate();
        result.advice.review_candidates = vec![strata_engine::RelocationAdvice {
            proposal: strata_engine::RelocationProposal::File {
                relocation: Move {
                    kind: MoveKind::Move,
                    files: vec![FileMove {
                        path: "dataset/src/origin/unit.ts".to_owned(),
                        from: "dataset/src/origin".to_owned(),
                    }],
                    to: "dataset/src/chosen".to_owned(),
                    reason: MoveReason::Clustering,
                    mirrors: Vec::new(),
                    blocked_mirrors: Vec::new(),
                },
            },
            destination: "dataset/src/chosen".to_owned(),
            supporting_profiles: vec![strata_engine::ProfileName::Anchored],
            qualified_profiles: Vec::new(),
            absent_profiles: vec![strata_engine::ProfileName::Greenfield],
            conflicting_destinations: Vec::new(),
            assessments: vec![strata_engine::ProfileAssessment {
                profile: strata_engine::ProfileName::Anchored,
                destination: "dataset/src/chosen".to_owned(),
                evidence: strata_engine::EvidenceSignals {
                    unique_owner: 0.0,
                    role_affinity: 0.0,
                    source_cohesion: 0.0,
                    destination_cohesion: 0.0,
                    producer_evidence: 0.0,
                    architectural_reach: 0.0,
                },
                weighted_score: 0.0,
                structural_score: 0.0,
                ambiguity_margin: 0.0,
                best_alternative: Some("dataset/src/alternative".to_owned()),
                thresholds: strata_engine::QualificationThresholds {
                    minimum_evidence: 0.6,
                    minimum_structural: 0.5,
                    minimum_ambiguity_margin: 0.15,
                },
                qualified: false,
            }],
            review_reasons: vec![strata_engine::ReviewReason::PartialProfileSupport],
        }];

        let text = report::Report::build(&result, "dataset", RenderOptions { verbose: true })
            .lines()
            .join("\n");

        assert!(text.contains("file `dataset/src/origin/unit.ts` → `dataset/src/chosen`"));
        assert!(text.contains("best alternative `dataset/src/alternative`"));
        assert!(!text.contains("best alternative `dataset/spec/alternative`"));
    }

    #[test]
    fn should_carry_no_control_characters_and_no_off_page_tokens() {
        let mut result = result_with_candidate();
        if let Some(candidate) = result
            .profiles
            .anchored
            .as_mut()
            .and_then(|mode| mode.candidates.first_mut())
        {
            candidate.delta_narration = vec![move_of(
                MoveKind::Move,
                &["logger.ts"],
                "src/util",
                "src/io",
                MoveReason::Clustering,
            )];
        }
        let lines = report_lines(&result, "fixture");

        for line in &lines {
            let bad = line.chars().find(|ch| ch.is_control());
            assert!(
                bad.is_none(),
                "control character {bad:?} reached the page: {line}"
            );
        }
        let text = lines.join("\n");
        for banned in ["snapshot", "payload", "mockup", "DSG05"] {
            assert!(
                !text.contains(banned),
                "provenance token {banned} stays off the page"
            );
        }
        for interactive in ["menu", "press ", "click", "navigate"] {
            assert!(
                !text.to_lowercase().contains(interactive),
                "interactive token {interactive} stays off the page"
            );
        }
    }

    #[test]
    fn should_banner_the_project_with_compact_saved_metadata() {
        let text = report_lines(&result_with_candidate(), "acme").join("\n");
        assert!(text.starts_with("Strata report — acme\n4 files · 2 symbols · 1 edges"));
        assert!(text.contains("Snapshot `deadbeef`"));
        assert!(text.contains("Structural findings"));
        assert!(!text.contains("future capability"));
    }

    #[test]
    fn should_report_no_moves_for_an_empty_narration() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.0,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: Vec::new(),
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        assert_eq!(buffer, b"no moves\n");
    }

    #[test]
    fn should_narrate_a_move_with_its_from_and_to_paths() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 1.25,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![move_of(
                MoveKind::Move,
                &["alpha"],
                "old",
                "new",
                MoveReason::Clustering,
            )],
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert_eq!(
            text,
            "moves (1 group(s), 1 file(s); gain +1.2500):\nmove — regrouped by clustering\n  1. alpha [old → new]\n"
        );
    }

    #[test]
    fn should_describe_diff_regressions_and_zero_changes_without_calling_them_gains() {
        let regression = Candidate {
            index: 1,
            score: 2.0,
            score_breakdown: zero_breakdown(),
            improvement: -0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![move_of(
                MoveKind::Move,
                &["worker.ts"],
                "src/feature",
                "src/shared",
                MoveReason::Clustering,
            )],
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        };
        let unchanged = Candidate {
            index: 1,
            score: 1.5,
            score_breakdown: zero_breakdown(),
            improvement: 0.0,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: Vec::new(),
            symbol_moves: vec![SymbolMove {
                symbol: "WorkInput".to_owned(),
                kind: SymbolKind::Type,
                from_path: "src/task/run.rs".to_owned(),
                to_path: "src/task/types.rs".to_owned(),
                delta: 0.0,
                broken_imports: 1,
            }],
            capacity_remainder: None,
        };
        let mut regression_output = Vec::new();
        let mut unchanged_output = Vec::new();

        render_diff(&regression, &mut regression_output).unwrap_or_default();
        render_diff(&unchanged, &mut unchanged_output).unwrap_or_default();

        let regression_text = String::from_utf8(regression_output).unwrap_or_default();
        let unchanged_text = String::from_utf8(unchanged_output).unwrap_or_default();
        assert!(
            regression_text.starts_with("moves (1 group(s), 1 file(s); regression 0.5000):"),
            "{regression_text}"
        );
        assert!(
            unchanged_text.starts_with("symbol moves (1 symbol(s); no improvement):"),
            "{unchanged_text}"
        );
        assert!(!regression_text.contains("gain"), "{regression_text}");
        assert!(!unchanged_text.contains("gain"), "{unchanged_text}");
    }

    #[test]
    fn should_number_every_file_of_a_large_move_group() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![move_of(
                MoveKind::Merge,
                &["a.ts", "b.ts", "c.ts", "d.ts"],
                "old",
                "new",
                MoveReason::Clustering,
            )],
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(
            text.contains("merge — regrouped by clustering\n"),
            "the group header names its reason: {text}"
        );
        for (step, file) in ["a.ts", "b.ts", "c.ts", "d.ts"].into_iter().enumerate() {
            assert!(
                text.contains(&format!("  {}. {file} [old → new]\n", step + 1)),
                "{file}: {text}"
            );
        }
    }

    #[test]
    fn should_render_each_files_own_source_in_a_merge() {
        let candidate = Candidate {
            index: 1,
            score: 0.0,
            score_breakdown: zero_breakdown(),
            improvement: 0.5,
            tree: file_node("lib", 1),
            conditional_splits: Vec::new(),
            delta_narration: vec![Move {
                kind: MoveKind::Merge,
                files: vec![
                    FileMove {
                        path: "a.ts".to_owned(),
                        from: "one".to_owned(),
                    },
                    FileMove {
                        path: "b.ts".to_owned(),
                        from: "two".to_owned(),
                    },
                ],
                to: "new".to_owned(),
                reason: MoveReason::Clustering,
                mirrors: Vec::new(),
                blocked_mirrors: Vec::new(),
            }],
            symbol_moves: Vec::new(),
            capacity_remainder: None,
        };
        let mut buffer = Vec::new();

        render_diff(&candidate, &mut buffer).unwrap_or_default();

        let text = String::from_utf8(buffer).unwrap_or_default();
        assert!(text.contains("  1. a.ts [one → new]\n"), "{text}");
        assert!(text.contains("  2. b.ts [two → new]\n"), "{text}");
    }
}

#[cfg(test)]
#[path = "../tests/support/combined_package_moves.rs"]
mod combined_package_moves;
