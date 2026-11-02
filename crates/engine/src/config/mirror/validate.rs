//! Key-attributed validation of a single mirror template string.

use crate::error::StrataError;

use super::MirrorTemplate;

pub(in crate::config) fn validate_mirror_template(
    template: &str,
    key: &str,
) -> Result<(), StrataError> {
    if template.is_empty()
        || template.starts_with('/')
        || template.split('/').any(|part| part == "..")
    {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror templates must be non-empty repo-relative paths".to_owned(),
        });
    }
    let mut rest = template;
    let mut placeholders = Vec::new();
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(StrataError::ConfigInvalid {
                key: Some(key.to_owned()),
                reason: "unterminated mirror placeholder".to_owned(),
            });
        };
        let placeholder = &after[..end];
        if !matches!(placeholder, "dir" | "stem") {
            return Err(StrataError::ConfigInvalid {
                key: Some(key.to_owned()),
                reason: "unknown mirror placeholder; expected {dir} or {stem}".to_owned(),
            });
        }
        placeholders.push(placeholder);
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "unmatched mirror placeholder terminator".to_owned(),
        });
    }
    if placeholders != ["dir", "stem"] {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror templates require exactly one {dir} followed by exactly one {stem}"
                .to_owned(),
        });
    }
    if MirrorTemplate::parse(template).is_none() {
        return Err(StrataError::ConfigInvalid {
            key: Some(key.to_owned()),
            reason: "mirror template grammar requires {dir} as a complete path segment and {stem} in the final filename segment"
                .to_owned(),
        });
    }
    Ok(())
}
