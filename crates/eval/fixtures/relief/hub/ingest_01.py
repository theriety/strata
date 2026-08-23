"""ingest stage 01: one link of the ingest chain."""

from .ingest_02 import stage_02

def stage_01(value: int) -> int:
    return stage_02(value + 1)
