#![allow(clippy::assertions_on_constants)]

use strata_core::score::KindWeights;

#[cfg(test)]
use crate::narrate::narrate;

use super::*;
use crate::analyze::test_support::*;

#[test]
fn should_preserve_nested_package_dependency_facts_for_pull_narration() {
    let (snapshot, candidate) = nested_package_fact_snapshot(false);
    let facts = file_facts(&snapshot, &KindWeights::default(), 15, &[], &[]);

    let moves = narrate(&snapshot.ir().containers, &candidate, &facts);

    assert_eq!(
        moves.first().map(|entry| entry.reason.to_string()),
        Some("pulled by anchor.ts (w 1.0)".to_owned())
    );
}

#[test]
fn should_preserve_nested_package_test_facts_for_follow_narration() {
    let (snapshot, candidate) = nested_package_fact_snapshot(true);
    let facts = file_facts(&snapshot, &KindWeights::default(), 15, &[], &[]);

    let moves = narrate(&snapshot.ir().containers, &candidate, &facts);

    assert_eq!(
        moves.first().map(|entry| entry.reason.to_string()),
        Some("follows unit.ts".to_owned())
    );
}
