//! Top crate: consumes both `core` and `util` through use-paths, method calls,
//! and a trait imported from `util` but implemented in `core`.

use fixture_core::Report;
use fixture_util::{Measure, Summarize};

/// Re-exports the leaf-crate `Measure` type through `app`'s public surface.
pub use fixture_util::Measure as ReMeasure;

/// Builds a report and returns its trait-driven summary.
pub fn describe(label: &str, magnitude: u64) -> String {
    let report = Report::new(label, magnitude);
    report.summarize()
}

/// Sums the doubled magnitudes of a report and a bare measure.
pub fn combined_total(label: &str, magnitude: u64, extra: u64) -> u64 {
    let report = Report::new(label, magnitude);
    let measure = Measure::new(extra);
    report.doubled() + measure.doubled()
}
