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
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_engine::{AnalyzeConfig, analyze, snapshot_from_root};

/// Every structurally clean fixture the parity and golden suites drive as a
/// group.
///
/// These fixtures carry no `cycle` or `polarity` hard violation, so the gating
/// invariants below (`--fail-on cycle` never raises the reserved exit 2) hold for
/// every member. The three violation-focused fixtures (`cyclic`, `over-capacity`,
/// `polarity-leak`) are deliberately *excluded* here — each would gate one of the
/// shared invariants — and instead receive their own parity and golden tests.
const FIXTURES: [&str; 11] = [
    "ts",
    "rust",
    "python",
    "workspace-rust",
    "workspace-move-rust",
    "nested-python",
    "nested-ts",
    "rust-cfg-test",
    "constellation-ts",
    "test-heavy-ts",
    "twin-follow-ts",
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

/// Resolves a narrated path against the fixture on disk.
///
/// Narrated paths are projected beneath the package-group root, which is the
/// fixture directory's own name, so they resolve against its parent.
fn on_disk(name: &str, path: &str) -> PathBuf {
    let root = fixture(name);
    match path.strip_prefix(name) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            root.join(rest.trim_start_matches('/'))
        }
        _ => root.join(path),
    }
}

/// Returns the parent of a `/`-separated path, or `""` at the top.
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Collects every real-path violation of one candidate's narration (ADR-0018).
///
/// Every `from` must be an existing directory and every moved or mirrored file
/// an existing file at pass start. Every `to` must be an existing directory or
/// a new directory whose missing ancestors, up to the nearest existing one,
/// are all created by the same candidate. A symbol move must leave an existing file for an existing file or
/// one created in a directory that exists or that the candidate creates.
fn move_path_violations(name: &str, candidate: &serde_json::Value) -> Vec<String> {
    let empty = Vec::new();
    let moves = candidate
        .get("deltaNarration")
        .and_then(serde_json::Value::as_array)
        .unwrap_or(&empty);
    let text = |value: &serde_json::Value, key: &str| -> String {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let list = |value: &serde_json::Value, key: &str| -> Vec<serde_json::Value> {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut created: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for relocation in moves {
        created.insert(text(relocation, "to"));
        for mirror in list(relocation, "mirrors") {
            created.insert(text(&mirror, "to"));
        }
    }
    // a new directory lands when every missing ancestor up to the nearest
    // existing one is itself created by the candidate: `mkdir -p` never has to
    // invent a directory the proposal does not name.
    let lands = |to: &str| {
        let mut parent = to;
        while !on_disk(name, parent).is_dir() {
            if parent != to && !created.contains(parent) {
                return false;
            }
            if parent.is_empty() {
                return false;
            }
            parent = parent_of(parent);
        }
        true
    };
    let mut violations = Vec::new();
    let check_file = |path: &str, from: &str, to: Option<&str>, found: &mut Vec<String>| {
        if !on_disk(name, path).is_file() {
            found.push(format!("moved file {path} does not exist"));
        }
        if !on_disk(name, from).is_dir() {
            found.push(format!("from {from} is not a real directory"));
        }
        if let Some(to) = to
            && !lands(to)
        {
            found.push(format!("to {to} has no real or created parent"));
        }
    };
    for relocation in moves {
        let to = text(relocation, "to");
        for file in list(relocation, "files") {
            check_file(
                &text(&file, "path"),
                &text(&file, "from"),
                Some(&to),
                &mut violations,
            );
        }
        for mirror in list(relocation, "mirrors") {
            check_file(
                &text(&mirror, "path"),
                &text(&mirror, "from"),
                Some(&text(&mirror, "to")),
                &mut violations,
            );
        }
        for blocked in list(relocation, "blockedMirrors") {
            check_file(
                &text(&blocked, "path"),
                &text(&blocked, "from"),
                None,
                &mut violations,
            );
        }
    }
    for symbol_move in list(candidate, "symbolMoves") {
        let from = text(&symbol_move, "fromPath");
        let to = text(&symbol_move, "toPath");
        if !on_disk(name, &from).is_file() {
            violations.push(format!("symbol source {from} does not exist"));
        }
        let directory = parent_of(&to);
        let named = on_disk(name, directory).is_dir() || created.contains(directory);
        let lands_in_directory = named && lands(directory);
        if !on_disk(name, &to).is_file() && !lands_in_directory {
            violations.push(format!(
                "symbol target {to} has no real or created directory"
            ));
        }
    }
    violations
}

/// Asserts every move endpoint of every candidate names a real path
/// (ADR-0018): no invented `src/crates/...` directory ever reaches the user.
fn assert_real_move_paths(name: &str, parsed: &serde_json::Value) {
    let violations = real_move_path_violations(name, parsed);
    assert!(
        violations.is_empty(),
        "{name} narrates paths that do not exist: {violations:#?}"
    );
}

/// Collects the real-path violations (ADR-0018) of every candidate in both
/// profiles of one analyze result, each tagged `<profile>/<rank>`.
fn real_move_path_violations(name: &str, parsed: &serde_json::Value) -> Vec<String> {
    let mut violations = Vec::new();
    for profile in ["anchored", "greenfield"] {
        let candidates = parsed
            .pointer(&format!("/profiles/{profile}/candidates"))
            .and_then(serde_json::Value::as_array);
        assert!(
            candidates.is_some(),
            "{name} result has no {profile} candidates array"
        );
        for (index, candidate) in candidates.into_iter().flatten().enumerate() {
            violations.extend(
                move_path_violations(name, candidate)
                    .into_iter()
                    .map(|violation| format!("{profile}/{}: {violation}", index + 1)),
            );
        }
    }
    violations
}

/// The number of candidates across both profiles of one analyze result.
fn candidate_count(parsed: &serde_json::Value) -> usize {
    ["anchored", "greenfield"]
        .iter()
        .filter_map(|profile| parsed.pointer(&format!("/profiles/{profile}/candidates")))
        .filter_map(serde_json::Value::as_array)
        .map(Vec::len)
        .sum()
}

/// Fixtures whose analysis fails by design (an unbounded re-export chain, an
/// unparseable source), so they narrate no moves to check.
const FAILING_FIXTURES: [&str; 2] = ["barrel-cycle", "syntax-error"];

/// Every fixture repository under `tests/e2e/fixtures` that analyzes, including
/// the violation-focused ones [`FIXTURES`] leaves out, in name order.
fn every_fixture() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fixtures");
    let mut names: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name != "goldens" && !FAILING_FIXTURES.contains(&name.as_str()))
        .collect();
    names.sort_unstable();
    names
}

/// Analyzes a fixture under the pure defaults plus `extra` flags and parses
/// the JSON result; unparsable output fails the test.
fn analyze_json(name: &str, extra: &[&str]) -> serde_json::Value {
    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let mut args = vec![
        "analyze",
        "--root",
        root_str,
        "--config",
        PURE_DEFAULTS,
        "--format",
        "json",
    ];
    args.extend_from_slice(extra);
    let (code, stdout) = run_strata(&args);
    assert_eq!(code, 0, "analyze exits 0 for {name} {extra:?}");
    let parsed = serde_json::from_slice(&stdout);
    assert!(
        parsed.is_ok(),
        "analyze output for {name} {extra:?} is not JSON: {parsed:?}"
    );
    parsed.unwrap_or_default()
}

/// Runs every downstream command for `name` against a saved result and asserts
/// each output matches its committed golden.
fn assert_goldens(name: &str) {
    let result_path = analyze_to_file(name);
    let result_str = result_path.to_str().unwrap_or_default();
    let result_json = std::fs::read_to_string(&result_path).unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap_or_default();
    assert_real_move_paths(name, &parsed);
    let anchored_candidate_exists = parsed
        .pointer("/profiles/anchored/candidates")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|candidates| !candidates.is_empty());

    let (tree_code, tree_out) =
        run_strata(&["tree", "--input", result_str, "--current", "--symbols"]);
    assert_eq!(tree_code, 0, "tree exits 0 for {name}");
    assert_golden(name, "tree.txt", &tree_out);

    if anchored_candidate_exists {
        let (diff_code, diff_out) =
            run_strata(&["diff", "--input", result_str, "current", "anchored/1"]);
        assert_eq!(diff_code, 0, "candidate diff exits 0 for {name}");
        assert_golden(name, "diff.txt", &diff_out);
    }

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
fn should_narrate_only_real_or_created_move_paths_for_every_fixture_in_both_wall_modes() {
    // ADR-0018 across every fixture, not only the golden set: a move target's
    // parent is an existing directory or a new folder the same candidate
    // creates. `workspace-ts-leak` once printed `atlas/src/atlas/core` — the
    // package key re-joined under its own see-through source root.
    let inspected = AtomicUsize::new(0);
    let counter = &inspected;
    let violations: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = every_fixture()
            .into_iter()
            .flat_map(|name| {
                [&[][..], &["--allow-cross-package-moves"][..]].map(|extra| (name.clone(), extra))
            })
            .map(|(name, extra)| {
                scope.spawn(move || {
                    let parsed = analyze_json(&name, extra);
                    counter.fetch_add(candidate_count(&parsed), Ordering::Relaxed);
                    real_move_path_violations(&name, &parsed)
                        .into_iter()
                        .map(|violation| format!("{name} {extra:?} {violation}"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        runs.into_iter()
            .flat_map(|run| {
                run.join()
                    .unwrap_or_else(|_| vec!["a run panicked".to_owned()])
            })
            .collect()
    });
    assert!(
        inspected.load(Ordering::Relaxed) > 0,
        "the invariant must inspect at least one candidate"
    );
    assert!(
        violations.is_empty(),
        "move endpoints must be real or created paths: {violations:#?}"
    );
}

#[test]
fn should_match_the_library_result_for_the_rust_workspace_move_fixture() {
    assert_parity("workspace-move-rust");
}

#[test]
fn should_match_the_goldens_for_the_rust_workspace_move_fixture() {
    assert_goldens("workspace-move-rust");
}

#[test]
fn should_narrate_a_same_package_move_through_a_source_root_for_the_workspace_move_fixture() {
    // The real-path invariant is only as strong as the moves it sees: this
    // fixture must keep proposing a same-package move under `crates/<x>/src`,
    // so the invariant would catch an invented `src/crates/...` endpoint.
    let result_path = analyze_to_file("workspace-move-rust");
    let result_json = std::fs::read_to_string(&result_path).unwrap_or_default();
    let _ = std::fs::remove_file(&result_path);
    let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap_or_default();

    let moves: Vec<(String, String, String)> = ["anchored", "greenfield"]
        .iter()
        .filter_map(|profile| {
            parsed
                .pointer(&format!("/profiles/{profile}/candidates"))
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .flatten()
        .flat_map(|candidate| {
            candidate
                .get("deltaNarration")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|relocation| {
            let to = relocation
                .get("to")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            relocation
                .get("files")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |file| {
                    let field = |key: &str| {
                        file.get(key)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    (field("path"), field("from"), to.clone())
                })
        })
        .collect();

    assert!(
        moves.contains(&(
            "workspace-move-rust/crates/core/src/beta/scale.rs".to_owned(),
            "workspace-move-rust/crates/core/src/beta".to_owned(),
            "workspace-move-rust/crates/core/src/alpha".to_owned(),
        )),
        "the misfiled scale helper must move within core's src; got {moves:#?}"
    );
    assert!(
        moves.iter().all(|(path, _, to)| {
            let package = |text: &str| text.split('/').take(3).collect::<Vec<_>>().join("/");
            package(path) == package(to)
        }),
        "no default-profile move may leave its crate; got {moves:#?}"
    );
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
fn should_match_the_library_result_for_the_twin_follow_fixture() {
    assert_parity("twin-follow-ts");
}

#[test]
fn should_match_the_goldens_for_the_twin_follow_fixture() {
    assert_goldens("twin-follow-ts");
}

#[test]
fn should_match_the_library_result_for_the_relief_nest_py_fixture() {
    assert_parity("relief-nest-py");
}

#[test]
fn should_match_the_goldens_for_the_relief_nest_py_fixture() {
    assert_goldens("relief-nest-py");
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
