"""A concrete rectangle."""

from pkg.shape import Shape, describe_shape


class Rectangle(Shape):
    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height

    def area(self) -> int:
        return self.width * self.height


def summarize(rectangle: Rectangle) -> str:
    return describe_shape(rectangle)
