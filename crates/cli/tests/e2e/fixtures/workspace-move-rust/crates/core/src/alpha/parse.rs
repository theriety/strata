//! Parses raw magnitudes into scaled weights.

use crate::beta::scale::{clamp, scale, unit};
use fixture_move_util::Weight;

/// Parses a magnitude into a clamped, scaled weight.
pub fn parse(raw: u64) -> Weight {
    Weight::new(clamp(scale(raw) + unit()))
}

/// Parses two magnitudes and keeps the larger.
pub fn parse_max(left: u64, right: u64) -> Weight {
    Weight::new(clamp(scale(left).max(scale(right))))
}
