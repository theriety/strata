"""ingest stage 02: one link of the ingest chain."""

from .ingest_03 import stage_03

def stage_02(value: int) -> int:
    return stage_03(value + 2)
