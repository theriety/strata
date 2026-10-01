//! The five `strata` subcommands, each a thin shell over the engine.
//!
//! Every command parses its flags, calls the same public `strata-engine`
//! functions any embedder would, and renders the typed result — there is no
//! analysis logic here that the library lacks. The commands share a small set of
//! helpers for loading a config (CLI flags over `strata.toml` over defaults),
//! reading a saved `AnalyzeResult`, and resolving a `mode/index` reference.

pub mod analyze;
pub mod diff;
pub mod report;
pub mod tree;
pub mod violations;

use std::path::Path;

use strata_engine::{
    AnalyzeConfig, AnalyzeResult, Candidate, ModeResult, RESULT_SCHEMA_VERSION, StrataError,
    Violation, load_config,
};

/// Loads the effective config: the file at `config_path` if it exists, else the
/// built-in defaults, with the CLI overrides applied last.
///
/// Configuration precedence is fixed — CLI flags override `strata.toml`, which
/// overrides defaults. A missing config file is not an error: defaults apply.
///
/// # Errors
///
/// Returns [`StrataError::ConfigInvalid`] if the file is present but invalid.
fn resolve_config(
    config_path: &Path,
    overrides: ConfigOverrides,
) -> Result<AnalyzeConfig, StrataError> {
    let mut config = if config_path.exists() {
        load_config(config_path)?
    } else {
        AnalyzeConfig::default()
    };
    overrides.apply(&mut config);
    config.validate()?;
    Ok(config)
}

/// Returns shared findings first, followed by profile-specific findings.
fn findings(result: &AnalyzeResult) -> Vec<Violation> {
    let mut findings = result.current.shared_findings.clone();
    if let Some(profile) = &result.profiles.anchored {
        findings.extend(profile.current.unique_findings.iter().cloned());
    }
    if let Some(profile) = &result.profiles.greenfield {
        findings.extend(profile.current.unique_findings.iter().cloned());
    }
    findings
}

/// The CLI-flag overrides layered over a loaded config.
///
/// Each `Some` value overrides the file (or default) value for that key; `None`
/// leaves the loaded value untouched, so a flag that was not passed never
/// disturbs a configured value.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConfigOverrides {
    /// The `--seed` override.
    pub seed: Option<u64>,
    /// The `-k, --candidates` override.
    pub candidates: Option<u32>,
    /// The `--jobs` override.
    pub jobs: Option<u32>,
    /// The `--mode` override.
    pub mode: Option<strata_engine::Mode>,
    /// The `--allow-cross-package-moves` switch; `false` leaves each profile's
    /// own key in force.
    pub allow_cross_package_moves: bool,
}

impl ConfigOverrides {
    /// Applies each present override onto `config`.
    fn apply(self, config: &mut AnalyzeConfig) {
        if let Some(mode) = self.mode {
            config.select_mode(mode);
        }
        if let Some(seed) = self.seed {
            config.override_seed(seed);
        }
        if let Some(candidates) = self.candidates {
            config.override_candidates(candidates);
        }
        if let Some(jobs) = self.jobs {
            config.analysis.jobs = jobs;
        }
        if self.allow_cross_package_moves {
            config.lift_package_wall();
        }
    }
}

/// Reads and deserializes a saved [`AnalyzeResult`] JSON from `path`.
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] if the file cannot be read, does not
/// deserialize into an `AnalyzeResult`, or carries an unsupported
/// `schemaVersion`.
fn read_result(path: &Path) -> Result<AnalyzeResult, StrataError> {
    let text = std::fs::read_to_string(path).map_err(|error| StrataError::InputUnreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| StrataError::InputUnreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or_default();
    if schema_version != RESULT_SCHEMA_VERSION {
        return Err(StrataError::InputUnreadable {
            path: path.to_path_buf(),
            reason: format!(
                "unsupported result schemaVersion {schema_version}; expected {RESULT_SCHEMA_VERSION}"
            ),
        });
    }
    let result: AnalyzeResult =
        serde_json::from_value(value).map_err(|error| StrataError::InputUnreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    Ok(result)
}

/// Selects one mode's result from `result` by its name (`anchored`/`greenfield`).
///
/// # Errors
///
/// Returns [`StrataError::CandidateNotFound`] when the named mode is absent.
fn mode_result<'a>(result: &'a AnalyzeResult, mode: &str) -> Result<&'a ModeResult, StrataError> {
    let found = match mode {
        "anchored" => result.profiles.anchored.as_ref(),
        "greenfield" => result.profiles.greenfield.as_ref(),
        _ => None,
    };
    found.ok_or_else(|| StrataError::CandidateNotFound {
        mode: mode.to_owned(),
        index: 0,
    })
}

/// Selects a 1-based candidate from a mode's result.
///
/// # Errors
///
/// Returns [`StrataError::CandidateNotFound`] when the index is out of range.
fn candidate_at<'a>(
    result: &'a AnalyzeResult,
    mode: &str,
    index: usize,
) -> Result<&'a Candidate, StrataError> {
    let mode_result = mode_result(result, mode)?;
    mode_result
        .candidates
        .iter()
        .find(|candidate| candidate.index as usize == index)
        .ok_or_else(|| StrataError::CandidateNotFound {
            mode: mode.to_owned(),
            index,
        })
}

#[cfg(test)]
mod tests {
    use strata_engine::ProfileName;

    use strata_engine::Mode;

    use super::*;

    #[test]
    fn should_keep_loaded_values_when_no_override_is_present() {
        let mut config = AnalyzeConfig::default();
        config.profiles.anchored.seed = 7;
        config.profiles.greenfield.seed = 8;

        ConfigOverrides::default().apply(&mut config);

        assert_eq!(config.profiles.anchored.seed, 7);
        assert_eq!(config.profiles.greenfield.seed, 8);
    }

    #[test]
    fn should_override_only_the_selected_profile() {
        let mut config = AnalyzeConfig::default();
        config.profiles.greenfield.seed = 8;
        config.profiles.greenfield.candidates = 2;

        ConfigOverrides {
            seed: Some(99),
            candidates: Some(5),
            jobs: Some(2),
            mode: Some(Mode::Anchored),
            allow_cross_package_moves: false,
        }
        .apply(&mut config);

        assert_eq!(config.profiles.anchored.seed, 99);
        assert_eq!(config.profiles.greenfield.seed, 8);
        assert_eq!(config.profiles.anchored.candidates, 5);
        assert_eq!(config.profiles.greenfield.candidates, 2);
        assert_eq!(config.analysis.jobs, 2);
        assert_eq!(config.analysis.profiles, vec![ProfileName::Anchored]);
    }

    #[test]
    fn should_override_both_selected_profiles() {
        let mut config = AnalyzeConfig::default();

        ConfigOverrides {
            seed: Some(99),
            candidates: Some(5),
            mode: Some(Mode::Both),
            ..ConfigOverrides::default()
        }
        .apply(&mut config);

        assert_eq!(config.profiles.anchored.seed, 99);
        assert_eq!(config.profiles.greenfield.seed, 99);
        assert_eq!(config.profiles.anchored.candidates, 5);
        assert_eq!(config.profiles.greenfield.candidates, 5);
    }

    #[test]
    fn should_leave_anchored_unchanged_for_a_greenfield_override() {
        let mut config = AnalyzeConfig::default();
        config.profiles.anchored.seed = 7;

        ConfigOverrides {
            seed: Some(99),
            mode: Some(Mode::Greenfield),
            ..ConfigOverrides::default()
        }
        .apply(&mut config);

        assert_eq!(config.profiles.anchored.seed, 7);
        assert_eq!(config.profiles.greenfield.seed, 99);
    }

    #[test]
    fn should_lift_the_package_wall_for_every_profile_when_the_flag_is_passed() {
        let mut config = AnalyzeConfig::default();
        config
            .profiles
            .anchored
            .relocation
            .allow_cross_package_moves = false;
        config
            .profiles
            .greenfield
            .relocation
            .allow_cross_package_moves = false;

        ConfigOverrides {
            mode: Some(Mode::Anchored),
            allow_cross_package_moves: true,
            ..ConfigOverrides::default()
        }
        .apply(&mut config);

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
    fn should_keep_each_profile_package_key_without_the_flag() {
        let mut config = AnalyzeConfig::default();
        config
            .profiles
            .greenfield
            .relocation
            .allow_cross_package_moves = true;

        ConfigOverrides::default().apply(&mut config);

        assert!(
            !config
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
    fn should_default_when_the_config_file_is_absent() {
        let missing = Path::new("/nonexistent/strata.toml");

        let config = resolve_config(missing, ConfigOverrides::default());

        assert!(config.is_ok());
    }

    #[test]
    fn should_report_a_missing_mode_as_candidate_not_found() {
        let result = AnalyzeResult {
            schema_version: RESULT_SCHEMA_VERSION,
            snapshot_hash: "h".to_owned(),
            summary: strata_engine::Summary {
                symbols: 0,
                edges: 0,
                files: 0,
                files_by_language: std::collections::BTreeMap::new(),
            },
            current: strata_engine::CurrentTree {
                tree: strata_engine::ContainerNode {
                    name: "r".to_owned(),
                    level: strata_engine::Level::File,
                    children: None,
                    symbols: Some(Vec::new()),
                    production_sloc: Some(0),
                },
                shared_findings: Vec::new(),
            },
            profiles: strata_engine::Profiles::default(),
            advice: strata_engine::Advice::default(),
        };

        let found = mode_result(&result, "anchored");

        assert!(matches!(found, Err(StrataError::CandidateNotFound { .. })));
    }
}
