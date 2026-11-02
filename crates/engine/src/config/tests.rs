//! Unit tests for the `strata.toml` schema, defaults, loading, and validation.

#![allow(clippy::assertions_on_constants)]

use std::time::Duration;

use super::*;
use crate::error::StrataError;

#[test]
fn should_default_to_the_config_less_baseline() {
    let config = AnalyzeConfig::default();

    assert_eq!(
        config.analysis.profiles,
        vec![ProfileName::Anchored, ProfileName::Greenfield]
    );
    assert_eq!(config.profiles.anchored.candidates, 3);
    assert_eq!(config.profiles.anchored.seed, 42);
    assert_eq!(config.profiles.anchored.capacity.file, 250);
    assert_eq!(config.profiles.anchored.capacity.folder, 20);
    assert_eq!(config.profiles.anchored.capacity.domain, 16);
    assert_eq!(config.profiles.anchored.capacity.package, 15);
    assert_eq!(config.profiles.anchored.solver.ilp_threshold, 300);
    assert_eq!(config.profiles.anchored.diversity.seeds_per_candidate, 10);
    assert!(config.profiles.greenfield.objective.path.abs() < f64::EPSILON);
    assert!(config.profiles.greenfield.objective.anchor.abs() < f64::EPSILON);
    assert!((config.profiles.anchored.objective.dependency_only - 0.05).abs() < f64::EPSILON);
    assert!((config.profiles.greenfield.objective.dependency_only - 0.05).abs() < f64::EPSILON);
    assert!((config.profiles.anchored.objective.companion_separation - 0.05).abs() < f64::EPSILON);
    assert!(
        (config.profiles.greenfield.objective.companion_separation - 0.05).abs() < f64::EPSILON
    );
}

#[test]
fn should_parse_an_explicit_zero_companion_separation_objective() {
    let parsed = toml::from_str::<AnalyzeConfig>(
        "[profiles.greenfield.objective]\ncompanion-separation = 0.0\n",
    );
    assert!(
        parsed.is_ok(),
        "zero disables companion separation for one profile"
    );
    let config = parsed.unwrap_or_default();

    assert!(
        config
            .profiles
            .greenfield
            .objective
            .companion_separation
            .abs()
            < f64::EPSILON
    );
    assert!((config.profiles.anchored.objective.companion_separation - 0.05).abs() < f64::EPSILON);
}

#[test]
fn should_reject_invalid_companion_separation_objectives_with_their_key_path() {
    for companion_separation in [-0.01, f64::INFINITY, f64::NAN] {
        let result = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    objective: ObjectiveConfig {
                        companion_separation,
                        ..ObjectiveConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        }
        .validate();

        assert!(
            matches!(
                result,
                Err(StrataError::ConfigInvalid { key: Some(key), .. })
                    if key == "profiles.anchored.objective.companion-separation"
            ),
            "negative and non-finite companion pricing is invalid"
        );
    }
}

#[test]
fn should_parse_an_explicit_zero_dependency_only_objective() {
    let config =
        toml::from_str::<AnalyzeConfig>("[profiles.anchored.objective]\ndependency-only = 0.0\n")
            .unwrap_or_default();

    assert!(config.profiles.anchored.objective.dependency_only.abs() < f64::EPSILON);
}

#[test]
fn should_keep_the_dependency_only_default_when_the_objective_is_partial() {
    let parsed = toml::from_str::<AnalyzeConfig>("[profiles.greenfield.objective]\nnaming = 0.8\n");
    assert!(
        parsed.is_ok(),
        "partial profile objective should parse: {:?}",
        parsed.as_ref().err()
    );
    let config = parsed.unwrap_or_default();

    assert!((config.profiles.greenfield.objective.naming - 0.8).abs() < f64::EPSILON);
    assert!((config.profiles.greenfield.objective.dependency_only - 0.05).abs() < f64::EPSILON);
}

#[test]
fn should_reject_invalid_dependency_only_objectives_with_their_key_path() {
    for dependency_only in [-0.01, f64::INFINITY, f64::NAN] {
        let config = AnalyzeConfig {
            profiles: ProfilesConfig {
                anchored: ProfileConfig {
                    objective: ObjectiveConfig {
                        dependency_only,
                        ..ObjectiveConfig::default()
                    },
                    ..ProfileConfig::default()
                },
                ..ProfilesConfig::default()
            },
            ..AnalyzeConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(StrataError::ConfigInvalid { key: Some(key), .. })
                if key == "profiles.anchored.objective.dependency-only"
        ));
    }
}

#[test]
fn should_parse_a_partial_toml_filling_omitted_keys_with_defaults() {
    let toml = "[analysis]\nprofiles = [\"anchored\"]\n[profiles.anchored]\ncandidates = 5\n";

    let config: AnalyzeConfig = toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

    assert_eq!(config.analysis.profiles, vec![ProfileName::Anchored]);
    assert_eq!(config.profiles.anchored.candidates, 5);
    // an omitted key keeps its default.
    assert_eq!(config.profiles.anchored.capacity.file, 250);
}

#[test]
fn should_round_trip_renamed_kebab_keys() {
    let toml = "[profiles.anchored.capacity]\npackage-group = 7\n[profiles.anchored.weights]\nvalue-import = 2.0\n";

    let config: AnalyzeConfig = toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

    assert_eq!(config.profiles.anchored.capacity.package_group, 7);
    assert!((config.profiles.anchored.weights.value_import - 2.0).abs() < f64::EPSILON);
}

#[test]
fn should_accept_source_roots_in_kebab_and_legacy_spelling() {
    let kebab = "[adapters]\nsource-roots = [\"app\"]\n";
    let legacy = "[adapters]\nsource_roots = [\"app\"]\n";

    let kebab: Result<AnalyzeConfig, _> = toml::from_str(kebab);
    let legacy: Result<AnalyzeConfig, _> = toml::from_str(legacy);

    assert_eq!(
        kebab.map(|config| config.adapters.source_roots).ok(),
        Some(vec!["app".to_owned()]),
        "the kebab key matches every other config key"
    );
    assert_eq!(
        legacy.map(|config| config.adapters.source_roots).ok(),
        Some(vec!["app".to_owned()]),
        "the snake_case spelling remains accepted"
    );
}

#[test]
fn should_keep_the_package_wall_up_unless_a_profile_lifts_it() {
    let toml = "[profiles.greenfield.relocation]\nallow-cross-package-moves = true\n";

    let config: Result<AnalyzeConfig, _> = toml::from_str(toml);

    assert_eq!(
        config
            .map(|config| (
                config
                    .profiles
                    .anchored
                    .relocation
                    .allow_cross_package_moves,
                config
                    .profiles
                    .greenfield
                    .relocation
                    .allow_cross_package_moves,
            ))
            .ok(),
        Some((false, true)),
        "the key defaults off and is owned per profile"
    );
}

#[test]
fn should_lift_the_package_wall_for_every_profile_on_override() {
    let mut config = AnalyzeConfig::default();
    config.select_mode(Mode::Anchored);

    config.lift_package_wall();

    assert!(
        config
            .profiles
            .anchored
            .relocation
            .allow_cross_package_moves
    );
    assert!(
        config
            .profiles
            .greenfield
            .relocation
            .allow_cross_package_moves
    );
}

#[test]
fn should_reject_an_unknown_key() {
    let toml = "[analysis]\nnonsense = true\n";

    let result: Result<AnalyzeConfig, _> = toml::from_str(toml);

    assert!(result.is_err());
}

/// A complete neutral profile document used by migration contract tests.
fn complete_profiles_toml() -> &'static str {
    r#"
[analysis]
profiles = ["anchored", "greenfield"]
jobs = 0

[profiles.anchored]
candidates = 3
seed = 42
[profiles.anchored.capacity]
file = 250
folder = 20
domain = 16
package = 15
package-group = 12
[profiles.anchored.objective]
imbalance = 0.1
naming = 0.3
path = 0.2
anchor = 1.0
capacity = 4.0
[profiles.anchored.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15
[profiles.anchored.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20
[profiles.anchored.weights]
value-import = 1.0
inheritance = 1.5
call = 1.0
type-reference = 0.3
re-export = 0.0
same-file-symbol = 1.0
same-file-type = 3.0
[profiles.anchored.solver]
ilp-threshold = 300
timeout-seconds = 60
[profiles.anchored.diversity]
seeds-per-candidate = 10
score-tolerance = 0.05
min-distance = 0.05
[profiles.anchored.tests]
helper-cap = 250
patterns = []
builtins = true

[profiles.greenfield]
candidates = 5
seed = 84
[profiles.greenfield.capacity]
file = 240
folder = 18
domain = 14
package = 13
package-group = 11
[profiles.greenfield.objective]
imbalance = 0.2
naming = 0.4
path = 0.7
anchor = 0.8
capacity = 5.0
[profiles.greenfield.qualification]
minimum-evidence = 0.70
minimum-structural = 0.55
minimum-ambiguity-margin = 0.20
[profiles.greenfield.qualification.weights]
unique-owner = 0.05
role-affinity = 0.15
source-cohesion = 0.20
destination-cohesion = 0.30
producer-evidence = 0.10
architectural-reach = 0.20
[profiles.greenfield.weights]
value-import = 1.1
inheritance = 1.6
call = 1.2
type-reference = 0.4
re-export = 0.1
same-file-symbol = 1.0
same-file-type = 1.0
[profiles.greenfield.solver]
ilp-threshold = 301
timeout-seconds = 61
[profiles.greenfield.diversity]
seeds-per-candidate = 11
score-tolerance = 0.06
min-distance = 0.06
[profiles.greenfield.tests]
helper-cap = 240
patterns = ["checks/**"]
builtins = false
"#
}

#[test]
fn should_parse_complete_independent_parameter_profiles() {
    let parsed: Result<AnalyzeConfig, _> = toml::from_str(complete_profiles_toml());

    assert!(
        parsed.is_ok(),
        "complete profile documents must parse: {parsed:?}"
    );
}

#[test]
fn should_honor_explicit_nonzero_greenfield_path_and_anchor_values() {
    let serialized = toml::from_str::<AnalyzeConfig>(complete_profiles_toml())
        .ok()
        .and_then(|config| serde_json::to_value(config).ok());

    assert_eq!(
        serialized
            .as_ref()
            .and_then(|value| value.pointer("/profiles/greenfield/objective/path"))
            .and_then(serde_json::Value::as_f64),
        Some(0.7)
    );
    assert_eq!(
        serialized
            .as_ref()
            .and_then(|value| value.pointer("/profiles/greenfield/objective/anchor"))
            .and_then(serde_json::Value::as_f64),
        Some(0.8)
    );
}

#[test]
fn should_keep_greenfield_objective_defaults_when_its_profile_is_partial() {
    let config = toml::from_str::<AnalyzeConfig>(
        "[profiles.greenfield]\ncandidates = 5\n[profiles.greenfield.objective]\nnaming = 0.8\n",
    )
    .unwrap_or_default();

    assert_eq!(config.profiles.greenfield.candidates, 5);
    assert!((config.profiles.greenfield.objective.naming - 0.8).abs() < f64::EPSILON);
    assert!(config.profiles.greenfield.objective.path.abs() < f64::EPSILON);
    assert!(config.profiles.greenfield.objective.anchor.abs() < f64::EPSILON);
}

#[test]
fn should_parse_complete_independent_qualification_policies() {
    let document = r"
[profiles.anchored.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15
[profiles.anchored.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20

[profiles.greenfield.qualification]
minimum-evidence = 0.70
minimum-structural = 0.55
minimum-ambiguity-margin = 0.20
[profiles.greenfield.qualification.weights]
unique-owner = 0.05
role-affinity = 0.15
source-cohesion = 0.20
destination-cohesion = 0.30
producer-evidence = 0.10
architectural-reach = 0.20
";

    let parsed = toml::from_str::<AnalyzeConfig>(document);
    assert!(
        parsed.is_ok(),
        "qualification is independently configurable per profile: {parsed:?}"
    );
    let config = parsed.unwrap_or_default();

    assert!((config.profiles.anchored.qualification.minimum_evidence - 0.60).abs() < f64::EPSILON);
    assert!(
        (config.profiles.greenfield.qualification.minimum_evidence - 0.70).abs() < f64::EPSILON
    );
    assert!(
        (config
            .profiles
            .greenfield
            .qualification
            .weights
            .destination_cohesion
            - 0.30)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn should_default_every_profile_to_the_conservative_qualification_policy() {
    let config = AnalyzeConfig::default();

    for (profile_name, profile) in [
        ("anchored", config.profiles.anchored),
        ("greenfield", config.profiles.greenfield),
    ] {
        let qualification = profile.qualification;
        let expected_values = [
            ("minimum-evidence", qualification.minimum_evidence, 0.60),
            ("minimum-structural", qualification.minimum_structural, 0.50),
            (
                "minimum-ambiguity-margin",
                qualification.minimum_ambiguity_margin,
                0.15,
            ),
            (
                "weights.unique-owner",
                qualification.weights.unique_owner,
                0.10,
            ),
            (
                "weights.role-affinity",
                qualification.weights.role_affinity,
                0.10,
            ),
            (
                "weights.source-cohesion",
                qualification.weights.source_cohesion,
                0.25,
            ),
            (
                "weights.destination-cohesion",
                qualification.weights.destination_cohesion,
                0.25,
            ),
            (
                "weights.producer-evidence",
                qualification.weights.producer_evidence,
                0.10,
            ),
            (
                "weights.architectural-reach",
                qualification.weights.architectural_reach,
                0.20,
            ),
        ];
        for (key, actual, expected) in expected_values {
            assert!(
                (actual - expected).abs() < f64::EPSILON,
                "{profile_name} default at {key}"
            );
        }
    }
}

#[test]
fn should_reject_out_of_range_qualification_values_with_precise_paths() {
    let cases = [("minimum-evidence", -0.01), ("minimum-structural", 1.01)];

    for (key, value) in cases {
        let document = if let Some(weight) = key.strip_prefix("weights.") {
            format!("[profiles.anchored.qualification.weights]\n{weight} = {value}\n")
        } else {
            format!("[profiles.anchored.qualification]\n{key} = {value}\n")
        };
        let result = toml::from_str::<AnalyzeConfig>(&document)
            .map_err(|parse| parse.to_string())
            .and_then(|config| config.validate().map_err(|error| error.to_string()));
        let Err(error) = result else {
            assert!(false, "invalid qualification values must be rejected");
            continue;
        };
        assert!(
            error.contains(&format!("profiles.anchored.qualification.{key}")),
            "diagnostic should name the invalid key: {error}"
        );
    }

    for (key, value) in [
        ("minimum-ambiguity-margin", f64::INFINITY),
        ("weights.unique-owner", f64::NAN),
    ] {
        let mut config = AnalyzeConfig::default();
        if key == "minimum-ambiguity-margin" {
            config
                .profiles
                .anchored
                .qualification
                .minimum_ambiguity_margin = value;
        } else {
            config.profiles.anchored.qualification.weights.unique_owner = value;
        }
        let Err(error) = config.validate() else {
            assert!(false, "non-finite qualification values must be rejected");
            continue;
        };
        let error = error.to_string();
        assert!(
            error.contains(&format!("profiles.anchored.qualification.{key}")),
            "diagnostic should name the invalid key: {error}"
        );
    }
}

fn overflowing_qualification_weights(profile: &str) -> Result<(), String> {
    let document = format!(
        "[profiles.{profile}.qualification.weights]\nunique-owner = 1e308\nrole-affinity = 1e308\n"
    );
    let config: AnalyzeConfig = toml::from_str(&document).map_err(|error| error.to_string())?;
    let error = config
        .validate()
        .err()
        .ok_or("finite individual weights with an overflowing total must fail")?
        .to_string();
    assert!(
        error.contains(&format!("profiles.{profile}.qualification.weights")),
        "{error}"
    );
    assert!(error.contains("finite"), "{error}");
    Ok(())
}

#[test]
fn should_reject_anchored_qualification_weight_overflow() -> Result<(), String> {
    overflowing_qualification_weights("anchored")
}

#[test]
fn should_reject_greenfield_qualification_weight_overflow() -> Result<(), String> {
    overflowing_qualification_weights("greenfield")
}

#[test]
fn should_accept_large_finite_qualification_weight_totals() -> Result<(), String> {
    for profile in ["anchored", "greenfield"] {
        let document = format!(
            "[profiles.{profile}.qualification.weights]\nunique-owner = 5e307\nrole-affinity = 5e307\n"
        );
        let config: AnalyzeConfig = toml::from_str(&document).map_err(|error| error.to_string())?;
        config.validate().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[test]
fn should_name_every_removed_legacy_key_in_its_diagnostic() {
    let legacy_documents = [
        ("[analysis]\nmode = \"both\"\n", "analysis.mode", "mode"),
        (
            "[analysis]\ncandidates = 3\n",
            "analysis.candidates",
            "candidates",
        ),
        ("[analysis]\nseed = 42\n", "analysis.seed", "seed"),
        ("[capacity]\nfile = 250\n", "capacity", "capacity"),
        ("[objective]\npath = 0.2\n", "objective", "objective"),
        ("[weights]\ncall = 1.0\n", "weights", "weights"),
        ("[solver]\nilp-threshold = 300\n", "solver", "solver"),
        (
            "[diversity]\nseeds-per-candidate = 10\n",
            "diversity",
            "diversity",
        ),
        ("[tests]\nhelper-cap = 250\n", "tests", "tests"),
    ];
    let path = std::env::temp_dir().join(format!(
        "strata-legacy-profile-config-{}.toml",
        std::process::id()
    ));

    for (document, expected_key, legacy_field) in legacy_documents {
        assert!(
            std::fs::write(&path, document).is_ok(),
            "write isolated legacy config fixture"
        );
        let diagnostic = load_config(&path);
        assert!(
            matches!(
                diagnostic,
                Err(StrataError::ConfigInvalid { key: Some(ref key), ref reason })
                    if key == expected_key
                        && reason == &format!("unknown field `{legacy_field}`")
            ),
            "expected exact diagnostic key={expected_key:?}, reason={:?}; got {diagnostic:?}",
            format!("unknown field `{legacy_field}`")
        );
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn should_reject_a_zero_cap_with_its_key_path() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                capacity: CapacityConfig {
                    file: 0,
                    ..CapacityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.capacity.file"
    ));
}

#[test]
fn should_parse_tests_patterns_and_builtins_from_toml() {
    let toml = "[profiles.anchored.tests]\npatterns = [\"*.spec.*\", \"apps/web/__tests__/**\"]\nbuiltins = false\n";

    let config: AnalyzeConfig = toml::from_str(toml).unwrap_or_else(|_| AnalyzeConfig::default());

    assert_eq!(
        config.profiles.anchored.tests.patterns,
        vec!["*.spec.*", "apps/web/__tests__/**"]
    );
    assert!(!config.profiles.anchored.tests.builtins);
    // an omitted key keeps its default.
    assert_eq!(config.profiles.anchored.tests.helper_cap, 250);
}

#[test]
fn should_reject_an_empty_tests_pattern_with_its_index() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                tests: TestsConfig {
                    patterns: vec!["*.spec.*".to_owned(), String::new()],
                    ..TestsConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.tests.patterns[1]"
    ));
}

#[test]
fn should_reject_an_invalid_tests_glob_with_its_index() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                tests: TestsConfig {
                    patterns: vec!["[".to_owned()],
                    ..TestsConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.tests.patterns[0]"
    ));
}

#[test]
fn should_reject_a_folder_cap_above_the_ceiling() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                capacity: CapacityConfig {
                    folder: 257,
                    ..CapacityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), reason })
            if key == "profiles.anchored.capacity.folder" && reason.contains("256") && reason.contains("257")
    ));
}

#[test]
fn should_reject_a_domain_cap_above_the_ceiling() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                capacity: CapacityConfig {
                    domain: 300,
                    ..CapacityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), reason })
            if key == "profiles.anchored.capacity.domain" && reason.contains("256") && reason.contains("300")
    ));
}

#[test]
fn should_reject_a_package_cap_above_the_ceiling() {
    // u32::MAX is the classic hostile value: the error must format it, not wrap.
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                capacity: CapacityConfig {
                    package: u32::MAX,
                    ..CapacityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), reason })
            if key == "profiles.anchored.capacity.package" && reason.contains("256") && reason.contains("4294967295")
    ));
}

#[test]
fn should_accept_caps_at_the_ceiling() {
    // 256 is inclusive: the bound rejects only what lies beyond it.
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                capacity: CapacityConfig {
                    folder: 256,
                    domain: 256,
                    package: 256,
                    ..CapacityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(config.validate().is_ok());
}

#[test]
fn should_reject_a_negative_coefficient() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                objective: ObjectiveConfig {
                    naming: -1.0,
                    ..ObjectiveConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.objective.naming"
    ));
}

#[test]
fn should_reject_an_unknown_language() {
    let config = AnalyzeConfig {
        adapters: AdaptersConfig {
            languages: vec!["cobol".to_owned()],
            ..AdaptersConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "adapters.languages"
    ));
}

#[test]
fn should_validate_the_default_config() {
    assert!(AnalyzeConfig::default().validate().is_ok());
}

#[test]
fn should_report_modes_that_a_mode_includes() {
    assert!(Mode::Anchored.includes_anchored());
    assert!(!Mode::Anchored.includes_greenfield());
    assert!(Mode::Both.includes_anchored());
    assert!(Mode::Both.includes_greenfield());
    assert!(Mode::Greenfield.includes_greenfield());
}

#[test]
fn should_convert_solver_config_into_limits() {
    let limits = SolverConfig::default().limits();

    assert_eq!(limits.ilp_threshold, 300);
    assert_eq!(limits.timeout, Duration::from_mins(1));
}

#[test]
fn should_map_objective_config_onto_anchored_coefficients() {
    let objective = ObjectiveConfig {
        imbalance: 0.4,
        naming: 0.5,
        path: 0.6,
        anchor: 0.7,
        capacity: 4.0,
        dependency_only: 0.8,
        companion_separation: 0.9,
    };

    let coefficients = objective.anchored();

    assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
    assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
    assert!((coefficients.beta - 0.6).abs() < f64::EPSILON);
    assert!((coefficients.mu - 0.7).abs() < f64::EPSILON);
    assert!((coefficients.dependency_only - 0.8).abs() < f64::EPSILON);
    assert!((coefficients.companion_separation - 0.9).abs() < f64::EPSILON);
}

#[test]
fn should_honor_explicit_path_and_anchor_in_greenfield_coefficients() {
    let objective = ObjectiveConfig {
        imbalance: 0.4,
        naming: 0.5,
        path: 0.6,
        anchor: 0.7,
        capacity: 4.0,
        dependency_only: 0.8,
        companion_separation: 0.9,
    };

    let coefficients = objective.greenfield();

    assert!((coefficients.lambda - 0.4).abs() < f64::EPSILON);
    assert!((coefficients.alpha - 0.5).abs() < f64::EPSILON);
    assert!((coefficients.beta - 0.6).abs() < f64::EPSILON);
    assert!((coefficients.mu - 0.7).abs() < f64::EPSILON);
    assert!((coefficients.dependency_only - 0.8).abs() < f64::EPSILON);
}

#[test]
fn should_map_weights_config_onto_kind_weights() {
    let weights = WeightsConfig {
        value_import: 2.0,
        inheritance: 3.0,
        call: 4.0,
        type_reference: 5.0,
        re_export: 6.0,
        same_file_symbol: 1.0,
        same_file_type: 3.0,
    };

    let table = weights.kind_weights();

    assert!((table.value_import - 2.0).abs() < f64::EPSILON);
    assert!((table.inheritance - 3.0).abs() < f64::EPSILON);
    assert!((table.call - 4.0).abs() < f64::EPSILON);
    assert!((table.type_reference - 5.0).abs() < f64::EPSILON);
    assert!((table.re_export - 6.0).abs() < f64::EPSILON);
}

#[test]
fn should_accept_zero_jobs_as_the_use_every_core_sentinel() {
    let config = AnalyzeConfig {
        analysis: AnalysisConfig {
            jobs: 0,
            ..AnalysisConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(config.validate().is_ok());
}

#[test]
fn should_reject_a_zero_candidate_count() {
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                candidates: 0,
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. }) if key == "profiles.anchored.candidates"
    ));
}

#[test]
fn should_reject_a_restart_pool_whose_factors_are_each_legal() {
    // 32 and 32 both pass their own ceilings; their product does not.
    let config = AnalyzeConfig {
        profiles: ProfilesConfig {
            anchored: ProfileConfig {
                candidates: 32,
                diversity: DiversityConfig {
                    seeds_per_candidate: 32,
                    ..DiversityConfig::default()
                },
                ..ProfileConfig::default()
            },
            ..ProfilesConfig::default()
        },
        ..AnalyzeConfig::default()
    };

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. })
            if key == "profiles.anchored.candidates * profiles.anchored.diversity.seeds-per-candidate"
    ));
}

#[test]
fn should_apply_generic_overrides_to_every_selected_profile() {
    let mut config = AnalyzeConfig::default();

    config.override_candidates(7);
    config.override_seed(99);

    assert_eq!(config.profiles.anchored.candidates, 7);
    assert_eq!(config.profiles.greenfield.candidates, 7);
    assert_eq!(config.profiles.anchored.seed, 99);
    assert_eq!(config.profiles.greenfield.seed, 99);
}

#[test]
fn should_leave_unselected_profiles_unchanged_during_generic_overrides() {
    let mut config = AnalyzeConfig::default();
    config.select_mode(Mode::Greenfield);

    config.override_candidates(7);
    config.override_seed(99);

    assert_eq!(config.profiles.anchored.candidates, 3);
    assert_eq!(config.profiles.anchored.seed, 42);
    assert_eq!(config.profiles.greenfield.candidates, 7);
    assert_eq!(config.profiles.greenfield.seed, 99);
}

#[test]
fn should_reject_same_file_weights_below_one() {
    let mut config = AnalyzeConfig::default();
    config.profiles.greenfield.weights.same_file_type = 0.9;

    assert!(matches!(
        config.validate(),
        Err(StrataError::ConfigInvalid { key: Some(key), .. })
            if key == "profiles.greenfield.weights.same-file-type"
    ));
}

#[test]
fn should_accept_the_shipped_defaults_within_every_search_ceiling() {
    assert!(AnalyzeConfig::default().validate().is_ok());
}
