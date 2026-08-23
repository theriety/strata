"""Area computation over shapes."""

from .shape import Shape


def of(shape: Shape) -> int:
    return len(shape.name())
