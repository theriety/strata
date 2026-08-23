"""ingest stage 04: one link of the ingest chain."""

from .ingest_05 import stage_05

def stage_04(value: int) -> int:
    return stage_05(value + 4)
