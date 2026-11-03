//! Reading, parsing, and validating a `unlace.toml` file.

use std::path::Path;

use crate::error::UnlaceError;

use super::AnalyzeConfig;

/// Parses and validates a `unlace.toml` from `path`, applying defaults to every
/// omitted key.
///
/// Unknown keys, type mismatches, and out-of-range values all surface as
/// [`UnlaceError::ConfigInvalid`]; a missing or unreadable file surfaces as
/// [`UnlaceError::InputUnreadable`]. Callers that want the no-config baseline
/// should use [`AnalyzeConfig::default`] rather than this function.
///
/// # Errors
///
/// Returns [`UnlaceError::InputUnreadable`] when the file cannot be read and
/// [`UnlaceError::ConfigInvalid`] when it cannot be parsed or fails validation.
pub fn load_config(path: impl AsRef<Path>) -> Result<AnalyzeConfig, UnlaceError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|error| UnlaceError::InputUnreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let document: toml::Value =
        toml::from_str(&text).map_err(|error| UnlaceError::ConfigInvalid {
            key: None,
            reason: error.message().to_owned(),
        })?;
    reject_legacy_keys(&document)?;
    let config: AnalyzeConfig =
        toml::from_str(&text).map_err(|error| UnlaceError::ConfigInvalid {
            key: None,
            reason: error.message().to_owned(),
        })?;
    config.validate()?;
    Ok(config)
}

/// Rejects removed configuration locations before typed deserialization so the
/// diagnostic retains the complete offending TOML key.
fn reject_legacy_keys(document: &toml::Value) -> Result<(), UnlaceError> {
    const ANALYSIS_KEYS: [&str; 3] = ["mode", "candidates", "seed"];
    const TOP_LEVEL_KEYS: [&str; 5] = ["capacity", "objective", "weights", "solver", "diversity"];

    if let Some(analysis) = document.get("analysis").and_then(toml::Value::as_table) {
        for field in ANALYSIS_KEYS {
            if analysis.contains_key(field) {
                return Err(unknown_legacy_key(format!("analysis.{field}"), field));
            }
        }
    }
    for field in TOP_LEVEL_KEYS {
        if document.get(field).is_some() {
            return Err(unknown_legacy_key(field.to_owned(), field));
        }
    }
    Ok(())
}

fn unknown_legacy_key(key: String, field: &str) -> UnlaceError {
    UnlaceError::ConfigInvalid {
        key: Some(key),
        reason: format!("unknown field `{field}`"),
    }
}
