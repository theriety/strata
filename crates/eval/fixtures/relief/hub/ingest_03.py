"""ingest stage 03: one link of the ingest chain."""

from .ingest_04 import stage_04

def stage_03(value: int) -> int:
    return stage_04(value + 3)
