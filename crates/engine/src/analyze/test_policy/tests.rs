//! Unit tests for the one test policy: node and use verdicts, the support-crate
//! rule and the polarity re-label.

use smol_str::SmolStr;
use unlace_ir::{ContainerTree, IntermediateRepresentation, Polarity, ScopeLevel};

use super::*;
use crate::analyze::test_support::{container, node};

fn policy(patterns: &[&str], builtins: bool) -> TestPolicy {
    match TestPolicy::new(&TestsConfig {
        patterns: patterns.iter().map(|&pattern| pattern.to_owned()).collect(),
        builtins,
        ..TestsConfig::default()
    }) {
        Ok(compiled) => compiled,
        Err(error) => unreachable!("fixture patterns compile: {error}"),
    }
}

/// One node per file, declared with the given polarity.
fn ir_of(files: &[(&str, Polarity)]) -> IntermediateRepresentation {
    let nodes = files
        .iter()
        .enumerate()
        .map(|(index, (_, polarity))| {
            let id = u32::try_from(index).unwrap_or(u32::MAX);
            node(id, "item", id, *polarity)
        })
        .collect();
    let containers = files
        .iter()
        .enumerate()
        .map(|(index, (path, _))| {
            let id = u32::try_from(index).unwrap_or(u32::MAX);
            container(id, path, ScopeLevel::File, None)
        })
        .collect();
    IntermediateRepresentation::new(nodes, Vec::new(), ContainerTree::new(containers))
}

fn relabelled(policy: &TestPolicy, files: &[(&str, Polarity)]) -> Vec<Polarity> {
    let mut ir = ir_of(files);
    policy.relabel(&mut ir);
    ir.nodes.iter().map(|node| node.polarity).collect()
}

#[test]
fn should_leave_every_polarity_alone_under_the_default_policy() {
    let files = [
        ("spec/a.spec.ts", Polarity::TestCase),
        ("spec/helper.ts", Polarity::TestSupport),
        ("src/app.ts", Polarity::Production),
        ("src/tests/support.rs", Polarity::Production),
    ];

    assert_eq!(
        relabelled(&TestPolicy::defaults(), &files),
        files.map(|(_, polarity)| polarity)
    );
}

#[test]
fn should_make_the_nodes_of_a_negated_file_production() {
    let files = [
        ("spec/fixtures/h.ts", Polarity::TestCase),
        ("spec/fixtures/s.ts", Polarity::TestSupport),
        ("spec/a.spec.ts", Polarity::TestCase),
    ];

    assert_eq!(
        relabelled(&policy(&["!spec/fixtures/**"], true), &files),
        [
            Polarity::Production,
            Polarity::Production,
            Polarity::TestCase
        ]
    );
}

#[test]
fn should_make_negation_win_over_a_positive_pattern() {
    let files = [
        ("checks/a.ts", Polarity::TestCase),
        ("checks/b.ts", Polarity::Production),
    ];

    assert_eq!(
        relabelled(&policy(&["checks/**", "!checks/a.ts"], true), &files),
        [Polarity::Production, Polarity::TestSupport]
    );
}

#[test]
fn should_make_a_pattern_marked_production_node_test_support_and_keep_a_stated_case() {
    let files = [
        ("checks/h.ts", Polarity::Production),
        ("checks/c.ts", Polarity::TestCase),
        ("src/app.ts", Polarity::Production),
    ];

    assert_eq!(
        relabelled(&policy(&["checks/**"], true), &files),
        [
            Polarity::TestSupport,
            Polarity::TestCase,
            Polarity::Production
        ]
    );
}

#[test]
fn should_drop_adapter_polarity_without_builtins_except_where_a_pattern_marks() {
    let files = [
        ("spec/a.spec.ts", Polarity::TestCase),
        ("checks/c.ts", Polarity::TestCase),
        ("src/helper.ts", Polarity::TestSupport),
    ];

    assert_eq!(
        relabelled(&policy(&["checks/**"], false), &files),
        [
            Polarity::Production,
            Polarity::TestCase,
            Polarity::Production
        ]
    );
}

#[test]
fn should_let_a_negation_override_the_test_source_fact_of_a_node_and_its_use() {
    let mut declared = node(0, "item", 0, Polarity::TestCase);
    declared.test_source = true;
    let tests = policy(&["!spec/fixtures/**", "checks/**"], true);

    assert!(tests.node_is_test(Some("spec/a.spec.ts"), &declared));
    assert!(!tests.node_is_test(Some("spec/fixtures/a.ts"), &declared));
    assert!(tests.node_is_test(Some("checks/a.ts"), &node(1, "x", 0, Polarity::Production)));
    assert!(!policy(&[], false).node_is_test(Some("spec/a.spec.ts"), &declared));

    assert!(tests.use_is_from_test(Some("src/a.ts"), true));
    assert!(!tests.use_is_from_test(Some("spec/fixtures/a.ts"), true));
    assert!(tests.use_is_from_test(Some("checks/a.ts"), false));
    assert!(!policy(&[], false).use_is_from_test(Some("src/a.ts"), true));
}

#[test]
fn should_skip_a_support_crate_only_with_builtins_and_without_a_negation() {
    let roots = [SmolStr::new("harness")];

    assert!(TestPolicy::defaults().is_support_file("harness/src/lib.rs", &roots));
    assert!(!TestPolicy::defaults().is_support_file("harnessed/src/lib.rs", &roots));
    assert!(!policy(&["!harness/src/**"], true).is_support_file("harness/src/lib.rs", &roots));
    assert!(!policy(&[], false).is_support_file("harness/src/lib.rs", &roots));
}
