"""Rate derivation over counters."""

from .counter import Counter


def per_second(counter: Counter, seconds: int) -> float:
    return counter.ticks / seconds
