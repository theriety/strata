"""Rectangle shape."""

from .shape import Shape, Dimensions


class Rectangle(Shape):
    def __init__(self, dimensions: Dimensions):
        self.dimensions = dimensions

    def area(self) -> int:
        return self.dimensions.width * self.dimensions.height


def describe(shape: Shape) -> str:
    return "shape"
