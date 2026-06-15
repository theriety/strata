//! End-to-end acceptance: the built `strata` binary delivers the expected output.
//!
//! Where `parity.rs` pins the AD-5 library/CLI byte-parity guarantee and the
//! committed goldens, this suite is the user-facing acceptance layer: it drives
//! the *compiled* binary the way a developer or a CI pipeline would (via
//! `assert_cmd`), and asserts the *behaviour* of every command — `analyze`,
//! `tree`, `report`, `diff`, and `violations` — through exit codes and structural
//! invariants.
//!
//! Oracle layering, by design:
//! - **Goldens** (in `parity.rs`) are the single oracle for rendered output —
//!   `tree`, `diff`, `violations`, `report`, and `analyze --format summary`. Bless
//!   them with `STRATA_BLESS=1`.
//! - **These hand-written asserts** check exit codes and structural invariants
//!   (the node/edge census, candidate counts, header presence, `move ...`
//!   narration, a `[violation]` line) — never the exact rendered bytes, which the
//!   goldens own.
//! - **Parity** (in `parity.rs`) pins library↔CLI byte-equality of the json face.
//!
//! Determinism is engineered, not hoped for. Every analysis is run with an
//! explicit non-existent `--config`, so the binary always falls back to the
//! built-in defaults regardless of the working directory the test harness runs
//! from; an identical snapshot and config therefore yield byte-identical output
//! (the deterministic-by-contract guarantee), and the invariants never flake on
//! ordering. Both μ-modes are exercised: anchored (μ > 0, a non-zero anchor term)
//! and greenfield (μ = 0, no anchor term), so the mode-dependent score and
//! narration are both pinned.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// A non-existent config path, forcing the binary onto its built-in defaults so
/// the output is identical no matter which directory the harness runs from. A
/// configured `strata.toml` in a parent directory must never leak into a run.
const PURE_DEFAULTS: &str = "/nonexistent/strata-acceptance.toml";

/// The captured result of one binary invocation: its exit code and decoded
/// streams.
///
/// A spawn failure (the binary missing or unrunnable) yields the sentinel code
/// `-1` with empty streams so the caller's status assertion fails loudly without
/// a panic — the workspace forbids `unwrap`/`expect`, so failure is surfaced
/// through assertions, never a hard stop.
struct Run {
    /// The process exit code, or `-1` when the binary could not be spawned.
    code: i32,
    /// The captured standard output, decoded lossily.
    stdout: String,
    /// The captured standard error, decoded lossily.
    stderr: String,
}

/// Returns the absolute path to a named language fixture under this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/e2e/fixtures")
        .join(name)
}

/// Runs the built `strata` binary with `args` and captures its outcome.
///
/// The binary is the real artifact cargo builds for this crate, invoked exactly
/// as a shell would — this is the acceptance contract, not an in-process call.
fn run(args: &[&str]) -> Run {
    let spawned = match Command::cargo_bin("strata") {
        Ok(mut command) => command.args(args).output().ok(),
        Err(_) => None,
    };
    match spawned {
        Some(output) => Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        None => Run {
            code: -1,
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

/// Runs `strata analyze --format json` over a fixture with pure defaults, writes
/// the result to a unique temp file, and returns that path.
///
/// The saved result is what `tree`, `diff`, and `report` consume, so the whole
/// suite exercises the same `analyze` output a real session would persist.
fn analyze_to_file(name: &str) -> PathBuf {
    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let path = std::env::temp_dir().join(format!("strata-accept-{name}-{}.json", nanos()));
    let path_str = path.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
        "--output",
        path_str,
    ]);
    assert_eq!(
        outcome.code, 0,
        "analyze must exit 0 to save a result for {name}"
    );
    path
}

/// Extracts the candidate-1 score from a summary face by parsing the float that
/// follows the first `score ` token.
///
/// The summary renders each candidate as `candidate 1 score <f> (...)`, so the
/// first `score ` occurrence is candidate 1's. A malformed or absent score yields
/// `f64::NAN` so a comparison against it fails loudly rather than passing by
/// accident — the workspace forbids `unwrap`/`expect`, so no parse panics here.
fn candidate_one_score(summary: &str) -> f64 {
    summary
        .split_once("score ")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|token| token.parse::<f64>().ok())
        .unwrap_or(f64::NAN)
}

/// Returns a process-unique nanosecond stamp for temp paths.
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

#[test]
fn should_deliver_the_analyze_summary_for_the_rust_fixture() {
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "both",
        "--format",
        "summary",
    ]);

    assert_eq!(
        outcome.code, 0,
        "analyze is a finding, not a failure: it exits 0"
    );
    // the census and per-mode candidate counts are the contracted summary surface.
    assert!(
        outcome
            .stdout
            .contains("summary: 6 symbols, 4 edges, 1 files"),
        "the expected node/edge census is delivered"
    );
    assert!(
        outcome.stdout.contains("mode anchored: 1 candidate(s)"),
        "anchored candidates are reported"
    );
    assert!(
        outcome.stdout.contains("mode greenfield: 1 candidate(s)"),
        "greenfield candidates are reported"
    );
}

#[test]
fn should_print_the_recommended_structure_only_when_show_suggestions_is_set() {
    // --show-suggestions is opt-in: it renders the best candidate's proposed tree
    // below the headlines; the plain run prints only the candidate headlines.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();
    let base = [
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "anchored",
        "--format",
        "summary",
    ];

    let mut with_flag = base.to_vec();
    with_flag.push("--show-suggestions");
    let shown = run(&with_flag);
    let plain = run(&base);

    assert_eq!(shown.code, 0, "the suggestion run exits 0");
    assert_eq!(plain.code, 0, "the plain run exits 0");
    assert!(
        shown.stdout.contains("suggested structure (candidate 1"),
        "the suggestion header leads the rendered tree"
    );
    assert!(
        shown.stdout.contains("rust [packageGroup]") && shown.stdout.contains("src/lib.rs [file]"),
        "the rendered candidate tree carries its real directory-derived names"
    );
    assert!(
        !plain.stdout.contains("suggested structure"),
        "the plain run omits the suggested structure entirely"
    );
}

#[test]
fn should_deliver_the_analyze_summary_for_the_python_fixture() {
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "both",
        "--format",
        "summary",
    ]);

    assert_eq!(
        outcome.code, 0,
        "a clean multi-file fixture analyzes and exits 0"
    );
    assert!(
        outcome
            .stdout
            .contains("summary: 4 symbols, 4 edges, 2 files"),
        "the two-file census is delivered"
    );
}

#[test]
fn should_pin_the_anchor_term_apart_for_the_two_modes() {
    // anchored (mu > 0) carries a non-zero anchor penalty that raises its score
    // above the otherwise-identical greenfield (mu = 0) candidate; pinning both
    // proves the two modes are genuinely distinct and not a shared code path. The
    // two-file python fixture regroups one file, so the move-distance penalty is
    // non-zero and the two modes' scores genuinely diverge.
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();

    let anchored = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "anchored",
        "--format",
        "summary",
    ]);
    let greenfield = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "greenfield",
        "--format",
        "summary",
    ]);

    assert_eq!(anchored.code, 0, "anchored mode exits 0");
    assert_eq!(greenfield.code, 0, "greenfield mode exits 0");
    // the invariant, not the exact bytes (those are pinned by the summary golden):
    // the anchor penalty strictly raises the anchored candidate's score above the
    // otherwise-identical greenfield one.
    let anchored_score = candidate_one_score(&anchored.stdout);
    let greenfield_score = candidate_one_score(&greenfield.stdout);
    assert!(
        anchored_score > greenfield_score,
        "the anchor penalty raises the anchored score ({anchored_score}) above greenfield ({greenfield_score})"
    );
    assert!(
        !anchored.stdout.contains("mode greenfield"),
        "an anchored-only run emits only anchored"
    );
    assert!(
        !greenfield.stdout.contains("mode anchored"),
        "a greenfield-only run emits only greenfield"
    );
}

#[test]
fn should_be_deterministic_across_repeated_analyze_runs() {
    // the deterministic-by-contract guarantee: identical snapshot + config + seed
    // yields byte-identical output, so a snapshot can never flake on ordering.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();
    let args = [
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ];

    let first = run(&args);
    let second = run(&args);

    assert_eq!(first.code, 0, "the first run exits 0");
    assert_eq!(second.code, 0, "the second run exits 0");
    assert_eq!(
        first.stdout, second.stdout,
        "two runs deliver byte-identical json"
    );
}

#[test]
fn should_be_deterministic_across_repeated_analyze_runs_for_the_new_fixtures() {
    // the determinism gate every new fixture must clear before its goldens are
    // blessed: an identical root + config yields byte-identical json on a repeat
    // run, so the committed goldens can never flake on ordering. Each fixture is a
    // richer or violation-focused shape than the original three.
    for name in [
        "workspace-rust",
        "nested-python",
        "nested-ts",
        "cyclic",
        "over-capacity",
        "polarity-leak",
    ] {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();
        let args = [
            "analyze",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--format",
            "json",
        ];

        let first = run(&args);
        let second = run(&args);

        assert_eq!(first.code, 0, "the first run exits 0 for {name}");
        assert_eq!(second.code, 0, "the second run exits 0 for {name}");
        assert_eq!(
            first.stdout, second.stdout,
            "two runs deliver byte-identical json for {name}"
        );
    }
}

#[test]
fn should_deliver_the_current_tree_for_the_rust_fixture() {
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["tree", "--input", result_str, "--current", "--symbols"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(
        outcome.code, 0,
        "tree renders the current structure and exits 0"
    );
    // the hierarchical tree descends the five fixed levels down to a file's symbols.
    assert!(
        outcome.stdout.contains("rust [packageGroup]"),
        "the tree is rooted at the package group"
    );
    assert!(
        outcome.stdout.contains("src/lib.rs [file] 16 sloc"),
        "the file node carries its production sloc summed from effective_size"
    );
}

#[test]
fn should_deliver_the_current_tree_for_the_multi_file_python_fixture() {
    let result = analyze_to_file("python");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["tree", "--input", result_str, "--current", "--symbols"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(
        outcome.code, 0,
        "tree renders a multi-file structure and exits 0"
    );
    assert!(
        outcome.stdout.contains("pkg/rectangle.py [file] 6 sloc"),
        "both source files appear as file nodes"
    );
    assert!(
        outcome.stdout.contains("pkg/shape.py [file] 3 sloc"),
        "both source files appear as file nodes"
    );
}

#[test]
fn should_truncate_a_candidate_tree_at_the_requested_depth() {
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&[
        "tree",
        "--input",
        result_str,
        "--mode",
        "anchored",
        "--candidate",
        "1",
        "--symbols",
        "--depth",
        "2",
    ]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a candidate tree renders and exits 0");
    // depth truncation is exercised exhaustively by parity's workspace-tree test;
    // here we keep only the structural invariant that --depth 2 truncates above the
    // symbol leaves: the root container is present, but no per-symbol leaf line
    // (rendered as `  - <name>`) survives at depth 2.
    assert!(
        outcome.stdout.contains("rust [packageGroup]"),
        "the truncated tree is still rooted at the package group: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("- Shape"),
        "depth 2 truncates above the file's symbol leaves: {}",
        outcome.stdout
    );
}

#[test]
fn should_deliver_a_well_formed_report_for_the_rust_fixture() {
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["report", "--input", result_str]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "report renders and exits 0");
    // the report sections must be present and well-formed Markdown.
    assert!(
        outcome.stdout.starts_with("# Strata report"),
        "the title heading leads"
    );
    assert!(
        outcome.stdout.contains("## Current layout"),
        "the current-layout section is present"
    );
    assert!(
        outcome.stdout.contains("## Anchored candidates"),
        "the anchored section is present"
    );
    assert!(
        outcome.stdout.contains("## Greenfield candidates"),
        "the greenfield section is present"
    );
    // the objective J(T) is surfaced per layout as a fixed-precision score line; the
    // exact score bytes are pinned by the `report.md` golden, so here we assert only
    // the structural invariant that the current-layout score line is present.
    assert!(
        outcome.stdout.contains("\nScore `"),
        "the current layout reports its objective J(T) as a score line: {}",
        outcome.stdout
    );
}

#[test]
fn should_deliver_the_anchored_diff_for_the_rust_fixture() {
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "anchored/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(
        outcome.code, 0,
        "diff renders the structural delta and exits 0"
    );
    // the rust fixture's anchored/1 equals the current layout, so the delta is the
    // `no moves` sentinel; the exact bytes are owned by the `diff.txt` golden.
    assert_eq!(
        outcome.stdout, "no moves\n",
        "an anchored candidate identical to the current layout is the `no moves` sentinel"
    );
}

#[test]
fn should_deliver_the_greenfield_diff_for_the_multi_file_python_fixture() {
    // the greenfield (mu = 0) delta regroups the two cohesive files under one real
    // folder: the file already in that folder stays, the other is narrated as a
    // move, exercising the path-delta narration end to end.
    let result = analyze_to_file("python");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "diff renders and exits 0");
    assert!(
        outcome.stdout.contains("move pkg/shape.py"),
        "the regrouped file is narrated as a move"
    );
}

#[test]
fn should_flag_visibility_violations_for_the_over_exported_rust_fixture() {
    // the rust fixture over-exports symbols needed only locally; the table must
    // list every flagged visibility violation deterministically.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "table",
    ]);

    assert_eq!(
        outcome.code, 0,
        "violations are findings, not failures, without --fail-on"
    );
    assert!(
        outcome.stdout.contains("visibility [violation] Shape"),
        "an over-exported symbol is flagged"
    );
}

#[test]
fn should_report_no_violations_for_a_clean_fixture() {
    // the typescript and python fixtures are structurally clean: no cycles, no
    // polarity leaks, no over-cap files -> the table reports none.
    for name in ["ts", "python"] {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let outcome = run(&[
            "violations",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--format",
            "table",
        ]);

        assert_eq!(outcome.code, 0, "a clean fixture exits 0 for {name}");
        assert_eq!(
            outcome.stdout, "no violations\n",
            "a clean fixture reports no violations for {name}"
        );
    }
}

#[test]
fn should_gate_with_exit_two_when_a_fail_on_class_is_present() {
    // the rust fixture carries hard visibility violations; gating on that class
    // raises the reserved exit code 2 — the CI gate the tool exists to provide.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "visibility",
    ]);

    assert_eq!(
        outcome.code, 2,
        "a present --fail-on class gates with exit 2"
    );
}

#[test]
fn should_never_gate_a_clean_fixture_even_with_fail_on() {
    // exit 2 is reserved exclusively for a real gating match; a clean fixture
    // never produces it, no matter which class is named.
    let root = fixture("ts");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "cycle,polarity,capacity,visibility",
    ]);

    assert_eq!(outcome.code, 0, "a clean fixture never gates");
}

#[test]
fn should_exit_non_zero_with_a_helpful_message_on_an_unreadable_input() {
    // a missing result file is a usage error: exit 1 with a coded, remediable
    // message on stderr — never the gating code 2.
    let outcome = run(&[
        "tree",
        "--input",
        "/nonexistent/strata-result.json",
        "--current",
    ]);

    assert_eq!(
        outcome.code, 1,
        "an unreadable input is exit 1, not the gating 2"
    );
    assert!(
        outcome.stderr.contains("error[INPUT_UNREADABLE]"),
        "the error carries its stable code: {}",
        outcome.stderr
    );
}

#[test]
fn should_exit_non_zero_with_a_helpful_message_on_a_missing_candidate() {
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "anchored/9"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 1, "a missing candidate is exit 1");
    assert!(
        outcome.stderr.contains("error[CANDIDATE_NOT_FOUND]"),
        "the error names the missing candidate: {}",
        outcome.stderr
    );
}

#[test]
fn should_reject_an_unknown_fail_on_class_as_a_usage_error() {
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "bogus",
    ]);

    assert_eq!(
        outcome.code, 1,
        "an unknown --fail-on class is a usage error, never the gating 2"
    );
    assert!(
        outcome.stderr.contains("error[CONFIG_INVALID]"),
        "the error explains the bad class: {}",
        outcome.stderr
    );
}

#[test]
fn should_exit_one_on_an_unknown_subcommand() {
    let outcome = run(&["wat-no-such-command"]);

    assert_eq!(
        outcome.code, 1,
        "an unknown subcommand is a usage error (exit 1), never the gating 2"
    );
}

#[test]
fn should_print_per_subcommand_help_and_exit_zero() {
    // every subcommand's --help is a success that prints its usage to stdout.
    for command in ["analyze", "tree", "diff", "violations", "report"] {
        let outcome = run(&[command, "--help"]);

        assert_eq!(outcome.code, 0, "{command} --help is a success");
        assert!(
            outcome.stdout.contains(&format!("Usage: strata {command}")),
            "{command} --help prints its usage line"
        );
    }
}

#[test]
fn should_print_top_level_help_and_exit_zero() {
    let outcome = run(&["--help"]);

    assert_eq!(outcome.code, 0, "--help is a success");
    assert!(
        outcome.stdout.contains("Usage: strata"),
        "the top-level help lists the usage"
    );
}

#[test]
fn should_deliver_a_well_formed_violations_json_face_for_a_violation_carrying_fixture() {
    // the json face of the gate is the serialized AnalyzeResult: it must parse and
    // carry the same capacity findings the table face lists. over-capacity emits
    // both a hard and a borderline capacity finding, so the violations json array
    // holds exactly those two capacity entries — proving the gate's machine-readable
    // surface is well-formed and complete.
    let root = fixture("over-capacity");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "the json face is a finding, not a failure");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
    let kinds: Vec<&str> = parsed
        .pointer("/current/violations")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("kind").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        kinds,
        vec!["capacity", "capacity"],
        "the violations json carries both capacity findings (hard + borderline): {}",
        outcome.stdout
    );
}

#[test]
fn should_match_the_analyze_json_face_only_when_no_capacity_finding_exists() {
    // capacity findings are computed in the violations path, never in engine analyze;
    // so the two json faces agree byte-for-byte for a visibility-only fixture (rust)
    // but genuinely diverge for over-capacity, where the violations face gains two
    // capacity entries the analyze face omits. This pins the exact, asymmetric
    // relationship — not an assumed equality.
    let rust = fixture("rust");
    let rust_str = rust.to_str().unwrap_or_default();
    let analyze_rust = run(&[
        "analyze",
        "--root",
        rust_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let violations_rust = run(&[
        "violations",
        "--root",
        rust_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(analyze_rust.code, 0, "analyze json exits 0 for rust");
    assert_eq!(violations_rust.code, 0, "violations json exits 0 for rust");
    assert_eq!(
        analyze_rust.stdout, violations_rust.stdout,
        "with no capacity finding the two json faces are byte-identical"
    );

    let over = fixture("over-capacity");
    let over_str = over.to_str().unwrap_or_default();
    let analyze_over = run(&[
        "analyze",
        "--root",
        over_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let violations_over = run(&[
        "violations",
        "--root",
        over_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(
        analyze_over.code, 0,
        "analyze json exits 0 for over-capacity"
    );
    assert_eq!(
        violations_over.code, 0,
        "violations json exits 0 for over-capacity"
    );
    assert_ne!(
        analyze_over.stdout, violations_over.stdout,
        "the violations face gains capacity findings the analyze face omits"
    );
}

#[test]
fn should_gate_each_violation_class_against_its_triggering_fixture() {
    // the headline gate matrix: each fixture carries exactly one hard class, and
    // naming that class with --fail-on raises the reserved exit 2 — the CI contract.
    // cyclic -> cycle, polarity-leak -> polarity, over-capacity -> capacity:violation.
    for (name, selector) in [
        ("cyclic", "cycle"),
        ("polarity-leak", "polarity"),
        ("over-capacity", "capacity:violation"),
    ] {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let outcome = run(&[
            "violations",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--fail-on",
            selector,
        ]);

        assert_eq!(
            outcome.code, 2,
            "{name} gates on its hard {selector} class with exit 2"
        );
    }
}

#[test]
fn should_not_gate_a_clean_fixture_on_the_cycle_class() {
    // exit 2 is reserved for a real hard-violation match: nested-python carries no
    // cycle, so the same selector that gates cyclic leaves it at 0.
    let root = fixture("nested-python");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "cycle",
    ]);

    assert_eq!(outcome.code, 0, "a cycle-free fixture never gates on cycle");
}

#[test]
fn should_never_gate_on_a_borderline_only_capacity_selector() {
    // a borderline finding never gates CI: over-capacity carries a borderline
    // capacity finding, but `capacity:borderline` matches nothing that gates, so the
    // run stays at 0 even though the borderline finding is present and reported.
    let root = fixture("over-capacity");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "capacity:borderline",
    ]);

    assert_eq!(
        outcome.code, 0,
        "a borderline-qualified selector never raises the gating exit 2"
    );
}

#[test]
fn should_gate_when_a_mixed_multi_selector_matches_one_class() {
    // a multi-selector --fail-on gates when any listed class is present: cyclic
    // matches only the `cycle` member of the set, and that alone raises exit 2.
    let root = fixture("cyclic");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "cycle,polarity,capacity:violation",
    ]);

    assert_eq!(
        outcome.code, 2,
        "a single matched class in a mixed selector still gates"
    );
}

#[test]
fn should_render_a_greenfield_candidate_tree_for_the_workspace_fixture() {
    // workspace-rust is the fixture whose greenfield (mu = 0) regroup produces a real
    // multi-level tree: the cross-crate files collapse under one cohesive folder, so
    // a greenfield candidate tree descends the full container hierarchy down to files.
    let result = analyze_to_file("workspace-rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&[
        "tree",
        "--input",
        result_str,
        "--mode",
        "greenfield",
        "--candidate",
        "1",
    ]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a greenfield candidate tree renders");
    assert!(
        outcome.stdout.contains("workspace-rust [packageGroup]"),
        "the greenfield tree is rooted at the package group: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("crates/core/src/lib.rs [file]"),
        "the regrouped files descend to real file leaves: {}",
        outcome.stdout
    );
}

#[test]
fn should_narrate_the_greenfield_moves_for_the_workspace_fixture() {
    // the greenfield delta of workspace-rust moves the cross-crate files into the
    // single cohesive folder, so the diff narrates at least one move group.
    let result = analyze_to_file("workspace-rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "the greenfield diff renders");
    assert!(
        outcome.stdout.contains("move "),
        "the regroup is narrated as a move: {}",
        outcome.stdout
    );
}

#[test]
fn should_omit_a_vi_distance_line_for_a_cross_mode_diff() {
    // the variation-of-information matrix only covers same-mode candidate pairs, so a
    // cross-mode diff (anchored/1 vs greenfield/1) narrates the right candidate's
    // moves but emits no `vi distance` line — the guard in commands/diff.rs only
    // writes the distance when both references share a mode. Pinned empirically: the
    // cross-mode run carries moves yet never a distance line.
    let result = analyze_to_file("workspace-rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "anchored/1", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a cross-mode diff renders and exits 0");
    assert!(
        !outcome.stdout.contains("vi distance"),
        "a cross-mode diff carries no variation-of-information distance: {}",
        outcome.stdout
    );
}

#[test]
fn should_report_a_vi_distance_line_for_a_same_mode_candidate_pair() {
    // the complement of the cross-mode case: over-capacity yields two anchored
    // candidates, and a same-mode diff (anchored/1 vs anchored/2) reports their
    // pairwise variation-of-information distance as a fixed-precision `vi distance`
    // line below the move narration.
    let result = analyze_to_file("over-capacity");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "anchored/1", "anchored/2"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a same-mode diff renders and exits 0");
    assert!(
        outcome.stdout.contains("vi distance "),
        "a same-mode pair reports its variation-of-information distance: {}",
        outcome.stdout
    );
}

#[test]
fn should_report_no_moves_for_a_self_diff() {
    // diffing the current layout against itself is a degenerate but legal reference
    // pair: it short-circuits to the `no moves` sentinel rather than narrating an
    // empty delta or erroring.
    let result = analyze_to_file("workspace-rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "current"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a self-diff renders and exits 0");
    assert_eq!(
        outcome.stdout, "no moves\n",
        "a current-vs-current diff is the `no moves` sentinel"
    );
}

#[test]
fn should_truncate_the_workspace_tree_at_each_requested_depth() {
    // depth truncates at container depth (root is depth 0): depth 0 prints only the
    // root, depth 1 adds its single child level, and a depth past the tree height is
    // a no-op that renders the full hierarchy. workspace-rust has a genuine multi-
    // level tree, so the truncation is observable rather than vacuous.
    let result = analyze_to_file("workspace-rust");
    let result_str = result.to_str().unwrap_or_default();
    let base = [
        "tree",
        "--input",
        result_str,
        "--mode",
        "greenfield",
        "--candidate",
        "1",
    ];

    let mut depth0 = base.to_vec();
    depth0.extend(["--depth", "0"]);
    let at_zero = run(&depth0);
    let mut depth1 = base.to_vec();
    depth1.extend(["--depth", "1"]);
    let at_one = run(&depth1);
    let mut depth99 = base.to_vec();
    depth99.extend(["--depth", "99"]);
    let at_height = run(&depth99);
    let full = run(&base);

    let _ = std::fs::remove_file(&result);
    assert_eq!(at_zero.code, 0, "depth 0 renders");
    assert_eq!(at_one.code, 0, "depth 1 renders");
    assert_eq!(at_height.code, 0, "an over-height depth renders");
    assert_eq!(full.code, 0, "the untruncated tree renders");
    // depth 0 is the root line alone.
    assert_eq!(
        at_zero.stdout, "workspace-rust [packageGroup]\n",
        "depth 0 prints only the root container"
    );
    // depth 1 adds exactly the next container level and no deeper.
    assert!(
        at_one.stdout.contains("crates [package]") && !at_one.stdout.contains("[domain]"),
        "depth 1 stops one level below the root: {}",
        at_one.stdout
    );
    // a depth past the tree height is a no-op equal to the untruncated render.
    assert_eq!(
        at_height.stdout, full.stdout,
        "a depth larger than the tree height renders the full tree"
    );
}

#[test]
fn should_write_a_report_to_a_file_matching_its_stdout_form() {
    // report --output writes the same Markdown it would otherwise print: the file
    // contents must equal the stdout form for the same input, so persisting a report
    // never silently reshapes it.
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();
    let report_path = std::env::temp_dir().join(format!("strata-accept-report-{}.md", nanos()));
    let report_str = report_path.to_str().unwrap_or_default();

    let to_file = run(&["report", "--input", result_str, "--output", report_str]);
    let to_stdout = run(&["report", "--input", result_str]);
    let written = std::fs::read_to_string(&report_path).unwrap_or_default();

    let _ = std::fs::remove_file(&result);
    let _ = std::fs::remove_file(&report_path);
    assert_eq!(to_file.code, 0, "report --output exits 0");
    assert_eq!(to_stdout.code, 0, "report to stdout exits 0");
    assert!(
        to_file.stdout.is_empty(),
        "report --output writes nothing to stdout"
    );
    assert_eq!(
        written, to_stdout.stdout,
        "the written file equals the stdout report form"
    );
}

#[test]
fn should_cap_the_candidate_count_per_mode_at_the_requested_k() {
    // -k (a.k.a --candidates) bounds the candidates produced per mode: over-capacity
    // is rich enough to yield two per mode, so -k 2 yields at most two in each, and
    // the per-mode headline count never exceeds the requested ceiling.
    let root = fixture("over-capacity");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "-k",
        "2",
        "--mode",
        "both",
        "--format",
        "summary",
    ]);

    assert_eq!(outcome.code, 0, "a -k run exits 0");
    let candidate_lines = outcome
        .stdout
        .lines()
        .filter(|line| line.trim_start().starts_with("candidate "))
        .count();
    // both modes together render at most 2 candidate headlines each -> at most 4.
    assert!(
        candidate_lines <= 4,
        "two modes capped at k=2 yield at most four candidate headlines, saw {candidate_lines}: {}",
        outcome.stdout
    );
    assert!(
        candidate_lines >= 1,
        "the run still produces at least one candidate: {}",
        outcome.stdout
    );
}

#[test]
fn should_produce_byte_identical_output_for_two_runs_with_the_same_seed() {
    // the deterministic-seed contract: two analyze runs with the same --seed over the
    // same root and config yield byte-identical json, so a seeded run is perfectly
    // reproducible.
    let root = fixture("workspace-rust");
    let root_str = root.to_str().unwrap_or_default();
    let args = [
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--seed",
        "424242",
        "--format",
        "json",
    ];

    let first = run(&args);
    let second = run(&args);

    assert_eq!(first.code, 0, "the first seeded run exits 0");
    assert_eq!(second.code, 0, "the second seeded run exits 0");
    assert_eq!(
        first.stdout, second.stdout,
        "two runs with the same seed are byte-identical"
    );
}

#[test]
fn should_produce_identical_output_regardless_of_the_jobs_count() {
    // determinism under parallelism: forcing single-threaded execution (--jobs 1)
    // yields the same json as the default parallelism, so the result never depends on
    // the worker count.
    let root = fixture("workspace-rust");
    let root_str = root.to_str().unwrap_or_default();
    let single = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--jobs",
        "1",
        "--format",
        "json",
    ]);
    let default = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(single.code, 0, "the single-threaded run exits 0");
    assert_eq!(default.code, 0, "the default-parallelism run exits 0");
    assert_eq!(
        single.stdout, default.stdout,
        "the result is independent of the jobs count"
    );
}

#[test]
fn should_omit_the_anchored_section_for_a_greenfield_only_run() {
    // --mode greenfield emits only the greenfield section: workspace-rust produces a
    // real greenfield candidate, and the summary names `mode greenfield` while
    // carrying no anchored section at all (no `mode anchored`, no `Anchored`).
    let root = fixture("workspace-rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "greenfield",
        "--format",
        "summary",
    ]);

    assert_eq!(outcome.code, 0, "a greenfield-only run exits 0");
    assert!(
        outcome.stdout.contains("mode greenfield:"),
        "the greenfield section is present: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("mode anchored"),
        "a greenfield-only run omits the anchored section"
    );
    assert!(
        !outcome.stdout.contains("Anchored"),
        "a greenfield-only run carries no anchored heading"
    );
}

#[test]
fn should_error_on_an_empty_repository_with_a_stable_code() {
    // an empty repository has no source files, so the engine cannot build a container
    // tree: analyze fails its snapshot validation and exits 1 with the stable
    // `error[SNAPSHOT_INVALID]` code (caused by "container tree has no root"). This is
    // the current, intentional behavior — an empty repo is a hard, coded error rather
    // than an empty success summary. Built at runtime because git cannot commit an
    // empty directory.
    let empty = std::env::temp_dir().join(format!("strata-accept-empty-{}", nanos()));
    let created = std::fs::create_dir_all(&empty).is_ok();
    let empty_str = empty.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        empty_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    let _ = std::fs::remove_dir_all(&empty);
    assert!(created, "the empty temp directory was created");
    assert_eq!(
        outcome.code, 1,
        "an empty repository is a stable error (exit 1), never the gating 2"
    );
    assert!(
        outcome.stderr.contains("error[SNAPSHOT_INVALID]"),
        "the empty-repo error carries its stable code: {}",
        outcome.stderr
    );
}

#[test]
fn should_produce_byte_identical_json_with_and_without_show_suggestions() {
    // --show-suggestions only enriches the summary face; the json face already
    // serializes every candidate's tree, so the flag is a documented no-op there: the
    // json output is byte-identical with and without it.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();
    let base = [
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ];

    let plain = run(&base);
    let mut with_flag = base.to_vec();
    with_flag.push("--show-suggestions");
    let shown = run(&with_flag);

    assert_eq!(plain.code, 0, "the plain json run exits 0");
    assert_eq!(shown.code, 0, "the suggestion json run exits 0");
    assert_eq!(
        plain.stdout, shown.stdout,
        "--show-suggestions is a no-op on the json face"
    );
}

#[test]
fn should_error_on_an_invalid_config_with_a_stable_code() {
    // a malformed --config is a hard, coded error: the config loader rejects the TOML
    // and the run exits 1 with the stable `error[CONFIG_INVALID]` code — never the
    // gating 2, and never a silent fallback to defaults (a present-but-invalid file is
    // an error, unlike an absent one).
    let bad = std::env::temp_dir().join(format!("strata-accept-badconfig-{}.toml", nanos()));
    let written = std::fs::write(&bad, b"this is = not valid = toml ][\n").is_ok();
    let bad_str = bad.to_str().unwrap_or_default();
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze", "--root", root_str, "--config", bad_str, "--format", "summary",
    ]);

    let _ = std::fs::remove_file(&bad);
    assert!(written, "the malformed config file was written");
    assert_eq!(
        outcome.code, 1,
        "an invalid config is a stable error (exit 1), never the gating 2"
    );
    assert!(
        outcome.stderr.contains("error[CONFIG_INVALID]"),
        "the invalid-config error carries its stable code: {}",
        outcome.stderr
    );
}
