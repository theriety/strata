"""ingest stage 07: one link of the ingest chain."""

from .ingest_08 import stage_08

def stage_07(value: int) -> int:
    return stage_08(value + 7)
