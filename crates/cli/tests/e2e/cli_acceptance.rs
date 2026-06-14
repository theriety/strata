//! End-to-end acceptance: the built `strata` binary delivers the expected output.
//!
//! Where `parity.rs` pins the AD-5 library/CLI byte-parity guarantee and the
//! committed goldens, this suite is the user-facing acceptance layer: it drives
//! the *compiled* binary the way a developer or a CI pipeline would (via
//! `assert_cmd`), and asserts the delivered output of every command — `analyze`,
//! `tree`, `report`, `diff`, and `violations` — with `insta` snapshots plus
//! explicit exit-code and invariant assertions.
//!
//! Determinism is engineered, not hoped for. Every analysis is run with an
//! explicit non-existent `--config`, so the binary always falls back to the
//! built-in defaults regardless of the working directory the test harness runs
//! from; an identical snapshot and config therefore yield byte-identical output
//! (the deterministic-by-contract guarantee), and the snapshots never flake on
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
    insta::assert_snapshot!("analyze_summary_rust", outcome.stdout);
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
    insta::assert_snapshot!("analyze_summary_python", outcome.stdout);
}

#[test]
fn should_pin_the_anchor_term_apart_for_the_two_modes() {
    // anchored (mu > 0) carries a non-zero anchor penalty that raises its score
    // above the otherwise-identical greenfield (mu = 0) candidate; pinning both
    // proves the two modes are genuinely distinct and not a shared code path.
    let root = fixture("rust");
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
    assert!(
        anchored.stdout.contains("candidate 1 score 4.3000"),
        "anchored score carries the anchor penalty"
    );
    assert!(
        greenfield.stdout.contains("candidate 1 score 3.3000"),
        "greenfield score omits the anchor penalty"
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
        outcome.stdout.contains("src/lib.rs [file] 6 sloc"),
        "the file node carries its production sloc"
    );
    insta::assert_snapshot!("tree_current_rust", outcome.stdout);
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
        outcome.stdout.contains("pkg/rectangle.py [file] 2 sloc"),
        "both source files appear as file nodes"
    );
    assert!(
        outcome.stdout.contains("pkg/shape.py [file] 2 sloc"),
        "both source files appear as file nodes"
    );
    insta::assert_snapshot!("tree_current_python", outcome.stdout);
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
    insta::assert_snapshot!("tree_candidate_rust_depth2", outcome.stdout);
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
    // the objective J(T) is surfaced per layout as a fixed-precision score line.
    assert!(
        outcome.stdout.contains("Score `3.3000`."),
        "the current objective J(T) is reported"
    );
    insta::assert_snapshot!("report_rust", outcome.stdout);
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
    insta::assert_snapshot!("diff_current_anchored_rust", outcome.stdout);
}

#[test]
fn should_deliver_the_greenfield_diff_for_the_multi_file_python_fixture() {
    // the greenfield (mu = 0) delta over a two-file fixture coalesces the co-moved
    // files into one grouped entry; pinning it covers the multi-symbol narration.
    let result = analyze_to_file("python");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "diff renders and exits 0");
    assert!(
        outcome.stdout.contains("pkg/rectangle.py, pkg/shape.py"),
        "co-moved files coalesce into one grouped entry"
    );
    insta::assert_snapshot!("diff_current_greenfield_python", outcome.stdout);
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
    insta::assert_snapshot!("violations_rust", outcome.stdout);
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
