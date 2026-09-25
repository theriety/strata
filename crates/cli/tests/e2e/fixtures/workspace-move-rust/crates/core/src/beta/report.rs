//! Renders weights for display.

use fixture_move_util::Weight;

/// Renders one weight.
pub fn render(weight: &Weight) -> String {
    format!("weight {}", weight.value)
}

/// Renders a list of weights.
pub fn render_all(weights: &[Weight]) -> Vec<String> {
    weights.iter().map(render).collect()
}
