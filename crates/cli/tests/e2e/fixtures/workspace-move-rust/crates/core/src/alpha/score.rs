//! Scores parsed weights.

use crate::alpha::parse::parse;
use crate::beta::scale::{clamp, scale, unit};

/// Scores a raw magnitude.
pub fn score(raw: u64) -> u64 {
    clamp(parse(raw).value + scale(unit()))
}

/// Scores a pair of raw magnitudes.
pub fn score_pair(left: u64, right: u64) -> u64 {
    clamp(scale(left) + scale(right))
}
