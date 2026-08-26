"""Emitter stage 04 that refines the shared emitter seed."""

from .emit_00 import emit_seed


def emit_stage(value: int) -> int:
    return emit_seed(value) + 4
