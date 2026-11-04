//! Scale helpers. Only `alpha` calls them, so they belong beside it.

/// The unit magnitude.
pub fn unit() -> u64 {
    1
}

/// Scales a magnitude.
pub fn scale(raw: u64) -> u64 {
    raw * 3
}

/// Clamps a magnitude to the reporting range.
pub fn clamp(raw: u64) -> u64 {
    raw.min(1_000)
}
