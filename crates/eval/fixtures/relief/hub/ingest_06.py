"""ingest stage 06: one link of the ingest chain."""

from .ingest_07 import stage_07

def stage_06(value: int) -> int:
    return stage_07(value + 6)
