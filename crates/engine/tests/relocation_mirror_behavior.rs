//! Generated graph regressions for pinned files and exact test-mirror followers.
//!
//! The fixture uses only neutral IR declarations. It deliberately includes two
//! production files with the same basename: basename inference is ambiguous,
//! while an exact `{dir}`/`{stem}` rule has one answer.

use serde_json::Value;
use smol_str::SmolStr;
use strata_engine::ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IntermediateRepresentation,
    Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};
use strata_engine::{AnalyzeConfig, ProfileName, analyze};

fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
    Container {
        id: ContainerId(id),
        name: SmolStr::new(name),
        level,
        parent: parent.map(ContainerId),
        synthetic: false,
    }
}

fn node(id: u32, name: &str, file: u32, polarity: Polarity) -> Node {
    Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity,
        container: ContainerId(file),
        visibility: ScopeLevel::File,
        effective_size: 1,
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

fn exact_mirror_snapshot(include_mirror: bool, fill_test_destination: bool) -> Snapshot {
    exact_mirror_snapshot_for_extension(include_mirror, fill_test_destination, "ts")
}

fn exact_mirror_snapshot_for_extension(
    include_mirror: bool,
    fill_test_destination: bool,
    extension: &str,
) -> Snapshot {
    exact_mirror_snapshot_with_names(
        include_mirror,
        fill_test_destination,
        &format!("task.{extension}"),
        &format!("spec/module/task.spec.{extension}"),
    )
}

fn exact_mirror_snapshot_with_names(
    include_mirror: bool,
    fill_test_destination: bool,
    source_name: &str,
    mirror_path: &str,
) -> Snapshot {
    let mirror_folder = mirror_path
        .rsplit_once('/')
        .map_or("spec/module", |(path, _)| path);
    let mirror_root = mirror_folder.split('/').next().unwrap_or("spec");
    let mut containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "source/module", ScopeLevel::Folder, Some(0)),
        container(2, "source/other", ScopeLevel::Folder, Some(0)),
        container(3, "source/target", ScopeLevel::Folder, Some(0)),
        container(4, mirror_folder, ScopeLevel::Folder, Some(0)),
        container(
            5,
            &format!("{mirror_root}/target"),
            ScopeLevel::Folder,
            Some(0),
        ),
        container(
            6,
            &format!("source/module/{source_name}"),
            ScopeLevel::File,
            Some(1),
        ),
        container(7, "source/module/alpha.ts", ScopeLevel::File, Some(1)),
        container(8, "source/module/beta.ts", ScopeLevel::File, Some(1)),
        container(9, "source/other/task.ts", ScopeLevel::File, Some(2)),
        container(10, "source/target/owner.ts", ScopeLevel::File, Some(3)),
    ];
    let mut nodes = vec![
        node(0, "perform_task", 6, Polarity::Production),
        node(1, "alpha", 7, Polarity::Production),
        node(2, "beta", 8, Polarity::Production),
        node(3, "other_task", 9, Polarity::Production),
        node(4, "own_task", 10, Polarity::Production),
    ];
    let mut edges = vec![call(0, 4)];

    if include_mirror {
        containers.push(container(11, mirror_path, ScopeLevel::File, Some(4)));
        nodes.push(node(5, "task_spec", 11, Polarity::TestCase));
        edges.push(call(5, 0));
    }
    if fill_test_destination {
        for (offset, name) in ["task-support-a.spec.ts", "task-support-b.spec.ts"]
            .into_iter()
            .enumerate()
        {
            let id = 11 + u32::from(include_mirror) + u32::try_from(offset).unwrap_or_default();
            containers.push(container(
                id,
                &format!("spec/module/{name}"),
                ScopeLevel::File,
                Some(4),
            ));
            let support_node =
                5 + u32::from(include_mirror) + u32::try_from(offset).unwrap_or_default();
            nodes.push(node(support_node, name, id, Polarity::TestCase));
            edges.push(call(5, support_node));
            edges.push(call(support_node, 5));
        }
    }

    let assembled = Snapshot::assemble(IntermediateRepresentation::new(
        nodes,
        edges,
        ContainerTree::new(containers),
    ));
    assert!(assembled.is_ok(), "neutral mirror fixture must assemble");
    match assembled {
        Ok(snapshot) => snapshot,
        Err(_) => std::process::abort(),
    }
}

#[test]
fn should_apply_builtin_mirrors_for_all_discovered_typescript_extensions() {
    for extension in ["ts", "tsx", "mts", "cts"] {
        let snapshot = exact_mirror_snapshot_for_extension(true, false, extension);
        let mut config = mirror_config();
        config.profiles.anchored.relocation.test_mirroring.builtins = true;
        config
            .profiles
            .anchored
            .relocation
            .test_mirroring
            .rules
            .clear();
        let json = serialized_analysis(&snapshot, &config);
        let source = format!("workspace/source/module/task.{extension}");
        let mirror = format!("workspace/spec/module/task.spec.{extension}");
        let primary = source_move(&json, &source);
        assert!(
            primary
                .and_then(|entry| entry.get("mirrors"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|entry| entry.get("path").and_then(Value::as_str) == Some(&mirror)),
            "built-in mirror missing for {extension}: {json}"
        );
    }
}

#[test]
fn should_apply_the_builtin_python_mirror() {
    let snapshot =
        exact_mirror_snapshot_with_names(true, false, "task.py", "tests/module/test_task.py");
    let mut config = mirror_config();
    config.profiles.anchored.relocation.test_mirroring.builtins = true;
    config
        .profiles
        .anchored
        .relocation
        .test_mirroring
        .rules
        .clear();
    let json = serialized_analysis(&snapshot, &config);
    let primary = source_move(&json, "workspace/source/module/task.py");
    assert!(
        primary
            .and_then(|entry| entry.get("mirrors"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|entry| entry.get("path").and_then(Value::as_str)
                == Some("workspace/tests/module/test_task.py")),
        "the Python built-in catalog must be shared with runtime: {json}"
    );
}

fn mirror_config() -> AnalyzeConfig {
    let mut config = AnalyzeConfig::default();
    config.analysis.profiles = vec![ProfileName::Anchored];
    config.profiles.anchored.capacity.folder = 2;
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules =
        vec![strata_engine::config::TestMirrorRule {
            source: "source/{dir}/{stem}.ts".to_owned(),
            tests: vec!["spec/{dir}/{stem}.spec.ts".to_owned()],
        }];
    config
}

fn serialized_analysis(snapshot: &Snapshot, config: &AnalyzeConfig) -> Value {
    analyze(snapshot, config)
        .ok()
        .and_then(|result| serde_json::to_value(result).ok())
        .unwrap_or(Value::Null)
}

fn moves(json: &Value) -> Vec<&Value> {
    json.pointer("/profiles/anchored/candidates/0/deltaNarration")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect()
}

fn source_move<'json>(json: &'json Value, source: &str) -> Option<&'json Value> {
    moves(json).into_iter().find(|entry| {
        entry
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|file| file.get("path").and_then(Value::as_str) == Some(source))
    })
}

#[test]
fn should_link_an_exact_nested_mirror_despite_a_colliding_basename() {
    let snapshot = exact_mirror_snapshot(true, false);
    let json = serialized_analysis(&snapshot, &mirror_config());
    let primary = source_move(&json, "workspace/source/module/task.ts");
    assert!(
        primary
            .and_then(|entry| entry.get("mirrors"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|mirror| {
                mirror.get("path").and_then(Value::as_str)
                    == Some("workspace/spec/module/task.spec.ts")
                    && mirror.get("to").and_then(Value::as_str) == Some("workspace/spec/target")
            }),
        "the exact nested mirror must follow the matching source: {json}"
    );
    assert!(
        moves(&json).into_iter().all(|entry| {
            entry
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .all(|file| {
                    file.get("path").and_then(Value::as_str)
                        != Some("workspace/spec/module/task.spec.ts")
                })
        }),
        "a mirror is linked once, never narrated independently: {json}"
    );
}

#[test]
fn should_serialize_linked_relocations_as_schema_version_seven() {
    let json = serialized_analysis(&exact_mirror_snapshot(true, false), &mirror_config());

    assert_eq!(json.get("schemaVersion").and_then(Value::as_u64), Some(7));
}

#[test]
fn should_keep_a_source_move_when_its_mirror_is_blocked_by_capacity() {
    let snapshot = exact_mirror_snapshot(true, true);
    let json = serialized_analysis(&snapshot, &mirror_config());
    let primary = source_move(&json, "workspace/source/module/task.ts");

    assert!(
        primary.is_some(),
        "the blocked follower must not veto its source: {json}"
    );
    assert!(
        primary
            .and_then(|entry| entry.get("blockedMirrors"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|mirror| {
                mirror.get("path").and_then(Value::as_str)
                    == Some("workspace/spec/module/task.spec.ts")
                    && mirror.get("intendedTo").and_then(Value::as_str)
                        == Some("workspace/spec/target")
                    && mirror.get("reason").and_then(Value::as_str) == Some("capacity")
            }),
        "the blocked mirror reports one deterministic reason: {json}"
    );
}

#[test]
fn should_not_change_a_source_move_when_no_mirror_exists() {
    let snapshot = exact_mirror_snapshot(false, false);
    let json = serialized_analysis(&snapshot, &mirror_config());
    let primary = source_move(&json, "workspace/source/module/task.ts");

    assert!(
        primary.is_some(),
        "the source remains independently eligible: {json}"
    );
    assert_eq!(
        primary
            .and_then(|entry| entry.get("mirrors"))
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        primary
            .and_then(|entry| entry.get("blockedMirrors"))
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
}

#[test]
fn should_not_follow_a_same_basename_test_when_mirroring_is_disabled() {
    let snapshot = exact_mirror_snapshot(true, false);
    let mut config = mirror_config();
    config.profiles.anchored.relocation.test_mirroring.enabled = false;
    config.profiles.anchored.relocation.pin_detected_test_files = false;
    let json = serialized_analysis(&snapshot, &config);
    let primary = source_move(&json, "workspace/source/module/task.ts");

    assert!(
        primary.is_some(),
        "setup: source relocation remains eligible: {json}"
    );
    assert!(
        primary
            .and_then(|entry| entry.get("mirrors"))
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "disabled mirroring must not infer a basename follower: {json}"
    );
    assert!(
        moves(&json).into_iter().all(|entry| entry
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .all(|file| file.get("path").and_then(Value::as_str)
                != Some("workspace/spec/module/task.spec.ts"))),
        "a test cannot independently follow through the legacy basename fallback: {json}"
    );
}

#[test]
fn should_pin_a_forbidden_file_without_removing_its_graph_evidence() {
    let snapshot = exact_mirror_snapshot(false, false);
    let baseline = serialized_analysis(&snapshot, &mirror_config());
    let mut pinned_config = mirror_config();
    pinned_config.profiles.anchored.relocation.forbid_file_moves =
        vec!["source/module/task.ts".to_owned()];
    let pinned = serialized_analysis(&snapshot, &pinned_config);

    assert!(
        source_move(&baseline, "workspace/source/module/task.ts").is_some(),
        "setup: the weighted source is normally eligible: {baseline}"
    );
    assert!(
        source_move(&pinned, "workspace/source/module/task.ts").is_none(),
        "the exact forbidden file cannot relocate: {pinned}"
    );
    assert_eq!(
        baseline.pointer("/profiles/anchored/current/score"),
        pinned.pointer("/profiles/anchored/current/score"),
        "pinning changes admission, not pass-start graph evidence"
    );
}

#[test]
fn should_apply_file_pinning_to_only_the_configured_profile() {
    let snapshot = exact_mirror_snapshot(false, false);
    let mut config = mirror_config();
    config.analysis.profiles = vec![ProfileName::Anchored, ProfileName::Greenfield];
    config.profiles.anchored.relocation.forbid_file_moves =
        vec!["source/module/task.ts".to_owned()];
    config.profiles.greenfield.capacity.folder = 2;
    config
        .profiles
        .greenfield
        .relocation
        .test_mirroring
        .builtins = false;
    let json = serialized_analysis(&snapshot, &config);

    assert!(source_move(&json, "workspace/source/module/task.ts").is_none());
    let greenfield_moves = json
        .pointer("/profiles/greenfield/candidates/0/deltaNarration")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    assert!(
        greenfield_moves.into_iter().any(|entry| {
            entry
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|file| {
                    file.get("path").and_then(Value::as_str)
                        == Some("workspace/source/module/task.ts")
                })
        }),
        "anchored pinning must not leak into greenfield: {json}"
    );
}
