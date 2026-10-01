//! Qualification and relocation checks, including mirror-template ambiguity detection.

use crate::error::StrataError;

use super::ceilings::{non_negative_finite, unit_interval};
use crate::config::mirror::{mirror_templates_overlap, validate_mirror_template};
use crate::config::{
    AnalyzeConfig, MirrorTemplate, QualificationConfig, RelocationConfig, builtin_test_mirror_rules,
};

impl AnalyzeConfig {
    pub(super) fn validate_qualification(
        prefix: &str,
        qualification: &QualificationConfig,
    ) -> Result<(), StrataError> {
        let key = |suffix: &str| format!("{prefix}.{suffix}");
        for (suffix, value) in [
            (
                "qualification.minimum-evidence",
                qualification.minimum_evidence,
            ),
            (
                "qualification.minimum-structural",
                qualification.minimum_structural,
            ),
            (
                "qualification.minimum-ambiguity-margin",
                qualification.minimum_ambiguity_margin,
            ),
        ] {
            unit_interval(&key(suffix), value)?;
        }
        let qualification_weights = qualification.weights;
        for (suffix, value) in [
            ("unique-owner", qualification_weights.unique_owner),
            ("role-affinity", qualification_weights.role_affinity),
            ("source-cohesion", qualification_weights.source_cohesion),
            (
                "destination-cohesion",
                qualification_weights.destination_cohesion,
            ),
            ("producer-evidence", qualification_weights.producer_evidence),
            (
                "architectural-reach",
                qualification_weights.architectural_reach,
            ),
        ] {
            non_negative_finite(&key(&format!("qualification.weights.{suffix}")), value)?;
        }
        let qualification_total = qualification_weights.unique_owner
            + qualification_weights.role_affinity
            + qualification_weights.source_cohesion
            + qualification_weights.destination_cohesion
            + qualification_weights.producer_evidence
            + qualification_weights.architectural_reach;
        non_negative_finite(&key("qualification.weights"), qualification_total)?;
        if qualification_total <= 0.0 {
            return Err(StrataError::ConfigInvalid {
                key: Some(key("qualification.weights")),
                reason: "at least one qualification weight must be positive".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn validate_relocation(
        prefix: &str,
        relocation: &RelocationConfig,
    ) -> Result<(), StrataError> {
        for (field, patterns) in [
            ("forbid-file-moves", &relocation.forbid_file_moves),
            ("forbid-symbol-moves", &relocation.forbid_symbol_moves),
        ] {
            for (index, pattern) in patterns.iter().enumerate() {
                let key = format!("{prefix}.relocation.{field}[{index}]");
                if pattern.is_empty() {
                    return Err(StrataError::ConfigInvalid {
                        key: Some(key),
                        reason: "a pattern must not be empty".to_owned(),
                    });
                }
                glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                    key: Some(key),
                    reason: error.to_string(),
                })?;
            }
        }

        let mirror_prefix = format!("{prefix}.relocation.test-mirroring.rules");
        let builtins = relocation
            .test_mirroring
            .builtins
            .then(builtin_test_mirror_rules)
            .unwrap_or_default();
        let mut parsed_rules: Vec<(MirrorTemplate, Vec<MirrorTemplate>)> = builtins
            .iter()
            .map(|rule| {
                let source = MirrorTemplate::parse(&rule.source).ok_or_else(|| {
                    StrataError::ConfigInvalid {
                        key: Some(format!("{prefix}.relocation.test-mirroring.builtins")),
                        reason: "a built-in source mirror template is invalid".to_owned(),
                    }
                })?;
                let tests = rule
                    .tests
                    .iter()
                    .map(|template| {
                        MirrorTemplate::parse(template).ok_or_else(|| StrataError::ConfigInvalid {
                            key: Some(format!("{prefix}.relocation.test-mirroring.builtins")),
                            reason: "a built-in test mirror template is invalid".to_owned(),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((source, tests))
            })
            .collect::<Result<Vec<_>, StrataError>>()?;
        for (rule_index, rule) in relocation.test_mirroring.rules.iter().enumerate() {
            let source_key = format!("{mirror_prefix}[{rule_index}].source");
            validate_mirror_template(&rule.source, &source_key)?;
            let source =
                MirrorTemplate::parse(&rule.source).ok_or_else(|| StrataError::ConfigInvalid {
                    key: Some(source_key.clone()),
                    reason: "mirror template is invalid".to_owned(),
                })?;
            if parsed_rules
                .iter()
                .any(|(existing, _)| mirror_templates_overlap(existing, &source))
            {
                return Err(StrataError::ConfigInvalid {
                    key: Some(source_key),
                    reason: "overlapping source template is ambiguous".to_owned(),
                });
            }
            let mut parsed_tests = Vec::new();
            for (test_index, test) in rule.tests.iter().enumerate() {
                let test_key = format!("{mirror_prefix}[{rule_index}].tests[{test_index}]");
                validate_mirror_template(test, &test_key)?;
                let template =
                    MirrorTemplate::parse(test).ok_or_else(|| StrataError::ConfigInvalid {
                        key: Some(test_key.clone()),
                        reason: "mirror template is invalid".to_owned(),
                    })?;
                let overlaps_same_rule = parsed_tests
                    .iter()
                    .any(|existing| mirror_templates_overlap(existing, &template));
                let overlaps_source_and_test =
                    parsed_rules.iter().any(|(existing_source, tests)| {
                        mirror_templates_overlap(existing_source, &source)
                            && tests
                                .iter()
                                .any(|existing| mirror_templates_overlap(existing, &template))
                    });
                if overlaps_same_rule || overlaps_source_and_test {
                    return Err(StrataError::ConfigInvalid {
                        key: Some(test_key),
                        reason: "overlapping test template is ambiguous".to_owned(),
                    });
                }
                parsed_tests.push(template);
            }
            parsed_rules.push((source, parsed_tests));
        }
        Ok(())
    }
}
