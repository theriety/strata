"""ingest stage 10: one link of the ingest chain."""

from .ingest_11 import stage_11

def stage_10(value: int) -> int:
    return stage_11(value + 10)
