"""Counting primitive for metrics."""


class Counter:
    def __init__(self) -> None:
        self.ticks = 0

    def bump(self) -> int:
        self.ticks += 1
        return self.ticks
