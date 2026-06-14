//! A small fixture crate: a shape trait and a rectangle that implements it.

/// A measurable two-dimensional shape.
pub trait Shape {
    /// Returns the shape's area.
    fn area(&self) -> u64;
}

/// Describes a shape's area as a one-line string.
pub fn describe_shape(shape: &dyn Shape) -> String {
    format!("area={}", shape.area())
}

/// A rectangle defined by its width and height.
pub struct Rectangle {
    /// The rectangle's width.
    pub width: u64,
    /// The rectangle's height.
    pub height: u64,
}

impl Shape for Rectangle {
    fn area(&self) -> u64 {
        self.width * self.height
    }
}

/// Summarizes a rectangle through the shape contract.
pub fn summarize(rectangle: &Rectangle) -> String {
    describe_shape(rectangle)
}
