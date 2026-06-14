"""Shape primitives."""


class Shape:
    """Base shape contract."""

    def area(self) -> int:
        raise NotImplementedError


def describe_shape(shape: Shape) -> str:
    return f"area={shape.area()}"
