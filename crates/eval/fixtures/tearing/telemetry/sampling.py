"""Sampled emission on top of the sink."""

from .sink import emit


def sample(event: str, every: int, tick: int) -> bool:
    if tick % every == 0:
        emit(event)
        return True
    return False
