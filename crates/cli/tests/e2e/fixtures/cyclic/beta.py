"""Beta module: closes the import cycle back onto alpha."""

from alpha import alpha_step


def beta_step(value: int) -> int:
    if value <= 0:
        return 0
    return alpha_step(value - 1)
