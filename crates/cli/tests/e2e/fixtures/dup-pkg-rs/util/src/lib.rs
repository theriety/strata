//! Leaf crate: one value type the `core` crate builds on.

/// A raw magnitude shared across the workspace.
pub struct Weight {
    /// The wrapped magnitude.
    pub value: u64,
}

impl Weight {
    /// Wraps a magnitude.
    pub fn new(value: u64) -> Self {
        Self { value }
    }
}
