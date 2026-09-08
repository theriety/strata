#![allow(clippy::assertions_on_constants)]

use strata_ir::{Polarity, ScopeLevel};

use crate::config::AnalyzeConfig;

use super::*;
use crate::analyze::test_support::*;

#[test]
fn should_be_deterministic_in_its_hash() {
    let make = || {
        snapshot(
            vec![node(0, "a", 0, Polarity::Production)],
            vec![],
            vec![container(0, "file", ScopeLevel::File, None)],
        )
    };

    let first = analyze(&make(), &AnalyzeConfig::default())
        .map(|result| result.snapshot_hash)
        .unwrap_or_default();
    let second = analyze(&make(), &AnalyzeConfig::default())
        .map(|result| result.snapshot_hash)
        .unwrap_or_default();

    assert_eq!(first, second);
    assert!(!first.is_empty());
}

#[test]
fn should_serialize_profile_results_as_schema_version_eight_without_modes() {
    let snapshot = snapshot(
        vec![node(0, "item", 0, Polarity::Production)],
        vec![],
        vec![container(0, "src/item.rs", ScopeLevel::File, None)],
    );

    let serialized = analyze(&snapshot, &AnalyzeConfig::default())
        .ok()
        .and_then(|result| serde_json::to_value(result).ok())
        .unwrap_or(serde_json::Value::Null);

    assert_eq!(
        serialized
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64),
        Some(8)
    );
    assert!(serialized.pointer("/current/tree").is_some());
    assert!(serialized.pointer("/current/sharedFindings").is_some());
    assert!(serialized.get("profiles").is_some());
    assert!(serialized.pointer("/advice/recommended").is_some());
    assert!(serialized.pointer("/advice/reviewCandidates").is_some());
    assert!(serialized.pointer("/advice/rejected").is_none());
    assert!(serialized.get("modes").is_none());
}
