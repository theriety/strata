"""Alpha: forms an import cycle with beta."""

from dupflat.beta import beta_step


def alpha_step(value: int) -> int:
    if value <= 0:
        return 0
    return beta_step(value - 1)
