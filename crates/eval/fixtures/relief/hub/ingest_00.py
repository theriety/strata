"""ingest stage 00: one link of the ingest chain."""

from .ingest_01 import stage_01

def stage_00(value: int) -> int:
    return stage_01(value + 0)
