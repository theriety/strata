//! Naming tokens: the shared tokenizer, basename, and token-set similarity.

use std::collections::BTreeSet;

/// Splits a path's basename into lowercase naming tokens.
///
/// The final extension is dropped, then the stem splits on every
/// non-alphanumeric separator and every lower-to-upper camel-case boundary:
/// `user-service.ts` and `UserService.spec.ts` share `{user, service}`. This is
/// the one tokenizer both the naming-cohesion objective term and the narration
/// reason consult, so the two never disagree about similarity.
pub(crate) fn tokenize(name: &str) -> BTreeSet<String> {
    let base = basename(name);
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);

    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    let mut previous_was_lower = false;
    for ch in stem.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && previous_was_lower && !current.is_empty() {
                tokens.insert(current.to_lowercase());
                current = String::new();
            }
            current.push(ch);
            previous_was_lower = ch.is_lowercase() || ch.is_numeric();
        } else {
            if !current.is_empty() {
                tokens.insert(current.to_lowercase());
                current = String::new();
            }
            previous_was_lower = false;
        }
    }
    if !current.is_empty() {
        tokens.insert(current.to_lowercase());
    }
    tokens
}

/// Returns the Jaccard similarity of two token sets (`0.0` when both are empty).
pub(super) fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = f64::from(u32::try_from(intersection).unwrap_or(u32::MAX));
    let union = f64::from(u32::try_from(union).unwrap_or(u32::MAX));
    intersection / union
}

/// Returns the final path segment.
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
