//! The AD-5 library/CLI parity test plus golden-file assertions, pinned across
//! all three language fixtures.
//!
//! For each fixture repo the parity test runs the real `unlace` binary with
//! `analyze --format json` and, in the same process, calls `snapshot_from_root`
//! plus `analyze` on the same root. The two must agree byte-for-byte: the CLI's
//! JSON (sans the trailing newline it adds) must equal `serde_json::to_vec` of the
//! library result. Because `analyze` is pure and deterministic, an identical root
//! and config always produce an identical result, so the comparison is stable.
//!
//! The golden tests then exercise the rendered faces of the downstream commands —
//! `tree`, `diff`, `violations`, `report`, and `analyze --format summary` —
//! against the saved result for every fixture and assert their human-readable
//! output is byte-identical to a committed golden file. Set `UNLACE_BLESS=1` to
//! regenerate the goldens from the current binary.
//!
//! Oracle layering, by design: **goldens here are the single oracle for rendered
//! output** (one bless path, `UNLACE_BLESS=1`); the hand-written asserts in
//! `cli_acceptance.rs` check only exit codes and structural invariants; and the
//! parity test above pins library↔CLI byte-equality of the json face.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use unlace_engine::{analyze, load_config, snapshot_from_root};

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
/// a configured `unlace.toml` in a parent directory must never leak into a run.
/// The library side loads the same path, which falls back to the defaults.
const PURE_DEFAULTS: &str = "/nonexistent/unlace-parity.toml";

/// A config pinning the file cap at 250 SLOC, so the over-cap fixtures
/// keep the capacity findings they exist to show under the 600 default.
const PINNED_CAP_250: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e/cap-250.toml");

/// A config pinning the domain and package caps at 1, so the nested
/// `dup-mix-ts` and `dup-pkg-rs` fixtures breach both while their folder cap keeps the default
/// and the greenfield search still proposes their move.
const TIGHT_DOMAIN_PACKAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/e2e/tight-domain-package.toml"
);

/// A config pinning the domain cap at 2 and the package cap at 1 for the
/// `dup-ws-ts` and `dup-pkg-ts` widgets (three direct sub-folders), so the
/// domain and package caps both breach and the greenfield search still proposes
/// a move whose improvement includes a capacity term.
const TIGHT_DOMAIN_2_PACKAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/e2e/tight-domain-2-package.toml"
);

/// Configs pinning the folder cap at 2 and 3, so the mixed-root fixtures keep
/// the folder breaches their candidate pricing is measured on.
const FOLDER_CAP_2: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e/folder-2.toml");
const FOLDER_CAP_3: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e/folder-3.toml");

/// The folder cap at 3 with cross-package moves allowed.
const CROSS_PACKAGE_FOLDER_3: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/e2e/cross-package-folder-3.toml"
);

/// Returns the config path a fixture is analyzed under: pure defaults, except
/// the over-cap fixtures, which pin the 250 cap, and the nested workspace, which
/// pins tight domain and package caps. The CLI and library sides both use it so
/// parity holds.
fn config_for(name: &str) -> &'static str {
    if matches!(name, "over-capacity" | "cyclic-oversized") {
        PINNED_CAP_250
    } else if matches!(name, "mixed-root-ts" | "dup-split-ts" | "cycle-loose-ts") {
        FOLDER_CAP_3
    } else if name == "cross-package-ts" {
        CROSS_PACKAGE_FOLDER_3
    } else if name == "dup-mix-ctl-ts" {
        FOLDER_CAP_2
    } else if matches!(name, "dup-ws-ts" | "dup-pkg-ts") {
        TIGHT_DOMAIN_2_PACKAGE
    } else if matches!(name, "dup-pkg-rs" | "dup-mix-ts") {
        TIGHT_DOMAIN_PACKAGE
    } else {
        PURE_DEFAULTS
    }
}

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

/// Returns the built `unlace` binary path cargo injects for integration tests.
fn unlace_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_unlace"))
}

/// Runs `unlace <args...>` and returns its (status code, stdout bytes).
///
/// A spawn failure returns the sentinel code `-1` with empty output so the
/// caller's status assertions fail loudly without a panic.
fn run_unlace(args: &[&str]) -> (i32, Vec<u8>) {
    match Command::new(unlace_bin()).args(args).output() {
        Ok(output) => (output.status.code().unwrap_or(-1), output.stdout),
        Err(_) => (-1, Vec::new()),
    }
}

/// Runs the library analyze over a fixture root with the default config.
///
/// A snapshot or analysis failure returns empty bytes so the parity assertion
/// fails on the mismatch rather than panicking.
fn library_json(root: &Path, config_path: &str) -> Vec<u8> {
    let config = load_config(config_path).unwrap_or_default();
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

    let (code, mut stdout) = run_unlace(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        config_for(name),
        "--format",
        "json",
    ]);
    assert_eq!(code, 0, "analyze exits 0 for {name}");
    // the CLI appends one trailing newline the library serialization omits.
    assert_eq!(stdout.pop(), Some(b'\n'), "trailing newline for {name}");

    let library = library_json(&root, config_for(name));
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
    analyze_to_file_with(name, &[])
}

/// [`analyze_to_file`] with extra `analyze` flags appended.
fn analyze_to_file_with(name: &str, extra: &[&str]) -> PathBuf {
    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let path = std::env::temp_dir().join(format!("unlace-e2e-{name}-{}.json", nanos()));
    let path_str = path.to_str().unwrap_or_default();

    let mut args = vec![
        "analyze",
        "--root",
        root_str,
        "--config",
        config_for(name),
        "--format",
        "json",
        "--output",
        path_str,
    ];
    args.extend_from_slice(extra);
    let (code, _) = run_unlace(&args);
    assert_eq!(code, 0, "analyze writes a result file for {name}");
    path
}

/// Asserts the bytes of a downstream command equal a committed golden, or
/// regenerates the golden when `UNLACE_BLESS=1` is set.
///
/// The actual bytes come from a real binary invocation; the expected bytes come
/// from the on-disk golden, so a drift in any renderer surfaces as a byte diff.
fn assert_golden(name: &str, file: &str, actual: &[u8]) {
    let path = golden_path(name, file);
    if std::env::var_os("UNLACE_BLESS").is_some() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, actual);
        return;
    }
    let expected = std::fs::read(&path);
    assert!(
        expected.is_ok(),
        "missing golden {}; run with UNLACE_BLESS=1 to generate it",
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
/// Every narrated path is repo-relative, so it joins the fixture root as it
/// stands. A fixture directory nested under a same-named one (`proj/proj/...`)
/// resolves only when the path keeps that real leading segment.
fn on_disk(name: &str, path: &str) -> PathBuf {
    fixture(name).join(path)
}

/// Returns the parent of a `/`-separated path, or `""` at the top.
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// ADR-21: a file moved to `to` lands at `<to>/<its file name>`, which must
/// not be a path another file already holds.
fn overwrite_violation(name: &str, path: &str, to: &str) -> Option<String> {
    let landing = format!("{to}/{}", path.rsplit('/').next().unwrap_or_default());
    (landing != path && on_disk(name, &landing).is_file())
        .then(|| format!("{path} would overwrite the existing {landing}"))
}

/// Collects every real-path violation of one candidate's narration (ADR-18).
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
        found.extend(to.and_then(|to| overwrite_violation(name, path, to)));
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
/// (ADR-18): no invented `src/crates/...` directory ever reaches the user.
fn assert_real_move_paths(name: &str, parsed: &serde_json::Value) {
    let violations = real_move_path_violations(name, parsed);
    assert!(
        violations.is_empty(),
        "{name} narrates paths that do not exist: {violations:#?}"
    );
}

/// Collects the real-path violations (ADR-18) of every candidate in both
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
        config_for(name),
        "--format",
        "json",
    ];
    args.extend_from_slice(extra);
    let (code, stdout) = run_unlace(&args);
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
        run_unlace(&["tree", "--input", result_str, "--current", "--symbols"]);
    assert_eq!(tree_code, 0, "tree exits 0 for {name}");
    assert_golden(name, "tree.txt", &tree_out);

    if anchored_candidate_exists {
        let (diff_code, diff_out) =
            run_unlace(&["diff", "--input", result_str, "current", "anchored/1"]);
        assert_eq!(diff_code, 0, "candidate diff exits 0 for {name}");
        assert_golden(name, "diff.txt", &diff_out);
    }

    let root = fixture(name);
    let root_str = root.to_str().unwrap_or_default();
    let (violations_code, violations_out) = run_unlace(&[
        "violations",
        "--root",
        root_str,
        "--config",
        config_for(name),
        "--format",
        "table",
    ]);
    assert_eq!(violations_code, 0, "violations exits 0 for {name}");
    assert_golden(name, "violations.txt", &violations_out);

    // the analyze-summary rendered face: run over the same root, both modes, so the
    // committed golden pins the human-readable summary bytes (census, scores, and
    // per-mode candidate headlines) the same deterministic way the other goldens are.
    let (summary_code, summary_out) = run_unlace(&[
        "analyze",
        "--root",
        root_str,
        "--config",
        config_for(name),
        "--mode",
        "both",
        "--format",
        "summary",
    ]);
    assert_eq!(summary_code, 0, "analyze summary exits 0 for {name}");
    assert_golden(name, "summary.txt", &summary_out);

    let (report_code, report_out) = run_unlace(&["report", "--input", result_str]);
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
    // ADR-18 across every fixture, not only the golden set: a move target's
    // parent is an existing directory or a new folder the same candidate
    // creates. `workspace-ts-leak` once printed `atlas/src/atlas/core` — the
    // package key re-joined under its own see-through source root. ADR-21's
    // parity invariant rides the same sweep: no file move, in either wall
    // mode, lands on a path an existing file holds.
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
        "move endpoints must be real or created paths and never overwrite a file: {violations:#?}"
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
fn should_draw_each_file_inside_its_own_crate_in_the_workspace_move_candidate_tree() {
    // The candidate tree must obey the package wall its move list obeys: the
    // upper levels once clustered util's `lib.rs` under `crates/core`, a tree
    // no default-profile move could ever produce.
    let result_path = analyze_to_file("workspace-move-rust");
    let result_str = result_path.to_str().unwrap_or_default();
    let (code, tree_out) = run_unlace(&[
        "tree",
        "--input",
        result_str,
        "--mode",
        "greenfield",
        "--candidate",
        "1",
    ]);
    let _ = std::fs::remove_file(&result_path);
    assert_eq!(code, 0, "candidate tree exits 0 for workspace-move-rust");
    assert_golden("workspace-move-rust", "greenfield-tree.txt", &tree_out);

    let text = String::from_utf8_lossy(&tree_out);
    let mut package = None;
    let mut misplaced = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_suffix(" [package]") {
            package = Some(name.to_owned());
        } else if let Some((path, _)) = line.split_once(" [file]") {
            let inside = package
                .as_deref()
                .is_some_and(|name| path.starts_with(&format!("{name}/")));
            if !inside {
                misplaced.push((path.to_owned(), package.clone()));
            }
        }
    }
    assert!(
        package.is_some() && misplaced.is_empty(),
        "every file must sit under its own crate's package; misplaced {misplaced:#?} in\n{text}"
    );
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
            "crates/core/src/beta/scale.rs".to_owned(),
            "crates/core/src/beta".to_owned(),
            "crates/core/src/alpha".to_owned(),
        )),
        "the misfiled scale helper must move within core's src; got {moves:#?}"
    );
    assert!(
        moves.iter().all(|(path, _, to)| {
            let package = |text: &str| text.split('/').take(2).collect::<Vec<_>>().join("/");
            package(path) == package(to)
        }),
        "no default-profile move may leave its crate; got {moves:#?}"
    );
}

#[test]
fn should_pin_a_colliding_crate_root_without_moving_or_folding_it() {
    // ADR-21: with the wall lifted the solver sends util's `lib.rs` into
    // core's `src`, where `crates/core/src/lib.rs` already lives. The move must
    // never be printed (it would overwrite core's crate root), and because
    // util's `lib.rs` is itself a crate root it is never folded either:
    // retiring it would break the crate. It only stays where it is.
    let result_path = analyze_to_file_with("workspace-move-rust", &["--allow-cross-package-moves"]);
    let result_json = std::fs::read_to_string(&result_path).unwrap_or_default();
    let _ = std::fs::remove_file(&result_path);
    let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap_or_default();
    let candidates: Vec<serde_json::Value> = ["anchored", "greenfield"]
        .iter()
        .filter_map(|profile| {
            parsed
                .pointer(&format!("/profiles/{profile}/candidates"))
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .flatten()
        .collect();
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let array = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };

    let root = fixture("workspace-move-rust");
    let mut landings = Vec::new();
    for relocation in candidates
        .iter()
        .flat_map(|candidate| array(candidate, "deltaNarration"))
    {
        let to = text(&relocation, "to");
        for file in array(&relocation, "files") {
            let path = text(&file, "path");
            let name = path.rsplit('/').next().unwrap_or_default().to_owned();
            landings.push((path, format!("{to}/{name}")));
        }
    }
    let overwrites: Vec<_> = landings
        .iter()
        .filter(|(_, landing)| {
            landing
                .strip_prefix("workspace-move-rust/")
                .is_some_and(|relative| root.join(relative).exists())
        })
        .collect();
    assert!(
        !landings.is_empty() && overwrites.is_empty(),
        "no file move may land on an existing file; got {overwrites:#?} of {landings:#?}"
    );

    let util_root = "crates/util/src/lib.rs";
    let moved_root: Vec<_> = landings
        .iter()
        .filter(|(path, _)| path.ends_with(util_root))
        .collect();
    assert!(
        moved_root.is_empty(),
        "util's crate root must stay where it is; got {moved_root:#?}"
    );
    let folded: Vec<String> = candidates
        .iter()
        .flat_map(|candidate| array(candidate, "symbolMoves"))
        .filter(|relocation| text(relocation, "fromPath") == util_root)
        .map(|relocation| text(&relocation, "symbol"))
        .collect();
    assert!(
        folded.is_empty(),
        "a crate root is never folded into symbol moves; got {folded:#?}"
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

/// `--full` keeps the complete, uncapped summary: every advice item, in plan order.
#[test]
fn should_match_the_full_summary_golden_for_the_relief_nest_py_fixture() {
    let name = "relief-nest-py";
    let root = fixture(name);
    let (code, out) = run_unlace(&[
        "analyze",
        "--root",
        root.to_str().unwrap_or_default(),
        "--config",
        PURE_DEFAULTS,
        "--mode",
        "both",
        "--format",
        "summary",
        "--full",
    ]);
    assert_eq!(code, 0, "analyze --full exits 0 for {name}");
    assert_golden(name, "summary-full.txt", &out);
}

/// Fixtures nested under a directory of their own name (`dup-hub-py/dup-hub-py/`),
/// the Python flat layout (`django/django/`): the package-group label equals a
/// real top-level directory, so a path that loses its first segment names a
/// file that does not exist. `dup-hub-py` relieves an over-cap folder with file
/// moves; `dup-geo-py` moves a symbol between two files; `dupflat` imports
/// through its own package name and carries a cycle, a polarity leak and a
/// test-only item. `dup-ws-ts` is the `workspace-ts-leak` workspace under such
/// a directory: its package directories (`dup-ws-ts/atlas`) begin with the
/// root's name, so a package or domain finding and a file move must keep that
/// first segment. `dup-pkg-ts` holds a package whose own directory has the
/// root's name (`tokio/tokio`), and `dup-mix-ts` a loose directory named like a
/// real package. `dup-pkg-rs`, the Rust form of `dup-pkg-ts`, is slow to bind
/// and is checked once, by the package-directory test below.
const SAME_NAME_FIXTURES: [&str; 7] = [
    "dup-hub-py",
    "dup-geo-py",
    "dupflat",
    "dup-ws-ts",
    "dup-pkg-ts",
    "dup-mix-ts",
    "dup-split-ts",
];

/// Reads `key` of a JSON object as text, or `""`.
fn json_text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Reads `key` of a JSON object as an array, or an empty list.
fn json_list(value: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The candidates of both profiles in one analyze result.
fn all_candidates(parsed: &serde_json::Value) -> Vec<serde_json::Value> {
    ["anchored", "greenfield"]
        .iter()
        .filter_map(|profile| parsed.pointer(&format!("/profiles/{profile}/candidates")))
        .flat_map(json_list_of)
        .collect()
}

/// The elements of a JSON array value, or an empty list.
fn json_list_of(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// Where a file named `path` lands when moved into folder `to`; `.` is the root.
fn landing(path: &str, to: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if to == "." {
        leaf.to_owned()
    } else {
        format!("{to}/{leaf}")
    }
}

/// Every path the JSON names across all candidates' file, mirror and symbol moves.
fn json_move_paths(parsed: &serde_json::Value) -> std::collections::BTreeSet<String> {
    let mut paths = std::collections::BTreeSet::new();
    for candidate in all_candidates(parsed) {
        for relocation in json_list(&candidate, "deltaNarration") {
            let to = json_text(&relocation, "to");
            for file in json_list(&relocation, "files") {
                let path = json_text(&file, "path");
                paths.insert(landing(&path, &to));
                paths.insert(path);
            }
            for mirror in json_list(&relocation, "mirrors") {
                let path = json_text(&mirror, "path");
                paths.insert(landing(&path, &json_text(&mirror, "to")));
                paths.insert(path);
                paths.insert(json_text(&mirror, "sourcePath"));
            }
        }
        for symbol_move in json_list(&candidate, "symbolMoves") {
            paths.insert(json_text(&symbol_move, "fromPath"));
            paths.insert(json_text(&symbol_move, "toPath"));
        }
    }
    paths
}

/// Every path the text names on its `- Move` lines, symbol names left out.
///
/// A long move wraps after its arrow, so a line ending in `→` continues on the
/// next one.
fn text_move_paths(text: &str) -> std::collections::BTreeSet<String> {
    let mut paths = std::collections::BTreeSet::new();
    let mut lines = text.lines().map(str::trim_start);
    while let Some(first) = lines.next() {
        if !first.starts_with("- Move ") {
            continue;
        }
        let mut line = first.to_owned();
        while line.ends_with('\u{2192}') {
            let Some(next) = lines.next() else { break };
            line = format!("{line} {next}");
        }
        let skip =
            usize::from(line.starts_with("- Move symbol ") || line.starts_with("- Move type "));
        paths.extend(
            line.split('`')
                .skip(1)
                .step_by(2)
                .skip(skip)
                .map(str::to_owned),
        );
    }
    paths
}

/// Every advice item's source path, which must exist, and its destination,
/// which must be the move's own destination.
fn advice_path_violations(name: &str, parsed: &serde_json::Value) -> Vec<String> {
    let mut violations = Vec::new();
    let items = ["recommended", "reviewCandidates"]
        .iter()
        .filter_map(|group| parsed.pointer(&format!("/advice/{group}")))
        .flat_map(json_list_of);
    for item in items {
        let proposal = item.get("proposal").cloned().unwrap_or_default();
        let relocation = proposal.get("relocation").cloned().unwrap_or_default();
        let destination = json_text(&item, "destination");
        let (source, expected) = if json_text(&proposal, "kind") == "symbol" {
            (
                json_text(&relocation, "fromPath"),
                json_text(&relocation, "toPath"),
            )
        } else {
            let file = json_list(&relocation, "files")
                .into_iter()
                .next()
                .unwrap_or_default();
            (json_text(&file, "path"), json_text(&relocation, "to"))
        };
        if !on_disk(name, &source).is_file() {
            violations.push(format!("advice source {source} does not exist"));
        }
        if destination != expected {
            violations.push(format!(
                "advice destination {destination} differs from its move's {expected}"
            ));
        }
    }
    violations
}

/// Every finding of one analyze result: shared, then each profile's own.
fn all_findings(parsed: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut findings = parsed
        .pointer("/current/sharedFindings")
        .map(json_list_of)
        .unwrap_or_default();
    for profile in ["anchored", "greenfield"] {
        findings.extend(
            parsed
                .pointer(&format!("/profiles/{profile}/current/uniqueFindings"))
                .map(json_list_of)
                .unwrap_or_default(),
        );
    }
    findings
}

/// The path-valued entries of a finding's `location`: every entry for a cycle,
/// polarity or capacity finding (each a whole real path), and the file for a
/// visibility or test-only finding (its second entry is a symbol name).
fn finding_paths(finding: &serde_json::Value) -> Vec<String> {
    let location: Vec<String> = json_list(finding, "location")
        .iter()
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect();
    match json_text(finding, "kind").as_str() {
        "visibility" | "testOnly" => location.into_iter().take(1).collect(),
        _ => location,
    }
}

#[test]
fn should_locate_every_finding_at_a_path_that_exists_on_disk_for_a_same_name_nested_root() {
    let mut kinds = std::collections::BTreeSet::new();
    for name in SAME_NAME_FIXTURES {
        let parsed = analyze_json(name, &["--mode", "both"]);
        for finding in all_findings(&parsed) {
            kinds.insert(json_text(&finding, "kind"));
            if json_text(&finding, "kind") == "capacity" {
                // one whole real path: the first `realPaths` entry.
                let first = finding
                    .pointer("/capacity/realPaths/0")
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    json_list(&finding, "location"),
                    vec![first],
                    "{name} capacity location is its first real path"
                );
                let real_paths = finding
                    .pointer("/capacity/realPaths")
                    .map(json_list_of)
                    .unwrap_or_default();
                for real_path in real_paths.iter().filter_map(serde_json::Value::as_str) {
                    assert!(
                        on_disk(name, real_path).exists(),
                        "{name} capacity real path {real_path} does not exist: {finding}"
                    );
                }
            }
            for path in finding_paths(&finding) {
                assert!(
                    on_disk(name, &path).exists(),
                    "{name} {} finding is located at {path}, which does not exist: {finding}",
                    json_text(&finding, "kind")
                );
            }
        }
    }
    // the fixtures between them exercise every finding kind a flat layout yields.
    assert_eq!(
        kinds.into_iter().collect::<Vec<_>>(),
        vec!["capacity", "cycle", "polarity", "testOnly"]
    );
}

#[test]
fn should_narrate_real_paths_that_exist_on_disk_for_a_same_name_nested_root() {
    let mut narrated = Vec::new();
    for name in SAME_NAME_FIXTURES {
        let parsed = analyze_json(name, &["--mode", "both"]);

        assert_real_move_paths(name, &parsed);

        if !json_move_paths(&parsed).is_empty() {
            narrated.push(name);
        }
        assert_eq!(
            advice_path_violations(name, &parsed),
            Vec::<String>::new(),
            "{name} advice names real paths"
        );
    }
    // the file-move, symbol-move and nested-workspace fixtures narrate moves.
    assert_eq!(
        narrated,
        vec![
            "dup-hub-py",
            "dup-geo-py",
            "dup-ws-ts",
            "dup-pkg-ts",
            "dup-mix-ts",
            "dup-split-ts"
        ]
    );
}

/// The `realPaths` of every capacity finding counted against `config_key`.
fn capacity_real_paths(parsed: &serde_json::Value, config_key: &str) -> Vec<Vec<String>> {
    all_findings(parsed)
        .iter()
        .filter(|finding| {
            finding
                .pointer("/capacity/configKey")
                .and_then(serde_json::Value::as_str)
                == Some(config_key)
        })
        .map(|finding| {
            finding
                .pointer("/capacity/realPaths")
                .map(json_list_of)
                .unwrap_or_default()
                .iter()
                .filter_map(|path| path.as_str().map(str::to_owned))
                .collect()
        })
        .collect()
}

#[test]
fn should_name_the_real_directories_of_a_workspace_nested_under_its_own_name() {
    let name = "dup-ws-ts";
    let parsed = analyze_json(name, &["--mode", "both"]);

    // `atlas` breaches the package cap with its two domains; `widget` breaches
    // the domain cap with its three direct sub-folders (`parts`, `dials`, `gears`). Both
    // directories begin with the root's own name, which is a real directory
    // and stays.
    assert_eq!(
        capacity_real_paths(&parsed, "capacity.package"),
        vec![vec!["dup-ws-ts/atlas".to_owned()]]
    );
    assert_eq!(
        capacity_real_paths(&parsed, "capacity.domain"),
        vec![vec!["dup-ws-ts/beacon/src/widget".to_owned()]]
    );

    // the greenfield move and the advice built on it name the same real folders.
    let relocation: Vec<(String, String, String)> = all_candidates(&parsed)
        .iter()
        .flat_map(|candidate| json_list(candidate, "deltaNarration"))
        .flat_map(|relocation| {
            let to = json_text(&relocation, "to");
            json_list(&relocation, "files")
                .iter()
                .map(|file| (json_text(file, "path"), json_text(file, "from"), to.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        relocation,
        vec![
            (
                "dup-ws-ts/atlas/src/agent/loop.ts".to_owned(),
                "dup-ws-ts/atlas/src/agent".to_owned(),
                "dup-ws-ts/atlas/src/core".to_owned(),
            ),
            (
                "dup-ws-ts/atlas/src/agent/plan.ts".to_owned(),
                "dup-ws-ts/atlas/src/agent".to_owned(),
                "dup-ws-ts/atlas/src/core".to_owned(),
            ),
        ]
    );
    let destinations: Vec<String> = ["recommended", "reviewCandidates"]
        .iter()
        .filter_map(|group| parsed.pointer(&format!("/advice/{group}")))
        .flat_map(json_list_of)
        .map(|item| json_text(&item, "destination"))
        .collect();
    assert!(!destinations.is_empty(), "the move is offered as advice");
    for destination in destinations {
        assert!(
            on_disk(name, &destination).is_dir(),
            "advice destination {destination} is a real directory"
        );
    }
}

#[test]
fn should_print_the_same_paths_in_text_and_json_for_a_same_name_nested_root() {
    for name in SAME_NAME_FIXTURES {
        let parsed = analyze_json(name, &["--mode", "both"]);
        let root = fixture(name);
        let (code, out) = run_unlace(&[
            "analyze",
            "--root",
            root.to_str().unwrap_or_default(),
            "--config",
            config_for(name),
            "--mode",
            "both",
            "--format",
            "summary",
            "--full",
        ]);
        assert_eq!(code, 0, "analyze summary exits 0 for {name}");
        let text = String::from_utf8_lossy(&out);

        assert_eq!(
            text_move_paths(&text),
            json_move_paths(&parsed),
            "{name} text moves name the JSON's paths"
        );
        let (table_code, table) = run_unlace(&[
            "violations",
            "--root",
            root.to_str().unwrap_or_default(),
            "--config",
            config_for(name),
            "--format",
            "table",
        ]);
        assert_eq!(table_code, 0, "violations exits 0 for {name}");
        let table = String::from_utf8_lossy(&table);
        for finding in all_findings(&parsed) {
            let kind = json_text(&finding, "kind");
            let location: Vec<String> = json_list(&finding, "location")
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect();
            // the table prints the JSON location as it is, then the detail.
            assert!(
                table.contains(&format!(
                    "{} :: {}",
                    location.join(", "),
                    json_text(&finding, "detail")
                )),
                "{name} table names the JSON location of the {kind} finding: {table}"
            );
            // the summary names each path of the finding in backticks.
            let named = if kind == "capacity" {
                vec![format!("in `{}`", finding_paths(&finding).join(""))]
            } else {
                location.iter().map(|entry| format!("`{entry}`")).collect()
            };
            for expected in named {
                assert!(
                    text.contains(&expected),
                    "{name} summary names {expected} for the {kind} finding: {text}"
                );
            }
        }
    }
}

/// A package whose directory has the repository folder's name (`tokio/tokio`,
/// `serde/serde`) has a package key equal to the package-group label. The key
/// is a real directory, so its finding names that directory and not the
/// repository root `.`.
#[test]
fn should_name_a_package_directory_that_has_the_repository_folders_name() {
    let ts = analyze_json("dup-pkg-ts", &["--mode", "both"]);
    let rs = analyze_json("dup-pkg-rs", &["--mode", "both"]);

    assert_eq!(
        capacity_real_paths(&ts, "capacity.package"),
        vec![vec!["dup-pkg-ts".to_owned()]],
        "the TypeScript package finding names its package directory"
    );
    assert_eq!(
        capacity_real_paths(&ts, "capacity.domain"),
        vec![vec!["beacon/src/widget".to_owned()]]
    );
    assert_eq!(
        capacity_real_paths(&rs, "capacity.package"),
        vec![vec!["dup-pkg-rs".to_owned()]],
        "the Rust crate finding names its crate directory"
    );
    for finding in all_findings(&rs) {
        for path in finding_paths(&finding) {
            assert!(
                on_disk("dup-pkg-rs", &path).exists(),
                "dup-pkg-rs finding is located at {path}, which does not exist"
            );
        }
    }
    assert_real_move_paths("dup-pkg-rs", &rs);
}

/// A loose directory (`atlas/`, outside every manifest) that shares its name
/// with a real package (`dup-mix-ts/atlas`) is keyed under the group label, so
/// its finding names the loose directory and not the package beside it.
#[test]
fn should_tell_a_loose_directory_from_a_real_package_of_the_same_name() {
    let parsed = analyze_json("dup-mix-ts", &["--mode", "both"]);

    assert_eq!(
        capacity_real_paths(&parsed, "capacity.domain"),
        vec![vec!["atlas".to_owned()]],
        "the loose directory's domain finding names that directory"
    );
    assert_eq!(
        capacity_real_paths(&parsed, "capacity.package"),
        vec![vec!["dup-mix-ts/atlas".to_owned()]],
        "the real package keeps its own directory"
    );
}

/// The `(capacity remainder, capacity score term)` of each profile's first
/// candidate in one analyze result.
fn first_candidate_capacity(parsed: &serde_json::Value) -> Vec<(serde_json::Value, f64)> {
    ["anchored", "greenfield"]
        .iter()
        .filter_map(|profile| {
            let candidate = parsed
                .pointer(&format!("/profiles/{profile}/candidates/0"))?
                .clone();
            let remainder = candidate.get("capacityRemainder").cloned()?;
            let term = candidate.pointer("/scoreBreakdown/capacity")?.as_f64()?;
            Some((remainder, term))
        })
        .collect()
}

/// Analyzes `name` under an explicit config, in both modes.
fn analyze_json_under(name: &str, config: &str) -> serde_json::Value {
    let root = fixture(name);
    let (code, stdout) = run_unlace(&[
        "analyze",
        "--root",
        root.to_str().unwrap_or_default(),
        "--config",
        config,
        "--mode",
        "both",
        "--format",
        "json",
    ]);
    assert_eq!(code, 0, "analyze exits 0 for {name}");
    serde_json::from_slice(&stdout).unwrap_or_default()
}

/// Candidate pricing sees the same directories the disk has: loose folders
/// beside a real package price like the same folders under another name, because
/// a made-up `<root>/<root>` directory must never merge or split their counts.
#[test]
fn should_price_loose_folders_beside_a_same_named_package_like_renamed_ones() {
    let mixed = first_candidate_capacity(&analyze_json_under("dup-mix-ts", FOLDER_CAP_2));
    let control = first_candidate_capacity(&analyze_json_under("dup-mix-ctl-ts", FOLDER_CAP_2));

    assert_eq!(mixed.len(), 2, "both profiles produce a candidate");
    assert_eq!(
        mixed, control,
        "renaming the loose `atlas` folders to `zeta` must not change the pricing"
    );
}

/// Loose top-level folders and root files beside packages stay in the
/// candidate's capacity count: the root still holds five entries after a move
/// that only touches `atlas/src/core`.
#[test]
fn should_keep_a_root_breach_priced_when_loose_folders_sit_beside_packages() {
    let parsed = analyze_json("mixed-root-ts", &["--mode", "both"]);

    let capacity = first_candidate_capacity(&parsed);
    assert_eq!(capacity.len(), 2, "both profiles produce a candidate");
    for (remainder, term) in capacity {
        assert_eq!(
            remainder
                .get("remaining")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "the root breach survives the candidate: {remainder}"
        );
        assert!(
            (term - 8.0 / 3.0).abs() < 1e-6,
            "the candidate's capacity term prices that breach, got {term}"
        );
    }
}

/// A folder the candidate splits out of a loose directory is named by its real
/// path: `atlas/foo/b`, never `dup-split-ts/atlas/foo/b` (the package beside it).
#[test]
fn should_name_a_split_inside_a_loose_directory_by_its_real_path() {
    let name = "dup-split-ts";
    let parsed = analyze_json(name, &["--mode", "both"]);

    let split_targets: std::collections::BTreeSet<String> = all_candidates(&parsed)
        .iter()
        .flat_map(|candidate| json_list(candidate, "deltaNarration"))
        .filter(|relocation| {
            json_list(relocation, "files")
                .iter()
                .any(|file| json_text(file, "from") == "atlas/foo")
        })
        .map(|relocation| json_text(&relocation, "to"))
        .collect();
    assert_eq!(
        split_targets.into_iter().collect::<Vec<_>>(),
        vec!["atlas/foo/b".to_owned()]
    );
    let destinations: Vec<String> = ["recommended", "reviewCandidates"]
        .iter()
        .filter_map(|group| parsed.pointer(&format!("/advice/{group}")))
        .flat_map(json_list_of)
        .map(|item| json_text(&item, "destination"))
        .collect();
    assert!(
        destinations.iter().any(|path| path == "atlas/foo/b"),
        "the advice offers the split: {destinations:?}"
    );
    assert!(
        destinations
            .iter()
            .all(|path| !path.starts_with("dup-split-ts/atlas/foo")),
        "no advice destination names the package beside the loose directory: {destinations:?}"
    );
}

/// A loose folder in an import cycle with a package file is still a loose
/// folder: the cluster is named after the cycle's heaviest file, and the folder
/// it stands for keeps its provenance. Its candidate prices and names the real
/// `a/` directory, the same as the pre-change binary did (score and capacity
/// term 4.0, a split into `a/m`), and no move is a no-op.
#[test]
fn should_price_a_loose_folder_in_a_cycle_with_a_package_file_by_its_real_path() {
    let name = "cycle-loose-ts";
    let parsed = analyze_json(name, &["--mode", "both"]);

    let capacity = first_candidate_capacity(&parsed);
    assert_eq!(capacity.len(), 2, "both profiles produce a candidate");
    for (remainder, term) in capacity {
        assert_eq!(
            remainder
                .get("remaining")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert!((term - 4.0).abs() < 1e-9, "capacity term {term}");
    }
    let moves: Vec<(String, String)> = all_candidates(&parsed)
        .iter()
        .flat_map(|candidate| json_list(candidate, "deltaNarration"))
        .flat_map(|relocation| {
            let to = json_text(&relocation, "to");
            json_list(&relocation, "files")
                .iter()
                .map(|file| (json_text(file, "from"), to.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!moves.is_empty(), "the loose folder is split");
    for (from, to) in &moves {
        assert_ne!(from, to, "a move to its own folder is no move");
        assert_eq!(to, "a/m", "the split names the real directory");
    }
    assert_real_move_paths(name, &parsed);
}

/// A file that lands in another package's folder is printed at that folder's
/// real directory, `beacon/src/widget`: the source root belongs to the package
/// that owns the folder, never to the package the file leaves.
#[test]
fn should_name_a_cross_package_destination_by_the_package_that_owns_the_folder() {
    let name = "cross-package-ts";
    let parsed = analyze_json(name, &["--mode", "both"]);

    let targets: std::collections::BTreeSet<String> = all_candidates(&parsed)
        .iter()
        .flat_map(|candidate| json_list(candidate, "deltaNarration"))
        .filter(|relocation| {
            json_list(relocation, "files")
                .iter()
                .any(|file| json_text(file, "path") == "atlas/src/core/use.ts")
        })
        .map(|relocation| json_text(&relocation, "to"))
        .collect();
    assert_eq!(
        targets.into_iter().collect::<Vec<_>>(),
        vec!["beacon/src/widget".to_owned()]
    );
    assert_real_move_paths(name, &parsed);
}

#[test]
fn should_match_the_library_result_for_the_mixed_root_fixture() {
    assert_parity("mixed-root-ts");
}

/// The golden pins the candidate's score and capacity remainder, which match the
/// pre-change binary's for this layout (3.0994 anchored, 2.9851 greenfield).
#[test]
fn should_match_the_goldens_for_the_mixed_root_fixture() {
    assert_goldens("mixed-root-ts");
}

#[test]
fn should_match_the_library_result_for_the_same_name_nested_fixtures() {
    for name in SAME_NAME_FIXTURES {
        assert_parity(name);
    }
}

#[test]
fn should_match_the_goldens_for_the_same_name_nested_fixtures() {
    for name in SAME_NAME_FIXTURES {
        assert_goldens(name);
    }
}

#[test]
fn should_exit_zero_for_violations_without_fail_on() {
    for name in FIXTURES {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let (code, _) = run_unlace(&["violations", "--root", root_str, "--config", PURE_DEFAULTS]);

        assert_eq!(code, 0, "a clean run without --fail-on exits 0 for {name}");
    }
}

#[test]
fn should_keep_exit_code_two_reserved_for_a_fail_on_match() {
    // a class the fixtures never violate must never gate, so the run stays at 0.
    for name in FIXTURES {
        let root = fixture(name);
        let root_str = root.to_str().unwrap_or_default();

        let (code, _) = run_unlace(&[
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

    let (code, _) = run_unlace(&[
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

    let (code, _) = run_unlace(&[
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
