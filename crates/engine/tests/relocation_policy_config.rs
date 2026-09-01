//! Public configuration contracts for pinned relocations and exact test mirrors.
//!
//! These tests deliberately exercise TOML rather than internal policy types:
//! configuration is the user-owned boundary, and its key paths and defaults
//! must remain stable even if the engine's compiled representation changes.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use strata_engine::{AnalyzeConfig, StrataError, load_config};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn load_toml(document: &str) -> Result<AnalyzeConfig, StrataError> {
    let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "strata-relocation-policy-{}-{sequence}.toml",
        std::process::id()
    ));
    let written = fs::write(&path, document);
    assert!(
        written.is_ok(),
        "the temporary configuration must be writable"
    );
    let loaded = load_config(&path);
    let _removed = fs::remove_file(path);
    loaded
}

fn invalid_key(document: &str) -> Option<String> {
    match load_toml(document) {
        Err(StrataError::ConfigInvalid { key, .. }) => key,
        _ => None,
    }
}

#[test]
fn should_default_to_pinning_detected_tests_without_custom_exclusions() {
    let config = AnalyzeConfig::default();

    for profile in [&config.profiles.anchored, &config.profiles.greenfield] {
        assert!(profile.relocation.pin_detected_test_files);
        assert!(profile.relocation.pin_detected_test_symbols);
        assert!(profile.relocation.forbid_file_moves.is_empty());
        assert!(profile.relocation.forbid_symbol_moves.is_empty());
        assert!(profile.relocation.test_mirroring.enabled);
        assert!(profile.relocation.test_mirroring.builtins);
        assert!(profile.relocation.test_mirroring.rules.is_empty());
    }
}

#[test]
fn should_keep_relocation_policies_independent_between_profiles() {
    let loaded = load_toml(
        r#"
[profiles.anchored.relocation]
pin-detected-test-files = false
pin-detected-test-symbols = true
forbid-file-moves = ["generated/**"]
forbid-symbol-moves = ["support/**"]

[profiles.anchored.relocation.test-mirroring]
enabled = false
builtins = false

[profiles.greenfield.relocation]
pin-detected-test-files = true
pin-detected-test-symbols = false
forbid-file-moves = ["vendor/**"]
forbid-symbol-moves = ["fixtures/**"]

[profiles.greenfield.relocation.test-mirroring]
enabled = true
builtins = true

[[profiles.greenfield.relocation.test-mirroring.rules]]
source = "custom/{dir}/{stem}.ts"
tests = ["checks/{dir}/{stem}.spec.ts", "custom-tests/{dir}/{stem}.test.ts"]
"#,
    );
    let config = loaded.as_ref().ok();

    assert_eq!(
        config.map(|value| (
            value.profiles.anchored.relocation.pin_detected_test_files,
            value.profiles.anchored.relocation.pin_detected_test_symbols,
            value.profiles.anchored.relocation.test_mirroring.enabled,
            value.profiles.anchored.relocation.test_mirroring.builtins,
        )),
        Some((false, true, false, false))
    );
    assert_eq!(
        config.map(|value| (
            value.profiles.greenfield.relocation.pin_detected_test_files,
            value
                .profiles
                .greenfield
                .relocation
                .pin_detected_test_symbols,
            value.profiles.greenfield.relocation.test_mirroring.enabled,
            value.profiles.greenfield.relocation.test_mirroring.builtins,
        )),
        Some((true, false, true, true))
    );
    assert_eq!(
        config.map(|value| value
            .profiles
            .greenfield
            .relocation
            .test_mirroring
            .rules
            .len()),
        Some(1),
        "one source template may own multiple exact test templates"
    );
}

#[test]
fn should_attribute_an_invalid_file_exclusion_glob_to_its_profile_and_index() {
    let key = invalid_key(
        r#"
[profiles.anchored.relocation]
forbid-file-moves = ["src/[unterminated"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.forbid-file-moves[0]")
    );
}

#[test]
fn should_attribute_an_invalid_symbol_exclusion_glob_to_its_profile_and_index() {
    let key = invalid_key(
        r#"
[profiles.greenfield.relocation]
forbid-symbol-moves = ["spec/[unterminated"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.greenfield.relocation.forbid-symbol-moves[0]")
    );
}

#[test]
fn should_reject_unknown_mirror_placeholders_at_the_exact_rule_key() {
    let key = invalid_key(
        r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{package}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[0].source")
    );
}

#[test]
fn should_reject_an_unknown_test_placeholder_at_the_exact_test_index() {
    let key = invalid_key(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts", "checks/{scope}/{stem}.test.ts"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[0].tests[1]")
    );
}

#[test]
fn should_reject_a_mirror_destination_that_escapes_the_repository() {
    let key = invalid_key(
        r#"
[profiles.greenfield.relocation.test-mirroring]
builtins = false

[[profiles.greenfield.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.py"
tests = ["../tests/{dir}/test_{stem}.py"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.greenfield.relocation.test-mirroring.rules[0].tests[0]")
    );
}

#[test]
fn should_reject_duplicate_source_templates_as_ambiguous() {
    let key = invalid_key(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts"]

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["tests/{dir}/{stem}.test.ts"]
"#,
    );

    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[1].source")
    );
}

#[test]
fn should_normalize_an_empty_captured_directory_without_changing_the_template() {
    let loaded = load_toml(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts"]
"#,
    );
    let rule = loaded.as_ref().ok().and_then(|config| {
        config
            .profiles
            .anchored
            .relocation
            .test_mirroring
            .rules
            .first()
    });

    assert_eq!(
        rule.map(|value| value.source.as_str()),
        Some("src/{dir}/{stem}.ts")
    );
    assert_eq!(
        rule.and_then(|value| value.tests.first())
            .map(String::as_str),
        Some("spec/{dir}/{stem}.spec.ts")
    );
}

#[test]
fn should_reject_missing_repeated_and_reversed_placeholders_at_the_exact_key() {
    for source in [
        "src/{dir}/fixed.ts",
        "src/{dir}/{dir}/{stem}.ts",
        "src/{stem}/{dir}.ts",
    ] {
        let key = invalid_key(&format!(
            r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "{source}"
tests = ["spec/{{dir}}/{{stem}}.spec.ts"]
"#
        ));
        assert_eq!(
            key.as_deref(),
            Some("profiles.anchored.relocation.test-mirroring.rules[0].source"),
            "invalid source template was accepted: {source}"
        );
    }
}

#[test]
fn should_reject_templates_outside_the_decidable_path_segment_grammar() {
    for source in [
        "src/prefix-{dir}/{stem}.ts",
        "src/{dir}/{stem}.ts/generated",
    ] {
        let key = invalid_key(&format!(
            r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "{source}"
tests = ["spec/{{dir}}/{{stem}}.spec.ts"]
"#,
        ));
        assert_eq!(
            key.as_deref(),
            Some("profiles.anchored.relocation.test-mirroring.rules[0].source"),
            "out-of-grammar source template was accepted: {source}"
        );
    }
}

#[test]
fn should_reject_duplicate_test_templates_at_the_second_test_key() {
    let key = invalid_key(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts", "spec/{dir}/{stem}.spec.ts"]
"#,
    );
    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[0].tests[1]")
    );
}

#[test]
fn should_reject_custom_source_that_overlaps_enabled_builtins() {
    let key = invalid_key(
        r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = ["custom/{dir}/{stem}.spec.ts"]
"#,
    );
    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[0].source")
    );
}

#[test]
fn should_reject_every_discovered_builtin_source_extension() {
    for extension in ["ts", "tsx", "mts", "cts", "py"] {
        let key = invalid_key(&format!(
            r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{{dir}}/{{stem}}.{extension}"
tests = ["custom/{{dir}}/{{stem}}.check.{extension}"]
"#
        ));
        assert_eq!(
            key.as_deref(),
            Some("profiles.anchored.relocation.test-mirroring.rules[0].source"),
            "built-in source extension was absent from validation: {extension}"
        );
    }
}

#[test]
fn should_allow_a_custom_source_absent_from_the_builtin_catalog() {
    let loaded = load_toml(
        r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.vue"
tests = ["spec/{dir}/{stem}.spec.vue"]
"#,
    );
    assert!(
        loaded.is_ok(),
        "an absent built-in must remain configurable: {loaded:?}"
    );
}

#[test]
fn should_reject_semantically_overlapping_nested_source_templates() {
    let key = invalid_key(
        r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/lib/{dir}/{stem}.ts"
tests = ["checks/{dir}/{stem}.spec.ts"]
"#,
    );
    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[0].source")
    );
}

#[test]
fn should_allow_an_identical_test_shape_when_source_languages_are_disjoint() {
    let loaded = load_toml(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "source-a/{dir}/{stem}.ts"
tests = ["checks/{dir}/{stem}.spec.ts"]

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "source-b/{dir}/{stem}.ts"
tests = ["checks/{dir}/{stem}.spec.ts"]
"#,
    );
    assert!(
        loaded.is_ok(),
        "disjoint source languages may intentionally share one test shape: {loaded:?}"
    );
}

#[test]
fn should_reject_overlapping_templates_that_require_unequal_captures() {
    let key = invalid_key(
        r#"
[profiles.anchored.relocation.test-mirroring]
builtins = false

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/pre-{stem}.ts"
tests = ["checks-a/{dir}/{stem}.spec.ts"]

[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}-post.ts"
tests = ["checks-b/{dir}/{stem}.spec.ts"]
"#,
    );
    assert_eq!(
        key.as_deref(),
        Some("profiles.anchored.relocation.test-mirroring.rules[1].source"),
        "both source templates accept src/x/pre-y-post.ts"
    );
}

#[test]
fn should_allow_a_builtin_test_shape_for_a_disjoint_custom_source_language() {
    let loaded = load_toml(
        r#"
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "lib/{dir}/{stem}.ts"
tests = ["spec/{dir}/{stem}.spec.ts"]
"#,
    );
    assert!(
        loaded.is_ok(),
        "a lib source cannot conflict with the built-in src/source languages: {loaded:?}"
    );
}
