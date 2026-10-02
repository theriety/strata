//! Language selection and per-language adapter dispatch.

use std::path::Path;

use strata_ir::{IrFragment, SourceFile};

use super::Language;
use crate::config::AnalyzeConfig;
use crate::error::StrataError;

/// Maps the config's language names to [`Language`] values, dropping any the
/// engine does not recognize (validation rejects unknown names upstream).
pub(super) fn enabled_languages(config: &AnalyzeConfig) -> Vec<Language> {
    config
        .adapters
        .languages
        .iter()
        .filter_map(|name| Language::from_name(name))
        .collect()
}

/// Partitions the discovered sources by language, dropping files that match no
/// enabled language.
pub(super) fn group_by_language(
    files: &[SourceFile],
    languages: &[Language],
) -> Vec<(Language, Vec<SourceFile>)> {
    languages
        .iter()
        .map(|&language| {
            let sources = files
                .iter()
                .filter(|file| language.matches_extension(&file.path))
                .cloned()
                .collect::<Vec<_>>();
            (language, sources)
        })
        .filter(|(_, sources)| !sources.is_empty())
        .collect()
}

/// Runs one language adapter's parse and bind phases over its source set.
pub(super) fn run_adapter(
    language: Language,
    sources: &[SourceFile],
    root: &Path,
) -> Result<IrFragment, StrataError> {
    let adapter = language.adapter(root);
    let trees = adapter
        .parse(sources)
        .map_err(|source| StrataError::AdapterParseFailure { source })?;
    adapter
        .bind(trees)
        .map_err(|source| StrataError::AdapterBindFailure { source })
}
