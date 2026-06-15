//! Middle crate: depends on `util`, implements its trait, exposes a new type.

use fixture_util::{Measure, Summarize};

/// A named report built around a `util::Measure`.
pub struct Report {
    /// Human-readable label.
    pub label: String,
    /// The underlying measure (cross-crate type reference).
    pub measure: Measure,
}

impl Report {
    /// Builds a report from a label and a raw magnitude.
    pub fn new(label: &str, magnitude: u64) -> Self {
        Self {
            label: label.to_string(),
            measure: Measure::new(magnitude),
        }
    }

    /// Returns twice the magnitude via a cross-crate method call.
    pub fn doubled(&self) -> u64 {
        self.measure.doubled()
    }
}

impl Summarize for Report {
    fn summarize(&self) -> String {
        format!("{}: {}", self.label, self.measure.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared test utility: builds a sample report. Used only by test cases, so
    /// it classifies as test *support* rather than a test case.
    fn sample_report() -> Report {
        Report::new("sample", 21)
    }

    #[test]
    fn report_doubles_its_measure() {
        let report = sample_report();
        assert_eq!(report.doubled(), 42);
    }
}
