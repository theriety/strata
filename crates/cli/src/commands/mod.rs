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
    load_config,
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
pub fn resolve_config(
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
}

impl ConfigOverrides {
    /// Applies each present override onto `config`.
    fn apply(self, config: &mut AnalyzeConfig) {
        if let Some(seed) = self.seed {
            config.analysis.seed = seed;
        }
        if let Some(candidates) = self.candidates {
            config.analysis.candidates = candidates;
        }
        if let Some(jobs) = self.jobs {
            config.analysis.jobs = jobs;
        }
        if let Some(mode) = self.mode {
            config.analysis.mode = mode;
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
pub fn read_result(path: &Path) -> Result<AnalyzeResult, StrataError> {
    let text = std::fs::read_to_string(path).map_err(|error| StrataError::InputUnreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let result: AnalyzeResult =
        serde_json::from_str(&text).map_err(|error| StrataError::InputUnreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    if result.schema_version != RESULT_SCHEMA_VERSION {
        return Err(StrataError::InputUnreadable {
            path: path.to_path_buf(),
            reason: format!(
                "unsupported result schemaVersion {}; expected {RESULT_SCHEMA_VERSION}",
                result.schema_version
            ),
        });
    }
    Ok(result)
}

/// Selects one mode's result from `result` by its name (`anchored`/`greenfield`).
///
/// # Errors
///
/// Returns [`StrataError::CandidateNotFound`] when the named mode is absent.
pub fn mode_result<'a>(
    result: &'a AnalyzeResult,
    mode: &str,
) -> Result<&'a ModeResult, StrataError> {
    let found = match mode {
        "anchored" => result.modes.anchored.as_ref(),
        "greenfield" => result.modes.greenfield.as_ref(),
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
pub fn candidate_at<'a>(
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
    use strata_engine::Mode;

    use super::*;

    #[test]
    fn should_keep_loaded_values_when_no_override_is_present() {
        let mut config = AnalyzeConfig::default();
        config.analysis.seed = 7;

        ConfigOverrides::default().apply(&mut config);

        assert_eq!(config.analysis.seed, 7);
    }

    #[test]
    fn should_override_each_present_flag() {
        let mut config = AnalyzeConfig::default();

        ConfigOverrides {
            seed: Some(99),
            candidates: Some(5),
            jobs: Some(2),
            mode: Some(Mode::Anchored),
        }
        .apply(&mut config);

        assert_eq!(config.analysis.seed, 99);
        assert_eq!(config.analysis.candidates, 5);
        assert_eq!(config.analysis.jobs, 2);
        assert_eq!(config.analysis.mode, Mode::Anchored);
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
                score: 0.0,
                score_breakdown: strata_engine::ScoreBreakdown {
                    cut: 0.0,
                    imbalance: 0.0,
                    naming: 0.0,
                    path: 0.0,
                    anchor: 0.0,
                },
                violations: Vec::new(),
            },
            modes: strata_engine::Modes::default(),
        };

        let found = mode_result(&result, "anchored");

        assert!(matches!(found, Err(StrataError::CandidateNotFound { .. })));
    }
}
