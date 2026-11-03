"""Beta: closes the import cycle with alpha."""

from dupflat.alpha import alpha_step


def beta_step(value: int) -> int:
    return alpha_step(value)
