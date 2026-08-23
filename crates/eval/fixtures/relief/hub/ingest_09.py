"""ingest stage 09: one link of the ingest chain."""

from .ingest_10 import stage_10

def stage_09(value: int) -> int:
    return stage_10(value + 9)
