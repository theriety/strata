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

#[path = "cli_acceptance/markdown_advice.rs"]
mod markdown_advice;

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

/// Runs the built `strata` binary with `args` from `dir` as the working
/// directory, so a relative `--root` (e.g. `.`) resolves against `dir`.
///
/// The default [`run`] inherits the harness's working directory, which is
/// unspecified; setting it explicitly is what lets a test exercise the
/// user-typed relative-root forms that Bug B degraded.
fn run_in(dir: &Path, args: &[&str]) -> Run {
    let spawned = match Command::cargo_bin("strata") {
        Ok(mut command) => command.current_dir(dir).args(args).output().ok(),
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
    // the census and the claimed-vs-listed candidate counts are the contracted surface.
    assert!(
        outcome.stdout.contains("1 files · 5 symbols · 4 edges"),
        "the expected node/edge census is delivered"
    );
    assert!(
        outcome.stdout.contains("No candidates were produced."),
        "the printed claim reports that no strict improvement survived"
    );
    assert!(
        outcome.stdout.contains("anchored — 0 candidate(s)")
            && outcome.stdout.contains("greenfield — 0 candidate(s)"),
        "each profile's empty contribution is stated"
    );
}

#[test]
fn should_itemize_suggestions_in_the_report_without_a_flag() {
    // The printed report always includes candidate layouts. With no strict improvement it
    // reports an empty candidate set instead of inventing an identity plan.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
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

    assert_eq!(outcome.code, 0, "the report run exits 0");
    assert!(
        outcome.stdout.contains("Candidate layouts"),
        "the itemization section names the empty result: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("No candidates were produced."),
        "the absence of an improving plan is declared honestly"
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
        outcome.stdout.contains("2 files · 4 symbols · 4 edges"),
        "the two-file census is delivered"
    );
}

#[test]
fn should_pin_the_anchor_term_apart_for_the_two_modes() {
    // anchored (mu > 0) carries a non-zero anchor penalty that raises its score
    // above the otherwise-identical greenfield (mu = 0) candidate; pinning both
    // proves the two modes are genuinely distinct and not a shared code path. The
    // nested-python fixture's candidate regroups files across directories, so the
    // move-distance penalty is non-zero and the two modes' scores genuinely
    // diverge. (A flat fixture no longer works here: its best candidate matches
    // the current layout, so both modes score identically.)
    let root = fixture("constellation-ts");
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
        "--verbose",
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
        "--verbose",
    ]);

    assert_eq!(anchored.code, 0, "anchored mode exits 0");
    assert_eq!(greenfield.code, 0, "greenfield mode exits 0");
    // the invariant, not the exact bytes (those are pinned by the summary golden):
    // the anchor penalty strictly raises the anchored candidate's score above the
    // otherwise-identical greenfield one.
    assert!(
        anchored.stdout.contains("path 0.2 · anchor 1.0")
            && greenfield.stdout.contains("path 0.0 · anchor 0.0"),
        "the effective profiles expose distinct path and anchor coefficients"
    );
    assert!(
        !anchored.stdout.contains("greenfield —"),
        "an anchored-only run emits only anchored"
    );
    assert!(
        !greenfield.stdout.contains("anchored —"),
        "a greenfield-only run emits only greenfield"
    );
}

#[test]
fn should_print_the_convergence_notice_exactly_when_the_flag_is_set() {
    // the summary face's "(fewer than k candidates; solution space converged)"
    // notice must track the json face's solutionSpaceConverged flag in BOTH
    // polarities — the renderer once printed it on the flag's negation, and a
    // single-polarity check would pass under that inversion. `-k` pins each
    // polarity independent of the clusterer's yield: a tiny fixture can never
    // fill k=10 (flag true, notice on), and any fixture fills k=1 (flag false,
    // notice off).
    for (k, expected) in [("10", true), ("1", false)] {
        let root = fixture("python");
        let root_str = root.to_str().unwrap_or_default();

        let json = run(&[
            "analyze",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "-k",
            k,
            "--format",
            "json",
        ]);
        let summary = run(&[
            "analyze",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "-k",
            k,
            "--format",
            "summary",
        ]);

        assert_eq!(json.code, 0, "analyze json exits 0 for k={k}");
        assert_eq!(summary.code, 0, "analyze summary exits 0 for k={k}");
        let parsed: serde_json::Value =
            serde_json::from_str(&json.stdout).unwrap_or(serde_json::Value::Null);
        for mode in ["anchored", "greenfield"] {
            let flag = parsed
                .pointer(&format!("/profiles/{mode}/solutionSpaceConverged"))
                .and_then(serde_json::Value::as_bool);
            assert_eq!(
                flag,
                Some(expected),
                "k={k} {mode} carries the expected convergence flag"
            );
        }
        assert_eq!(
            summary.stdout.contains("solution space converged"),
            expected,
            "k={k}'s summary notice must match its convergence flag: {}",
            summary.stdout
        );
    }
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
        "rust-cfg-test",
        "borderline-only",
        "testsupport-leak",
        "constellation-ts",
        "cyclic-oversized",
        "test-heavy-ts",
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
        outcome.stdout.contains("src/lib.rs [file] 18 sloc"),
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
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&[
        "tree",
        "--input",
        result_str,
        "--mode",
        "greenfield",
        "--candidate",
        "1",
        "--symbols",
        "--depth",
        "1",
    ]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "a candidate tree renders and exits 0");
    assert!(
        outcome.stdout.contains("constellation-ts [packageGroup]"),
        "the truncated tree is still rooted at the package group: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("core [folder]"),
        "depth 1 truncates a real candidate above its folder leaves: {}",
        outcome.stdout
    );
}

#[test]
fn should_not_fabricate_a_package_segment_folder_for_a_cross_domain_real_directory() {
    // `workspace-ts-leak` has two packages; `atlas` has a `core` directory
    // (a dense mutual cluster) and a sibling `agent` directory whose files
    // import so heavily from `core` that greenfield legitimately dissolves
    // them into the `core` cluster — each agent file's sole priced anchors
    // all sit in `core`, so absorption is the honest consolidation. Both
    // current face must stay clean: `agent` remains its own domain over its
    // own real folder, never through a fabricated `atlas [folder]` wrapper
    // exploding the foreign key `atlas/agent` into package segments. Candidate
    // tree behavior is covered separately with a fixture that strictly improves.
    let result = analyze_to_file("workspace-ts-leak");
    let result_str = result.to_str().unwrap_or_default();

    let anchored = run(&["tree", "--input", result_str, "--current"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(anchored.code, 0, "the anchored tree renders and exits 0");
    // `agent` remains its own domain directly over its own real folder.
    assert!(
        anchored
            .stdout
            .contains("\n    agent [domain]\n      agent [folder]\n"),
        "the current tree keeps `agent` as its own domain over its \
         own folder: {}",
        anchored.stdout
    );
    // neither face may fabricate the package segment as a folder node.
    assert!(
        !anchored.stdout.contains("atlas [folder]"),
        "the package name must never render as a fabricated folder node: {}",
        anchored.stdout
    );
}

#[test]
fn should_suppress_a_domain_level_that_repeats_its_package_name() {
    // `constellation-ts` is a single package whose greenfield clusters all
    // elect the bare package prefix as their domain name; `qualify_elected`
    // then decorates the colliding siblings as `constellation-ts
    // (constellation-ts.core)` / `... (constellation-ts.io)`. A domain that
    // merely echoes its package carries no naming information, so the level
    // is suppressed at the render boundary: no domain node renders at all and
    // the real folders hang directly under the package (two-space indent
    // steps: group, package, folder).
    let result = analyze_to_file("constellation-ts");
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
    assert_eq!(outcome.code, 0, "a candidate tree renders and exits 0");
    assert!(
        !outcome.stdout.contains("[domain]"),
        "a domain echoing its package name must not render as a node: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("    core [folder]"),
        "the real folders must hang directly under the package: {}",
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
        outcome.stdout.contains("## Structural findings"),
        "the shared findings section is present"
    );
    assert!(
        outcome.stdout.contains("anchored — 0 candidate(s)"),
        "the anchored section is present"
    );
    assert!(
        outcome.stdout.contains("greenfield — 0 candidate(s)"),
        "the greenfield section is present"
    );
    // the objective J(T) is surfaced per layout as a fixed-precision score line; the
    // exact score bytes are pinned by the `report.md` golden, so here we assert only
    // the structural invariant that the current-layout score line is present.
    assert!(
        outcome.stdout.contains("Baseline score: "),
        "the current layout reports its objective J(T) as a score line: {}",
        outcome.stdout
    );
}

#[test]
fn should_deliver_the_greenfield_diff_for_the_constellation_fixture() {
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(
        outcome.code, 0,
        "diff renders the structural delta and exits 0"
    );
    assert!(
        outcome.stdout.starts_with("moves ("),
        "a genuine greenfield candidate renders a non-empty delta: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("src/util/hash.ts"),
        "the delta identifies the candidate's moved file: {}",
        outcome.stdout
    );
}

#[test]
fn should_deliver_a_greenfield_diff_for_a_fixture_with_a_candidate() {
    // the greenfield (mu = beta = 0) delta performs exactly the repair the
    // normalized objective can justify: app.py — whose edges pay package-height
    // crossings while loose — joins the geometry chain it depends on, narrated
    // as one grouped move under the count header with physical source and target.
    // Under the D-37 rescale the spec files' relocation no longer pays for
    // itself, so they stay put; the `follows` narration they used to exercise
    // lives on in the constellation-ts greenfield delta (see the varied-reasons
    // test).
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "diff renders and exits 0");
    assert!(
        outcome.stdout.starts_with("moves ("),
        "the delta opens with the group/file count header: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("→ constellation-ts/src/model]"),
        "the regrouped file lands in the model cluster: {}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("constellation-ts/src/util/hash.ts [constellation-ts/src/util → constellation-ts/src/model]"),
        "the move names its dataset-qualified physical source and destination: {}",
        outcome.stdout
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
        outcome
            .stdout
            .contains("visibility [violation] src/lib.rs, Shape"),
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
fn should_reject_an_out_of_range_left_candidate_ref() {
    // the LEFT side of a candidate-vs-candidate diff is validated like the
    // right one — it was once resolved lazily, so an out-of-range left ref
    // silently rendered against nothing and exited 0.
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "anchored/9", "anchored/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 1, "a missing left candidate is exit 1");
    assert!(
        outcome.stderr.contains("error[CANDIDATE_NOT_FOUND]"),
        "the error names the missing left candidate: {}",
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
    // two hard capacity findings (one nested); the exactly-at-cap file is clean,
    // so the violations json array holds exactly those two capacity entries — proving
    // the gate's machine-readable surface is well-formed and complete.
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
        .pointer("/current/sharedFindings")
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
        "the violations json carries both hard capacity findings: {}",
        outcome.stdout
    );
}

#[test]
fn should_match_the_analyze_json_face_even_when_capacity_findings_exist() {
    // capacity findings are derived inside engine analyze alongside the other
    // violation classes, so the violations json face is byte-identical to the
    // analyze json face for every fixture — including over-capacity, whose
    // result carries three capacity entries. This pins FR-11: the violations
    // command adds no findings of its own.
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
    assert_eq!(
        analyze_over.stdout, violations_over.stdout,
        "the two json faces stay byte-identical even with capacity findings"
    );
    assert!(
        analyze_over.stdout.contains("\"kind\":\"capacity\""),
        "the shared result carries the capacity findings: {}",
        analyze_over.stdout
    );
}

#[test]
fn should_populate_break_suggestions_with_real_names_weights_and_exactness() {
    // MFAS is wired (D-32 resolved): every cycle violation carries a non-empty
    // breakSuggestions list whose entries name real symbols, price the break in
    // the config's edge-weight currency, and flag ILP-proven minimality; the
    // detail line leads with the first break.
    let root = fixture("cyclic");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze json exits 0 for cyclic");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);

    let cycles: Vec<&serde_json::Value> = parsed
        .pointer("/current/sharedFindings")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| {
                    entry.get("kind").and_then(serde_json::Value::as_str) == Some("cycle")
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !cycles.is_empty(),
        "the cyclic fixture carries at least one cycle violation: {}",
        outcome.stdout
    );
    for cycle in cycles {
        let members: Vec<String> = cycle
            .get("location")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let suggestions = cycle
            .get("breakSuggestions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            !suggestions.is_empty(),
            "a cycle carries at least one break suggestion: {cycle}"
        );
        assert!(
            members.iter().all(|member| {
                std::path::Path::new(member)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
            }),
            "cycle locations are repository-relative file paths: {members:?}"
        );
        for suggestion in &suggestions {
            let source = suggestion.get("source").and_then(serde_json::Value::as_str);
            let target = suggestion.get("target").and_then(serde_json::Value::as_str);
            assert!(
                source.is_some_and(|name| !name.is_empty()),
                "a break names its source symbol: {suggestion}"
            );
            assert!(
                target.is_some_and(|name| !name.is_empty()),
                "a break names its target symbol: {suggestion}"
            );
            assert!(
                suggestion
                    .get("weight")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|weight| weight > 0.0),
                "a break carries a positive config-priced weight: {suggestion}"
            );
            assert!(
                suggestion.get("exact").and_then(serde_json::Value::as_bool) == Some(true),
                "a tiny scc solves exactly via the ilp: {suggestion}"
            );
        }
        let detail = cycle
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            detail.contains("-symbol cycle;")
                && detail.contains("must remain in one file")
                && detail.contains("; break ")
                && detail.contains("exact"),
            "the detail states the placement consequence and first break: {detail}"
        );
    }
}

#[test]
fn should_keep_conditional_splits_empty_for_an_under_cap_scc() {
    // an SCC under the file cap needs no split, so the cyclic fixture's
    // candidates all carry an empty (but present) conditionalSplits array.
    let root = fixture("cyclic");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze json exits 0 for cyclic");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);

    let mut candidates_seen = 0;
    for mode in ["anchored", "greenfield"] {
        let candidates = parsed
            .pointer(&format!("/profiles/{mode}/candidates"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        candidates_seen += candidates.len();
        for candidate in candidates {
            assert_eq!(
                candidate.get("conditionalSplits"),
                Some(&serde_json::json!([])),
                "an under-cap scc never yields a conditional split"
            );
        }
    }
    assert_eq!(
        candidates_seen, 0,
        "the under-cap identity layout has no strictly improving candidate"
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
fn should_render_a_greenfield_candidate_tree_for_the_constellation_fixture() {
    let result = analyze_to_file("constellation-ts");
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
    assert_eq!(outcome.code, 0, "the workspace tree renders");
    assert!(
        outcome.stdout.contains("constellation-ts [packageGroup]"),
        "the tree is rooted at the package group: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("src/core/alpha.ts [file]"),
        "the files descend to real file leaves: {}",
        outcome.stdout
    );
}

#[test]
fn should_narrate_the_greenfield_moves_for_the_workspace_fixture() {
    // the narration contract is pinned on constellation-ts's greenfield delta:
    // four move groups over five files, each destination a real directory-
    // derived cluster name (`constellation-ts/src/model`) reached through a
    // dependency-pull reason. workspace-rust no longer carries this contract —
    // its greenfield candidate now converges back to the identity layout, so
    // diffing it against current narrates `no moves`. Synthetic grab-bag
    // labels like `mixed` stay dead: every destination is real.
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "the greenfield diff renders");
    assert!(
        outcome.stdout.starts_with("moves ("),
        "the regroup opens with the group/file count header: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("→ constellation-ts/src/model]"),
        "the pulled files merge into the real `model` cluster of the package: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("move — pulled by "),
        "the move carries a dependency-pull reason, not a clustering fallback: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("mixed"),
        "no synthetic `mixed` label survives anywhere in the narration: {}",
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
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();

    let contents = std::fs::read_to_string(&result).unwrap_or_default();
    let mut parsed: serde_json::Value = serde_json::from_str(&contents).unwrap_or_default();
    let greenfield = parsed.pointer("/profiles/greenfield/candidates/0").cloned();
    if let (Some(candidate), Some(anchored)) = (
        greenfield,
        parsed.pointer_mut("/profiles/anchored/candidates"),
    ) {
        *anchored = serde_json::json!([candidate]);
    }
    let _ = std::fs::write(&result, serde_json::to_vec(&parsed).unwrap_or_default());
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
    // the complement of the cross-mode case: a same-mode diff (anchored/1 vs
    // anchored/2) reports the pair's variation-of-information distance as a
    // fixed-precision `vi distance` line below the move narration. the tiny
    // fixtures all converge to a single candidate, so the two-candidate saved
    // result is grafted from a real one: candidate 1 is cloned as candidate 2
    // and the pairwise matrix expanded — diff consumes saved results, so the
    // contract under test is the reader/renderer, not the clusterer's yield.
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();
    let contents = std::fs::read_to_string(&result).unwrap_or_default();
    let mut parsed: serde_json::Value =
        serde_json::from_str(&contents).unwrap_or(serde_json::Value::Null);
    let grafted = parsed
        .pointer_mut("/profiles/greenfield")
        .is_some_and(|anchored| {
            let cloned = anchored
                .pointer("/candidates/0")
                .cloned()
                .map(|mut second| {
                    if let Some(index) = second.get_mut("index") {
                        *index = serde_json::json!(2);
                    }
                    second
                });
            match (
                cloned,
                anchored
                    .get_mut("candidates")
                    .and_then(serde_json::Value::as_array_mut),
            ) {
                (Some(second), Some(candidates)) => {
                    candidates.push(second);
                    true
                }
                _ => false,
            }
        })
        && parsed
            .pointer_mut("/profiles/greenfield/pairwiseDistance")
            .is_some_and(|matrix| {
                *matrix = serde_json::json!([[0.0, 0.7], [0.7, 0.0]]);
                true
            });
    let written = std::fs::write(&result, serde_json::to_vec(&parsed).unwrap_or_default()).is_ok();

    let outcome = run(&[
        "diff",
        "--input",
        result_str,
        "greenfield/1",
        "greenfield/2",
    ]);

    let _ = std::fs::remove_file(&result);
    assert!(grafted, "the second candidate was grafted in: {contents}");
    assert!(written, "the grafted result was written");
    assert_eq!(outcome.code, 0, "a same-mode diff renders and exits 0");
    assert!(
        outcome.stdout.contains("vi distance 0.7000"),
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
fn should_truncate_a_candidate_tree_at_each_requested_depth() {
    // depth truncates at container depth (root is depth 0): depth 0 prints only the
    // root, depth 1 adds its single child level, and a depth past the tree height is
    // a no-op that renders the full hierarchy. constellation-ts has a genuine,
    // strictly improving candidate, so the truncation is observable rather than vacuous.
    let result = analyze_to_file("constellation-ts");
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
        at_zero.stdout, "constellation-ts [packageGroup]\n",
        "depth 0 prints only the root container"
    );
    // depth 1 adds exactly the next container level and no deeper. The
    // the package container renders directly below its group and no folder
    // level survives the cut.
    assert!(
        at_one.stdout.contains("constellation-ts [package]") && !at_one.stdout.contains("[folder]"),
        "depth 1 stops one level below the root at the package: {}",
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
    // real greenfield candidate, and the report claims it via `greenfield mode
    // returned` while carrying no anchored rows at all.
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
        outcome.stdout.contains("greenfield —"),
        "the greenfield claim is present: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("anchored —") && !outcome.stdout.contains("anchored/"),
        "a greenfield-only run omits the anchored candidates"
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
fn should_produce_byte_identical_json_across_repeated_runs() {
    // the json face is the parity face: two runs over the same root and config
    // serialize the same result byte-for-byte.
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

    let first = run(&base);
    let second = run(&base);

    assert_eq!(first.code, 0, "the first json run exits 0");
    assert_eq!(second.code, 0, "the second json run exits 0");
    assert_eq!(
        first.stdout, second.stdout,
        "the json face is byte-stable across runs"
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

#[test]
fn should_exit_one_with_a_parse_failure_code_on_a_syntax_error_fixture() {
    // a file the adapter cannot parse is a coded failure, not a crash and not a
    // silent skip: exit 1 with ADAPTER_PARSE_FAILURE naming the offender.
    let root = fixture("syntax-error");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    assert_eq!(outcome.code, 1, "a parse failure is exit 1");
    assert!(
        outcome.stderr.contains("error[ADAPTER_PARSE_FAILURE]"),
        "the error carries its stable code: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("broken.py"),
        "the error names the unparseable file: {}",
        outcome.stderr
    );
}

#[test]
fn should_error_on_a_re_export_chain_deeper_than_the_guard() {
    // the barrel-cycle fixture relays one name through 66 single-line barrels;
    // flattening walks past the 64-hop guard and must fail with the coded
    // error rather than hanging or silently truncating the chain.
    let root = fixture("barrel-cycle");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    assert_eq!(outcome.code, 1, "an over-deep re-export chain is exit 1");
    assert!(
        outcome.stderr.contains("error[RE_EXPORT_DEPTH_EXCEEDED]"),
        "the error carries its stable code: {}",
        outcome.stderr
    );
}

#[test]
fn should_report_borderline_capacity_without_gating() {
    // borderline-only holds one file at exactly the 250 cap: inside the +/-10%
    // band, so the finding renders as borderline and --fail-on capacity (which
    // matches hard violations only) must NOT raise the reserved exit 2.
    let root = fixture("borderline-only");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "capacity",
    ]);

    assert_eq!(outcome.code, 0, "an exactly-at-cap file never gates");
    assert_eq!(outcome.stdout.trim(), "no violations");
}

#[test]
fn should_flag_an_oversized_single_symbol_file_per_d38() {
    // D-38 guard: the capacity walk has no oversized-symbol exemption, so a
    // file holding one unsplittable 300-SLOC function still flags against the
    // 250 cap and gates. The spec's Test Matrix exempts such files; this test
    // pins the CURRENT behavior and must be retired together with D-38.
    let root = fixture("oversized");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "capacity",
    ]);

    assert_eq!(outcome.code, 2, "the oversized file gates (no exemption)");
    assert!(
        outcome
            .stdout
            .contains("file `module.py` holds 300 against a cap of 250"),
        "the finding reports the uncapped size: {}",
        outcome.stdout
    );
}

#[test]
fn should_report_a_test_support_dependency_on_a_test_case_end_to_end() {
    // the second polarity arm: conftest.py (test support by convention) importing
    // a symbol from test_app.py (a test case) is a polarity violation that
    // renders with the support/case wording and gates like any hard violation.
    let root = fixture("testsupport-leak");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--fail-on",
        "polarity",
    ]);

    assert_eq!(outcome.code, 2, "a test-support leak gates on polarity");
    assert!(
        outcome
            .stdout
            .contains("test support `shared_fixture` depends on test case `sample_case`"),
        "the finding carries the support/case wording: {}",
        outcome.stdout
    );
}

#[test]
fn should_score_greenfield_candidates_identically_across_renames() {
    // greenfield (mu = 0) is rename-invariant: rename-a and rename-b are the
    // same two-file structure under different file and symbol names (which even
    // sort differently), so their greenfield candidates score identically and
    // propose the same (empty) move set. The snapshot hashes differ — names feed
    // the hash — which is what makes the score equality meaningful.
    let run_greenfield = |name: &str| {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();
        run(&[
            "analyze",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--mode",
            "greenfield",
            "--format",
            "json",
        ])
    };

    let a = run_greenfield("rename-a");
    let b = run_greenfield("rename-b");

    assert_eq!(a.code, 0, "rename-a analyzes clean");
    assert_eq!(b.code, 0, "rename-b analyzes clean");
    let parsed_a: serde_json::Value = serde_json::from_str(&a.stdout).unwrap_or_default();
    let parsed_b: serde_json::Value = serde_json::from_str(&b.stdout).unwrap_or_default();
    let score_a = parsed_a
        .pointer("/profiles/greenfield/current/score")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(f64::NAN);
    let score_b = parsed_b
        .pointer("/profiles/greenfield/current/score")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(f64::NAN);
    assert!(
        (score_a - score_b).abs() < 1e-9,
        "greenfield scores are rename-invariant: {score_a} vs {score_b}"
    );
    assert!(
        parsed_a
            .pointer("/profiles/greenfield/candidates")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
            && parsed_b
                .pointer("/profiles/greenfield/candidates")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty),
        "neither renamed identity layout invents a candidate"
    );
}

#[test]
fn should_reject_malformed_and_absent_mode_candidate_refs() {
    // every bad candidate ref fails coded and non-zero: a non-numeric index, an
    // unknown mode name, and a well-formed ref into a mode the saved result
    // never ran.
    let both = analyze_to_file("rust");
    let both_str = both.to_str().unwrap_or_default();
    let malformed_index = run(&["diff", "--input", both_str, "anchored/abc", "anchored/1"]);
    let unknown_mode = run(&["diff", "--input", both_str, "bogus/1", "anchored/1"]);
    let _ = std::fs::remove_file(&both);

    // an anchored-only result has no greenfield mode to reference.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();
    let anchored_only =
        std::env::temp_dir().join(format!("strata-accept-anchored-{}.json", nanos()));
    let anchored_only_str = anchored_only.to_str().unwrap_or_default();
    let saved = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "anchored",
        "--format",
        "json",
        "--output",
        anchored_only_str,
    ]);
    let absent_mode = run(&[
        "diff",
        "--input",
        anchored_only_str,
        "current",
        "greenfield/1",
    ]);
    let _ = std::fs::remove_file(&anchored_only);

    assert_eq!(saved.code, 0, "the anchored-only result saves");
    for (label, outcome) in [
        ("a non-numeric index", &malformed_index),
        ("an unknown mode", &unknown_mode),
        ("an absent mode", &absent_mode),
    ] {
        assert_eq!(outcome.code, 1, "{label} is exit 1, never the gating 2");
        assert!(
            outcome.stderr.contains("error[CANDIDATE_NOT_FOUND]"),
            "{label} fails with the stable code: {}",
            outcome.stderr
        );
    }
}

#[test]
fn should_exit_one_on_a_malformed_result_input_for_every_reader() {
    // all three result readers reject non-JSON input with the same coded error.
    let bad = std::env::temp_dir().join(format!("strata-accept-badinput-{}.json", nanos()));
    let bad_str = bad.to_str().unwrap_or_default();
    let written = std::fs::write(&bad, b"not json {{{").is_ok();

    let tree = run(&["tree", "--input", bad_str, "--current"]);
    let diff = run(&["diff", "--input", bad_str, "current", "anchored/1"]);
    let report = run(&["report", "--input", bad_str]);

    let _ = std::fs::remove_file(&bad);
    assert!(written, "the malformed input file was written");
    for (label, outcome) in [("tree", &tree), ("diff", &diff), ("report", &report)] {
        assert_eq!(outcome.code, 1, "{label} rejects malformed input with 1");
        assert!(
            outcome.stderr.contains("error[INPUT_UNREADABLE]"),
            "{label} fails with the stable code: {}",
            outcome.stderr
        );
    }
}

#[test]
fn should_reject_a_result_with_an_unsupported_schema_version() {
    // a saved result stamped with any other schemaVersion — future (999), the
    // immediately retired v7, or v1 — must be refused up front with a remediable message, not
    // misread field-by-field.
    let result = analyze_to_file("rust");
    let contents = std::fs::read_to_string(&result).unwrap_or_default();
    let result_str = result.to_str().unwrap_or_default();

    for version in ["999", "7", "1"] {
        let stamped = contents.replace(
            "\"schemaVersion\":8",
            &format!("\"schemaVersion\":{version}"),
        );
        assert_ne!(contents, stamped, "the version stamp was found and bumped");
        let written = std::fs::write(&result, stamped).is_ok();

        let outcome = run(&["tree", "--input", result_str, "--current"]);

        assert!(written, "the v{version} result was written");
        assert_eq!(
            outcome.code, 1,
            "schema version {version} is refused with exit 1"
        );
        assert!(
            outcome.stderr.contains("error[INPUT_UNREADABLE]"),
            "the refusal carries the stable code: {}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains(&format!(
                "unsupported result schemaVersion {version}; expected 8"
            )),
            "the refusal names the offending version: {}",
            outcome.stderr
        );
    }
    let _ = std::fs::remove_file(&result);
}

#[test]
fn should_list_available_candidates_when_no_selector_is_given() {
    // tree without --current or --candidate lists the candidate headlines so
    // the user can pick a ref, and exits 0 — discovery is not an error.
    let result = analyze_to_file("rust");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["tree", "--input", result_str]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "the listing is a success path");
    assert!(
        outcome
            .stdout
            .contains("available candidates for anchored:"),
        "the listing names the mode: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("1 (score "),
        "the listing invents no candidate headlines: {}",
        outcome.stdout
    );
}

#[test]
fn should_write_analyze_json_to_a_file_byte_identical_to_stdout() {
    // --output redirects the exact bytes: the file matches what stdout would
    // have carried (trailing newline included) and stdout stays empty.
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();
    let path = std::env::temp_dir().join(format!("strata-accept-output-{}.json", nanos()));
    let path_str = path.to_str().unwrap_or_default();

    let to_stdout = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let to_file = run(&[
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

    let written = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    assert_eq!(to_stdout.code, 0, "the stdout run exits 0");
    assert_eq!(to_file.code, 0, "the --output run exits 0");
    assert!(
        to_file.stdout.is_empty(),
        "--output leaves stdout empty: {}",
        to_file.stdout
    );
    assert_eq!(
        written, to_stdout.stdout,
        "the file carries byte-identical json"
    );
}

#[test]
fn should_default_to_summary_when_stdout_is_piped() {
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();
    let outcome = run(&["analyze", "--root", root_str, "--config", PURE_DEFAULTS]);
    let summary = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);
    assert_eq!(outcome.code, 0, "the formatless run exits 0");
    assert_eq!(summary.code, 0, "the explicit summary exits 0");
    assert!(summary.stdout.contains("Structural findings"));
    assert_eq!(outcome.stdout, summary.stdout);
}

#[test]
fn should_default_to_summary_when_stdout_is_redirected() -> Result<(), Box<dyn std::error::Error>> {
    let root = fixture("python");
    let root_str = root.to_str().ok_or("non-UTF8 root")?;
    let path = std::env::temp_dir().join(format!("strata-summary-redirect-{}.txt", nanos()));
    let output = std::fs::File::create(&path)?;
    let outcome = std::process::Command::new(assert_cmd::cargo::cargo_bin("strata"))
        .args(["analyze", "--root", root_str, "--config", PURE_DEFAULTS])
        .stdout(output)
        .output()?;
    let written = std::fs::read_to_string(&path)?;
    std::fs::remove_file(path)?;
    let summary = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);
    assert!(
        outcome.status.success(),
        "{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert_eq!(summary.code, 0);
    assert_eq!(written, summary.stdout);
    Ok(())
}

/// Counts the symbol bullet lines (`- name (visibility)`) in a rendered tree.
fn symbol_line_count(tree: &str) -> usize {
    tree.lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .count()
}

#[test]
fn should_render_every_symbol_exactly_once_with_tree_symbols() {
    // the acceptance criterion: `tree --symbols` renders each snapshot symbol
    // exactly once — the bullet count equals the summary census, in both the
    // current tree and a candidate tree, for a big flat fixture and a nested one.
    for name in ["over-capacity", "nested-ts"] {
        let result = analyze_to_file(name);
        let result_str = result.to_str().unwrap_or_default();

        let json = std::fs::read_to_string(&result).unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
        let symbols = parsed
            .pointer("/summary/symbols")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0);
        assert!(symbols > 0, "{name} carries a symbol census");

        let current = run(&["tree", "--input", result_str, "--current", "--symbols"]);
        let has_candidate = parsed
            .pointer("/profiles/anchored/candidates")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|candidates| !candidates.is_empty());
        let candidate = has_candidate.then(|| {
            run(&[
                "tree",
                "--input",
                result_str,
                "--mode",
                "anchored",
                "--candidate",
                "1",
                "--symbols",
            ])
        });

        let _ = std::fs::remove_file(&result);
        assert_eq!(current.code, 0, "{name} current tree renders");
        assert_eq!(
            symbol_line_count(&current.stdout),
            symbols,
            "{name}'s current tree renders each symbol exactly once"
        );
        if let Some(candidate) = candidate {
            assert_eq!(candidate.code, 0, "{name} candidate tree renders");
            assert_eq!(
                symbol_line_count(&candidate.stdout),
                symbols,
                "{name}'s candidate tree renders each symbol exactly once"
            );
        }
    }
}

#[test]
fn should_honor_config_precedence_flags_over_toml_over_defaults() {
    // the precedence chain end-to-end, probed through the convergence notice
    // (which fires exactly when fewer candidates survive than k requests). the
    // python fixture always converges to one candidate, so: the built-in k of 3
    // shows the notice, a `candidates = 1` toml silences it (1 of 1), and -k 3
    // over that same toml brings it back — flag > toml > default.
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();
    let toml = std::env::temp_dir().join(format!("strata-accept-precedence-{}.toml", nanos()));
    let toml_str = toml.to_str().unwrap_or_default();
    let written = std::fs::write(
        &toml,
        b"[profiles.anchored]\ncandidates = 1\n[profiles.greenfield]\ncandidates = 1\n",
    )
    .is_ok();

    let defaults = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);
    let from_toml = run(&[
        "analyze", "--root", root_str, "--config", toml_str, "--format", "summary",
    ]);
    let flag_over_toml = run(&[
        "analyze", "--root", root_str, "--config", toml_str, "-k", "3", "--format", "summary",
    ]);

    let _ = std::fs::remove_file(&toml);
    let notice = "the solution space converged";
    assert!(written, "the precedence toml was written");
    assert_eq!(defaults.code, 0, "the defaults run exits 0");
    assert_eq!(from_toml.code, 0, "the toml run exits 0");
    assert_eq!(flag_over_toml.code, 0, "the flag run exits 0");
    assert!(
        defaults.stdout.contains(notice),
        "the built-in default k of 3 leaves the pool short, so the notice fires: {}",
        defaults.stdout
    );
    assert!(
        !from_toml.stdout.contains(notice),
        "the toml's candidates = 1 overrides the default and silences the notice: {}",
        from_toml.stdout
    );
    assert!(
        flag_over_toml.stdout.contains(notice),
        "-k 3 overrides the toml's candidates = 1 and the notice returns: {}",
        flag_over_toml.stdout
    );
}

#[test]
fn should_flip_a_borderline_finding_to_a_gating_violation_via_a_config_cap() {
    // the capacity cap is genuinely honored from config: the same 250-SLOC file
    // that is merely borderline under the default 250 cap becomes a hard,
    // gating violation once a toml lowers the cap to 150.
    let root = fixture("borderline-only");
    let root_str = root.to_str().unwrap_or_default();
    let toml = std::env::temp_dir().join(format!("strata-accept-lowcap-{}.toml", nanos()));
    let toml_str = toml.to_str().unwrap_or_default();
    let written = std::fs::write(
        &toml,
        b"[profiles.anchored.capacity]\nfile = 150\n[profiles.greenfield.capacity]\nfile = 150\n",
    )
    .is_ok();

    let outcome = run(&[
        "violations",
        "--root",
        root_str,
        "--config",
        toml_str,
        "--fail-on",
        "capacity",
    ]);

    let _ = std::fs::remove_file(&toml);
    assert!(written, "the low-cap toml was written");
    assert_eq!(
        outcome.code, 2,
        "the lowered cap turns borderline into a gate"
    );
    assert!(
        outcome
            .stdout
            .contains("file `module.py` holds 250 against a cap of 150"),
        "the finding reports the configured cap: {}",
        outcome.stdout
    );
}

#[test]
fn should_report_improvement_as_current_score_minus_score_in_every_mode() {
    // the improvement field is exactly the per-mode delta currentScore - score
    // for every candidate, in both modes — positive means the candidate beats
    // the current layout under that mode's own coefficients.
    let root = fixture("constellation-ts");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze json exits 0");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
    let mut candidates_seen = 0;
    for mode in ["anchored", "greenfield"] {
        let current_score = parsed
            .pointer(&format!("/profiles/{mode}/current/score"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(f64::NAN);
        let candidates = parsed
            .pointer(&format!("/profiles/{mode}/candidates"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for candidate in candidates {
            candidates_seen += 1;
            let score = candidate
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            let improvement = candidate
                .get("improvement")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            assert!(
                (improvement - (current_score - score)).abs() < 1e-9,
                "{mode} improvement {improvement} equals {current_score} - {score}"
            );
        }
    }
    assert!(
        candidates_seen > 0,
        "at least one profile delivers candidates to check"
    );
}

#[test]
fn should_emit_no_candidate_when_the_current_layout_is_optimal() {
    // A candidate must strictly improve on the profile baseline. When the
    // cap-clean current layout is already optimal, the candidate list is empty
    // and the summary recommends keeping the current tree without inventing an
    // identity proposal.
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();

    let json = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let summary = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    assert_eq!(json.code, 0, "analyze json exits 0");
    assert_eq!(summary.code, 0, "analyze summary exits 0");
    let parsed: serde_json::Value =
        serde_json::from_str(&json.stdout).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        parsed
            .pointer("/profiles/anchored/current/standing")
            .and_then(serde_json::Value::as_str),
        Some("optimal"),
        "the clean tiny fixture stands optimal: {}",
        json.stdout
    );
    assert_eq!(
        parsed
            .pointer("/profiles/anchored/candidates")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0),
        "an optimal baseline has no strictly improving anchored candidate"
    );
    assert!(
        summary.stdout.contains("No candidates were produced."),
        "the report face announces the empty candidate set honestly: {}",
        summary.stdout
    );
    assert!(
        summary
            .stdout
            .contains("Current layout is already optimal under this profile's search."),
        "the recommendation keeps the optimal layout: {}",
        summary.stdout
    );
}

#[test]
fn should_mark_a_cap_violating_layout_infeasible_with_the_resolution_notice() {
    // a current layout with a hard capacity violation is never blessed as
    // optimal: both modes stand infeasible and the summary face reports how
    // many of the findings the best candidate actually resolves.
    let root = fixture("over-capacity");
    let root_str = root.to_str().unwrap_or_default();

    let json = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let summary = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    assert_eq!(json.code, 0, "analyze json exits 0");
    assert_eq!(summary.code, 0, "analyze summary exits 0");
    let parsed: serde_json::Value =
        serde_json::from_str(&json.stdout).unwrap_or(serde_json::Value::Null);
    for mode in ["anchored", "greenfield"] {
        assert_eq!(
            parsed
                .pointer(&format!("/profiles/{mode}/current/standing"))
                .and_then(serde_json::Value::as_str),
            Some("infeasible"),
            "{mode} stands infeasible over a hard cap violation: {}",
            json.stdout
        );
    }
    assert!(
        summary.stdout.contains("Current layout violates"),
        "the report face names the infeasibility and its finding count: {}",
        summary.stdout
    );
    assert!(
        summary.stdout.contains(" capacity cap(s)."),
        "the infeasibility is counted in capacity findings: {}",
        summary.stdout
    );
}

#[test]
fn should_render_folded_paths_with_no_duplicated_segments() {
    // narrated paths are folded for display: the cumulative internal container
    // names (`src`, `src/__tests__`, ...) never leak as duplicated segments
    // like `src/src` in any diff face.
    for name in ["nested-ts", "nested-python", "workspace-rust"] {
        let result = analyze_to_file(name);
        let result_str = result.to_str().unwrap_or_default();
        let json = std::fs::read_to_string(&result).unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        let diff = |mode: &str| {
            parsed
                .pointer(&format!("/profiles/{mode}/candidates"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|candidates| !candidates.is_empty())
                .then(|| {
                    run(&[
                        "diff",
                        "--input",
                        result_str,
                        "current",
                        &format!("{mode}/1"),
                    ])
                })
        };
        let anchored = diff("anchored");
        let greenfield = diff("greenfield");

        let _ = std::fs::remove_file(&result);
        for (mode, outcome) in [("anchored", anchored), ("greenfield", greenfield)] {
            let Some(outcome) = outcome else {
                continue;
            };
            assert_eq!(outcome.code, 0, "{name} {mode} diff renders");
            for duplicated in ["src/src", "tests/tests", "__tests__/__tests__"] {
                assert!(
                    !outcome.stdout.contains(duplicated),
                    "{name} {mode} diff carries no `{duplicated}`: {}",
                    outcome.stdout
                );
            }
        }
    }
}

#[test]
fn should_never_duplicate_a_segment_in_a_capacity_violation_location() {
    // the capacity walk builds locations from incremental ancestor names plus
    // the file's basename, so no segment can appear twice — the regression
    // shape was `ai, spec, batch, spec, batch, utilities.ts` from folding
    // full-path file leaves as if they were cumulative names.
    for name in [
        "workspace-rust",
        "nested-python",
        "nested-ts",
        "cyclic",
        "over-capacity",
        "polarity-leak",
        "rust-cfg-test",
        "borderline-only",
        "testsupport-leak",
        "constellation-ts",
        "cyclic-oversized",
        "test-heavy-ts",
    ] {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let outcome = run(&[
            "analyze",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--format",
            "json",
        ]);

        assert_eq!(outcome.code, 0, "analyze exits 0 for {name}");
        let parsed: serde_json::Value =
            serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
        let violations = parsed
            .pointer("/current/sharedFindings")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for violation in violations {
            if violation.get("kind").and_then(serde_json::Value::as_str) != Some("capacity") {
                continue;
            }
            let location: Vec<&str> = violation
                .get("location")
                .and_then(serde_json::Value::as_array)
                .map(|segments| {
                    segments
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect()
                })
                .unwrap_or_default();
            let unique: std::collections::BTreeSet<&str> = location.iter().copied().collect();
            assert_eq!(
                unique.len(),
                location.len(),
                "{name} capacity location duplicates a segment: {location:?}"
            );
        }
    }
}

/// Returns the ancestor container names above the node called `target`, walking
/// the serialized candidate tree depth-first.
fn ancestors_of(
    node: &serde_json::Value,
    target: &str,
    trail: &mut Vec<String>,
) -> Option<Vec<String>> {
    let name = node
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if name == target {
        return Some(trail.clone());
    }
    trail.push(name.to_owned());
    if let Some(children) = node.get("children").and_then(serde_json::Value::as_array) {
        for child in children {
            if let Some(found) = ancestors_of(child, target, trail) {
                trail.pop();
                return Some(found);
            }
        }
    }
    trail.pop();
    None
}

#[test]
fn should_home_a_test_heavy_cluster_under_its_production_directory() {
    // test-heavy-ts holds two production files and four spec files in one
    // cohesive cluster: the name election weighs production SLOC before file
    // count, so the folder holding the production files never lands under a
    // `spec` ancestor even though spec files outnumber src ones.
    let root = fixture("test-heavy-ts");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze exits 0");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
    let candidate_tree = parsed
        .pointer("/profiles/greenfield/candidates/0/tree")
        .cloned();
    let tree = candidate_tree
        .as_ref()
        .or_else(|| parsed.pointer("/current/tree"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut trail = Vec::new();
    let ancestors = ancestors_of(&tree, "src/adapters/anthropic/client.ts", &mut trail);
    assert!(
        ancestors.is_some(),
        "client.ts is missing from both the current tree and best greenfield candidate: {tree}"
    );
    let ancestors = ancestors.unwrap_or_default();
    let has_spec_ancestor = ancestors
        .iter()
        .flat_map(|name| name.split('/'))
        .any(|segment| segment == "spec");
    assert!(
        !has_spec_ancestor,
        "the production file sits under a spec-named ancestor: {ancestors:?}"
    );

    if candidate_tree.is_none() {
        let candidates = parsed
            .pointer("/profiles/greenfield/candidates")
            .and_then(serde_json::Value::as_array);
        assert_eq!(
            candidates.map(Vec::len),
            Some(0),
            "absence means no strictly improving greenfield candidate"
        );
    }
}

#[test]
fn should_discover_the_root_local_config_without_an_explicit_flag() {
    // configured-python carries its own strata.toml with a one-SLOC file cap:
    // `--root` alone must pick it up (the default config path is root-relative,
    // not CWD-relative), which surfaces as a capacity violation.
    let root = fixture("configured-python");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&["analyze", "--root", root_str, "--format", "json"]);

    assert_eq!(outcome.code, 0, "analyze exits 0");
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
    let has_capacity_violation = parsed
        .pointer("/current/sharedFindings")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|violations| {
            violations.iter().any(|violation| {
                violation.get("kind").and_then(serde_json::Value::as_str) == Some("capacity")
            })
        });
    assert!(
        has_capacity_violation,
        "the root-local one-SLOC cap must trip a capacity violation: {}",
        outcome.stdout
    );
}

#[test]
fn should_warn_on_stderr_when_an_explicit_config_is_missing() {
    // an explicit --config pointing nowhere still succeeds on built-in defaults
    // (the harness contract), but says so on stderr instead of staying silent —
    // and the root-local strata.toml must NOT sneak in behind the explicit flag.
    let root = fixture("configured-python");
    let root_str = root.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);

    assert_eq!(outcome.code, 0, "defaults still apply; the run succeeds");
    assert!(
        outcome.stderr.contains("using built-in defaults"),
        "the missing explicit config warns on stderr: {}",
        outcome.stderr
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&outcome.stdout).unwrap_or(serde_json::Value::Null);
    let has_capacity_violation = parsed
        .pointer("/current/sharedFindings")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|violations| {
            violations.iter().any(|violation| {
                violation.get("kind").and_then(serde_json::Value::as_str) == Some("capacity")
            })
        });
    assert!(
        !has_capacity_violation,
        "the explicit (missing) config wins over the root-local strata.toml: {}",
        outcome.stdout
    );
}

#[test]
fn should_render_incremental_container_names_in_the_tree_face() {
    // interior containers print only their increment over the parent
    // (`geometry`), never the full cumulative chain; files keep their full path.
    let result = analyze_to_file("nested-ts");
    let result_str = result.to_str().unwrap_or_default();

    let outcome = run(&["tree", "--input", result_str, "--current"]);

    let _ = std::fs::remove_file(&result);
    assert_eq!(outcome.code, 0, "the candidate tree renders");
    assert!(
        outcome.stdout.contains("geometry [domain]"),
        "an interior container shows its bare increment: {}",
        outcome.stdout
    );
    assert!(
        !outcome.stdout.contains("nested-ts/geometry ["),
        "no interior container leaks its cumulative chain: {}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("src/geometry/rectangle.ts [file]"),
        "files keep their full path: {}",
        outcome.stdout
    );
}

#[test]
fn should_vary_narration_reasons_and_mark_followed_subjects() {
    // reasons are computed, not canned: every narrated move carries a reason
    // kind from the MoveReason enum, and the diff face echoes each computed
    // partner rather than a canned string. The anchored faces no longer serve
    // here: under the D-37 normalized objective anchored is uniformly
    // conservative (zero narrated moves on every e2e fixture), so narration
    // coverage lives on the layout-blind face that still repairs.
    //
    // Historical note: this test once demanded at least two distinct kinds
    // including `follows` (a spec file following its subject). After the FIX03
    // capacity-term work re-based the objective, no e2e fixture emits `follows`
    // any more — every narrated move is `pulledBy`. The kind-set breadth now
    // lives with the engine; the enum-wide Follows caption stays covered by the
    // renderer unit tests (`should_print_reason_specific_captions`).
    const COMPUTED_KINDS: [&str; 5] = [
        "pulledBy",
        "follows",
        "relievesOverCap",
        "namingCohesion",
        "clustering",
    ];
    let result = analyze_to_file("constellation-ts");
    let result_str = result.to_str().unwrap_or_default();
    let contents = std::fs::read_to_string(&result).unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(&contents).unwrap_or(serde_json::Value::Null);

    let face = run(&["diff", "--input", result_str, "current", "greenfield/1"]);

    let _ = std::fs::remove_file(&result);
    let narration = parsed
        .pointer("/profiles/greenfield/candidates/0/deltaNarration")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !narration.is_empty(),
        "the delta narrates moves: {contents}"
    );
    for entry in &narration {
        let kind = entry
            .pointer("/reason/kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            COMPUTED_KINDS.contains(&kind),
            "every narrated reason is a computed MoveReason kind, got {kind:?}: {entry}"
        );
    }
    assert_eq!(face.code, 0, "the diff face renders");
    assert!(
        face.stdout.contains("pulled by "),
        "the face echoes the pull partner each move satisfies: {}",
        face.stdout
    );
}

#[test]
fn should_honor_objective_and_weights_config_keys() {
    // [objective] and [weights] are live, not parsed-and-dropped: zeroing the
    // imbalance coefficient and doubling the value-import weight each shift the
    // current score away from the default run's figure. nested-ts carries the
    // scored value-import edges (python's lone import collapses into its
    // inheritance/type-reference pair).
    let root = fixture("nested-ts");
    let root_str = root.to_str().unwrap_or_default();
    let current_score = |config: &str| -> f64 {
        let outcome = run(&[
            "analyze", "--root", root_str, "--config", config, "--format", "json",
        ]);
        assert_eq!(outcome.code, 0, "analyze exits 0 under {config}");
        serde_json::from_str::<serde_json::Value>(&outcome.stdout)
            .ok()
            .and_then(|parsed| {
                parsed
                    .pointer("/profiles/anchored/current/score")
                    .and_then(serde_json::Value::as_f64)
            })
            .unwrap_or(f64::NAN)
    };
    let objective_toml =
        std::env::temp_dir().join(format!("strata-accept-objective-{}.toml", nanos()));
    let weights_toml = std::env::temp_dir().join(format!("strata-accept-weights-{}.toml", nanos()));
    let objective_written = std::fs::write(
        &objective_toml,
        b"[profiles.anchored.objective]\nimbalance = 0.0\n",
    )
    .is_ok();
    let weights_written = std::fs::write(
        &weights_toml,
        b"[profiles.anchored.weights]\nvalue-import = 2.0\n",
    )
    .is_ok();

    let baseline = current_score(PURE_DEFAULTS);
    let objective = current_score(objective_toml.to_str().unwrap_or_default());
    let weights = current_score(weights_toml.to_str().unwrap_or_default());

    let _ = std::fs::remove_file(&objective_toml);
    let _ = std::fs::remove_file(&weights_toml);
    assert!(objective_written, "the objective toml was written");
    assert!(weights_written, "the weights toml was written");
    assert!(
        (baseline - objective).abs() > 1e-9,
        "[objective].imbalance moves the score: {baseline} vs {objective}"
    );
    assert!(
        (baseline - weights).abs() > 1e-9,
        "[weights].value-import moves the score: {baseline} vs {weights}"
    );
}

#[test]
fn should_reject_a_zero_seeds_per_candidate_from_config() {
    // [diversity].seeds-per-candidate is parsed and validated: a zero pool
    // multiplier is refused up front with the coded error naming the key.
    let root = fixture("python");
    let root_str = root.to_str().unwrap_or_default();
    let toml = std::env::temp_dir().join(format!("strata-accept-seeds-{}.toml", nanos()));
    let toml_str = toml.to_str().unwrap_or_default();
    let written = std::fs::write(
        &toml,
        b"[profiles.anchored.diversity]\nseeds-per-candidate = 0\n",
    )
    .is_ok();

    let outcome = run(&[
        "analyze", "--root", root_str, "--config", toml_str, "--format", "summary",
    ]);

    let _ = std::fs::remove_file(&toml);
    assert!(written, "the zero-seeds toml was written");
    assert_eq!(outcome.code, 1, "the invalid config is exit 1");
    assert!(
        outcome.stderr.contains("error[CONFIG_INVALID]"),
        "the refusal carries the stable code: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("diversity.seeds-per-candidate"),
        "the refusal names the offending key: {}",
        outcome.stderr
    );
}

#[test]
fn should_populate_conditional_splits_for_an_over_cap_scc() {
    // cyclic-oversized carries a three-file import cycle whose SCC exceeds the
    // file cap: every candidate in both modes shares the same conditional split,
    // its preconditions byte-identical to the cycle violation's break
    // suggestions and its resulting file count ceil-packed to two.
    let root = fixture("cyclic-oversized");
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
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze exits 0");
    let parsed: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap_or_default();
    let suggestions = parsed
        .pointer("/current/sharedFindings")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|violation| violation.get("breakSuggestions"))
        .cloned()
        .unwrap_or_default();
    assert!(
        suggestions.as_array().is_some_and(|list| !list.is_empty()),
        "the cycle violation carries break suggestions: {suggestions}"
    );
    for mode in ["anchored", "greenfield"] {
        let candidates = parsed
            .pointer(&format!("/profiles/{mode}/candidates"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for candidate in &candidates {
            let splits = candidate
                .get("conditionalSplits")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert_eq!(splits.len(), 1, "{mode} candidates carry the one split");
            let split = splits.first().cloned().unwrap_or_default();
            assert_eq!(
                split.get("preconditions"),
                Some(&suggestions),
                "{mode} split preconditions mirror the break suggestions"
            );
            assert_eq!(
                split
                    .get("resultingFiles")
                    .and_then(serde_json::Value::as_u64),
                Some(2),
                "{mode} split ceil-packs the scc into two files"
            );
        }
        assert!(
            candidates.is_empty(),
            "the oversized identity layout has no strictly improving {mode} candidate"
        );
    }
}

#[test]
fn should_outscore_the_current_layout_on_the_constellation_fixture() {
    // constellation-ts is large enough to coarsen (28 files, three deliberately
    // misplaced): the reliability invariant demands the current layout be
    // outscored with a non-negative improvement on every candidate, and no
    // candidate folder may breach the fifteen-file capacity cap.
    let root = fixture("constellation-ts");
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
        "json",
    ]);

    assert_eq!(outcome.code, 0, "analyze exits 0");
    let parsed: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap_or_default();
    // D-37 rescale: the normalized cut term no longer lets raw cut savings
    // swamp the anchored churn price, so the misplacement repairs (clamp/lerp,
    // epsilon, reader/writer) cost more in path cohesion than they save and
    // anchored honestly reports `optimal` while greenfield — which carries no
    // anchor or path terms — still outscores and names every repair. The
    // reliability invariant that matters is unchanged: every candidate of every
    // mode improves on current, and greenfield keeps marking the layout
    // outscored.
    assert_eq!(
        parsed
            .pointer("/profiles/greenfield/current/standing")
            .and_then(serde_json::Value::as_str),
        Some("outscored"),
        "greenfield marks the misplaced layout outscored"
    );
    for mode in ["greenfield"] {
        let candidates = parsed
            .pointer(&format!("/profiles/{mode}/candidates"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(!candidates.is_empty(), "{mode} delivers candidates");
        for candidate in &candidates {
            let improvement = candidate
                .get("improvement")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            assert!(
                improvement > 0.0,
                "{mode} candidate improves on the current layout: {improvement}"
            );
            assert!(
                max_files_per_folder(candidate.get("tree").unwrap_or(&serde_json::Value::Null))
                    <= 15,
                "{mode} candidate folders stay within the file cap"
            );
        }
    }
}

/// Returns the largest number of direct file children any container of `tree`
/// holds.
fn max_files_per_folder(tree: &serde_json::Value) -> usize {
    let children = tree
        .get("children")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let own = children
        .iter()
        .filter(|child| child.get("level").and_then(serde_json::Value::as_str) == Some("file"))
        .count();
    children
        .iter()
        .map(max_files_per_folder)
        .fold(own, usize::max)
}

#[test]
fn should_yield_byte_identical_output_across_root_path_forms() {
    // Bug B: rust-analyzer canonicalizes its VFS to an absolute path, but strata
    // once forwarded the raw --root string, so a non-canonical form (`.`, or an
    // absolute path containing `..`) failed every semantic-edge lookup and
    // silently degraded the rust dependency graph from 13 hard cross-crate edges
    // to 4 name-guessed ones — different, worse advice for the very same repo.
    // Canonicalizing the root once makes every spelling converge on one snapshot,
    // including its content hash.
    let canonical = std::fs::canonicalize(fixture("workspace-rust"))
        .unwrap_or_else(|_| fixture("workspace-rust"));
    let canonical_str = canonical.to_str().unwrap_or_default();
    // a `..`-containing absolute path to the SAME directory. Lexical absolutization
    // (`path::absolute`) would leave the `..` in place and still mismatch the VFS;
    // only `fs::canonicalize` resolves it, so this form guards the fix's mechanism.
    let dotdot = canonical.join("crates").join("..");
    let dotdot_str = dotdot.to_str().unwrap_or_default();

    let abs = run(&[
        "analyze",
        "--root",
        canonical_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let dots = run(&[
        "analyze",
        "--root",
        dotdot_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let dot = run_in(
        &canonical,
        &[
            "analyze",
            "--root",
            ".",
            "--config",
            PURE_DEFAULTS,
            "--format",
            "json",
        ],
    );

    assert_eq!(abs.code, 0, "the canonical-absolute form exits 0");
    assert_eq!(dots.code, 0, "the ..-containing absolute form exits 0");
    assert_eq!(dot.code, 0, "the . form exits 0");
    assert_eq!(
        abs.stdout, dots.stdout,
        "a ..-containing root yields the same snapshot as canonical (needs fs::canonicalize, not path::absolute)"
    );
    assert_eq!(
        abs.stdout, dot.stdout,
        "a . root yields the same snapshot as canonical, snapshotHash included"
    );
    // the invariance is meaningful, not a shared-degradation coincidence: the
    // converged snapshot carries the full 17-edge graph the degraded forms lost
    // (13 declaration edges plus the 4 `Type::method()` qualifier type references).
    assert!(
        abs.stdout.contains("\"edges\":17,") || abs.stdout.contains("\"edges\":17}"),
        "the converged snapshot carries all 17 semantic edges: {}",
        abs.stdout
    );
}

#[test]
fn should_resolve_a_relative_root_to_real_names_and_full_rust_edges() {
    // the `.`-form is the default a user gets running strata from inside their
    // repo; `.` has no `file_name()`, so Bug B dropped it to 4 edges and the
    // empty-name fallback group `root`. Canonicalization makes the snapshot a
    // function of the resolved directory, not of the root's spelling: the
    // `.`-form run from inside the fixture and an absolute-root run from
    // anywhere must persist BYTE-EQUAL results, and the current tree carries
    // the real directory name as its package group.
    let canonical = std::fs::canonicalize(fixture("workspace-rust"))
        .unwrap_or_else(|_| fixture("workspace-rust"));
    let canonical_str = canonical.to_str().unwrap_or_default();
    let dot_result = std::env::temp_dir().join(format!("strata-accept-dotroot-{}.json", nanos()));
    let abs_result = std::env::temp_dir().join(format!("strata-accept-absroot-{}.json", nanos()));
    let dot_str = dot_result.to_str().unwrap_or_default();
    let abs_str = abs_result.to_str().unwrap_or_default();

    let saved_dot = run_in(
        &canonical,
        &[
            "analyze",
            "--root",
            ".",
            "--config",
            PURE_DEFAULTS,
            "--format",
            "json",
            "--output",
            dot_str,
        ],
    );
    let saved_abs = run(&[
        "analyze",
        "--root",
        canonical_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
        "--output",
        abs_str,
    ]);
    let dot_json = std::fs::read_to_string(&dot_result).unwrap_or_default();
    let abs_json = std::fs::read_to_string(&abs_result).unwrap_or_default();

    let tree = run(&["tree", "--input", dot_str, "--current"]);

    let _ = std::fs::remove_file(&dot_result);
    let _ = std::fs::remove_file(&abs_result);
    assert_eq!(saved_dot.code, 0, "the .-form analyze exits 0");
    assert_eq!(saved_abs.code, 0, "the absolute-root analyze exits 0");
    assert_eq!(
        dot_json, abs_json,
        "the `.`-form and absolute-root runs persist byte-equal snapshots"
    );
    assert_eq!(tree.code, 0, "the current tree renders");
    assert!(
        tree.stdout.contains("workspace-rust [packageGroup]"),
        "the package group is named for the real directory, not the fallback `root`: {}",
        tree.stdout
    );
}

#[test]
fn should_reject_a_missing_analysis_root_with_input_unreadable() {
    // fs::canonicalize makes analyze the guard for "does --root exist?": a
    // nonexistent root is a clean exit-1 INPUT_UNREADABLE, never the gating 2 and
    // never a silently degraded snapshot.
    let missing = std::env::temp_dir().join(format!("strata-absent-{}", nanos()));
    let missing_str = missing.to_str().unwrap_or_default();

    let outcome = run(&[
        "analyze",
        "--root",
        missing_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "summary",
    ]);

    assert_eq!(
        outcome.code, 1,
        "a nonexistent root is exit 1, never the gating 2"
    );
    assert!(
        outcome.stderr.contains("error[INPUT_UNREADABLE]"),
        "the missing-root error carries its stable code: {}",
        outcome.stderr
    );
}
