//! A producer-shaped saved result with a file move and a symbol entering that file.

use strata_engine::{
    AnalyzeResult, ContainerNode, FileMove, Level, Move, MoveKind, MoveReason, SymbolKind,
    SymbolMove, SymbolPlacement,
};

fn file(name: &str, symbols: &[&str]) -> ContainerNode {
    ContainerNode {
        name: name.into(),
        level: Level::File,
        children: None,
        symbols: Some(
            symbols
                .iter()
                .map(|symbol_name| SymbolPlacement {
                    name: (*symbol_name).into(),
                    visibility: Level::File,
                })
                .collect(),
        ),
        production_sloc: Some(1),
    }
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

pub fn configure(result: &mut AnalyzeResult) -> Result<(), &'static str> {
    result.current.tree = folder(
        "workspace",
        Level::PackageGroup,
        vec![
            folder(
                "left",
                Level::Package,
                vec![file("left/source.ts", &["Options"])],
            ),
            folder(
                "right",
                Level::Package,
                vec![file("right/destination.ts", &[])],
            ),
        ],
    );
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .or(result.profiles.anchored.as_mut())
        .ok_or("missing profile")?;
    profile.candidates.truncate(1);
    let candidate = profile.candidates.first_mut().ok_or("missing candidate")?;
    candidate.tree = folder(
        "workspace",
        Level::PackageGroup,
        vec![
            folder("left", Level::Package, vec![file("left/source.ts", &[])]),
            folder(
                "right",
                Level::Package,
                vec![folder(
                    "nested",
                    Level::Folder,
                    vec![file("right/destination.ts", &["Options"])],
                )],
            ),
        ],
    );
    candidate.delta_narration = vec![Move {
        kind: MoveKind::Move,
        files: vec![FileMove {
            path: "workspace/right/destination.ts".into(),
            from: "workspace/right".into(),
        }],
        to: "workspace/right/nested".into(),
        reason: MoveReason::Clustering,
        mirrors: vec![],
        blocked_mirrors: vec![],
    }];
    candidate.symbol_moves = vec![SymbolMove {
        symbol: "Options".into(),
        kind: SymbolKind::Type,
        from_path: "left/source.ts".into(),
        to_path: "right/destination.ts".into(),
        delta: -0.1,
        broken_imports: 0,
    }];
    Ok(())
}

pub fn assert_paths(report: &str) -> Result<(), &'static str> {
    let (_, trees) = report.split_once("Before").ok_or("missing before tree")?;
    let (before, remainder) = trees.split_once("After").ok_or("missing after tree")?;
    let after = remainder.split("Advice").next().ok_or("missing advice")?;
    for tree in [before, after] {
        assert_eq!(
            tree.matches("destination.ts *").count(),
            1,
            "file identity duplicated: {tree}"
        );
        assert_eq!(
            tree.matches("source.ts *").count(),
            1,
            "source must survive: {tree}"
        );
        for package in ["left/", "right/"] {
            assert_eq!(
                tree.matches(package).count(),
                1,
                "package directory lost or duplicated: {tree}"
            );
        }
    }
    assert!(before.lines().collect::<Vec<_>>().windows(3).any(|lines| {
        matches!(lines, [package, file, symbol] if package.contains("left/") && file.contains("source.ts *") && symbol.contains("Options` [moves out]"))
    }), "wrong source: {before}");
    assert!(after.lines().collect::<Vec<_>>().windows(4).any(|lines| {
        matches!(lines, [package, folder, file, symbol] if package.contains("right/") && folder.contains("nested/") && file.contains("destination.ts *") && symbol.contains("Options` [moved in]"))
    }), "symbol must follow the whole-file move: {after}");
    assert!(
        !before.contains("nested/")
            && !before.contains("[moved in]")
            && !after.contains("[moves out]")
    );
    Ok(())
}
