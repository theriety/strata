"""Facade: touches each feature once, owns no logic of its own."""

from geometry.area import of as area_of
from geometry.shape import Shape
from metrics.counter import Counter
from metrics.rate import per_second
from textwrap.columns import columns


def main() -> str:
    shape = Shape()
    counter = Counter()
    counter.bump()
    return "{}, {}, {}".format(area_of(shape), per_second(counter, 1), columns("abc", 1, 2))
