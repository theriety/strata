//! Leaf crate of the fixture: a value type and a trait the other crates use.

/// A measurable quantity carried across crate boundaries.
pub struct Measure {
    /// The wrapped magnitude.
    pub value: u64,
}

impl Measure {
    /// Builds a measure from a raw magnitude.
    pub fn new(value: u64) -> Self {
        Self { value }
    }

    /// Doubles the magnitude (method call resolved cross-crate).
    pub fn doubled(&self) -> u64 {
        self.value * 2
    }
}

/// A trait implemented in `core` and consumed in `app`.
pub trait Summarize {
    /// Produces a one-line summary.
    fn summarize(&self) -> String;
}
