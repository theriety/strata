//! The AD-5 library/CLI parity test plus golden-file assertions, pinned across
//! all three language fixtures.
//!
//! For each fixture repo the parity test runs the real `strata` binary with
//! `analyze --format json` and, in the same process, calls `snapshot_from_root`
//! plus `analyze` on the same root. The two must agree byte-for-byte: the CLI's
//! JSON (sans the trailing newline it adds) must equal `serde_json::to_vec` of the
//! library result. Because `analyze` is pure and deterministic, an identical root
//! and config always produce an identical result, so the comparison is stable.
//!
//! The golden tests then exercise the rendered faces of the downstream commands —
//! `tree`, `diff`, `violations`, `report`, and `analyze --format summary` —
//! against the saved result for every fixture and assert their human-readable
//! output is byte-identical to a committed golden file. Set `STRATA_BLESS=1` to
//! regenerate the goldens from the current binary.
//!
//! Oracle layering, by design: **goldens here are the single oracle for rendered
//! output** (one bless path, `STRATA_BLESS=1`); the hand-written asserts in
//! `cli_acceptance.rs` check only exit codes and structural invariants; and the
//! parity test above pins library↔CLI byte-equality of the json face.

use std::path::{Path, PathBuf};
use std::process::Command;

use strata_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Every structurally clean fixture the parity and golden suites drive as a
/// group.
///
/// These fixtures carry no `cycle` or `polarity` hard violation, so the gating
/// invariants below (`--fail-on cycle` never raises the reserved exit 2) hold for
/// every member. The three violation-focused fixtures (`cyclic`, `over-capacity`,
/// `polarity-leak`) are deliberately *excluded* here — each would gate one of the
/// shared invariants — and instead receive their own parity and golden tests.
const FIXTURES: [&str; 9] = [
    "ts",
    "rust",
    "python",
    "workspace-rust",
    "nested-python",
    "nested-ts",
    "rust-cfg-test",
    "constellation-ts",
    "test-heavy-ts",
];

/// A non-existent config path, forcing the binary onto its built-in defaults so
/// every analysis is identical no matter which directory the harness runs from —
/// a configured `strata.toml` in a parent directory must never leak into a run.
/// The library side pairs it with `AnalyzeConfig::default()`.
const PURE_DEFAULTS: &str = "/nonexistent/strata-parity.toml";

/// Returns the absolute path to a named language fixture under this crate.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/e2e/fixtures")
        .join(name)
}

/// Returns the absolute path to a fixture's committed golden file.
///
/// Goldens live in a `goldens/<fixture>/` tree kept out of the fixture repos so
/// the analyzer never scans them as sources.
fn golden_path(name: &str, file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/e2e/fixtures/goldens")
        .join(name)
        .join(file)
}

/// Returns the built `strata` binary path cargo injects for integration tests.
fn strata_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_strata"))
}

/// Runs `strata <args...>` and returns its (status code, stdout bytes).
///
/// A spawn failure returns the sentinel code `-1` with empty output so the
/// caller's status assertions fail loudly without a panic.
fn run_strata(args: &[&str]) -> (i32, Vec<u8>) {
    match Command::new(strata_bin()).args(args).output() {
        Ok(output) => (output.status.code().unwrap_or(-1), output.stdout),
        Err(_) => (-1, Vec::new()),
    }
}

/// Runs the library analyze over a fixture root with the default config.
///
/// A snapshot or analysis failure returns empty bytes so the parity assertion
/// fails on the mismatch rather than panicking.
fn library_json(root: &Path) -> Vec<u8> {
    let config = AnalyzeConfig::default();
    let result = snapshot_from_root(root, &config)
        .and_then(|snapshot| analyze(&snapshot, &config))
        .ok();
    result
        .and_then(|result| serde_json::to_vec(&result).ok())
        .unwrap_or_default()
}

/// Asserts CLI `analyze --format json` bytes equal the serialized library result.
fn assert_parity(name: &str) {
    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();

    let (code, mut stdout) = run_strata(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    assert_eq!(code, 0, "analyze exits 0 for {name}");
    // the CLI appends one trailing newline the library serialization omits.
    assert_eq!(stdout.pop(), Some(b'\n'), "trailing newline for {name}");

    let library = library_json(&root);
    assert_eq!(
        stdout, library,
        "cli json must be byte-identical to the library result for {name}"
    );
}

/// Writes a fixture's `AnalyzeResult` JSON to a temp file and returns its path.
///
/// The result is produced by the same binary under test so the golden commands
/// consume exactly what `analyze` emits. A failing analyze leaves the assertion
/// in the caller to fail rather than panicking here.
fn analyze_to_file(name: &str) -> PathBuf {
    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let path = std::env::temp_dir().join(format!("strata-e2e-{name}-{}.json", nanos()));
    let path_str = path.to_str().unwrap_or_default();

    let (code, _) = run_strata(&[
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
    assert_eq!(code, 0, "analyze writes a result file for {name}");
    path
}

/// Asserts the bytes of a downstream command equal a committed golden, or
/// regenerates the golden when `STRATA_BLESS=1` is set.
///
/// The actual bytes come from a real binary invocation; the expected bytes come
/// from the on-disk golden, so a drift in any renderer surfaces as a byte diff.
fn assert_golden(name: &str, file: &str, actual: &[u8]) {
    let path = golden_path(name, file);
    if std::env::var_os("STRATA_BLESS").is_some() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, actual);
        return;
    }
    let expected = std::fs::read(&path);
    assert!(
        expected.is_ok(),
        "missing golden {}; run with STRATA_BLESS=1 to generate it",
        path.display()
    );
    let expected = expected.unwrap_or_default();
    assert_eq!(
        String::from_utf8_lossy(actual),
        String::from_utf8_lossy(&expected),
        "{name}/{file} must match its golden"
    );
}

/// Runs every downstream command for `name` against a saved result and asserts
/// each output matches its committed golden.
fn assert_goldens(name: &str) {
    let result_path = analyze_to_file(name);
    let result_str = result_path.to_str().unwrap_or_default();

    let (tree_code, tree_out) =
        run_strata(&["tree", "--input", result_str, "--current", "--symbols"]);
    assert_eq!(tree_code, 0, "tree exits 0 for {name}");
    assert_golden(name, "tree.txt", &tree_out);

    let (diff_code, diff_out) =
        run_strata(&["diff", "--input", result_str, "current", "anchored/1"]);
    assert_eq!(diff_code, 0, "diff exits 0 for {name}");
    assert_golden(name, "diff.txt", &diff_out);

    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let (violations_code, violations_out) = run_strata(&[
        "violations",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "table",
    ]);
    assert_eq!(violations_code, 0, "violations exits 0 for {name}");
    assert_golden(name, "violations.txt", &violations_out);

    // the analyze-summary rendered face: run over the same root, both modes, so the
    // committed golden pins the human-readable summary bytes (census, scores, and
    // per-mode candidate headlines) the same deterministic way the other goldens are.
    let (summary_code, summary_out) = run_strata(&[
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
    assert_eq!(summary_code, 0, "analyze summary exits 0 for {name}");
    assert_golden(name, "summary.txt", &summary_out);

    let (report_code, report_out) = run_strata(&["report", "--input", result_str]);
    assert_eq!(report_code, 0, "report exits 0 for {name}");
    assert_golden(name, "report.md", &report_out);

    let _ = std::fs::remove_file(&result_path);
}

#[test]
fn should_match_the_library_result_for_the_typescript_fixture() {
    assert_parity("ts");
}

#[test]
fn should_match_the_library_result_for_the_python_fixture() {
    assert_parity("python");
}

#[test]
fn should_match_the_library_result_for_the_rust_fixture() {
    assert_parity("rust");
}

#[test]
fn should_match_the_goldens_for_the_typescript_fixture() {
    assert_goldens("ts");
}

#[test]
fn should_match_the_goldens_for_the_python_fixture() {
    assert_goldens("python");
}

#[test]
fn should_match_the_goldens_for_the_rust_fixture() {
    assert_goldens("rust");
}

#[test]
fn should_match_the_library_result_for_the_rust_workspace_fixture() {
    assert_parity("workspace-rust");
}

#[test]
fn should_match_the_library_result_for_the_nested_python_fixture() {
    assert_parity("nested-python");
}

#[test]
fn should_match_the_library_result_for_the_nested_typescript_fixture() {
    assert_parity("nested-ts");
}

#[test]
fn should_match_the_library_result_for_the_cyclic_fixture() {
    assert_parity("cyclic");
}

#[test]
fn should_match_the_library_result_for_the_over_capacity_fixture() {
    assert_parity("over-capacity");
}

#[test]
fn should_match_the_library_result_for_the_polarity_leak_fixture() {
    assert_parity("polarity-leak");
}

#[test]
fn should_match_the_goldens_for_the_rust_workspace_fixture() {
    assert_goldens("workspace-rust");
}

#[test]
fn should_match_the_goldens_for_the_nested_python_fixture() {
    assert_goldens("nested-python");
}

#[test]
fn should_match_the_goldens_for_the_nested_typescript_fixture() {
    assert_goldens("nested-ts");
}

#[test]
fn should_match_the_goldens_for_the_cyclic_fixture() {
    assert_goldens("cyclic");
}

#[test]
fn should_match_the_goldens_for_the_over_capacity_fixture() {
    assert_goldens("over-capacity");
}

#[test]
fn should_match_the_goldens_for_the_polarity_leak_fixture() {
    assert_goldens("polarity-leak");
}

#[test]
fn should_match_the_library_result_for_the_rust_cfg_test_fixture() {
    assert_parity("rust-cfg-test");
}

#[test]
fn should_match_the_goldens_for_the_rust_cfg_test_fixture() {
    assert_goldens("rust-cfg-test");
}

#[test]
fn should_match_the_library_result_for_the_constellation_fixture() {
    assert_parity("constellation-ts");
}

#[test]
fn should_match_the_goldens_for_the_constellation_fixture() {
    assert_goldens("constellation-ts");
}

#[test]
fn should_match_the_library_result_for_the_cyclic_oversized_fixture() {
    assert_parity("cyclic-oversized");
}

#[test]
fn should_match_the_goldens_for_the_cyclic_oversized_fixture() {
    assert_goldens("cyclic-oversized");
}

#[test]
fn should_match_the_library_result_for_the_test_heavy_fixture() {
    assert_parity("test-heavy-ts");
}

#[test]
fn should_match_the_goldens_for_the_test_heavy_fixture() {
    assert_goldens("test-heavy-ts");
}

#[test]
fn should_exit_zero_for_violations_without_fail_on() {
    for name in FIXTURES {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let (code, _) = run_strata(&["violations", "--root", root_str, "--config", PURE_DEFAULTS]);

        assert_eq!(code, 0, "a clean run without --fail-on exits 0 for {name}");
    }
}

#[test]
fn should_keep_exit_code_two_reserved_for_a_fail_on_match() {
    // a class the fixtures never violate must never gate, so the run stays at 0.
    for name in FIXTURES {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let (code, _) = run_strata(&[
            "violations",
            "--root",
            root_str,
            "--config",
            PURE_DEFAULTS,
            "--fail-on",
            "cycle",
        ]);

        assert_ne!(code, 2, "a clean fixture never gates for {name}");
    }
}

#[test]
fn should_accept_a_severity_qualified_fail_on_selector() {
    // the `kind:severity` grammar parses and runs; a clean fixture never gates.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let (code, _) = run_strata(&[
        "violations",
        "--root",
        root_str,
        "--fail-on",
        "capacity:violation",
    ]);

    assert_eq!(
        code, 0,
        "a clean fixture never gates even with a severity qualifier"
    );
}

#[test]
fn should_reject_a_malformed_fail_on_selector_without_gating() {
    // an unknown class is a usage error (exit 1), never the gating code 2.
    let root = fixture("rust");
    let root_str = root.to_str().unwrap_or_default();

    let (code, _) = run_strata(&[
        "violations",
        "--root",
        root_str,
        "--fail-on",
        "bogus:violation",
    ]);

    assert_eq!(code, 1, "a malformed --fail-on is a usage error");
}

/// Returns a unique nanosecond stamp for temp paths.
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}
