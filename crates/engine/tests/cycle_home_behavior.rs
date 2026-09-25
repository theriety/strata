//! Public analysis regressions for physical homes inside a condensed file cycle.

use strata_engine::ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IntermediateRepresentation,
    Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};
use strata_engine::ir::{Layout, build_laminar_tree};
use strata_engine::{AnalyzeConfig, ContainerNode, Level, analyze};

fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
    Container {
        id: ContainerId(id),
        name: name.into(),
        level,
        parent: parent.map(ContainerId),
        synthetic: false,
    }
}

fn node(id: u32, name: &str, file: u32, size: u32) -> Node {
    Node {
        id: NodeId(id),
        name: name.into(),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container: ContainerId(file),
        visibility: ScopeLevel::File,
        effective_size: size,
        re_export: false,
    }
}

fn call(source: u32, target: u32) -> Edge {
    Edge {
        source: NodeId(source),
        target: NodeId(target),
        kind: EdgeKind::Call,
        hardness: Hardness::Hard,
        confidence: 1.0,
    }
}

fn cycle_snapshot(
    left_size: u32,
    right_size: u32,
    with_resident: bool,
) -> Result<Snapshot, String> {
    let mut nodes = vec![
        node(0, "motor", 3, left_size),
        node(1, "shaft", 4, right_size),
        node(2, "ignition", 5, 1),
        node(3, "torque", 3, 1),
        node(4, "start", 6, 1),
    ];
    let mut containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "engine", ScopeLevel::Folder, Some(0)),
        container(2, "drivetrain", ScopeLevel::Folder, Some(0)),
        container(3, "engine/motor.ts", ScopeLevel::File, Some(1)),
        container(4, "drivetrain/shaft.ts", ScopeLevel::File, Some(2)),
        container(5, "engine/ignition.ts", ScopeLevel::File, Some(1)),
        container(6, "app.ts", ScopeLevel::File, Some(0)),
    ];
    if with_resident {
        nodes.push(node(5, "coupling", 7, 1));
        containers.push(container(
            7,
            "drivetrain/coupling.ts",
            ScopeLevel::File,
            Some(2),
        ));
    }
    Snapshot::assemble(IntermediateRepresentation::new(
        nodes,
        vec![call(0, 1), call(1, 3), call(2, 3), call(4, 0)],
        ContainerTree::new(containers),
    ))
    .map_err(|error| error.to_string())
}

fn file_homes(tree: &ContainerNode) -> Vec<(String, String)> {
    collect_homes(tree, "")
}

fn collect_homes(tree: &ContainerNode, ancestors: &str) -> Vec<(String, String)> {
    let parent_segments: Vec<_> = ancestors
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let child_segments: Vec<_> = tree.name.split('/').collect();
    let overlap = (0..=parent_segments.len().min(child_segments.len()))
        .rev()
        .find(|count| {
            parent_segments
                .iter()
                .skip(parent_segments.len() - count)
                .eq(child_segments.iter().take(*count))
        })
        .unwrap_or_default();
    let parent_path = parent_segments
        .iter()
        .copied()
        .chain(child_segments.iter().skip(overlap).copied())
        .collect::<Vec<_>>()
        .join("/");
    let mut homes = Vec::new();
    for child in tree.children.iter().flatten() {
        if child.level == Level::File {
            homes.push((child.name.clone(), parent_path.clone()));
        } else {
            homes.extend(collect_homes(child, &parent_path));
        }
    }
    homes.sort();
    homes
}

#[test]
fn should_preserve_cycle_homes_without_an_unrelated_nondominant_resident() -> Result<(), String> {
    for (left_size, right_size) in [(5, 1), (1, 2)] {
        let snapshot = cycle_snapshot(left_size, right_size, false)?;
        let result =
            analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
        let profile = result
            .profiles
            .greenfield
            .as_ref()
            .ok_or("missing greenfield")?;
        let candidate = profile
            .candidates
            .first()
            .ok_or("missing greenfield candidate")?;

        assert!(
            candidate
                .delta_narration
                .iter()
                .flat_map(|movement| &movement.files)
                .any(|file| file.path.ends_with("/app.ts") || file.path == "app.ts"),
            "a legal unrelated move must remain available"
        );

        let cycle_homes = |tree: &ContainerNode| {
            file_homes(tree)
                .into_iter()
                .filter(|(path, _)| path.ends_with("/motor.ts") || path.ends_with("/shaft.ts"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cycle_homes(&candidate.tree),
            cycle_homes(&result.current.tree),
            "unchanged cycle members retain their physical homes (left SLOC {left_size})"
        );
    }
    Ok(())
}

#[test]
fn should_keep_cycle_members_beside_their_unrelated_original_residents() -> Result<(), String> {
    let snapshot = cycle_snapshot(5, 1, true)?;
    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    let candidate = result
        .profiles
        .greenfield
        .as_ref()
        .and_then(|profile| profile.candidates.first())
        .ok_or("missing greenfield candidate")?;
    let homes: Vec<_> = file_homes(&candidate.tree)
        .into_iter()
        .filter(|(path, _)| path.starts_with("drivetrain/"))
        .collect();

    assert_eq!(
        homes,
        vec![
            (
                "drivetrain/coupling.ts".to_owned(),
                "workspace/drivetrain".to_owned()
            ),
            (
                "drivetrain/shaft.ts".to_owned(),
                "workspace/drivetrain".to_owned()
            ),
        ],
        "a physical directory must remain whole: {:#?}",
        candidate.tree
    );
    Ok(())
}

#[test]
fn should_move_every_member_when_a_cycle_legally_changes_home() -> Result<(), String> {
    let snapshot = Snapshot::assemble(IntermediateRepresentation::new(
        vec![
            node(0, "motor", 3, 1),
            node(1, "torque", 3, 1),
            node(2, "shaft", 4, 1),
            node(3, "owner", 5, 1),
            node(4, "resident", 6, 1),
        ],
        vec![call(0, 2), call(2, 1), call(0, 3), call(1, 3), call(2, 3)],
        ContainerTree::new(vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/motor.ts", ScopeLevel::File, Some(1)),
            container(4, "shaft.ts", ScopeLevel::File, Some(0)),
            container(5, "destination/owner.ts", ScopeLevel::File, Some(2)),
            container(6, "origin/resident.ts", ScopeLevel::File, Some(1)),
        ]),
    ))
    .map_err(|error| error.to_string())?;
    let mut config = AnalyzeConfig::default();
    config.profiles.greenfield.relocation.forbid_file_moves =
        vec!["destination/owner.ts".to_owned()];
    config.profiles.greenfield.relocation.forbid_symbol_moves = vec!["**".to_owned()];
    let result = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    let candidate = result
        .profiles
        .greenfield
        .as_ref()
        .and_then(|profile| profile.candidates.first())
        .ok_or("missing greenfield candidate")?;
    let homes: Vec<_> = file_homes(&candidate.tree)
        .into_iter()
        .filter(|(path, _)| path.ends_with("motor.ts") || path.ends_with("shaft.ts"))
        .collect();

    assert_eq!(
        homes,
        vec![
            (
                "origin/motor.ts".to_owned(),
                "workspace/destination".to_owned()
            ),
            ("shaft.ts".to_owned(), "workspace/destination".to_owned()),
        ],
        "an admitted whole-cycle move must move every physical member: {candidate:#?}"
    );
    Ok(())
}

fn declared_cycle(paths: [&str; 4], layout: &Layout) -> Result<Snapshot, String> {
    let paths: Vec<smol_str::SmolStr> = paths.into_iter().map(Into::into).collect();
    let built = build_laminar_tree(&paths, "workspace", layout);
    let files = paths
        .iter()
        .map(|path| {
            built
                .files
                .get(path)
                .copied()
                .ok_or_else(|| format!("missing file {path}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut nodes = Vec::new();
    for (id, name, file_index, size) in [
        (0, "motor", 0, 5),
        (1, "shaft", 1, 1),
        (2, "ignition", 2, 1),
        (3, "torque", 0, 1),
        (4, "start", 3, 1),
    ] {
        let file = files.get(file_index).ok_or("missing fixture file")?;
        nodes.push(node(id, name, file.0, size));
    }
    Snapshot::assemble(IntermediateRepresentation::new(
        nodes,
        vec![call(0, 1), call(1, 3), call(2, 3), call(4, 0)],
        built.tree,
    ))
    .map_err(|error| error.to_string())
}

#[test]
fn should_preserve_a_cycle_members_only_synthetic_root_home() -> Result<(), String> {
    let snapshot = declared_cycle(
        [
            "engine/motor.ts",
            "shaft.ts",
            "engine/ignition.ts",
            "outside/app.ts",
        ],
        &Layout::default(),
    )?;
    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    let candidate = result
        .profiles
        .greenfield
        .as_ref()
        .and_then(|profile| profile.candidates.first())
        .ok_or("missing greenfield candidate")?;
    let root_home = |tree: &ContainerNode| {
        file_homes(tree)
            .into_iter()
            .filter(|(path, _)| path == "shaft.ts")
            .collect::<Vec<_>>()
    };

    assert_eq!(
        root_home(&candidate.tree),
        root_home(&result.current.tree),
        "the sole root member must not acquire a physical folder: {:#?}",
        candidate.tree
    );
    Ok(())
}

#[test]
fn should_distinguish_repeated_folder_names_below_a_transparent_package_root() -> Result<(), String>
{
    let snapshot = declared_cycle(
        [
            "pkg/src/left/common/motor.ts",
            "pkg/src/right/common/shaft.ts",
            "pkg/src/left/common/ignition.ts",
            "pkg/src/outside/app.ts",
        ],
        &Layout {
            package_roots: vec!["pkg".into()],
            source_roots: vec!["src".into()],
        },
    )?;
    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    let candidate = result
        .profiles
        .greenfield
        .as_ref()
        .and_then(|profile| profile.candidates.first())
        .ok_or("missing greenfield candidate")?;
    let cycle_homes = |tree: &ContainerNode| {
        file_homes(tree)
            .into_iter()
            .filter(|(path, _)| path.ends_with("motor.ts") || path.ends_with("shaft.ts"))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        cycle_homes(&candidate.tree),
        cycle_homes(&result.current.tree),
        "repeated leaf names must retain their distinct qualified homes: {:#?}",
        candidate.tree
    );
    Ok(())
}
