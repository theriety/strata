"""A small module that exceeds the fixture's one-SLOC file cap."""

def first(value: int) -> int:
    doubled = value * 2
    return doubled + 1

def second(value: int) -> int:
    tripled = value * 3
    return tripled + 2
