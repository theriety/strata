"""Geometry primitives."""


class Dimensions:
    """A width/height pair."""

    def __init__(self, width, height):
        self.width = width
        self.height = height


class Shape:
    """Base shape contract."""

    def area(self) -> int:
        raise NotImplementedError
