"""ingest stage 05: one link of the ingest chain."""

from .ingest_06 import stage_06

def stage_05(value: int) -> int:
    return stage_06(value + 5)
