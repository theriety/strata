//! Presentation behavior through the compiled CLI, using small saved results.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::Command;
use strata_engine::{
    AnalyzeResult, ContainerNode, FileMove, Level, MirrorMove, Move, MoveKind, MoveReason,
    SymbolKind, SymbolMove, SymbolPlacement,
};

use unicode_width::UnicodeWidthStr;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn execute(args: &[&str]) -> TestResult<String> {
    let output = Command::cargo_bin("strata")?.args(args).output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn analyze(format: &str, verbose: bool) -> TestResult<String> {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fixtures/constellation-ts");
    let mut args = vec![
        "analyze",
        "--root",
        root.to_str().ok_or("non-UTF8 root")?,
        "--config",
        "/nonexistent/report-presentation.toml",
        "--format",
        format,
    ];
    if verbose {
        args.push("--verbose");
    }
    execute(&args)
}

fn saved_report(result: &AnalyzeResult, verbose: bool) -> TestResult<String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "strata-presentation-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, serde_json::to_vec(result)?)?;
    let mut args = vec!["report", "--input", path.to_str().ok_or("non-UTF8 input")?];
    if verbose {
        args.push("--verbose");
    }
    let outcome = execute(&args);
    fs::remove_file(path)?;
    outcome
}

fn plain_content(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .map(|line| line.trim_start_matches('#').trim().replace("**", ""))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn should_keep_same_ordered_report_content_at_each_verbosity() -> TestResult {
    let result = serde_json::from_str(&analyze("json", false)?)?;
    for verbose in [false, true] {
        let terminal = plain_content(&analyze("summary", verbose)?);
        let markdown = plain_content(&saved_report(&result, verbose)?);
        assert_eq!(terminal, markdown);
        let findings = terminal
            .find("Structural findings")
            .ok_or("missing findings")?;
        let candidates = terminal
            .find("Candidate layouts")
            .ok_or("missing candidates")?;
        let advice = terminal.find("Advice").ok_or("missing advice")?;
        assert!(findings < candidates && candidates < advice);
    }
    Ok(())
}

#[test]
fn should_leave_json_bytes_unchanged_with_verbose() -> TestResult {
    assert_eq!(analyze("json", false)?, analyze("json", true)?);
    Ok(())
}

fn file(name: &str, symbol: Option<&str>) -> ContainerNode {
    ContainerNode {
        name: name.into(),
        level: Level::File,
        children: None,
        symbols: Some(
            symbol
                .into_iter()
                .map(|symbol_name| SymbolPlacement {
                    name: symbol_name.into(),
                    visibility: Level::File,
                })
                .collect(),
        ),
        production_sloc: Some(1),
    }
}

fn tree(source: Option<&str>, destination: Option<&str>) -> ContainerNode {
    ContainerNode {
        name: "project".into(),
        level: Level::Package,
        children: Some(vec![
            file("source.ts", source),
            file("destination.ts", destination),
            file("untouched.ts", None),
        ]),
        symbols: None,
        production_sloc: None,
    }
}

#[test]
fn should_place_moved_symbol_under_actual_before_and_after_files() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.current.tree = tree(Some("Options"), None);
    result.profiles.anchored = None;
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing profile")?;
    profile.candidates.truncate(1);
    let candidate = profile.candidates.first_mut().ok_or("missing candidate")?;
    candidate.tree = tree(None, Some("Options"));
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "source.ts".into(),
        to_path: "destination.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];

    let output = saved_report(&result, false)?;
    let before_start = output.find("Before").ok_or("missing before tree")?;
    let after_start = output[before_start..]
        .find("After")
        .ok_or("missing after tree")?
        + before_start;
    let before = &output[before_start..after_start];
    let after = output[after_start..]
        .split("Advice")
        .next()
        .ok_or("missing advice boundary")?;
    assert!(before.contains("source.ts *\n") && after.contains("destination.ts *\n"));
    assert!(before.lines().collect::<Vec<_>>().windows(2).any(|lines| {
        matches!(lines, [parent, symbol] if parent.contains("source.ts *") && symbol.contains("type `Options` [to destination.ts]"))
    }));
    assert!(!before.contains("[moved in]"));
    assert!(after.lines().collect::<Vec<_>>().windows(2).any(|lines| {
        matches!(lines, [parent, symbol] if parent.contains("destination.ts *") && symbol.contains("type `Options` [from source.ts]"))
    }));
    assert!(!after.contains("[moves out]"));
    assert!(!before.contains("untouched.ts") && !after.contains("untouched.ts"));
    assert!(
        after.contains("source.ts *"),
        "empty source file is retained"
    );
    Ok(())
}

#[test]
fn should_show_every_candidate_and_signed_improvement_without_advice_duplication() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.profiles.anchored = None;
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing profile")?;
    let mut first = profile
        .candidates
        .first()
        .ok_or("missing candidate")?
        .clone();
    first.index = 1;
    first.score = profile.current.score;
    first.improvement = 0.0;
    let mut second = first.clone();
    second.index = 2;
    second.score += 0.25;
    second.improvement = -0.25;
    profile.candidates = vec![first, second];

    let report = plain_content(&saved_report(&result, false)?);

    let first_position = report
        .find("candidate 1")
        .ok_or("missing first candidate")?;
    let second_position = report
        .find("candidate 2")
        .ok_or("missing second candidate")?;
    let advice_position = report.find("Advice").ok_or("missing advice")?;
    assert!(first_position < second_position && second_position < advice_position);
    assert!(report.contains("+0.0000") && report.contains("-0.2500"));
    assert_eq!(report.matches("Recommended (").count(), 1);
    Ok(())
}

#[test]
fn should_distinguish_no_candidate_from_candidate_with_no_moves() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.profiles.anchored = None;
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing profile")?;
    profile.candidates.truncate(1);
    let candidate = profile.candidates.first_mut().ok_or("missing candidate")?;
    candidate.tree = result.current.tree.clone();
    candidate.delta_narration.clear();
    candidate.symbol_moves.clear();
    let unchanged = saved_report(&result, false)?;
    result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing profile")?
        .candidates
        .clear();
    let empty = saved_report(&result, false)?;

    assert!(unchanged.contains("No moves versus the current layout."));
    assert!(!empty.contains("candidate 1"));
    assert!(empty.to_lowercase().contains("no candidates"));
    Ok(())
}

#[test]
fn should_wrap_long_symbol_paths_without_losing_their_characters() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.profiles.anchored = None;
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing profile")?;
    profile.candidates.truncate(1);
    let candidate = profile.candidates.first_mut().ok_or("missing candidate")?;
    let from = format!("src/{}/options.ts", "界e\u{301}_".repeat(30));
    let to = format!("src/{}/options.ts", "different_component_".repeat(8));
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "LongOptions".into(),
        kind: SymbolKind::Type,
        from_path: from.clone(),
        to_path: to.clone(),
        delta: -0.1,
        broken_imports: 0,
    }];

    let report = saved_report(&result, false)?;

    assert!(
        report.lines().all(|line| line.width() <= 100),
        "report exceeds its width contract"
    );
    let unwrapped: String = report
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    assert!(
        unwrapped.contains(&from) && unwrapped.contains(&to),
        "wrapping must preserve paths"
    );
    let (_, trees) = report.split_once("Before").ok_or("missing before")?;
    let (before, after) = trees.split_once("After").ok_or("missing after")?;
    for (tree, annotation) in [
        (before, format!("type `LongOptions` [to {to}]")),
        (after, format!("type `LongOptions` [from {from}]")),
    ] {
        let mut lines = tree.lines();
        let first = lines
            .find(|line| line.contains("type `LongOptions`"))
            .ok_or("missing symbol annotation")?;
        let (ancestry, content) = first.split_once("└── ").ok_or("missing symbol branch")?;
        let continuation = format!("{ancestry}    ");
        let mut restored = content.to_owned();
        while restored.len() < annotation.len() {
            let line = lines.next().ok_or("missing symbol continuation")?;
            restored.push_str(
                line.strip_prefix(&continuation)
                    .ok_or("symbol continuation lost tree geometry")?,
            );
        }
        assert_eq!(
            restored, annotation,
            "endpoint lost during wrapping: {tree}"
        );
    }
    Ok(())
}

#[test]
fn should_follow_relocated_symbol_destination_and_mirrored_test_in_paired_trees() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.current.tree = folder(
        "project",
        Level::Package,
        vec![
            file("source.ts", Some("Options")),
            file("destination.ts", None),
            folder(
                "tests",
                Level::Folder,
                vec![file("destination.spec.ts", None)],
            ),
        ],
    );
    result.profiles.anchored = None;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| {
            profile.candidates.truncate(1);
            profile.candidates.first_mut()
        })
        .ok_or("missing candidate")?;
    candidate.tree = folder(
        "project",
        Level::Package,
        vec![
            file("source.ts", None),
            folder(
                "nested",
                Level::Folder,
                vec![file("destination.ts", Some("Options"))],
            ),
            folder(
                "tests",
                Level::Folder,
                vec![folder(
                    "nested",
                    Level::Folder,
                    vec![file("destination.spec.ts", None)],
                )],
            ),
        ],
    );
    candidate.delta_narration = vec![Move {
        kind: MoveKind::Move,
        files: vec![FileMove {
            path: "project/destination.ts".into(),
            from: "project".into(),
        }],
        to: "project/nested".into(),
        reason: MoveReason::Clustering,
        mirrors: vec![MirrorMove {
            source_path: "project/destination.ts".into(),
            path: "project/tests/destination.spec.ts".into(),
            from: "project/tests".into(),
            to: "project/tests/nested".into(),
        }],
        blocked_mirrors: vec![],
    }];
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "source.ts".into(),
        to_path: "destination.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];

    let report = saved_report(&result, false)?;
    let before_start = report.find("Before").ok_or("missing before")?;
    let after_start = before_start
        + report[before_start..]
            .find("After")
            .ok_or("missing after")?;
    let before = &report[before_start..after_start];
    let after = report[after_start..]
        .split("Advice")
        .next()
        .ok_or("missing advice")?;

    assert!(
        !before
            .lines()
            .any(|line| line.trim_end().ends_with("nested/"))
            && after.matches("nested/").count() == 2
    );
    for tree in [before, after] {
        assert!(tree.contains("destination.spec.ts *"));
    }
    assert!(after.lines().collect::<Vec<_>>().windows(3).any(|lines| {
        matches!(lines, [folder, file, symbol] if folder.contains("nested/") && file.contains("destination.ts *") && symbol.contains("Options` [from source.ts]"))
    }));
    assert!(before.contains("Options` [to nested/destination.ts]"));
    Ok(())
}

fn folder(name: &str, level: Level, children: Vec<ContainerNode>) -> ContainerNode {
    ContainerNode {
        name: name.into(),
        level,
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

#[test]
fn should_keep_duplicate_filenames_distinct_across_roots() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.current.tree = folder(
        "workspace",
        Level::PackageGroup,
        vec![
            folder(
                "left",
                Level::Package,
                vec![file("options.ts", Some("Options"))],
            ),
            folder("right", Level::Package, vec![file("options.ts", None)]),
        ],
    );
    result.profiles.anchored = None;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| {
            profile.candidates.truncate(1);
            profile.candidates.first_mut()
        })
        .ok_or("missing candidate")?;
    candidate.tree = folder(
        "workspace",
        Level::PackageGroup,
        vec![
            folder("left", Level::Package, vec![file("options.ts", None)]),
            folder(
                "right",
                Level::Package,
                vec![file("options.ts", Some("Options"))],
            ),
        ],
    );
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "left/options.ts".into(),
        to_path: "right/options.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];

    let report = saved_report(&result, false)?;
    let before_start = report.find("Before").ok_or("missing before")?;
    let after_start = before_start
        + report[before_start..]
            .find("After")
            .ok_or("missing after")?;
    let before = &report[before_start..after_start];
    let after = report[after_start..]
        .split("Advice")
        .next()
        .ok_or("missing advice")?;

    assert!(before.lines().collect::<Vec<_>>().windows(3).any(|lines| {
        matches!(lines, [root, file, symbol] if root.contains("left/") && file.contains("options.ts *") && symbol.contains("Options` [to right/options.ts]"))
    }));
    assert!(after.lines().collect::<Vec<_>>().windows(3).any(|lines| {
        matches!(lines, [root, file, symbol] if root.contains("right/") && file.contains("options.ts *") && symbol.contains("Options` [from left/options.ts]"))
    }));
    for tree in [before, after] {
        assert_eq!(tree.matches("options.ts *").count(), 2);
    }
    Ok(())
}

#[test]
fn should_preserve_symbol_directory_named_like_project_root() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.current.tree = folder(
        "project",
        Level::Package,
        vec![folder(
            "project",
            Level::Folder,
            vec![
                file("source.ts", Some("Options")),
                file("destination.ts", None),
            ],
        )],
    );
    result.profiles.anchored = None;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| {
            profile.candidates.truncate(1);
            profile.candidates.first_mut()
        })
        .ok_or("missing candidate")?;
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "project/source.ts".into(),
        to_path: "project/destination.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];
    candidate.tree = folder(
        "project",
        Level::Package,
        vec![folder(
            "project",
            Level::Folder,
            vec![
                file("source.ts", None),
                file("destination.ts", Some("Options")),
            ],
        )],
    );

    let report = saved_report(&result, false)?;

    assert!(report.contains("`project/source.ts`") && report.contains("`project/destination.ts`"));
    let before_start = report.find("Before").ok_or("missing before")?;
    let after_start = before_start
        + report[before_start..]
            .find("After")
            .ok_or("missing after")?;
    for tree in [
        &report[before_start..after_start],
        report[after_start..]
            .split("Advice")
            .next()
            .ok_or("missing advice")?,
    ] {
        assert_eq!(
            tree.lines()
                .filter(|line| line.trim_end().ends_with("project/"))
                .count(),
            2,
            "root and same-name child must both survive: {tree}"
        );
    }
    Ok(())
}

#[test]
fn should_preserve_same_name_destination_folder_for_whole_file_move() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.current.tree = folder("project", Level::Package, vec![file("source.ts", None)]);
    result.profiles.anchored = None;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| {
            profile.candidates.truncate(1);
            profile.candidates.first_mut()
        })
        .ok_or("missing candidate")?;
    candidate.symbol_moves.clear();
    candidate.delta_narration = vec![Move {
        kind: MoveKind::Move,
        files: vec![FileMove {
            path: "project/source.ts".into(),
            from: "project".into(),
        }],
        to: "project/project".into(),
        reason: MoveReason::Clustering,
        mirrors: vec![],
        blocked_mirrors: vec![],
    }];
    candidate.tree = folder(
        "project",
        Level::Package,
        vec![folder(
            "project",
            Level::Folder,
            vec![file("source.ts", None)],
        )],
    );

    let report = saved_report(&result, false)?;
    let after_start = report.find("After").ok_or("missing after")?;
    let after = report[after_start..]
        .split("Advice")
        .next()
        .ok_or("missing advice")?;

    assert_eq!(
        after.matches("project/").count(),
        2,
        "root and destination directory must remain: {after}"
    );
    assert!(after.contains("source.ts *"));
    Ok(())
}

#[test]
fn should_preserve_literal_repeated_spaces_and_backticks_in_move_paths() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.profiles.anchored = None;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| {
            profile.candidates.truncate(1);
            profile.candidates.first_mut()
        })
        .ok_or("missing candidate")?;
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "src/a`  b.ts".into(),
        to_path: "src/c  d.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];

    let report = saved_report(&result, false)?;

    for path in ["src/a`  b.ts", "src/c  d.ts"] {
        assert!(
            report.contains(path),
            "literal filename whitespace must survive: {report}"
        );
    }
    assert!(report.contains("a`  b.ts *") && report.contains("c  d.ts *"));
    Ok(())
}

#[path = "support/combined_package_moves.rs"]
mod combined_package_moves;

#[test]
fn should_follow_combined_multi_package_moves_in_markdown() -> TestResult {
    let mut result: AnalyzeResult = serde_json::from_str(&analyze("json", false)?)?;
    result.profiles.anchored = None;
    combined_package_moves::configure(&mut result)?;

    combined_package_moves::assert_paths(&saved_report(&result, false)?)?;
    Ok(())
}
